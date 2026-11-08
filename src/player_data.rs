use std::collections::HashMap;

use anime_game_data::{AnimeGameData, Property, SkillType};
use anyhow::Result;
pub use auto_artifactarium::Achievement;
pub use auto_artifactarium::r#gen::protos::{AvatarInfo, Item};
use indexmap::IndexMap;
use protobuf::Message as _;
use serde::{Deserialize, Serialize};

use crate::good::{self, fake_uninitialized_4th_line};
use crate::profiles::{self, Profile, StoredAchievement};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExportSettings {
    pub include_characters: bool,
    pub include_artifacts: bool,
    pub include_weapons: bool,
    pub include_materials: bool,
    pub fake_initialize_4th_line: bool,

    pub min_character_level: u32,
    pub min_character_ascension: u32,
    pub min_character_constellation: u32,

    pub min_artifact_level: u32,
    pub min_artifact_rarity: u32,

    pub min_weapon_level: u32,
    pub min_weapon_refinement: u32,
    pub min_weapon_ascension: u32,
    pub min_weapon_rarity: u32,
}

/// The characters and items of the account in use, as the exports see them, for
/// showing them.
#[derive(Clone, Debug, Default)]
pub struct Roster {
    pub characters: Vec<AvatarInfo>,
    pub items: Vec<Item>,
    pub mora: Option<u64>,
}

pub struct PlayerData {
    game_data: AnimeGameData,
    achievements: Vec<Achievement>,
    characters: Vec<AvatarInfo>,
    items: Vec<Item>,
    mora: Option<u64>,
    /// Sizes of the full lists taken in the current connection.  Partial
    /// updates look like full lists, and are told apart by being much smaller.
    character_baseline: usize,
    item_baseline: usize,

    character_equip_guid_map: HashMap<u64, u32>,
}

impl PlayerData {
    pub fn new(game_data: AnimeGameData) -> Self {
        Self {
            game_data,
            achievements: Vec::new(),
            characters: Vec::new(),
            items: Vec::new(),
            mora: None,
            character_baseline: 0,
            item_baseline: 0,
            character_equip_guid_map: HashMap::new(),
        }
    }

    /// Returns what there was before if the number is a new one.
    pub fn process_mora(&mut self, mora: u64) -> Option<Option<u64>> {
        let before = self.mora;
        self.mora = Some(mora);
        (before != Some(mora)).then_some(before)
    }

    pub fn process_achievements(&mut self, achievements: &[Achievement]) {
        self.achievements = achievements.into();
    }

    pub fn roster(&self) -> Roster {
        Roster {
            characters: self.characters.clone(),
            items: self.items.clone(),
            mora: self.mora,
        }
    }

    /// Forgets everything, for when the data is of another account.
    pub fn clear(&mut self) {
        self.achievements.clear();
        self.characters.clear();
        self.items.clear();
        self.mora = None;
        self.character_equip_guid_map.clear();
        self.character_baseline = 0;
        self.item_baseline = 0;
    }

    /// Whether there are items, characters and achievements.
    pub fn availability(&self) -> (bool, bool, bool) {
        (
            !self.items.is_empty(),
            !self.characters.is_empty(),
            !self.achievements.is_empty(),
        )
    }

    /// Takes the data of a stored profile in place of what is there.
    pub fn load_profile(&mut self, profile: &Profile) {
        self.clear();
        self.mora = profile.mora;
        self.items = profile
            .items
            .iter()
            .filter_map(|bytes| Item::parse_from_bytes(bytes).ok())
            .collect();
        let characters: Vec<AvatarInfo> = profile
            .characters
            .iter()
            .filter_map(|bytes| AvatarInfo::parse_from_bytes(bytes).ok())
            .collect();
        self.process_characters(&characters);
        self.achievements = profile
            .achievements
            .iter()
            .map(|achievement| Achievement {
                id: achievement.id,
                status: achievement.status,
                finish_timestamp: achievement.finished_at,
            })
            .collect();
        // What is stored is not a connection to compare later lists with.
        self.character_baseline = 0;
        self.item_baseline = 0;
    }

