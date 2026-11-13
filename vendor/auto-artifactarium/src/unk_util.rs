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

/// Things in the world have ids that begin with these: a thing that is in the
/// scene is "an entity", and the kind of it (a chest, a plant) is "a gadget".
const ENTITY_IDS: std::ops::Range<u64> = 0x2000_0000..0x3000_0000;
const GADGET_IDS: std::ops::Range<u64> = 70_000_000..80_000_000;
/// The group a gadget of the world belongs to is told by a number of nine digits.
const GROUP_IDS: std::ops::Range<u64> = 100_000_000..1_000_000_000;

/// A gadget that came into view: what it is, which one it is in the game's own
/// scripts, and where.  The group and the id in it (the "config") stay the same
/// from session to session, which the number of the entity does not.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneGadget {
    pub entity_id: u32,
    pub gadget_id: u32,
    pub group_id: u32,
    pub config_id: u32,
    pub position: Option<[f32; 3]>,
    /// What state the gadget is in, if the game says (a chest is 101 closed, 102
    /// opened: an opened chest is not sent in the world, but it may be elsewhere).
    pub state: Option<u32>,
}

/// Three small floats in a row: a place in the world.
fn position_in(bytes: &[u8]) -> Option<[f32; 3]> {
    let message = Unk::parse_from_bytes(bytes).ok()?;
    let mut numbers: Vec<(u32, f32)> = Vec::new();
    for (field, value) in message.unknown_fields().iter() {
        if let Fixed32(bits) = value {
            numbers.push((field, f32::from_bits(bits)));
        }
    }
    if numbers.len() != 3 || numbers.iter().any(|(_, n)| !n.is_finite() || n.abs() > 100_000.) {
        return None;
    }
    numbers.sort_by_key(|(field, _)| *field);
    Some([numbers[0].1, numbers[1].1, numbers[2].1])
}

/// A thing that is placed in the world, if `bytes` is one: its entity id, a
/// part that has the gadget, the group and the config, and where it is.  All of
/// it is found by what the numbers look like, not by the number of the field.
fn scene_gadget(bytes: &[u8]) -> Option<SceneGadget> {
    let entity = Unk::parse_from_bytes(bytes).ok()?;
    let mut entity_id = None;
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut inner = Vec::new();
    for (_, value) in entity.unknown_fields().iter() {
        match value {
            Varint(v) if ENTITY_IDS.contains(&v) && entity_id.is_none() => entity_id = Some(v),
            LengthDelimited(part) => inner.push(part.to_vec()),
            _ => (),
        }
    }
    parts.extend(inner.iter().map(Vec::as_slice));
    let entity_id = entity_id?;

    let mut found = None;
    let mut position = None;
    for part in &parts {
        let Ok(message) = Unk::parse_from_bytes(part) else {
            continue;
        };
        let mut gadget = None;
        let mut group = None;
        let mut numbers: Vec<(u32, u64)> = Vec::new();
        let mut places: Vec<Vec<u8>> = Vec::new();
        for (field, value) in message.unknown_fields().iter() {
            match value {
                Varint(v) if GADGET_IDS.contains(&v) && gadget.is_none() => gadget = Some(v),
                Varint(v) if GROUP_IDS.contains(&v) && group.is_none() => group = Some((field, v)),
                Varint(v) => numbers.push((field, v)),
                LengthDelimited(bytes) => places.push(bytes.to_vec()),
                _ => (),
            }
        }
        if let (Some(gadget), Some((group_field, group))) = (gadget, group) {
            // The config is the number after the group.
            let config = numbers
                .iter()
                .find(|(field, _)| *field == group_field + 1)
                .map(|(_, v)| *v);
            if let Some(config) = config
                && found.is_none()
            {
                let state = numbers.iter().find(|(field, _)| *field == 6).and_then(|(_, v)| u32::try_from(*v).ok());
                found = Some((gadget, group, config, state));
            }
        }
        // The place is a message of floats in a message of its own.
        if position.is_none() {
            position = places.iter().find_map(|place| position_in(place));
        }
    }
    let (gadget_id, group_id, config_id, state) = found?;
    Some(SceneGadget {
        entity_id: u32::try_from(entity_id).ok()?,
        gadget_id: u32::try_from(gadget_id).ok()?,
        group_id: u32::try_from(group_id).ok()?,
        config_id: u32::try_from(config_id).ok()?,
        position,
        state,
    })
}

/// Monsters (enemies and animals) are entities too, with ids that begin with these, and their
/// kinds are numbers of eight digits that begin with a 2.
const MONSTER_ENTITY_IDS: std::ops::Range<u64> = 0x1000_0000..0x1800_0000;
const MONSTER_IDS: std::ops::Range<u64> = 20_000_000..30_000_000;

/// A monster that came into view: which kind it is, which one it is in the game's own scripts
/// (the group and the id in it stay the same for a monster, the id of the entity does not: it
/// is another each time the monster is sent), and where.  One that was killed is not sent.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneMonster {
    pub entity_id: u32,
    pub monster_id: u32,
    pub group_id: u32,
    pub config_id: u32,
    pub position: Option<[f32; 3]>,
}

