//! Makes `src/data/upgrade_data.json.gz`, which Irminsul reads for names, upgrade
//! costs and the numbers that grow with levels.
//!
//! It takes the game's data files from the repository Irminsul takes its other
//! names from, and keeps what is needed.  Run it after a game update:
//!
//! ```text
//! cargo run --release --example gen_upgrade_data
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;

use anyhow::{Context, Result, anyhow};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;

#[path = "../src/upgrade_data.rs"]
#[allow(dead_code)]
mod upgrade_data;

use upgrade_data::*;

// GitHub mirror of the same data: raw.githubusercontent.com has no tight limit,
// unlike GitLab, which answers 429 after a few runs.
const COMMITS_API_URL: &str =
    "https://api.github.com/repos/DimbreathBot/AnimeGameData/commits?per_page=1";
const REPO_URL: &str = "https://raw.githubusercontent.com/DimbreathBot/AnimeGameData";
const OUTPUT: &str = "src/data/upgrade_data.json.gz";

fn num(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn float(value: &Value, key: &str) -> f32 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.) as f32
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

struct Source {
    client: reqwest::Client,
    git_ref: String,
}

impl Source {
    async fn bytes(&self, path: &str) -> Result<Vec<u8>> {
        let url = format!("{REPO_URL}/{}/{path}", self.git_ref);
        println!("downloading {path}");
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("request to {url} failed"))?
            .error_for_status()
            .with_context(|| format!("request to {url} failed"))?;
        Ok(response.bytes().await?.to_vec())
    }

    async fn table(&self, name: &str) -> Result<Vec<Value>> {
        let bytes = self.bytes(&format!("ExcelBinOutput/{name}.json")).await?;
        match serde_json::from_slice(&bytes)? {
            Value::Array(entries) => Ok(entries),
            _ => Err(anyhow!("{name} is not a list")),
        }
    }

    async fn text_map(&self, file: &str) -> Result<HashMap<String, String>> {
        let bytes = self.bytes(&format!("TextMap/{file}.json")).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

struct Texts {
    en: HashMap<String, String>,
    ru: HashMap<String, String>,
}

impl Texts {
    fn text(&self, hash: u64) -> Option<Text> {
        let key = hash.to_string();
        let en = self.en.get(&key)?;
        Some(Text {
            en: en.clone(),
            ru: self.ru.get(&key).cloned().unwrap_or_default(),
        })
    }

    fn name(&self, entry: &Value) -> Option<Text> {
        self.text(num(entry, "nameTextMapHash"))
    }
}

fn costs(entry: &Value) -> Vec<Cost> {
    list(entry, "costItems")
        .iter()
        .filter_map(|item| {
            let id = num(item, "id") as u32;
            let count = num(item, "count") as u32;
            (id != 0 && count != 0).then_some(Cost { id, count })
        })
        .collect()
}

fn step(entry: &Value) -> Step {
    Step {
        // Characters' ascensions name the cost differently than the rest.
        mora: num(entry, "coinCost").max(num(entry, "scoinCost")) as u32,
        items: costs(entry),
    }
}

fn added(entry: &Value, prop: &str) -> f32 {
    list(entry, "addProps")
        .iter()
        .find(|added| string(added, "propType") == prop)
        .map_or(0., |added| float(added, "value"))
}

/// The ascensions of a character or a weapon from the rows of its table.
fn promotions(rows: &[&Value], sub_prop: &str) -> Vec<Promotion> {
    let by_level: BTreeMap<u64, &Value> = rows
        .iter()
        .map(|row| (num(row, "promoteLevel"), *row))
        .collect();
    let highest = by_level.keys().next_back().copied().unwrap_or(0);
    (0..=highest)
        .map(|level| {
            let Some(row) = by_level.get(&level) else {
                return Promotion::default();
            };
            Promotion {
                cost: step(row),
                max_level: num(row, "unlockMaxLevel") as u32,
                add_attack: added(row, "FIGHT_PROP_BASE_ATTACK"),
                add_sub: if sub_prop.is_empty() {
                    0.
                } else {
                    added(row, sub_prop)
                },
            }
        })
        .collect()
}

fn rarity(quality: &str) -> u32 {
    match quality {
        "QUALITY_ORANGE" | "QUALITY_ORANGE_SP" => 5,
        "QUALITY_PURPLE" => 4,
        "QUALITY_BLUE" => 3,
        "QUALITY_GREEN" => 2,
        _ => 1,
    }
}

/// Whether a material is one a player upgrades with, going by what the game
/// calls its kind.
fn is_upgrade_material(category: &str, material_type: &str) -> bool {
    category.starts_with("Local Specialty")
        || matches!(
            category,
            "Common Currency"
                | "Character Ascension Material"
                | "Character Talent Material"
                | "Character Level-Up Material"
                | "Character and Weapon Enhancement Material"
                | "Weapon Ascension Material"
                | "Refinement Material"
        )
        || matches!(
            material_type,
            "MATERIAL_EXP_FRUIT" | "MATERIAL_WEAPON_EXP_STONE"
        )
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::builder().user_agent("irminsul-gen").build()?;
    let commits: Vec<Value> = serde_json::from_slice(
        &client
            .get(COMMITS_API_URL)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?,
    )?;
    let git_ref = commits
        .first()
        .and_then(|commit| commit["sha"].as_str())
        .context("no commits found")?
        .to_owned();
    println!("game data {git_ref}");
    let source = Source { client, git_ref };

    let texts = {
        let en = source.text_map("TextMap_MediumEN").await?;
        let mut ru = source.text_map("TextMap_MediumRU_0").await?;
        ru.extend(source.text_map("TextMap_MediumRU_1").await?);
        Texts { en, ru }
    };

    let avatar_rows = source.table("AvatarExcelConfigData").await?;
    let avatar_promote_rows = source.table("AvatarPromoteExcelConfigData").await?;
    let depot_rows = source.table("AvatarSkillDepotExcelConfigData").await?;
    let skill_rows = source.table("AvatarSkillExcelConfigData").await?;
    let proud_rows = source.table("ProudSkillExcelConfigData").await?;
    let talent_rows = source.table("AvatarTalentExcelConfigData").await?;
    let weapon_rows = source.table("WeaponExcelConfigData").await?;
    let weapon_promote_rows = source.table("WeaponPromoteExcelConfigData").await?;
    let weapon_curve_rows = source.table("WeaponCurveExcelConfigData").await?;
    let material_rows = source.table("MaterialExcelConfigData").await?;
    let reliquary_rows = source.table("ReliquaryExcelConfigData").await?;
    let set_rows = source.table("ReliquarySetExcelConfigData").await?;
    let equip_affix_rows = source.table("EquipAffixExcelConfigData").await?;
    let affix_rows = source.table("ReliquaryAffixExcelConfigData").await?;
    let main_prop_rows = source.table("ReliquaryMainPropExcelConfigData").await?;
    let level_rows = source.table("ReliquaryLevelExcelConfigData").await?;

    let combine_rows = source.table("CombineExcelConfigData").await?;
    let avatar_level_rows = source.table("AvatarLevelExcelConfigData").await?;
    let weapon_level_rows = source.table("WeaponLevelExcelConfigData").await?;

    let mut data = UpgradeData {
        git_hash: source.git_ref.clone(),
        ..Default::default()
    };

    // Experience to the next level.
    let mut avatar_levels: Vec<(u64, u32)> = avatar_level_rows
        .iter()
        .map(|row| (num(row, "level"), num(row, "exp") as u32))
        .collect();
    avatar_levels.sort();
    data.avatar_exp = avatar_levels.into_iter().map(|(_, exp)| exp).collect();
    for rarity in 1..=5u32 {
        let mut levels: Vec<(u64, u32)> = weapon_level_rows
            .iter()
            .map(|row| {
                let exp = list(row, "requiredExps")
                    .get(rarity as usize - 1)
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                (num(row, "level"), exp as u32)
            })
            .collect();
        levels.sort();
        data.weapon_exp
            .insert(rarity, levels.into_iter().map(|(_, exp)| exp).collect());
    }

    // Characters.
    let mut avatar_promote_by_id: HashMap<u64, Vec<&Value>> = HashMap::new();
    for row in &avatar_promote_rows {
        avatar_promote_by_id
            .entry(num(row, "avatarPromoteId"))
            .or_default()
            .push(row);
    }
    for row in &avatar_rows {
        let Some(name) = texts.name(row) else {
            continue;
        };
        let Some(promote_rows) = avatar_promote_by_id.get(&num(row, "avatarPromoteId")) else {
            continue;
        };
        data.avatars.insert(
            num(row, "id") as u32,
            Avatar {
                name,
                icon: string(row, "iconName").to_owned(),
                rarity: rarity(string(row, "qualityType")),
                weapon_type: string(row, "weaponType").to_owned(),
                default_depot: num(row, "skillDepotId") as u32,
                promotions: promotions(promote_rows, ""),
            },
        );
    }

    // Skills and constellations.
    let skills: HashMap<u64, &Value> = skill_rows.iter().map(|row| (num(row, "id"), row)).collect();
    let mut proud_by_group: HashMap<u64, Vec<&Value>> = HashMap::new();
    for row in &proud_rows {
        proud_by_group
            .entry(num(row, "proudSkillGroupId"))
            .or_default()
            .push(row);
    }
    let constellation_names: HashMap<u64, Text> = talent_rows
        .iter()
        .filter_map(|row| Some((num(row, "talentId"), texts.name(row)?)))
        .collect();

    let talent = |skill_id: u64| -> Option<Talent> {
        let skill = skills.get(&skill_id)?;
        let mut levels = proud_by_group
            .get(&num(skill, "proudSkillGroupId"))
            .cloned()
            .unwrap_or_default();
        levels.sort_by_key(|row| num(row, "level"));
        Some(Talent {
            skill_id: skill_id as u32,
            name: texts.name(skill)?,
            upgrades: levels
                .iter()
                .filter(|row| (2..=10).contains(&num(row, "level")))
                .map(|row| step(row))
                .collect(),
        })
    };
    for row in &depot_rows {
        let skill_ids = list(row, "skills");
        let (Some(normal), Some(skill), Some(burst)) = (
            skill_ids.first().and_then(Value::as_u64).and_then(&talent),
            skill_ids.get(1).and_then(Value::as_u64).and_then(&talent),
            talent(num(row, "energySkill")),
        ) else {
            continue;
        };
        let element = skills
            .get(&num(row, "energySkill"))
            .map_or("", |burst| string(burst, "costElemType"))
            .to_owned();
        let constellations = list(row, "talents")
            .iter()
            .filter_map(|id| constellation_names.get(&id.as_u64()?).cloned())
            .collect();
        data.depots.insert(
            num(row, "id") as u32,
            Depot {
                element,
                normal,
                skill,
                burst,
                constellations,
            },
        );
    }

    // Weapons and how their stats grow.
    let mut weapon_promote_by_id: HashMap<u64, Vec<&Value>> = HashMap::new();
    for row in &weapon_promote_rows {
        weapon_promote_by_id
            .entry(num(row, "weaponPromoteId"))
            .or_default()
            .push(row);
    }
    for row in &weapon_rows {
        if string(row, "itemType") != "ITEM_WEAPON" {
            continue;
        }
        let Some(name) = texts.name(row) else {
            continue;
        };
        let props = list(row, "weaponProp");
        let attack = props.first();
        let sub = props.get(1).filter(|sub| !string(sub, "propType").is_empty());
        let sub_prop = sub.map_or("", |sub| string(sub, "propType"));
        let rows = weapon_promote_by_id
            .get(&num(row, "weaponPromoteId"))
            .cloned()
            .unwrap_or_default();
        data.weapons.insert(
            num(row, "id") as u32,
            Weapon {
                name,
                icon: string(row, "icon").to_owned(),
                rarity: num(row, "rankLevel") as u32,
                weapon_type: string(row, "weaponType").to_owned(),
                attack: attack.map_or(0., |attack| float(attack, "initValue")),
                attack_curve: attack.map_or("", |attack| string(attack, "type")).to_owned(),
                sub_prop: sub_prop.to_owned(),
                sub_value: sub.map_or(0., |sub| float(sub, "initValue")),
                sub_curve: sub.map_or("", |sub| string(sub, "type")).to_owned(),
                promotions: promotions(&rows, sub_prop),
            },
        );
    }
    for row in &weapon_curve_rows {
        let level = num(row, "level") as usize;
        for info in list(row, "curveInfos") {
            let curve = data
                .weapon_curves
                .entry(string(info, "type").to_owned())
                .or_default();
            if curve.len() < level {
                curve.resize(level, 1.);
            }
            curve[level - 1] = float(info, "value");
        }
    }

    // Materials: those something is upgraded with, and those the upgrades need.
    let mut needed: BTreeSet<u32> = BTreeSet::new();
    let mut note = |step: &Step| needed.extend(step.items.iter().map(|cost| cost.id));
    for avatar in data.avatars.values() {
        avatar.promotions.iter().for_each(|p| note(&p.cost));
    }
    for depot in data.depots.values() {
        for talent in [&depot.normal, &depot.skill, &depot.burst] {
            talent.upgrades.iter().for_each(&mut note);
        }
    }
    for weapon in data.weapons.values() {
        weapon.promotions.iter().for_each(|p| note(&p.cost));
    }
    needed.insert(202);
    for row in &material_rows {
        let id = num(row, "id") as u32;
        let Some(name) = texts.name(row) else {
            continue;
        };
        let category = texts.text(num(row, "typeDescTextMapHash")).unwrap_or_default();
        if !needed.contains(&id) && !is_upgrade_material(&category.en, string(row, "materialType"))
        {
            continue;
        }
        data.materials.insert(
            id,
            Material {
                name,
                icon: string(row, "icon").to_owned(),
                category,
                rarity: num(row, "rankLevel") as u32,
            },
        );
    }

    // Artifacts.
    let set_names: HashMap<u64, Text> = equip_affix_rows
        .iter()
        .filter_map(|row| Some((num(row, "id"), texts.name(row)?)))
        .collect();
    for row in &set_rows {
        let Some(name) = row
            .get("equipAffixId")
            .and_then(Value::as_u64)
            .and_then(|id| set_names.get(&id))
        else {
            continue;
        };
        data.sets.insert(num(row, "setId") as u32, name.clone());
    }
    for row in &reliquary_rows {
        let set = num(row, "setId") as u32;
        if set == 0 || !data.sets.contains_key(&set) {
            continue;
        }
        data.artifacts.insert(
            num(row, "id") as u32,
            Artifact {
                icon: string(row, "icon").to_owned(),
                set,
                slot: string(row, "equipType").to_owned(),
                rarity: num(row, "rankLevel") as u32,
            },
        );
    }
    for row in &affix_rows {
        data.affixes.insert(
            num(row, "id") as u32,
            Affix {
                prop: string(row, "propType").to_owned(),
                value: float(row, "propValue"),
            },
        );
    }
    for row in &main_prop_rows {
        data.main_props
            .insert(num(row, "id") as u32, string(row, "propType").to_owned());
    }
    for row in &level_rows {
        let rarity = num(row, "rank") as u32;
        let level = num(row, "level") as usize;
        for prop in list(row, "addProps") {
            let values = data
                .main_values
                .entry(rarity)
                .or_default()
                .entry(string(prop, "propType").to_owned())
                .or_default();
            if values.len() < level {
                values.resize(level, 0.);
            }
            // The first row is level 0, which the game counts as 1.
            values[level - 1] = float(prop, "value");
        }
    }

    // What can be crafted from lower tiers of the materials upgrades use: three
    // of one make one of the next.
    let mut used: BTreeSet<u32> = BTreeSet::new();
    let mut note = |step: &Step| used.extend(step.items.iter().map(|cost| cost.id));
    for avatar in data.avatars.values() {
        avatar.promotions.iter().for_each(|p| note(&p.cost));
    }
    for depot in data.depots.values() {
        for talent in [&depot.normal, &depot.skill, &depot.burst] {
            talent.upgrades.iter().for_each(&mut note);
        }
    }
    for weapon in data.weapons.values() {
        weapon.promotions.iter().for_each(|p| note(&p.cost));
    }
    let mut seen = BTreeSet::new();
    for row in &combine_rows {
        let inputs: Vec<(u32, u32)> = list(row, "materialItems")
            .iter()
            .map(|item| (num(item, "id") as u32, num(item, "count") as u32))
            .filter(|(id, _)| *id != 0)
            .collect();
        let result = num(row, "resultItemId") as u32;
        if let [(input, count)] = inputs[..]
            && count == 3
            && num(row, "resultItemCount") == 1
            && used.contains(&result)
            && used.contains(&input)
            && seen.insert(result)
        {
            data.recipes.push(Recipe {
                result,
                input,
                input_count: count,
                mora: num(row, "scoinCost") as u32,
            });
        }
    }

    println!(
        "{} characters, {} skill sets, {} weapons, {} materials, {} artifacts in {} sets",
        data.avatars.len(),
        data.depots.len(),
        data.weapons.len(),
        data.materials.len(),
        data.artifacts.len(),
        data.sets.len()
    );

    let json = serde_json::to_vec(&data)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&json)?;
    let packed = encoder.finish()?;
    std::fs::write(OUTPUT, &packed).with_context(|| format!("unable to write {OUTPUT}"))?;
    println!("wrote {OUTPUT}: {} bytes, {} unpacked", packed.len(), json.len());
    Ok(())
}
