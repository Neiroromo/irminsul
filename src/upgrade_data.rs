//! What the game's own data files say about characters, weapons, materials and
//! artifacts, as opposed to what the packets say about what a player has: names,
//! what upgrades cost, and the numbers that grow with levels.
//!
//! The file this is read from is made by `examples/gen_upgrade_data.rs` from
//! the same data files the rest of Irminsul takes its names from.  This file
//! holds only the types and the calculations, so that example can use it too.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A name in the languages Irminsul shows.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Text {
    pub en: String,
    pub ru: String,
}

impl Text {
    pub fn get(&self, russian: bool) -> &str {
        if russian && !self.ru.is_empty() {
            &self.ru
        } else {
            &self.en
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Cost {
    pub id: u32,
    pub count: u32,
}

/// What it takes to get to the next level of an ascension, or of a talent.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Step {
    pub mora: u32,
    pub items: Vec<Cost>,
}

/// An ascension: the step to it, and what it allows.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Promotion {
    pub cost: Step,
    /// The highest level before the next ascension is needed.
    pub max_level: u32,
    /// Weapons: the base attack and the sub stat this ascension adds.
    #[serde(default)]
    pub add_attack: f32,
    #[serde(default)]
    pub add_sub: f32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Talent {
    pub skill_id: u32,
    pub name: Text,
    /// The step to level 2 first, to level 10 last.
    pub upgrades: Vec<Step>,
}

/// The skills of a character.  The Traveler has one per element.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Depot {
    /// "Fire", "Water" and so on, as the game names the elements.
    pub element: String,
    pub normal: Talent,
    pub skill: Talent,
    pub burst: Talent,
    pub constellations: Vec<Text>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Avatar {
    pub name: Text,
    /// The name of the picture of it, as the game names its files.
    #[serde(default)]
    pub icon: String,
    pub rarity: u32,
    pub weapon_type: String,
    pub default_depot: u32,
    /// Index is the ascension, the first one is the state at level 1.
    pub promotions: Vec<Promotion>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Weapon {
    pub name: Text,
    #[serde(default)]
    pub icon: String,
    pub rarity: u32,
    pub weapon_type: String,
    pub attack: f32,
    pub attack_curve: String,
    /// The sub stat, as the game names the property, and how it grows.
    pub sub_prop: String,
    pub sub_value: f32,
    pub sub_curve: String,
    /// Index is the ascension.
    pub promotions: Vec<Promotion>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Material {
    pub name: Text,
    #[serde(default)]
    pub icon: String,
    /// What the game calls this kind of item, e.g. "Local Specialty (Liyue)".
    pub category: Text,
    pub rarity: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Artifact {
    #[serde(default)]
    pub icon: String,
    pub set: u32,
    /// "EQUIP_BRACER" and so on.
    pub slot: String,
    pub rarity: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Affix {
    pub prop: String,
    pub value: f32,
}

/// A way to make a material at the crafting bench: so many of one lower in
/// rarity make one of it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Recipe {
    pub result: u32,
    pub input: u32,
    pub input_count: u32,
    pub mora: u32,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct UpgradeData {
    pub git_hash: String,
    pub avatars: BTreeMap<u32, Avatar>,
    pub depots: BTreeMap<u32, Depot>,
    pub weapons: BTreeMap<u32, Weapon>,
    pub materials: BTreeMap<u32, Material>,
    pub artifacts: BTreeMap<u32, Artifact>,
    pub sets: BTreeMap<u32, Text>,
    /// Substats, by the ids in the artifacts.
    pub affixes: BTreeMap<u32, Affix>,
    /// The main stat of an artifact, by the id in the artifact.
    pub main_props: BTreeMap<u32, String>,
    /// How weapon stats grow: per curve, the multiplier of each level, the
    /// first being level 1.
    pub weapon_curves: BTreeMap<String, Vec<f32>>,
    /// Main stat values: per rarity and property, the value at each level,
    /// the first being level 0.
    pub main_values: BTreeMap<u32, BTreeMap<String, Vec<f32>>>,
    /// The experience that takes a character from a level to the next: the
    /// first is level 1 to 2.
    #[serde(default)]
    pub avatar_exp: Vec<u32>,
    /// The same for weapons, per rarity.
    #[serde(default)]
    pub weapon_exp: BTreeMap<u32, Vec<u32>>,
    /// What can be made from lower tiers of the materials used for upgrades.
    #[serde(default)]
    pub recipes: Vec<Recipe>,
}

impl UpgradeData {
    /// The base attack and the sub stat of a weapon at a level and ascension.
    pub fn weapon_stats(&self, weapon: &Weapon, level: u32, ascension: u32) -> (f32, f32) {
        let multiplier = |curve: &str| {
            self.weapon_curves
                .get(curve)
                .and_then(|curve| curve.get(level.saturating_sub(1) as usize))
                .copied()
                .unwrap_or(1.)
        };
        let promotion = weapon.promotions.get(ascension as usize);
        let attack = weapon.attack * multiplier(&weapon.attack_curve)
            + promotion.map_or(0., |promotion| promotion.add_attack);
        let sub = weapon.sub_value * multiplier(&weapon.sub_curve)
            + promotion.map_or(0., |promotion| promotion.add_sub);
        (attack, sub)
    }

    /// The value of an artifact's main stat at a level.
    pub fn main_value(&self, rarity: u32, prop: &str, level: u32) -> Option<f32> {
        self.main_values
            .get(&rarity)?
            .get(prop)?
            .get(level as usize)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weapon() -> (UpgradeData, Weapon) {
        let mut data = UpgradeData::default();
        data.weapon_curves
            .insert("ATK".to_owned(), vec![1., 2., 3.]);
        data.weapon_curves
            .insert("SUB".to_owned(), vec![1., 1.5, 2.]);
        let weapon = Weapon {
            attack: 40.,
            attack_curve: "ATK".to_owned(),
            sub_prop: "CRIT".to_owned(),
            sub_value: 0.1,
            sub_curve: "SUB".to_owned(),
            promotions: vec![
                Promotion::default(),
                Promotion {
                    add_attack: 30.,
                    add_sub: 0.,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        (data, weapon)
    }

    #[test]
    fn weapon_stats_follow_the_level_and_ascension() {
        let (data, weapon) = weapon();
        let (attack, sub) = data.weapon_stats(&weapon, 1, 0);
        assert_eq!((attack, sub), (40., 0.1));
        let (attack, sub) = data.weapon_stats(&weapon, 3, 1);
        assert_eq!(attack, 40. * 3. + 30.);
        assert!((sub - 0.2).abs() < 1e-6);
    }

    #[test]
    fn main_values_are_looked_up_by_rarity_and_level() {
        let mut data = UpgradeData::default();
        data.main_values
            .entry(5)
            .or_default()
            .insert("FIGHT_PROP_HP".to_owned(), vec![717., 920.]);
        assert_eq!(data.main_value(5, "FIGHT_PROP_HP", 1), Some(920.));
        assert_eq!(data.main_value(4, "FIGHT_PROP_HP", 1), None);
        assert_eq!(data.main_value(5, "FIGHT_PROP_HP", 9), None);
    }
}