fn scene_monster(bytes: &[u8]) -> Option<SceneMonster> {
    let entity = Unk::parse_from_bytes(bytes).ok()?;
    let mut entity_id = None;
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for (_, value) in entity.unknown_fields().iter() {
        match value {
            Varint(v) if MONSTER_ENTITY_IDS.contains(&v) && entity_id.is_none() => entity_id = Some(v),
            LengthDelimited(part) => parts.push(part.to_vec()),
            _ => (),
        }
    }
    let entity_id = entity_id?;
    let mut found = None;
    let mut position = None;
    for part in &parts {
        let Ok(message) = Unk::parse_from_bytes(part) else {
            continue;
        };
        let mut monster = None;
        let mut group = None;
        let mut numbers: Vec<(u32, u64)> = Vec::new();
        let mut places: Vec<Vec<u8>> = Vec::new();
        for (field, value) in message.unknown_fields().iter() {
            match value {
                Varint(v) if field == 1 && MONSTER_IDS.contains(&v) && monster.is_none() => monster = Some(v),
                Varint(v) if GROUP_IDS.contains(&v) && group.is_none() => group = Some((field, v)),
                Varint(v) => numbers.push((field, v)),
                LengthDelimited(bytes) => places.push(bytes.to_vec()),
                _ => (),
            }
        }
        if let (Some(monster), Some((group_field, group))) = (monster, group)
            && found.is_none()
        {
            // The config is the number after the group.
            let config = numbers.iter().find(|(field, _)| *field == group_field + 1).map_or(0, |(_, v)| *v);
            found = Some((monster, group, config));
        }
        if position.is_none() {
            position = places.iter().find_map(|place| position_in(place));
        }
    }
    let (monster_id, group_id, config_id) = found?;
    Some(SceneMonster {
        entity_id: u32::try_from(entity_id).ok()?,
        monster_id: u32::try_from(monster_id).ok()?,
        group_id: u32::try_from(group_id).ok()?,
        config_id: u32::try_from(config_id).ok()?,
        position,
    })
}

/// The monsters that came into view in this command, if it says that (it is the command the
/// gadgets come in too).
pub fn matches_scene_monsters(data: &[u8]) -> Option<Vec<SceneMonster>> {
    if data.len() < 40 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let monsters: Vec<SceneMonster> = message
        .unknown_fields()
        .iter()
        .filter_map(|(_, value)| match value {
            LengthDelimited(bytes) => scene_monster(bytes),
            _ => None,
        })
        .collect();
    (!monsters.is_empty()).then_some(monsters)
}

/// The gadgets that came into view in this command, if it says that.  What it
/// holds is a list of entities, most of which are not gadgets of the world.
pub fn matches_scene_gadgets(data: &[u8]) -> Option<Vec<SceneGadget>> {
    if data.len() < 40 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let gadgets: Vec<SceneGadget> = message
        .unknown_fields()
        .iter()
        .filter_map(|(_, value)| match value {
            LengthDelimited(bytes) => scene_gadget(bytes),
            _ => None,
        })
        .collect();
    (!gadgets.is_empty()).then_some(gadgets)
}

/// Where a player is, as the game tells it every few seconds: the scene, who,
/// and the place in the game's coordinates (x, height, z).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerLocation {
    pub scene: u32,
    pub uid: u32,
    pub position: [f32; 3],
    /// The way the player faces, in degrees (0 is along z, 90 along x), if told.
    pub yaw: Option<f32>,
}

/// The way round in a message of floats that has only that (the field 2).
fn yaw_in(bytes: &[u8]) -> Option<f32> {
    let message = Unk::parse_from_bytes(bytes).ok()?;
    let mut yaw = None;
    for (field, value) in message.unknown_fields().iter() {
        match value {
            Fixed32(bits) if field == 2 => {
                let number = f32::from_bits(bits);
                if !number.is_finite() || number.abs() > 3600. {
                    return None;
                }
                yaw = Some(number);
            }
            Fixed32(_) if field == 1 || field == 3 => (),
            _ => return None,
        }
    }
    yaw
}

/// A quest (a step of a story) whose state the server says: its id, the state (1 not
/// begun, 2 begun, 3 done, 4 failed), and the id of the story it is a step of.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestUpdate {
    pub quest: u32,
    pub state: u32,
    pub parent: u32,
}