    /// Puts the chosen kinds of data into `profile`, replacing what it held.
    pub fn fill_profile(
        &self,
        profile: &mut Profile,
        items: bool,
        characters: bool,
        achievements: bool,
    ) {
        let now = profiles::now();
        if self.mora.is_some() {
            profile.mora = self.mora;
        }
        if items {
            profile.items = self
                .items
                .iter()
                .filter_map(|item| item.write_to_bytes().ok())
                .collect();
            profile.items_at = Some(now.clone());
        }
        if characters {
            profile.characters = self
                .characters
                .iter()
                .filter_map(|character| character.write_to_bytes().ok())
                .collect();
            profile.characters_at = Some(now.clone());
        }
        if achievements {
            profile.achievements = self
                .achievements
                .iter()
                .map(|achievement| StoredAchievement {
                    id: achievement.id,
                    status: achievement.status,
                    finished_at: achievement.finish_timestamp,
                })
                .collect();
            profile.achievements_at = Some(now);
        }
    }

    /// A new connection to the game server starts, so its lists replace the
    /// old ones whatever their size.
    pub fn start_session(&mut self) {
        self.character_baseline = 0;
        self.item_baseline = 0;
    }

    /// Returns false when the list was ignored as a partial update.
    pub fn process_characters(&mut self, avatars: &[AvatarInfo]) -> bool {
        if avatars.len() * 2 < self.character_baseline {
            return false;
        }
        self.character_baseline = avatars.len();
        self.character_equip_guid_map.clear();
        for avatar in avatars {
            for guid in &avatar.equip_guid_list {
                self.character_equip_guid_map
                    .insert(*guid, avatar.avatar_id);
            }
        }
        self.characters = avatars.into();
        true
    }

    /// Returns false when the list was ignored as a partial update.
    /// Takes in the stacks and weapons the game says changed.
    pub fn apply_item_changes(&mut self, updates: &[Item]) -> AppliedItems {
        AppliedItems {
            stacks: apply_item_updates(&mut self.items, updates),
            weapons: apply_weapon_updates(&mut self.items, updates),
        }
    }

    /// Takes in a talent that went up.  Returns the character, the talent and
    /// its level, if it is a new one.
    pub fn apply_talent_level(&mut self, update: &auto_artifactarium::TalentUpdate) -> Option<(u32, u32, u32)> {
        apply_talent_update(&mut self.characters, &update.guids, &update.ids, &update.levels)
    }

    /// Takes in a change of a character's properties.  Returns the character
    /// and its new level, if the level changed.
    pub fn apply_avatar_props(
        &mut self,
        guids: &[u64],
        props: &[(u32, i64)],
    ) -> Option<(u32, Option<u32>)> {
        apply_avatar_props(&mut self.characters, guids, props)
    }

    /// Takes in new stats of a character.  Returns whether one was found.
    pub fn apply_avatar_stats(&mut self, guids: &[u64], stats: &[(u32, f32)]) -> bool {
        match self
            .characters
            .iter_mut()
            .find(|avatar| avatar.avatar_type == 1 && guids.contains(&avatar.guid))
        {
            Some(avatar) => {
                for (key, value) in stats {
                    avatar.fight_prop_map.insert(*key, *value);
                }
                true
            }
            None => false,
        }
    }

    pub fn process_items(&mut self, items: &[Item]) -> bool {
        if items.len() * 2 < self.item_baseline {
            return false;
        }
        self.item_baseline = items.len();
        self.items = items.into();
        true
    }

    pub fn export_achievements(&self) -> Result<String> {
        crate::achievement::export_achievements(&self.achievements)
    }

    pub fn finished_achievement_ids(&self) -> Vec<u32> {
        crate::achievement::finished_ids(&self.achievements)
    }

    pub fn export_genshin_optimizer(&self, settings: &ExportSettings) -> Result<String> {
        let mut good = good::Good {
            format: "GOOD".to_string(),
            version: 3,
            source: "Irminsul".to_string(),
            characters: Vec::new(),
            artifacts: Vec::new(),
            weapons: Vec::new(),
            materials: HashMap::new(),
        };

        if settings.include_characters {
            good.characters = self.export_genshin_optimizer_characters(settings);
        }

        if settings.include_artifacts {
            good.artifacts = if settings.fake_initialize_4th_line {
                let artifacts = self.export_genshin_optimizer_artifacts(settings);
                fake_uninitialized_4th_line(artifacts)
            } else {
                self.export_genshin_optimizer_artifacts(settings)
            };
        }

        if settings.include_weapons {
            good.weapons = self.export_genshin_optimizer_weapons(settings);
        }

        if settings.include_materials {
            good.materials = self.export_genshin_optimizer_materials();
        }

        let json = serde_json::to_string(&good)?;
        tracing::trace!("{json}");
        Ok(json)
    }

