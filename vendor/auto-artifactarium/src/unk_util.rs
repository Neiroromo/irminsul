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
}