/// The quests a message says the state of: each in a message of field 3 with the id
/// (field 1), the state (field 2) and the story (field 6).  The whole list comes when
/// logging in, and one or a few each time a quest changes.
pub fn find_quest_updates(data: &[u8]) -> Vec<QuestUpdate> {
    let Ok(message) = Unk::parse_from_bytes(data) else {
        return Vec::new();
    };
    let mut updates = Vec::new();
    for (field, value) in message.unknown_fields().iter() {
        let LengthDelimited(part) = value else {
            continue;
        };
        if field != 3 {
            continue;
        }
        let Ok(quest) = Unk::parse_from_bytes(part) else {
            continue;
        };
        let (mut id, mut state, mut parent) = (None, None, None);
        for (f, v) in quest.unknown_fields().iter() {
            if let Varint(v) = v {
                match f {
                    1 => id = Some(v),
                    2 => state = Some(v),
                    6 => parent = Some(v),
                    _ => (),
                }
            }
        }
        if let (Some(id), Some(state), Some(parent)) = (id, state, parent)
            && (1..=4).contains(&state)
            && id >= 100
            && id <= u64::from(u32::MAX)
            && parent <= u64::from(u32::MAX)
        {
            updates.push(QuestUpdate { quest: id as u32, state: state as u32, parent: parent as u32 });
        }
    }
    updates
}

/// Where a story of the quest list is for the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryStatus {
    /// Offered (it can be begun), with no step of it begun.
    Available,
    /// Some step of it is begun and not done.
    InProgress,
    /// All the steps it has are done.
    Done,
}

/// A story of the quest list, as it is told when logging in: its id, since when (seconds),
/// the status, and the steps that are not done, with their state.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryEntry {
    pub story: u32,
    pub since: u64,
    pub status: StoryStatus,
    pub steps: usize,
    pub open_steps: Vec<(u32, u32)>,
}

/// The whole quest list: in field 9 each story (id in field 5, time in field 8) and, in its
/// field 12, the steps it has (id in field 1, state in field 11, 3 being done).  A story with
/// no steps is one that is offered.
pub fn find_quest_list(data: &[u8]) -> Vec<StoryEntry> {
    let Ok(message) = Unk::parse_from_bytes(data) else {
        return Vec::new();
    };
    let mut stories = Vec::new();
    for (field, value) in message.unknown_fields().iter() {
        let LengthDelimited(part) = value else {
            continue;
        };
        if field != 9 {
            continue;
        }
        let Ok(story) = Unk::parse_from_bytes(part) else {
            continue;
        };
        let (mut id, mut since) = (None, 0);
        let (mut steps, mut open_steps) = (0usize, Vec::new());
        for (f, v) in story.unknown_fields().iter() {
            match v {
                Varint(v) if f == 5 => id = Some(v as u32),
                Varint(v) if f == 8 => since = v,
                LengthDelimited(step) if f == 12 => {
                    let Ok(step) = Unk::parse_from_bytes(step) else {
                        continue;
                    };
                    let (mut quest, mut state) = (None, None);
                    for (g, w) in step.unknown_fields().iter() {
                        if let Varint(w) = w {
                            match g {
                                1 => quest = Some(w as u32),
                                11 => state = Some(w as u32),
                                _ => (),
                            }
                        }
                    }
                    steps += 1;
                    if let (Some(quest), Some(state)) = (quest, state)
                        && state != 3
                    {
                        open_steps.push((quest, state));
                    }
                }
                _ => (),
            }
        }
        let Some(story) = id else {
            continue;
        };
        let status = if steps == 0 {
            StoryStatus::Available
        } else if open_steps.is_empty() {
            StoryStatus::Done
        } else {
            StoryStatus::InProgress
        };
        stories.push(StoryEntry { story, since, status, steps, open_steps });
    }
    stories
}

/// A story (a quest of the quest list) and the state of the quests it has: field 2 of the
/// message has the story (field 5), and in field 12 each quest (field 1) with its state
/// (field 11).
pub fn find_story_update(data: &[u8]) -> Option<(u32, Vec<(u32, u32)>)> {
    let message = Unk::parse_from_bytes(data).ok()?;
    for (field, value) in message.unknown_fields().iter() {
        let LengthDelimited(part) = value else {
            continue;
        };
        if field != 2 {
            continue;
        }
        let story = Unk::parse_from_bytes(part).ok()?;
        let mut id = None;
        let mut quests = Vec::new();
        for (f, v) in story.unknown_fields().iter() {
            match v {
                Varint(v) if f == 5 => id = Some(v as u32),
                LengthDelimited(sub) if f == 12 => {
                    let sub = Unk::parse_from_bytes(sub).ok()?;
                    let (mut quest, mut state) = (None, None);
                    for (g, w) in sub.unknown_fields().iter() {
                        if let Varint(w) = w {
                            match g {
                                1 => quest = Some(w as u32),
                                11 => state = Some(w as u32),
                                _ => (),
                            }
                        }
                    }
                    if let (Some(quest), Some(state)) = (quest, state) {
                        quests.push((quest, state));
                    }
                }
                _ => (),
            }
        }
        return id.map(|id| (id, quests));
    }
    None
}

/// Where an avatar is and which way it faces, as the client says it moves.
#[derive(Clone, Debug, PartialEq)]
pub struct AvatarMotion {
    pub entity: u32,
    pub position: [f32; 3],
    pub yaw: Option<f32>,
}

/// Entities of avatars have the kind 8 in their high byte.
fn is_avatar_entity(entity: u64) -> bool {
    entity >> 24 == 8
}