    pub fn export_genshin_optimizer_characters(
        &self,
        settings: &ExportSettings,
    ) -> Vec<good::Character> {
        // TPS avatars are not normal characters and are excluded from export.
        let tps_avatar_ids: Vec<u32> = [
            self.game_data.get_tps_avatar_id_female(),
            self.game_data.get_tps_avatar_id_male(),
        ]
        .into_iter()
        .filter_map(Result::ok)
        .collect();

        self.characters
            .iter()
            .filter_map(|character| {
                if character.avatar_type != 1 || tps_avatar_ids.contains(&character.avatar_id) {
                    return None;
                }

                let name = self.game_data.get_character(character.avatar_id).ok()?;
                let level = character.prop_map.get(&4001).map(|prop| prop.val as u32)?;
                let ascension = character.prop_map.get(&1002).map(|prop| prop.val as u32)?;
                let constellation = character.talent_id_list.len() as u32;

                let mut auto = 1;
                let mut skill = 1;
                let mut burst = 1;
                let mut element = None;

                for (id, level) in &character.skill_level_map {
                    let Some(ty) = self.game_data.get_skill_type(*id).ok() else {
                        continue;
                    };
                    match ty {
                        SkillType::Auto => auto = *level,
                        SkillType::Skill => skill = *level,
                        SkillType::Burst => {
                            burst = *level;
                            element = self.game_data.get_skill_element(*id).ok().copied();
                        }
                    }
                }

                if level < settings.min_character_level
                    || ascension < settings.min_character_ascension
                    || constellation < settings.min_character_constellation
                {
                    return None;
                }

                // The Traveler is the only character that can change elements.
                // The GOOD format lets you optionally suffix the Traveler's
                // name with their element (e.g. `TravelerCryo`).
                let mut key = good::to_good_key(name);
                if key == good::TRAVELER_KEY
                    && let Some(element) = element
                {
                    key.push_str(element.as_ref());
                }

                Some(good::Character {
                    key,
                    level,
                    constellation,
                    ascension,
                    talent: good::TalentLevel { auto, skill, burst },
                })
            })
            .collect()
    }

    pub fn round(property: Property, value: f32) -> f32 {
        // The game rounds percentages to 0.1 and non percentages to whole numbers.
        if property.is_percentage() {
            (value * 10.).round() / 10.
        } else {
            value.round()
        }
    }

    pub fn export_genshin_optimizer_artifacts(
        &self,
        settings: &ExportSettings,
    ) -> Vec<good::Artifact> {
        self.items
            .iter()
            .filter_map(|item| {
                if !item.has_equip() {
                    return None;
                }
                let equip = item.equip();
                let location = self
                    .character_equip_guid_map
                    .get(&item.guid)
                    .and_then(|id| {
                        self.game_data
                            .get_character(*id)
                            .ok()
                            .map(|location| good::to_good_key(location).to_string())
                    })
                    .unwrap_or_default();

                if !equip.has_reliquary() {
                    return None;
                }
                let artifact_data = self.game_data.get_artifact(item.item_id).ok()?;
                let artifact = equip.reliquary();
                let mut substats: IndexMap<Property, (f32, f32)> = IndexMap::new();
                for substat_id in artifact.append_prop_id_list.iter() {
                    let Some(substat) = self.game_data.get_affix(*substat_id).ok() else {
                        continue;
                    };
                    let entry = substats
                        .entry(substat.property)
                        .or_insert((0., substat.value as f32));
                    entry.0 += substat.value as f32;
                }
                let substats = substats
                    .into_iter()
                    .map(|(property, (value, initial_value))| good::Substat {
                        key: property.good_name().to_string(),
                        value: Self::round(property, value),
                        initial_value: Self::round(property, initial_value),
                    })
                    .collect();
                let unactivated_substats = artifact
                    .unactivated_prop_id_list
                    .iter()
                    .filter_map(|substat_id| {
                        let substat = self.game_data.get_affix(*substat_id).ok()?;
                        Some(good::Substat {
                            key: substat.property.good_name().to_string(),
                            value: Self::round(substat.property, substat.value as f32),
                            initial_value: Self::round(substat.property, substat.value as f32),
                        })
                    })
                    .collect();
                let total_rolls = artifact.append_prop_id_list.len() as u32;

                let level = artifact.level - 1;
                let rarity = artifact_data.rarity;
                let astral_mark = artifact.starred;
                let elixir_crafted = !artifact.elixer_choices.is_empty();
                let main_stat_key = self
                    .game_data
                    .get_property(artifact.main_prop_id)
                    .ok()?
                    .good_name()
                    .to_string();

                if level < settings.min_artifact_level || rarity < settings.min_artifact_rarity {
                    return None;
                }

                Some(good::Artifact {
                    set_key: good::to_good_key(&artifact_data.set),
                    slot_key: artifact_data.slot.good_name().to_string(),
                    level,
                    rarity,
                    main_stat_key,
                    location,
                    lock: equip.is_locked,
                    substats,
                    total_rolls,
                    astral_mark,
                    elixir_crafted,
                    unactivated_substats,
                })
            })
            .collect()
    }

