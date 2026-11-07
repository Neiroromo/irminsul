use std::collections::HashMap;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use protobuf::Message;
use protobuf::UnknownValueRef::*;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};

use crate::r#gen::protos::AvatarInfo;
use crate::r#gen::protos::Item;
use crate::r#gen::protos::Unk;

pub fn matches_get_player_token_rsp(
    data: Vec<u8>,
    rsa_keys: Vec<RsaPrivateKey>,
) -> Option<Vec<u64>> {
    let d_msg = Unk::parse_from_bytes(&data);
    match d_msg {
        Ok(d_msg) => {
            let mut to_ret: Vec<u64> = vec![];
            let unknown_fields = d_msg.unknown_fields();
            for (field_number, field_data) in unknown_fields.iter() {
                tracing::debug!("field: {}: {:?}", field_number, field_data);
                let possible_encrypted = match field_data {
                    LengthDelimited(encrypted_bytes) => {
                        let encrypted = BASE64_STANDARD.decode(encrypted_bytes);
                        match encrypted {
                            Ok(encrypted) => Some(encrypted),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let possible_seeds: Vec<u64> = match possible_encrypted {
                    Some(possible_encrypted) => rsa_keys
                        .iter()
                        .filter_map(|key| key.decrypt(Pkcs1v15Encrypt, &possible_encrypted).ok())
                        .collect::<Vec<Vec<u8>>>()
                        .iter()
                        .filter(|&seed| seed.len() == 8)
                        .map(|seed| u64::from_be_bytes(seed.as_slice().try_into().unwrap()))
                        .collect(),
                    _ => vec![],
                };
                to_ret.extend(possible_seeds)
            }
            if to_ret.len() != 0 {
                Some(to_ret)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[derive(Clone, Default)]
pub struct Achievement {
    pub id: u32,
    pub status: u32,
    pub finish_timestamp: Option<u32>,
}

pub fn matches_achievement_all_data_notify(data: Vec<u8>) -> Option<Vec<Achievement>> {
    if data.len() < 1000 {
        return None;
    }
    let d_msg = Unk::parse_from_bytes(&data);
    match d_msg {
        Ok(d_msg) => {
            let mut achievement_list: Vec<HashMap<u32, u64>> = vec![];
            let mut list_tag: Option<u32> = None;
            let unknown_fields = d_msg.unknown_fields();
            // let tags = unknown_fields.iter().map(|(tag, _)| tag).collect::<HashSet<u32>>();
            // if tags.len() != 2 { return None }
            for (field_number, field_data) in unknown_fields.iter() {
                match field_data {
                    LengthDelimited(bytes) => {
                        let d_msg_inside = Unk::parse_from_bytes(bytes);
                        let unknown_fields_inside;
                        match d_msg_inside {
                            Ok(d_msg_inside) => {
                                unknown_fields_inside = d_msg_inside.unknown_fields().clone()
                            }
                            _ => continue,
                        }
                        let mut achievement_map: HashMap<u32, u64> = HashMap::new();
                        for (field_number_inside, field_data_inside) in unknown_fields_inside.iter()
                        {
                            match field_data_inside {
                                Varint(value) => {
                                    let _ = achievement_map.insert(field_number_inside, value);
                                }
                                _ => return None, // because proto has only repeated Achievement and repeated uint32, this isn't the right packet.
                            }
                        }
                        achievement_list.push(achievement_map);
                        match list_tag {
                            Some(x) => {
                                if field_number != x {
                                    return None;
                                } // if we found several possible tags for the list. Not possible.
                            }
                            None => list_tag = Some(field_number),
                        }
                    }
                    _ => (),
                }
            }
            if achievement_list.len() == 0 {
                return None;
            }

            // Now, try to find which field corresponds to the right places
            let mut tag_finish_timestamp = None;
            let mut tag_id = None;
            let mut possible_tag_status: Vec<u32> =
                achievement_list[0].clone().into_keys().collect();
            for achievement_map in &achievement_list {
                for (&tag, &value) in achievement_map.iter() {
                    if value > 1420066800 {
                        // Wed Dec 31 2014 23:00:00 GMT+0000
                        tag_finish_timestamp = match tag_finish_timestamp {
                            Some(t) => {
                                if t != tag {
                                    return None;
                                } else {
                                    tag_finish_timestamp
                                }
                            }
                            _ => Some(tag),
                        }
                    }
                    if value == 80014 {
                        // Onward and Upward: Ascend a character to Phase 2 for the first time
                        tag_id = Some(tag)
                    }
                    if possible_tag_status.contains(&tag) {
                        if value > 3 {
                            possible_tag_status.retain(|&x| x != tag)
                        }
                    }
                }
            }

            if tag_finish_timestamp == None || tag_id == None || possible_tag_status.len() == 0 {
                return None;
            }

            // Finally, collect the Achievements
            let tag_status = possible_tag_status[0];
            let mut achievements: Vec<Achievement> = vec![];
            for achievement_map in &achievement_list {
                let mut achievement = Achievement {
                    ..Default::default()
                };
                for (&tag, &value) in achievement_map.iter() {
                    if tag_finish_timestamp.unwrap() == tag {
                        achievement.finish_timestamp = Some(value as u32);
                    }
                    if tag_id.unwrap() == tag {
                        achievement.id = value as u32;
                    }
                    if tag_status == tag {
                        achievement.status = value as u32;
                    }
                }
                achievements.push(achievement)
            }
            assert!(achievements.len() > 0);
            Some(achievements)
        }
        _ => None,
    }
}

/// Finds the repeated `T` field of a packet by content.
///
/// Command ids and field numbers are reshuffled with every game version, so
/// instead of a fixed schema the packet is parsed as a generic message, every
/// length-delimited field is tried as a `T`, and the field number with the most
/// valid entries wins.  Fields where too many entries don't validate are
/// rejected, as they are something else that happens to parse.
fn find_best_field<T: Message>(data: &[u8], is_valid: impl Fn(&T) -> bool) -> Option<Vec<T>> {
    let message = Unk::parse_from_bytes(data).ok()?;

    // Field number -> (entries seen, valid entries).
    let mut fields: HashMap<u32, (usize, Vec<T>)> = HashMap::new();
    for (field_number, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        let (seen, valid) = fields.entry(field_number).or_default();
        *seen += 1;
        if let Ok(entry) = T::parse_from_bytes(bytes)
            && is_valid(&entry)
        {
            valid.push(entry);
        }
    }

    fields
        .into_values()
        .filter(|(seen, valid)| valid.len() * 5 >= seen * 4)
        .map(|(_, valid)| valid)
        .max_by_key(|valid| valid.len())
}

pub fn matches_items_all_data_notify(data: &[u8]) -> Option<Vec<Item>> {
    let items = find_best_field::<Item>(data, |item| {
        item.item_id != 0
            && item.guid != 0
            && (item.has_material() || item.has_equip() || item.has_furniture())
    })?;

    // Differentiate items packets from other that look alike.
    if items.len() < 10 {
        return None;
    }

    Some(items)
}

/// The game says a stack of materials changed in a small message of its own,
/// with the item (its id, its guid, how many there are now, not how many were
/// added) and nothing else of the same kind in it.  Found by content, like the
/// full lists; a few at most, since more is a list.
pub fn matches_item_update(data: &[u8]) -> Option<Vec<Item>> {
    if data.len() > 600 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let mut items = Vec::new();
    let mut other = 0;
    for (_, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        match Item::parse_from_bytes(bytes) {
            Ok(item)
                if (100..1_000_000).contains(&item.item_id)
                    && item.guid >= 1_000_000
                    && (item.has_material()
                        // A weapon says its level and ascension when they
                        // change.  Combat messages have things that look like
                        // equipment too, with small guids; weapons of the
                        // account have big ones.
                        || (item.has_equip()
                            && item.equip().has_weapon()
                            && item.guid >= 1_000_000_000_000)) =>
            {
                items.push(item)
            }
            _ => other += 1,
        }
    }
    // Anything else of this size in it makes it a message of another kind that
    // has an item in it.
    (!items.is_empty() && items.len() <= 8 && other == 0).then_some(items)
}

/// What the game says about a character's properties when they change: the
/// level, the experience, the ascension.
pub struct AvatarPropUpdate {
    /// Numbers in the message that can be the character's guid; which of them
    /// is one is for the caller to tell, who knows the characters.
    pub guids: Vec<u64>,
    /// Property id and what it is now.
    pub props: Vec<(u32, i64)>,
}

/// Guids are big numbers; the counters and times beside them are not the guids
/// of anyone, which the caller finds out.
fn guid_candidates(message: &Unk) -> Vec<u64> {
    message
        .unknown_fields()
        .iter()
        .filter_map(|(_, value)| match value {
            Varint(v) if v >= 1_000_000_000 => Some(v),
            _ => None,
        })
        .collect()
}

/// A property that changed, as the game says it: entries of an id and a number,
/// and the guid of the character beside them.  Matched by content.
pub fn matches_avatar_prop_update(data: &[u8]) -> Option<AvatarPropUpdate> {
    if data.len() > 400 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let mut props = Vec::new();
    for (_, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        let Ok(entry) = Unk::parse_from_bytes(bytes) else {
            continue;
        };
        // Exactly an id and a value, which also leaves out the entries that
        // have the value in a message of its own.
        if entry.unknown_fields().iter().count() != 2 {
            continue;
        }
        if let (Some(Varint(key)), Some(Varint(number))) =
            (entry.unknown_fields().get(1), entry.unknown_fields().get(2))
            && (1_000..5_000).contains(&key)
        {
            props.push((key as u32, number as i64));
        }
    }
    // Level (4001), experience (1001) or ascension (1002) is what is of use.
    let useful = props.iter().any(|(key, _)| matches!(key, 1001 | 1002 | 4001));
    let guids = guid_candidates(&message);
    (useful && !guids.is_empty()).then_some(AvatarPropUpdate { guids, props })
}

/// The numbers the game says a character's stats are now, when they change:
/// pairs of an id and a number with a fraction, and the guid beside them.
pub fn matches_avatar_fight_props(data: &[u8]) -> Option<(Vec<u64>, Vec<(u32, f32)>)> {
    if data.len() > 800 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let mut stats = Vec::new();
    for (_, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        let Ok(entry) = Unk::parse_from_bytes(bytes) else {
            continue;
        };
        if entry.unknown_fields().iter().count() != 2 {
            continue;
        }
        if let (Some(Varint(key)), Some(Fixed32(bits))) =
            (entry.unknown_fields().get(1), entry.unknown_fields().get(2))
            && key < 5_000
        {
            let number = f32::from_bits(bits);
            if number.is_finite() {
                stats.push((key as u32, number));
            }
        }
    }
    let guids = guid_candidates(&message);
    (stats.len() >= 3 && !guids.is_empty()).then_some((guids, stats))
}

/// A talent that went up a level: numbers only, among them the guid of the
/// character, the id of the talent, and the level before and after.
pub struct TalentUpdate {
    pub guids: Vec<u64>,
    /// Numbers that can be the id of the talent.
    pub ids: Vec<u64>,
    /// Small numbers, the levels among them.
    pub levels: Vec<u64>,
}

pub fn matches_talent_update(data: &[u8]) -> Option<TalentUpdate> {
    if data.len() > 80 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let (mut guids, mut ids, mut levels) = (Vec::new(), Vec::new(), Vec::new());
    for (_, value) in message.unknown_fields().iter() {
        match value {
            Varint(v) if v >= 1_000_000_000_000 => guids.push(v),
            Varint(v) if (1_000..1_000_000).contains(&v) => ids.push(v),
            Varint(v) if v <= 15 => levels.push(v),
            Varint(_) => (),
            // Something that is not only numbers is another kind of message.
            _ => return None,
        }
    }
    (!guids.is_empty() && !ids.is_empty() && levels.len() >= 2)
        .then_some(TalentUpdate { guids, ids, levels })
}

pub fn matches_avatars_all_data_notify(data: &[u8]) -> Option<Vec<AvatarInfo>> {
    let avatars = find_best_field::<AvatarInfo>(data, |avatar| {
        avatar.avatar_id != 0
            && avatar.guid != 0
            && !avatar.prop_map.is_empty()
            && !avatar.skill_level_map.is_empty()
    })?;

    // Incremental updates carry a single avatar, the full list has more.
    if avatars.len() < 2 {
        return None;
    }

    Some(avatars)
}

/// Player ids have nine digits.
const UID_RANGE: std::ops::RangeInclusive<u64> = 100_000_000..=999_999_999;

/// Every varint of `data` read as a generic message, going into nested
/// messages down to `max_level`, as (level, field number, value).
fn varints_at(data: &[u8], level: u32, max_level: u32, found: &mut Vec<(u32, u32, u64)>) {
    let Ok(message) = Unk::parse_from_bytes(data) else {
        return;
    };
    for (field, value) in message.unknown_fields().iter() {
        match value {
            Varint(value) => found.push((level, field, value)),
            LengthDelimited(bytes) if level < max_level => {
                varints_at(bytes, level + 1, max_level, found)
            }
            _ => (),
        }
    }
}

/// The player's id in the body of the server's answer to the token request:
/// the only number with nine digits in it.  Matched by content, as field
/// numbers change with the game version.
pub fn find_player_uid(body: &[u8]) -> Option<u32> {
    let mut found = Vec::new();
    varints_at(body, 0, 1, &mut found);
    found
        .into_iter()
        .filter(|(_, _, value)| UID_RANGE.contains(value))
        .min_by_key(|(level, _, _)| *level)
        .map(|(_, _, value)| value as u32)
}

/// A string that can be a player's name.
fn plausible_nickname(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let length = text.chars().count();
    if !(1..=24).contains(&length)
        || text.chars().any(|c| c.is_control())
        || text.trim() != text
        || text.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(text.to_owned())
}

/// An entry of the player property map: a key in the range the player's
/// properties have, and a value message.
fn is_player_prop_entry(bytes: &[u8]) -> bool {
    let Ok(entry) = Unk::parse_from_bytes(bytes) else {
        return false;
    };
    let key = matches!(entry.unknown_fields().get(1), Some(Varint(key)) if (10000..=10999).contains(&key));
    let value = matches!(entry.unknown_fields().get(2), Some(LengthDelimited(_)));
    key && value
}

fn nickname_in(data: &[u8], level: u32) -> Option<String> {
    let message = Unk::parse_from_bytes(data).ok()?;
    let mut names = Vec::new();
    let mut nested = Vec::new();
    let mut prop_entries = 0;
    for (_, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        if is_player_prop_entry(bytes) {
            prop_entries += 1;
        } else {
            if let Some(name) = plausible_nickname(bytes) {
                names.push(name);
            }
            nested.push(bytes.to_vec());
        }
    }

    // The message that carries the player's properties carries the name too.
    if prop_entries >= 6 {
        if let Some(name) = names.into_iter().next() {
            return Some(name);
        }
    }
    if level < 1 {
        return nested.iter().find_map(|nested| nickname_in(nested, level + 1));
    }
    None
}

/// The mora in a player property entry: key 10016, and a value message that
/// names the same property and carries the number.
fn mora_entry(bytes: &[u8]) -> Option<u64> {
    // PROP_PLAYER_SCOIN.  (10004 is a flag that is often 1, and 10013 is
    // the player's level.)
    const MORA_PROP: u64 = 10016;
    let entry = Unk::parse_from_bytes(bytes).ok()?;
    let Some(Varint(MORA_PROP)) = entry.unknown_fields().get(1) else {
        return None;
    };
    let Some(LengthDelimited(value)) = entry.unknown_fields().get(2) else {
        return None;
    };
    let value = Unk::parse_from_bytes(value).ok()?;
    let Some(Varint(MORA_PROP)) = value.unknown_fields().get(1) else {
        return None;
    };
    match (value.unknown_fields().get(4), value.unknown_fields().get(2)) {
        (Some(Varint(mora)), _) | (_, Some(Varint(mora))) => Some(mora),
        _ => None,
    }
}

fn mora_in(data: &[u8], level: u32) -> Option<u64> {
    let message = Unk::parse_from_bytes(data).ok()?;
    for (_, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        if let Some(mora) = mora_entry(bytes) {
            return Some(mora);
        }
        if level < 1
            && let Some(mora) = mora_in(bytes, level + 1)
        {
            return Some(mora);
        }
    }
    None
}

/// Every property of the player in the message that has them: the key and
/// the number.  For the log, to see what the game sends.
pub fn find_player_props(body: &[u8]) -> Option<Vec<(u64, u64)>> {
    if body.len() > 30_000 {
        return None;
    }
    let message = Unk::parse_from_bytes(body).ok()?;
    let mut found = Vec::new();
    for (_, value) in message.unknown_fields().iter() {
        let LengthDelimited(bytes) = value else {
            continue;
        };
        let Ok(entry) = Unk::parse_from_bytes(bytes) else {
            continue;
        };
        let (Some(Varint(key)), Some(LengthDelimited(inner))) =
            (entry.unknown_fields().get(1), entry.unknown_fields().get(2))
        else {
            continue;
        };
        let Ok(inner) = Unk::parse_from_bytes(inner) else {
            continue;
        };
        if !matches!(inner.unknown_fields().get(1), Some(Varint(same)) if same == key) {
            continue;
        }
        if let (Some(Varint(number)), _) | (_, Some(Varint(number))) =
            (inner.unknown_fields().get(4), inner.unknown_fields().get(2))
        {
            found.push((key, number));
        }
    }
    // The message with the player's properties has many.
    (found.len() >= 6).then_some(found)
}

/// The player's mora, from a message with the player's properties (it is not
/// among the items).  Matched by content, whatever the command id is.
pub fn find_player_mora(body: &[u8]) -> Option<u64> {
    // The messages with properties are small; the big ones are item lists.
    if body.len() > 30_000 {
        return None;
    }
    mora_in(body, 0)
}

/// The player's name, from the message with the player's properties.
pub fn find_player_nickname(body: &[u8]) -> Option<String> {
    nickname_in(body, 0)
}

#[cfg(test)]
mod tests {
    use protobuf::CodedOutputStream;

    use super::*;
    use crate::r#gen::protos::PropValue;

    /// A packet holding `messages` as repeated length-delimited `field`, next
    /// to a couple of unrelated fields.
    fn packet(field: u32, messages: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint32(1, 7).unwrap();
            for message in messages {
                writer.write_bytes(field, message).unwrap();
            }
            writer.write_bytes(field + 20, b"not a message").unwrap();
            writer.flush().unwrap();
        }
        out
    }

    fn items(count: u32) -> Vec<Vec<u8>> {
        (0..count)
            .map(|i| {
                let mut item = Item::new();
                item.item_id = 100 + i;
                item.guid = 1000 + u64::from(i);
                item.mut_material().count = 3;
                item.write_to_bytes().unwrap()
            })
            .collect()
    }

    fn avatars(count: u32) -> Vec<Vec<u8>> {
        (0..count)
            .map(|i| {
                let mut avatar = AvatarInfo::new();
                avatar.avatar_id = 10000000 + i;
                avatar.guid = 2000 + u64::from(i);
                avatar.equip_guid_list = vec![1000, 1001, 1002];
                let mut level = PropValue::new();
                level.val = 90;
                avatar.prop_map.insert(4001, level);
                avatar.skill_level_map.insert(10001, 9);
                avatar.write_to_bytes().unwrap()
            })
            .collect()
    }

    #[test]
    fn items_are_found_at_any_field_number() {
        for field in [2, 5, 6, 11, 1000] {
            let found = matches_items_all_data_notify(&packet(field, &items(25)));
            assert_eq!(found.map(|items| items.len()), Some(25), "field {field}");
        }
    }

    #[test]
    fn few_items_are_not_a_full_inventory() {
        assert!(matches_items_all_data_notify(&packet(6, &items(3))).is_none());
    }

    #[test]
    fn avatars_are_found_at_any_field_number() {
        for field in [2, 6, 7, 14, 1000] {
            let found = matches_avatars_all_data_notify(&packet(field, &avatars(12)));
            assert_eq!(found.map(|avatars| avatars.len()), Some(12), "field {field}");
        }
    }

    #[test]
    fn single_avatar_is_not_the_full_list() {
        assert!(matches_avatars_all_data_notify(&packet(6, &avatars(1))).is_none());
    }

    #[test]
    fn items_are_not_avatars() {
        assert!(matches_avatars_all_data_notify(&packet(6, &items(40))).is_none());
    }

    #[test]
    fn avatar_list_is_matched_as_avatars() {
        assert!(matches_avatars_all_data_notify(&packet(6, &avatars(40))).is_some());
    }

    #[test]
    fn unrelated_data_is_not_matched() {
        assert!(matches_items_all_data_notify(&[]).is_none());
        assert!(matches_avatars_all_data_notify(&[]).is_none());
        let noise: Vec<u8> = (0..4000u32).map(|i| (i * 31 % 251) as u8).collect();
        assert!(matches_items_all_data_notify(&noise).is_none());
        assert!(matches_avatars_all_data_notify(&noise).is_none());
        let strings = vec![b"hello world".to_vec(); 30];
        assert!(matches_items_all_data_notify(&packet(3, &strings)).is_none());
    }

    fn prop_entry(key: u32) -> Vec<u8> {
        let mut value = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut value);
            writer.write_uint32(1, key).unwrap();
            writer.write_int64(4, 60).unwrap();
            writer.flush().unwrap();
        }
        let mut entry = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut entry);
            writer.write_uint32(1, key).unwrap();
            writer.write_bytes(2, &value).unwrap();
            writer.flush().unwrap();
        }
        entry
    }

    /// A message like the one with the player's name and properties.
    fn player_data(name: Option<&str>, props: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            if let Some(name) = name {
                writer.write_string(2, name).unwrap();
            }
            writer.write_uint64(3, 1_793_900_000).unwrap();
            writer.write_bool(5, true).unwrap();
            for index in 0..props {
                writer.write_bytes(9, &prop_entry(10001 + index)).unwrap();
            }
            writer.flush().unwrap();
        }
        out
    }

    #[test]
    fn the_mora_is_the_property_10016() {
        // Properties 10001 to 10016, each worth 60.
        assert_eq!(find_player_mora(&player_data(Some("Name"), 16)), Some(60));
        // Only 10001 to 10004.
        assert_eq!(find_player_props(&player_data(Some("Name"), 16)).map(|p| p.len()), Some(16));
        assert_eq!(find_player_mora(&player_data(Some("Name"), 4)), None);
        assert_eq!(find_player_mora(&[1, 2, 3]), None);
    }

    /// The message that came when 93 of an item became 94, as the game sent it.
    fn item_update(id: u64, guid: u64, count: u64) -> Vec<u8> {
        let mut item = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut item);
            writer.write_uint64(1, id).unwrap();
            writer.write_uint64(2, guid).unwrap();
            let mut material = Vec::new();
            {
                let mut inner = CodedOutputStream::vec(&mut material);
                inner.write_uint64(1, count).unwrap();
                inner.flush().unwrap();
            }
            writer.write_bytes(5, &material).unwrap();
            writer.flush().unwrap();
        }
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint64(3, 3406).unwrap();
            writer.write_uint64(6, 1_794_041_540_104).unwrap();
            writer.write_bytes(1, &item).unwrap();
            writer.write_uint64(8, 1).unwrap();
            writer.flush().unwrap();
        }
        out
    }

    #[test]
    fn a_changed_stack_is_found_with_its_new_count() {
        let found = matches_item_update(&item_update(100021, 3_009_946_418_573_332_139, 94)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].item_id, 100021);
        assert_eq!(found[0].material().count, 94);
    }

    #[test]
    fn other_small_messages_are_not_item_updates() {
        // An item that is part of something else of the same size.
        let mut with_other = item_update(100021, 3_009_946_418_573_332_139, 94);
        let mut writer_buf = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut writer_buf);
            writer.write_bytes(9, b"hello there").unwrap();
            writer.flush().unwrap();
        }
        with_other.extend(writer_buf);
        assert!(matches_item_update(&with_other).is_none());
        assert!(matches_item_update(&[1, 2, 3]).is_none());
        // A guid that is not one.
        assert!(matches_item_update(&item_update(100021, 12, 94)).is_none());
    }

    fn prop_entry_plain(key: u64, number: u64) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint64(1, key).unwrap();
            writer.write_uint64(2, number).unwrap();
            writer.flush().unwrap();
        }
        out
    }

    /// The message that came when a character went from level 80 to 81.
    fn avatar_props(guid: u64, level: u64) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint64(3, 2959).unwrap();
            writer.write_uint64(6, 1_794_043_267_891).unwrap();
            writer.write_bytes(3, &prop_entry_plain(1001, 143_184)).unwrap();
            writer.write_bytes(3, &prop_entry_plain(4001, level)).unwrap();
            writer.write_uint64(5, guid).unwrap();
            writer.flush().unwrap();
        }
        out
    }

    #[test]
    fn a_characters_new_level_is_found() {
        let update = matches_avatar_prop_update(&avatar_props(3_009_946_418_572_951_763, 81)).unwrap();
        assert!(update.guids.contains(&3_009_946_418_572_951_763));
        assert!(update.props.contains(&(4001, 81)));
        assert!(update.props.contains(&(1001, 143_184)));
    }

    #[test]
    fn properties_without_a_guid_or_of_other_kinds_are_not_taken() {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_bytes(3, &prop_entry_plain(4001, 81)).unwrap();
            writer.flush().unwrap();
        }
        assert!(matches_avatar_prop_update(&out).is_none());
        // Player properties are of other ids.
        let mut player = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut player);
            writer.write_bytes(3, &prop_entry_plain(10016, 5)).unwrap();
            writer.write_uint64(5, 3_009_946_418_572_951_763).unwrap();
            writer.flush().unwrap();
        }
        assert!(matches_avatar_prop_update(&player).is_none());
    }

    #[test]
    fn stats_with_fractions_are_found() {
        let entry = |key: u64, number: f32| {
            let mut out = Vec::new();
            {
                let mut writer = CodedOutputStream::vec(&mut out);
                writer.write_uint64(1, key).unwrap();
                writer.write_float(2, number).unwrap();
                writer.flush().unwrap();
            }
            out
        };
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_uint64(6, 3_009_946_418_572_951_763).unwrap();
            for (key, number) in [(2000, 15000.5), (2001, 900.), (2002, 700.)] {
                writer.write_bytes(14, &entry(key, number)).unwrap();
            }
            writer.flush().unwrap();
        }
        let (guids, stats) = matches_avatar_fight_props(&message).unwrap();
        assert!(guids.contains(&3_009_946_418_572_951_763));
        assert_eq!(stats[0], (2000, 15000.5));
        assert_eq!(stats.len(), 3);
    }

    /// The message that came when a talent went from level 4 to 5.
    fn talent_update(guid: u64, skill: u64, old: u64, new: u64) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint64(3, 593).unwrap();
            writer.write_uint64(6, 1_794_044_347_730).unwrap();
            writer.write_uint64(1, new).unwrap();
            writer.write_uint64(3, old).unwrap();
            writer.write_uint64(8, guid).unwrap();
            writer.write_uint64(9, 4401).unwrap();
            writer.write_uint64(11, skill).unwrap();
            writer.flush().unwrap();
        }
        out
    }

    #[test]
    fn a_talent_level_is_found_with_who_and_which() {
        let update = matches_talent_update(&talent_update(3_009_946_418_573_107_101, 10441, 4, 5)).unwrap();
        assert!(update.guids.contains(&3_009_946_418_573_107_101));
        assert!(update.ids.contains(&10441));
        assert!(update.levels.contains(&4) && update.levels.contains(&5));
    }

    #[test]
    fn a_weapon_that_changed_is_an_item_update_too() {
        let mut weapon = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut weapon);
            writer.write_uint32(1, 20).unwrap();
            writer.write_uint32(3, 1).unwrap();
            writer.flush().unwrap();
        }
        let mut equip = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut equip);
            writer.write_bytes(2, &weapon).unwrap();
            writer.flush().unwrap();
        }
        let mut item = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut item);
            writer.write_uint64(1, 11306).unwrap();
            writer.write_uint64(2, 3_009_946_418_575_745_645).unwrap();
            writer.write_bytes(6, &equip).unwrap();
            writer.flush().unwrap();
        }
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_uint64(3, 851).unwrap();
            writer.write_bytes(1, &item).unwrap();
            writer.write_uint64(8, 1).unwrap();
            writer.flush().unwrap();
        }
        let found = matches_item_update(&message).unwrap();
        assert_eq!(found[0].equip().weapon().level, 20);
        assert_eq!(found[0].equip().weapon().promote_level, 1);
    }

    fn token_response(uid: Option<u32>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint32(1, 0).unwrap();
            if let Some(uid) = uid {
                writer.write_uint32(4, uid).unwrap();
            }
            // A time in seconds has ten digits, an id of a character eight.
            writer.write_uint64(7, 1_793_892_910).unwrap();
            writer.write_uint32(8, 10_000_049).unwrap();
            writer.write_string(9, "some text").unwrap();
            writer.flush().unwrap();
        }
        out
    }

    #[test]
    fn the_player_id_is_the_nine_digit_number() {
        assert_eq!(find_player_uid(&token_response(Some(700_807_765))), Some(700_807_765));
        assert_eq!(find_player_uid(&token_response(None)), None);
        assert_eq!(find_player_uid(&[]), None);
    }

    #[test]
    fn the_player_id_is_found_in_a_nested_message() {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint32(1, 0).unwrap();
            writer.write_bytes(3, &token_response(Some(812_345_678))).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(find_player_uid(&out), Some(812_345_678));
    }

    #[test]
    fn the_name_is_in_the_message_with_the_properties() {
        for name in ["yawn", "Тестер", "A B"] {
            let packet = player_data(Some(name), 8);
            assert_eq!(find_player_nickname(&packet).as_deref(), Some(name));
        }

        // Wrapped in an outer message.
        let mut wrapped = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut wrapped);
            writer.write_uint32(1, 7).unwrap();
            writer.write_bytes(2, &player_data(Some("yawn"), 8)).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(find_player_nickname(&wrapped).as_deref(), Some("yawn"));
    }

    #[test]
    fn no_name_without_the_properties_or_a_name() {
        assert_eq!(find_player_nickname(&player_data(Some("yawn"), 2)), None);
        assert_eq!(find_player_nickname(&player_data(None, 8)), None);
        assert_eq!(find_player_nickname(&player_data(Some("123456789"), 8)), None);
        assert_eq!(find_player_nickname(&[]), None);
    }
}