/// The motion of an avatar in a message (the id of the entity in field 1, and in field 2 the
/// motion: the place in field 1 and the way round in field 2).  Looked for a few levels
/// down, as the commands are put in other commands; the last one is taken.
pub fn find_avatar_motion(data: &[u8]) -> Option<AvatarMotion> {
    let mut found = None;
    motion_in(data, 0, &mut found);
    found
}

fn motion_in(bytes: &[u8], depth: u32, found: &mut Option<AvatarMotion>) {
    let Ok(message) = Unk::parse_from_bytes(bytes) else {
        return;
    };
    let mut entity = None;
    let mut motion: Option<&[u8]> = None;
    let mut inner: Vec<Vec<u8>> = Vec::new();
    for (field, value) in message.unknown_fields().iter() {
        match value {
            Varint(v) if field == 1 && is_avatar_entity(v) => entity = Some(v as u32),
            LengthDelimited(part) => {
                if field == 2 && motion.is_none() {
                    motion = Some(part);
                }
                if depth < 7 && part.len() > 4 {
                    inner.push(part.to_vec());
                }
            }
            _ => (),
        }
    }
    if let (Some(entity), Some(motion)) = (entity, motion)
        && let Ok(info) = Unk::parse_from_bytes(motion)
    {
        let mut position = None;
        let mut yaw = None;
        for (field, value) in info.unknown_fields().iter() {
            if let LengthDelimited(part) = value {
                if field == 1 {
                    position = place_in(part);
                } else if field == 2 {
                    yaw = yaw_in(part);
                }
            }
        }
        if let Some(position) = position {
            *found = Some(AvatarMotion { entity, position, yaw });
            return;
        }
    }
    for part in inner {
        motion_in(&part, depth + 1, found);
    }
}

/// The place in a message of floats: the one with an x and a z (the direction
/// a player faces has only the way round, as the rest is zero, and zeros are
/// not told).
fn place_in(bytes: &[u8]) -> Option<[f32; 3]> {
    let message = Unk::parse_from_bytes(bytes).ok()?;
    let mut numbers = [None; 3];
    for (field, value) in message.unknown_fields().iter() {
        if let Fixed32(bits) = value
            && (1..=3).contains(&field)
        {
            let number = f32::from_bits(bits);
            if !number.is_finite() || number.abs() > 100_000. {
                return None;
            }
            numbers[field as usize - 1] = Some(number);
        }
    }
    let [Some(x), y, Some(z)] = numbers else {
        return None;
    };
    Some([x, y.unwrap_or(0.), z])
}

/// Where a teleport goes: the scene, and the number of the point in it (the
/// waypoint).  The game says it just before it changes the scene, in a short
/// message of the scene (field 2) and the point (field 5).
pub fn find_teleport_target(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() > 40 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let (mut scene, mut point) = (None, None);
    for (field, value) in message.unknown_fields().iter() {
        if let Varint(v) = value {
            match field {
                2 => scene = Some(v),
                5 => point = Some(v),
                _ => (),
            }
        }
    }
    let (scene, point) = (scene?, point?);
    ((1..=5_000).contains(&scene) && (1..=100_000).contains(&point)).then_some((scene as u32, point as u32))
}

/// Where the player comes in when the scene changes: the scene (said as text
/// that begins with it, field 293) and the place (field 14).
pub fn find_scene_arrival(data: &[u8]) -> Option<(u32, [f32; 3])> {
    let message = Unk::parse_from_bytes(data).ok()?;
    let mut scene = None;
    let mut place = None;
    for (field, value) in message.unknown_fields().iter() {
        if let LengthDelimited(bytes) = value {
            if field == 293 {
                let text = std::str::from_utf8(&bytes).ok()?;
                scene = text.split('-').next()?.parse::<u32>().ok();
            } else if field == 14 {
                place = position_in(&bytes);
            }
        }
    }
    Some((scene?, place?))
}

/// The message that says where a player is: the scene (a small number), and a
/// message of the player in which are the id of the player and the place.
pub fn matches_player_location(data: &[u8]) -> Option<PlayerLocation> {
    matches_player_location_of(data, None)
}