    pub fn export_genshin_optimizer_weapons(&self, settings: &ExportSettings) -> Vec<good::Weapon> {
        self.items
            .iter()
            .filter_map(|item| {
                if !item.has_equip() {
                    return None;
                }
                let equip = item.equip();
                let location = self
                    .character_equip_guid_map
                    .get(&item.guid)
                    .and_then(|id| {
                        self.game_data
                            .get_character(*id)
                            .ok()
                            .map(|location| good::to_good_key(location).to_string())
                    })
                    .unwrap_or_default();
                if !equip.has_weapon() {
                    return None;
                }
                let weapon_data = self.game_data.get_weapon(item.item_id).ok()?;
                let weapon = equip.weapon();
                let refinement = weapon
                    .affix_map
                    .values()
                    .cloned()
                    .next()
                    .unwrap_or_default()
                    + 1;

                let level = weapon.level;
                let ascension = weapon.promote_level;

                if level < settings.min_weapon_level
                    || refinement < settings.min_weapon_refinement
                    || ascension < settings.min_weapon_ascension
                    || weapon_data.rarity < settings.min_weapon_rarity
                {
                    return None;
                }

                Some(good::Weapon {
                    key: good::to_good_key(&weapon_data.name),
                    level,
                    ascension,
                    refinement,
                    location,
                    lock: equip.is_locked,
                })
            })
            .collect()
    }

    pub fn export_genshin_optimizer_materials(&self) -> HashMap<String, u32> {
        self.items
            .iter()
            .filter_map(|item| {
                if !item.has_material() {
                    return None;
                }
                let material = item.material();
                let name = self.game_data.get_material(item.item_id).ok()?;

                Some((good::to_good_key(name), material.count))
            })
            .collect()
    }
}


/// Puts the stacks of materials the game says changed into the list: the count
/// is what there is now.  A stack that is not there is a new one; one with
/// nothing left is taken away.  Returns the id of each item and how much more
/// (or less) of it there is.
pub fn apply_item_updates(items: &mut Vec<Item>, updates: &[Item]) -> Vec<(u32, i64)> {
    let mut changes = Vec::new();
    for update in updates {
        if !update.has_material() {
            continue;
        }
        let now = update.material().count as i64;
        match items.iter().position(|item| item.guid == update.guid) {
            Some(index) => {
                if !items[index].has_material() {
                    continue;
                }
                let before = items[index].material().count as i64;
                if now == 0 {
                    items.remove(index);
                } else {
                    items[index] = update.clone();
                }
                if now != before {
                    changes.push((update.item_id, now - before));
                }
            }
            None => {
                if now > 0 {
                    items.push(update.clone());
                    changes.push((update.item_id, now));
                }
            }
        }
    }
    changes
}

#[cfg(test)]
mod item_update_tests {
    use super::*;
    use auto_artifactarium::r#gen::protos::Material;

    fn stack(item_id: u32, guid: u64, count: u32) -> Item {
        let mut item = Item::new();
        item.item_id = item_id;
        item.guid = guid;
        let mut material = Material::new();
        material.count = count;
        item.set_material(material);
        item
    }