/// A player in a message: the id (the one that is `expected`, if it is among
/// the numbers that can be ids, else the first), and the place.  The message
/// may have the player in a message of its own, a level or two down.
fn player_in(bytes: &[u8], expected: Option<u64>, depth: u32) -> Option<(u64, [f32; 3], Option<f32>)> {
    let player = Unk::parse_from_bytes(bytes).ok()?;
    let mut uids = Vec::new();
    let mut places = Vec::new();
    let mut yaw = None;
    let mut nested = Vec::new();
    for (_, value) in player.unknown_fields().iter() {
        match value {
            Varint(v) if UID_RANGE.contains(&v) => uids.push(v),
            LengthDelimited(part) => {
                if let Some(place) = place_in(part) {
                    places.push(place);
                } else if let Some(turn) = yaw_in(part) {
                    yaw = yaw.or(Some(turn));
                } else if depth < 2 && part.len() > 4 {
                    nested.push(part.to_vec());
                }
            }
            _ => (),
        }
    }
    // Of the messages that can be a place, the one that is far from the middle
    // of the world is the place.
    places.sort_by(|a, b| {
        (b[0].abs() + b[2].abs())
            .partial_cmp(&(a[0].abs() + a[2].abs()))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let uid = expected.filter(|expected| uids.contains(expected)).or_else(|| uids.first().copied());
    if let (Some(uid), Some(position)) = (uid, places.first()) {
        return Some((uid, *position, yaw));
    }
    nested.iter().find_map(|inner| player_in(inner, expected, depth + 1))
}

/// Where a player is, as `matches_player_location`, with the id of the account
/// that is followed given, so that it is that id that is taken when the message
/// has more than one number that can be one.
pub fn matches_player_location_of(data: &[u8], expected: Option<u32>) -> Option<PlayerLocation> {
    if data.len() > 600 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let mut scene = None;
    let mut inner = Vec::new();
    for (_, value) in message.unknown_fields().iter() {
        match value {
            Varint(v) if (1..=2000).contains(&v) && scene.is_none() => scene = Some(v),
            Varint(_) => (),
            LengthDelimited(bytes) => inner.push(bytes.to_vec()),
            _ => return None,
        }
    }
    let scene = scene?;
    for bytes in &inner {
        if let Some((uid, position, yaw)) = player_in(bytes, expected.map(u64::from), 0) {
            return Some(PlayerLocation {
                yaw,
                scene: u32::try_from(scene).ok()?,
                uid: u32::try_from(uid).ok()?,
                position,
            });
        }
    }
    None
}

/// What the game answers when something in the world is used: a chest opened,
/// an item picked up.
#[derive(Clone, Debug, PartialEq)]
pub struct GadgetInteraction {
    pub gadget_id: u32,
    pub entity_id: u32,
    /// What was done: 3 is opening a chest, 1 picking something up.
    pub kind: u64,
    /// The answer comes twice for a chest: first as the opening begins, then
    /// when it is done.  This is the first.
    pub begins: bool,
}

pub fn matches_gadget_interaction(data: &[u8]) -> Option<GadgetInteraction> {
    if data.len() > 60 {
        return None;
    }
    let message = Unk::parse_from_bytes(data).ok()?;
    let (mut gadget, mut entity, mut kind) = (None, None, None);
    let mut threes = 0;
    for (field, value) in message.unknown_fields().iter() {
        match value {
            Varint(v) if GADGET_IDS.contains(&v) && gadget.is_none() => gadget = Some(v),
            Varint(v) if ENTITY_IDS.contains(&v) && entity.is_none() => entity = Some(v),
            Varint(v) if field == 12 => kind = Some(v),
            // The first field with a 3 is the number of the packet.
            Varint(_) if field == 3 => threes += 1,
            Varint(_) => (),
            // Something that is not only numbers is another kind of message.
            _ => return None,
        }
    }
    Some(GadgetInteraction {
        gadget_id: u32::try_from(gadget?).ok()?,
        entity_id: u32::try_from(entity?).ok()?,
        kind: kind?,
        begins: threes >= 2,
    })
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

/// Player ids have nine digits, or ten for the newer accounts of the Asian
/// server (which begin with 18).  Times in seconds have ten digits as well.
const UID_RANGE: std::ops::RangeInclusive<u64> = 100_000_000..=4_294_967_295;

/// A number this close to the time (in seconds) is the time, not an id.
const TIME_MARGIN: u64 = 3 * 24 * 3600;

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
/// the number that has the digits of an id (nine or ten), and is not the time.
/// Matched by content, as field numbers change with the game version.
/// `time` is the time of the packet, in seconds, if it is known.
pub fn find_player_uid(body: &[u8], time: Option<u64>) -> Option<u32> {
    let now = time.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    });
    let mut found = Vec::new();
    varints_at(body, 0, 1, &mut found);
    found
        .into_iter()
        .filter(|(_, _, value)| UID_RANGE.contains(value) && value.abs_diff(now) > TIME_MARGIN)
        .min_by_key(|(level, _, _)| *level)
        .map(|(_, _, value)| value as u32)
}

/// The id of the player whose name is `name`: a message that has the name in
/// it, and an id next to it.  The game says this of the player in a few
/// messages (a card of the player, the friends), whatever the server is.
pub fn find_uid_beside_name(body: &[u8], name: &str) -> Option<u32> {
    fn search(data: &[u8], name: &[u8], level: u32) -> Option<u32> {
        let message = Unk::parse_from_bytes(data).ok()?;
        let mut named = false;
        let mut ids = Vec::new();
        let mut nested = Vec::new();
        for (_, value) in message.unknown_fields().iter() {
            match value {
                LengthDelimited(bytes) if bytes == name => named = true,
                LengthDelimited(bytes) if level < 4 && bytes.len() > 4 => nested.push(bytes.to_vec()),
                Varint(v) if UID_RANGE.contains(&v) => ids.push(v),
                _ => (),
            }
        }
        if named && let Some(&id) = ids.first() {
            return u32::try_from(id).ok();
        }
        nested.iter().find_map(|inner| search(inner, name, level + 1))
    }
    if name.is_empty() || body.len() > 200_000 {
        return None;
    }
    search(body, name.as_bytes(), 0)
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

    /// A chest as the game told about it (an entity of a message with many).
    const CHEST_ENTITY: &str = "080410eeb2808002221c0a0f0d8d3f1a4515444b4e431df4499ac41205154a4cb3421a0032002a0c08a11f120708a11f100120013207080115804fc347320208043202080732020817320808d00f15804fc347320308d10f320308d20f320308d30f320308e40f320308e517320808e617150000803f320808f20715804fc347320308d40f38016a1108baabbd2110f2f0b53f18bc20480150019a0100aa01190a00220f0d8d3f1a4515444b4e431df4499ac432020a003a00ca011c12140884d1960110d6cb0c18f9d4b4ffffffffffff011a0410b1bc05";

    fn from_hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn a_chest_in_view_is_read_with_its_place() {
        let data = packet(2, &[from_hex(CHEST_ENTITY)]);
        let gadgets = matches_scene_gadgets(&data).unwrap();
        assert_eq!(gadgets.len(), 1);
        let chest = &gadgets[0];
        assert_eq!(chest.entity_id, 536_877_422);
        assert_eq!(chest.gadget_id, 70_211_002);
        assert_eq!(chest.group_id, 133_003_378);
        assert_eq!(chest.config_id, 4156);
        let [x, y, z] = chest.position.unwrap();
        assert!((x - 2468.0).abs() < 0.5 && (y - 206.3).abs() < 0.5 && (z + 1234.3).abs() < 0.5);
    }

    /// Runs the matchers over a probe-raw log (`IRMINSUL_RAW_LOG=path`), to see
    /// what they find in real traffic.  Not run by default.
    #[test]
    #[ignore]
    fn the_matchers_over_a_raw_log() {
        let Ok(path) = std::env::var("IRMINSUL_RAW_LOG") else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        for line in text.lines() {
            let mut parts = line.splitn(4, ' ');
            let (Some(time), Some(command), Some(_), Some(hex)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Some(command) = command.strip_prefix("cmd=") else {
                continue;
            };
            let data = from_hex(hex);
            if let Some(gadgets) = matches_scene_gadgets(&data) {
                println!("{time} cmd {command}: {} gadgets, first {:?}", gadgets.len(), gadgets[0]);
            }
            if let Some(event) = matches_gadget_interaction(&data) {
                println!("{time} cmd {command}: {event:?}");
            }
            if let Some(location) = matches_player_location(&data) {
                println!("{time} cmd {command}: {location:?}");
            }
            if let Ok(name) = std::env::var("IRMINSUL_RAW_NAME")
                && let Some(uid) = find_uid_beside_name(&data, &name)
            {
                println!("{time} cmd {command}: id {uid} beside the name");
            }
        }
    }

    #[test]
    fn where_a_player_is_is_read_from_the_message_of_a_few_seconds() {
        // As the game said it: the scene, and a message with the id of the
        // player, the way the player faces (only the y, as the rest is zero)
        // and the place.
        let mut place = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut place);
            writer.write_fixed32(1, 2481.0_f32.to_bits()).unwrap();
            writer.write_fixed32(2, 207.0_f32.to_bits()).unwrap();
            writer.write_fixed32(3, (-1242.0_f32).to_bits()).unwrap();
            writer.flush().unwrap();
        }
        let mut facing = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut facing);
            writer.write_fixed32(2, 94.5_f32.to_bits()).unwrap();
            writer.flush().unwrap();
        }
        let mut player = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut player);
            writer.write_uint32(6, 1_826_078_112).unwrap();
            writer.write_bytes(13, &facing).unwrap();
            writer.write_bytes(14, &place).unwrap();
            writer.flush().unwrap();
        }
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_uint32(1, 3).unwrap();
            writer.write_bytes(4, &player).unwrap();
            writer.flush().unwrap();
        }
        let location = matches_player_location(&message).unwrap();
        assert_eq!(location.scene, 3);
        assert_eq!(location.uid, 1_826_078_112);
        assert_eq!(location.position, [2481.0, 207.0, -1242.0]);
        assert_eq!(location.yaw, Some(94.5));
        // Items and the like are not where a player is.
        assert!(matches_player_location(&packet(2, &items(2))).is_none());
        assert!(matches_player_location(&[]).is_none());
    }

    /// The place of a player, in a message that has more than the id and the place.
    fn location_message(uid: u32, other: u32, nested: bool, padding: usize) -> Vec<u8> {
        let mut place = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut place);
            writer.write_fixed32(1, 2481.0_f32.to_bits()).unwrap();
            writer.write_fixed32(2, 207.0_f32.to_bits()).unwrap();
            writer.write_fixed32(3, (-1242.0_f32).to_bits()).unwrap();
            writer.flush().unwrap();
        }
        let mut player = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut player);
            // A number that could be an id, before the real one.
            writer.write_uint32(2, other).unwrap();
            writer.write_uint32(6, uid).unwrap();
            writer.write_bytes(14, &place).unwrap();
            writer.write_bytes(15, &vec![7u8; padding]).unwrap();
            writer.flush().unwrap();
        }
        let wrapped = if nested {
            let mut wrapper = Vec::new();
            {
                let mut writer = CodedOutputStream::vec(&mut wrapper);
                writer.write_uint32(1, 5).unwrap();
                writer.write_bytes(2, &player).unwrap();
                writer.flush().unwrap();
            }
            wrapper
        } else {
            player
        };
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_uint32(1, 3).unwrap();
            writer.write_bytes(4, &wrapped).unwrap();
            writer.flush().unwrap();
        }
        message
    }

    #[test]
    fn a_players_place_is_read_whatever_the_id_is_and_however_it_is_wrapped() {
        // An id of nine digits, a bigger message, another number that could be an id.
        let message = location_message(700_807_762, 3_300_000_000, false, 150);
        assert!(message.len() > 120);
        let found = matches_player_location_of(&message, Some(700_807_762)).unwrap();
        assert_eq!(found.uid, 700_807_762, "the id that is expected is taken");
        assert_eq!(found.position, [2481.0, 207.0, -1242.0]);
        // Without being told which, the first is taken (as it was).
        assert_eq!(matches_player_location_of(&message, None).unwrap().uid, 3_300_000_000);
        // The player one message further down.
        let nested = location_message(700_807_762, 3_300_000_000, true, 0);
        assert_eq!(matches_player_location_of(&nested, Some(700_807_762)).unwrap().uid, 700_807_762);
    }

    #[test]
    fn what_is_not_a_gadget_in_view_is_not_taken() {
        // Items and avatars are lists too, and are not gadgets.
        assert!(matches_scene_gadgets(&packet(2, &items(25))).is_none());
        assert!(matches_scene_gadgets(&packet(2, &avatars(5))).is_none());
    }

    fn interaction(begins: bool, gadget: u32, kind: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint32(3, 912).unwrap();
            writer.write_uint64(6, 1_794_065_943_402).unwrap();
            writer.write_uint32(2, gadget).unwrap();
            if begins {
                writer.write_uint32(3, 1).unwrap();
            }
            writer.write_uint32(8, 536_877_422).unwrap();
            writer.write_uint32(12, kind).unwrap();
            writer.flush().unwrap();
        }
        out
    }

    #[test]
    fn opening_a_chest_is_told_twice() {
        let begins = matches_gadget_interaction(&interaction(true, 70_211_002, 3)).unwrap();
        assert_eq!(begins.gadget_id, 70_211_002);
        assert_eq!(begins.entity_id, 536_877_422);
        assert_eq!(begins.kind, 3);
        assert!(begins.begins);
        let done = matches_gadget_interaction(&interaction(false, 70_211_002, 3)).unwrap();
        assert!(!done.begins);
        let pickup = matches_gadget_interaction(&interaction(false, 70_600_041, 1)).unwrap();
        assert_eq!(pickup.kind, 1);
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
        assert_eq!(find_player_uid(&token_response(Some(700_807_765)), None), Some(700_807_765));
        assert_eq!(find_player_uid(&token_response(None), None), None);
        assert_eq!(find_player_uid(&[], None), None);
    }

    #[test]
    fn an_id_of_ten_digits_is_not_taken_for_the_time() {
        // The time (field 7) is in the body too, and sorts the same as an id.
        // The id is told from it by the time of the packet.
        let time = Some(1_793_892_912);
        assert_eq!(
            find_player_uid(&token_response(Some(1_826_078_112)), time),
            Some(1_826_078_112)
        );
        // Whichever comes first in the message.
        let mut out = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut out);
            writer.write_uint64(2, 1_793_892_910).unwrap();
            writer.write_uint32(4, 1_826_078_112).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(find_player_uid(&out, time), Some(1_826_078_112));
        // And with no time at all, nothing but a time is no id.
        let mut only_time = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut only_time);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            writer.write_uint64(2, now).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(find_player_uid(&only_time, None), None);
    }

    #[test]
    fn the_id_is_found_next_to_the_name() {
        // As in a card of the player: the id, then the name, in a message in
        // a message.
        let mut card = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut card);
            writer.write_uint32(1, 1_826_078_112).unwrap();
            writer.write_string(2, "neiroromo").unwrap();
            writer.write_uint32(3, 3).unwrap();
            writer.write_uint32(9, 210_001).unwrap();
            writer.flush().unwrap();
        }
        let mut outer = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut outer);
            writer.write_uint32(3, 912).unwrap();
            writer.write_bytes(13, &card).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(find_uid_beside_name(&outer, "neiroromo"), Some(1_826_078_112));
        assert_eq!(find_uid_beside_name(&outer, "someone else"), None);
        assert_eq!(find_uid_beside_name(&outer, ""), None);
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
        assert_eq!(find_player_uid(&out, None), Some(812_345_678));
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

    #[test]
    fn the_motion_of_the_avatar_is_read_from_what_the_client_sends() {
        let mut place = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut place);
            writer.write_fixed32(1, 2101.435_f32.to_bits()).unwrap();
            writer.write_fixed32(2, 214.395_f32.to_bits()).unwrap();
            writer.write_fixed32(3, (-1362.494_f32).to_bits()).unwrap();
            writer.flush().unwrap();
        }
        let mut turn = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut turn);
            writer.write_fixed32(2, 301.364_f32.to_bits()).unwrap();
            writer.flush().unwrap();
        }
        let mut motion = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut motion);
            writer.write_bytes(1, &place).unwrap();
            writer.write_bytes(2, &turn).unwrap();
            writer.write_uint32(4, 2).unwrap();
            writer.flush().unwrap();
        }
        let mut entity = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut entity);
            writer.write_uint32(1, 134_229_199).unwrap();
            writer.write_bytes(2, &motion).unwrap();
            writer.flush().unwrap();
        }
        // Put in other messages, as it is.
        let mut level = entity;
        for _ in 0..3 {
            let mut outer = Vec::new();
            {
                let mut writer = CodedOutputStream::vec(&mut outer);
                writer.write_uint32(6, 7).unwrap();
                writer.write_bytes(8, &level).unwrap();
                writer.flush().unwrap();
            }
            level = outer;
        }
        let found = find_avatar_motion(&level).expect("a motion");
        assert_eq!(found.entity, 134_229_199);
        assert_eq!(found.position, [2101.435, 214.395, -1362.494]);
        assert_eq!(found.yaw, Some(301.364));
        // The entity of something else (a monster, 0x20 high) is not the avatar.
        let mut other = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut other);
            writer.write_uint32(1, 536_870_913).unwrap();
            writer.write_bytes(2, &motion).unwrap();
            writer.flush().unwrap();
        }
        assert!(find_avatar_motion(&other).is_none());
    }

    #[test]
    fn the_state_of_quests_is_read() {
        let mut quest = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut quest);
            writer.write_uint32(1, 30803).unwrap();
            writer.write_uint32(2, 3).unwrap();
            writer.write_uint32(6, 308).unwrap();
            writer.write_uint32(8, 11882).unwrap();
            writer.flush().unwrap();
        }
        let mut other = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut other);
            writer.write_uint32(1, 30804).unwrap();
            writer.write_uint32(2, 2).unwrap();
            writer.write_uint32(6, 308).unwrap();
            writer.flush().unwrap();
        }
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_uint64(6, 1_794_137_655_051).unwrap();
            writer.write_bytes(3, &quest).unwrap();
            writer.write_bytes(3, &other).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(
            find_quest_updates(&message),
            vec![
                QuestUpdate { quest: 30803, state: 3, parent: 308 },
                QuestUpdate { quest: 30804, state: 2, parent: 308 }
            ]
        );
    }

    #[test]
    fn the_quests_of_a_story_are_read() {
        let mut quest = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut quest);
            writer.write_uint32(1, 35721).unwrap();
            writer.write_uint32(11, 3).unwrap();
            writer.flush().unwrap();
        }
        let mut story = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut story);
            writer.write_uint32(1, 1).unwrap();
            writer.write_uint32(5, 357).unwrap();
            writer.write_bytes(12, &quest).unwrap();
            writer.flush().unwrap();
        }
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_bytes(2, &story).unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(find_story_update(&message), Some((357, vec![(35721, 3)])));
    }

    #[test]
    fn the_quest_list_tells_done_begun_and_offered_stories() {
        let step = |id: u32, state: u32| {
            let mut bytes = Vec::new();
            {
                let mut writer = CodedOutputStream::vec(&mut bytes);
                writer.write_uint32(1, id).unwrap();
                writer.write_uint32(11, state).unwrap();
                writer.flush().unwrap();
            }
            bytes
        };
        let story = |id: u32, steps: &[Vec<u8>]| {
            let mut bytes = Vec::new();
            {
                let mut writer = CodedOutputStream::vec(&mut bytes);
                writer.write_uint32(5, id).unwrap();
                writer.write_uint64(8, 1_794_137_749).unwrap();
                for step in steps {
                    writer.write_bytes(12, step).unwrap();
                }
                writer.flush().unwrap();
            }
            bytes
        };
        let mut message = Vec::new();
        {
            let mut writer = CodedOutputStream::vec(&mut message);
            writer.write_bytes(9, &story(20030, &[step(2003001, 3), step(2003002, 3)])).unwrap();
            writer.write_bytes(9, &story(306, &[step(30601, 3), step(30609, 2)])).unwrap();
            writer.write_bytes(9, &story(71005, &[])).unwrap();
            writer.flush().unwrap();
        }
        let list = find_quest_list(&message);
        assert_eq!(list.len(), 3);
        assert_eq!((list[0].story, list[0].status), (20030, StoryStatus::Done));
        assert_eq!((list[1].story, list[1].status, list[1].open_steps.clone()), (306, StoryStatus::InProgress, vec![(30609, 2)]));
        assert_eq!((list[2].story, list[2].status), (71005, StoryStatus::Available));
    }
}