    #[test]
    fn a_stack_takes_its_new_count_and_says_how_much_it_changed() {
        let mut items = vec![stack(100021, 7, 93), stack(100022, 8, 5)];
        let changes = apply_item_updates(&mut items, &[stack(100021, 7, 95)]);
        assert_eq!(changes, vec![(100021, 2)]);
        assert_eq!(items[0].material().count, 95);
        assert_eq!(items[1].material().count, 5);
    }

    #[test]
    fn a_new_stack_is_added_and_an_empty_one_taken_away() {
        let mut items = vec![stack(100021, 7, 3)];
        let added = apply_item_updates(&mut items, &[stack(104122, 9, 1)]);
        assert_eq!(added, vec![(104122, 1)]);
        assert_eq!(items.len(), 2);
        let spent = apply_item_updates(&mut items, &[stack(100021, 7, 0)]);
        assert_eq!(spent, vec![(100021, -3)]);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn the_same_count_is_no_change() {
        let mut items = vec![stack(100021, 7, 3)];
        assert!(apply_item_updates(&mut items, &[stack(100021, 7, 3)]).is_empty());
    }
}


/// Puts the properties the game says changed into the character they are of.
/// Returns the character's id, and the new level if it is a new one.
pub fn apply_avatar_props(
    characters: &mut [AvatarInfo],
    guids: &[u64],
    props: &[(u32, i64)],
) -> Option<(u32, Option<u32>)> {
    let avatar = characters
        .iter_mut()
        .find(|avatar| avatar.avatar_type == 1 && guids.contains(&avatar.guid))?;
    let mut level = None;
    for (key, number) in props {
        let entry = avatar.prop_map.entry(*key).or_default();
        if *key == 4001 && entry.val != *number {
            level = Some(*number as u32);
        }
        entry.type_ = *key;
        entry.val = *number;
        entry.set_ival(*number);
    }
    Some((avatar.avatar_id, level))
}

#[cfg(test)]
mod avatar_update_tests {
    use super::*;

    fn avatar(id: u32, guid: u64, level: i64) -> AvatarInfo {
        let mut avatar = AvatarInfo::new();
        avatar.avatar_id = id;
        avatar.guid = guid;
        avatar.avatar_type = 1;
        let entry = avatar.prop_map.entry(4001).or_default();
        entry.type_ = 4001;
        entry.val = level;
        avatar
    }

    #[test]
    fn a_new_level_goes_to_the_character_with_that_guid() {
        let mut characters = vec![avatar(10000002, 111, 80), avatar(10000003, 222, 50)];
        let result = apply_avatar_props(&mut characters, &[5, 222], &[(1001, 10), (4001, 51)]);
        assert_eq!(result, Some((10000003, Some(51))));
        assert_eq!(characters[1].prop_map[&4001].val, 51);
        assert_eq!(characters[0].prop_map[&4001].val, 80);
        // The same level again is not a new one.
        let again = apply_avatar_props(&mut characters, &[222], &[(4001, 51)]);
        assert_eq!(again, Some((10000003, None)));
    }

    #[test]
    fn nobody_with_the_guid_means_nothing_is_taken() {
        let mut characters = vec![avatar(10000002, 111, 80)];
        assert_eq!(apply_avatar_props(&mut characters, &[999], &[(4001, 81)]), None);
    }
}


/// What the game's change messages did to the items.
pub struct AppliedItems {
    /// The id of each stack and how much more (or less) there is of it.
    pub stacks: Vec<(u32, i64)>,
    /// The weapons whose level, ascension or refinement changed.
    pub weapons: Vec<WeaponChange>,
}

pub struct WeaponChange {
    pub item_id: u32,
    pub level: u32,
    /// The level or the ascension is not what it was.
    pub leveled: bool,
    /// The refinement, from 1, and whether it is not what it was.
    pub refinement: u32,
    pub refined: bool,
}

/// Puts the weapons the game says changed in the place of the ones the
/// account has.
pub fn apply_weapon_updates(items: &mut [Item], updates: &[Item]) -> Vec<WeaponChange> {
    let mut changed = Vec::new();
    for update in updates {
        if !(update.has_equip() && update.equip().has_weapon()) {
            continue;
        }
        let Some(item) = items
            .iter_mut()
            .find(|item| item.guid == update.guid && item.has_equip() && item.equip().has_weapon())
        else {
            continue;
        };
        let (before, now) = (item.equip().weapon(), update.equip().weapon());
        // The refinement is how far the weapon's effect is, counted from 0.
        let refinement = |weapon: &auto_artifactarium::r#gen::protos::Weapon| {
            weapon.affix_map.values().next().copied().unwrap_or(0) + 1
        };
        let leveled = before.level != now.level || before.promote_level != now.promote_level;
        let refined = refinement(before) != refinement(now);
        let (level, now_refinement) = (now.level, refinement(now));
        *item = update.clone();
        if leveled || refined {
            changed.push(WeaponChange {
                item_id: update.item_id,
                level,
                leveled,
                refinement: now_refinement,
                refined,
            });
        }
    }
    changed
}

/// Puts a talent's new level into the character it is of.  The talent is the
/// one of the character with an id that is in the message, and the new level
/// is the larger of two levels in it that follow each other.
pub fn apply_talent_update(
    characters: &mut [AvatarInfo],
    guids: &[u64],
    ids: &[u64],
    levels: &[u64],
) -> Option<(u32, u32, u32)> {
    let avatar = characters
        .iter_mut()
        .find(|avatar| avatar.avatar_type == 1 && guids.contains(&avatar.guid))?;
    let skill = ids
        .iter()
        .map(|id| *id as u32)
        .find(|id| avatar.skill_level_map.contains_key(id))?;
    let new = levels
        .iter()
        .copied()
        .filter(|level| *level >= 2 && levels.contains(&(level - 1)))
        .max()? as u32;
    let before = avatar.skill_level_map.insert(skill, new);
    (before != Some(new)).then_some((avatar.avatar_id, skill, new))
}

#[cfg(test)]
mod talent_weapon_tests {
    use super::*;

    #[test]
    fn a_talent_goes_up_one_level() {
        let mut avatar = AvatarInfo::new();
        avatar.avatar_id = 10000025;
        avatar.guid = 3_009_946_418_573_107_101;
        avatar.avatar_type = 1;
        avatar.skill_level_map.insert(10441, 4);
        let mut characters = vec![avatar];
        let result = apply_talent_update(&mut characters, &[3_009_946_418_573_107_101], &[4401, 10441], &[5, 4]);
        assert_eq!(result, Some((10000025, 10441, 5)));
        assert_eq!(characters[0].skill_level_map[&10441], 5);
        // The second message of the same upgrade changes nothing.
        let again = apply_talent_update(&mut characters, &[3_009_946_418_573_107_101], &[10441], &[4, 5]);
        assert_eq!(again, None);
        // An id that is not a talent of the character is not taken.
        assert_eq!(
            apply_talent_update(&mut characters, &[3_009_946_418_573_107_101], &[4401], &[5, 6]),
            None
        );
    }

    fn weapon(guid: u64, level: u32, promote: u32) -> Item {
        use auto_artifactarium::r#gen::protos::{Equip, Weapon};
        let mut weapon = Weapon::new();
        weapon.level = level;
        weapon.promote_level = promote;
        let mut equip = Equip::new();
        equip.set_weapon(weapon);
        let mut item = Item::new();
        item.item_id = 11306;
        item.guid = guid;
        item.set_equip(equip);
        item
    }

    #[test]
    fn a_weapon_that_went_up_replaces_the_old_one() {
        let mut items = vec![weapon(5, 1, 0)];
        let changed = apply_weapon_updates(&mut items, &[weapon(5, 20, 0)]);
        assert_eq!(changed.len(), 1);
        assert_eq!((changed[0].item_id, changed[0].level, changed[0].leveled), (11306, 20, true));
        assert!(!changed[0].refined);
        assert_eq!(items[0].equip().weapon().level, 20);
        // Ascending at the same level is a change too.
        assert_eq!(apply_weapon_updates(&mut items, &[weapon(5, 20, 1)]).len(), 1);
        // So is the refinement, and only that.
        let mut refined = weapon(5, 20, 1);
        let mut inner = refined.equip().weapon().clone();
        inner.affix_map.insert(111306, 1);
        let mut equip = refined.equip().clone();
        equip.set_weapon(inner);
        refined.set_equip(equip);
        let change = apply_weapon_updates(&mut items, &[refined]);
        assert_eq!(change.len(), 1);
        assert!(change[0].refined && !change[0].leveled);
        assert_eq!(change[0].refinement, 2);
        // A weapon the account does not have is not taken in.
        assert!(apply_weapon_updates(&mut items, &[weapon(6, 90, 6)]).is_empty());
    }
}
