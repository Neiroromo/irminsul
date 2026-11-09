//! The planner: pick characters and weapons, say how far each should be taken,
//! and see in total what has to be farmed and how much of it there is already.
//!
//! The goals are kept in a file, so they are there next time.  Where a character
//! or weapon stands now comes from the account in use; one that is not owned,
//! or not out yet, is counted from the start.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, ScrollArea, Stroke,
    TextEdit, Ui, vec2,
};
use serde::{Deserialize, Serialize};

use crate::achievement_data::Language;
use crate::achievement_tab::DIM;
use crate::icons::Icons;
use crate::live_data;
use crate::player_data::Roster;
use crate::roster_tab::{
    BAD, Cell, MORA, OK, add_step, ascension_of, card, depot_of, element, item_cell, material_menu,
    language_choice, level_of, merge, section, stars, talent_level, weapon_type_name,
};
use crate::upgrade_data::{Step, Text, UpgradeData};
use crate::upgrade_store;
use crate::APP_ID;

/// Experience items and how much experience each gives.
const CHARACTER_BOOKS: [(u32, u64); 3] = [(104001, 1000), (104002, 5000), (104003, 20000)];
const WEAPON_ORES: [(u32, u64); 3] = [(104011, 400), (104012, 2000), (104013, 10000)];

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Goal {
    Character {
        id: u32,
        level: u32,
        ascension: u32,
        talents: [u32; 3],
    },
    Weapon {
        id: u32,
        /// The weapon of the account this is, if it is one of those.
        guid: Option<u64>,
        level: u32,
        ascension: u32,
    },
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    goals: Vec<Goal>,
    /// The floating window.
    #[serde(default)]
    overlay: crate::overlay::OverlaySettings,
    /// Materials made of each other are shown as one picture and per cent.
    #[serde(default)]
    percent_mode: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Character,
    Weapon,
}

pub struct Planner {
    goals: Vec<Goal>,
    percent_mode: bool,
    overlay: crate::overlay::Overlay,
    query: String,
    only_missing: bool,
}

fn file_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_PLANNER_FILE") {
        return Some(path.into());
    }
    Some(eframe::storage_dir(APP_ID)?.join("planner.json"))
}

/// What taking something to a goal needs.
#[derive(Default)]
struct Needs {
    /// Items, the mora of the levels included.
    items: BTreeMap<u32, u32>,
    /// Experience of the levels.
    exp: u64,
    /// Whether some step of the way is not known.
    incomplete: bool,
}

/// Where something stands now.
struct Now {
    level: u32,
    ascension: u32,
    talents: [u32; 3],
    /// Whether it is the account's, or counted from the start.
    owned: bool,
}

fn short(count: u64) -> String {
    let trimmed = |value: f64, suffix: &str| {
        let text = format!("{value:.1}");
        format!("{}{suffix}", text.trim_end_matches('0').trim_end_matches('.'))
    };
    if count >= 1_000_000 {
        trimmed(count as f64 / 1_000_000., "M")
    } else if count >= 10_000 {
        trimmed(count as f64 / 1_000., "k")
    } else {
        count.to_string()
    }
}

/// A number with its thousands apart.
pub(crate) fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(digit);
    }
    out
}

fn step_into(needs: &mut Needs, step: Option<Step>) {
    match step {
        Some(step) => add_step(&mut needs.items, &step),
        None => needs.incomplete = true,
    }
}

fn character_now(data: &UpgradeData, roster: &Roster, id: u32) -> Now {
    let info = roster
        .characters
        .iter()
        .find(|info| info.avatar_id == id && info.avatar_type == 1);
    match info {
        Some(info) => {
            let mut talents = [1; 3];
            if let Some(depot) = depot_of(data, info) {
                for (index, talent) in [&depot.normal, &depot.skill, &depot.burst]
                    .into_iter()
                    .enumerate()
                {
                    talents[index] = talent_level(info, talent).0;
                }
            }
            Now {
                level: level_of(info),
                ascension: ascension_of(info),
                talents,
                owned: true,
            }
        }
        None => Now {
            level: 1,
            ascension: 0,
            talents: [1; 3],
            owned: false,
        },
    }
}

fn weapon_now(roster: &Roster, guid: Option<u64>) -> Now {
    let item = guid.and_then(|guid| {
        roster
            .items
            .iter()
            .find(|item| item.guid == guid && item.has_equip() && item.equip().has_weapon())
    });
    match item {
        Some(item) => Now {
            level: item.equip().weapon().level,
            ascension: item.equip().weapon().promote_level,
            talents: [1; 3],
            owned: true,
        },
        None => Now {
            level: 1,
            ascension: 0,
            talents: [1; 3],
            owned: false,
        },
    }
}

fn exp_between(table: &[u32], from: u32, to: u32) -> u64 {
    (from..to)
        .filter_map(|level| table.get(level as usize - 1))
        .map(|exp| *exp as u64)
        .sum()
}

fn character_needs(
    data: &UpgradeData,
    live: &live_data::Upcoming,
    id: u32,
    now: &Now,
    level: u32,
    ascension: u32,
    talents: [u32; 3],
) -> Needs {
    let mut needs = Needs::default();
    let embedded = data.avatars.get(&id);
    let new = live.characters.iter().find(|c| c.id == id);

    for step in now.ascension + 1..=ascension {
        let cost = match (embedded, new) {
            (Some(avatar), _) => avatar
                .promotions
                .get(step as usize)
                .map(|promotion| promotion.cost.clone()),
            (None, Some(new)) => new.ascension_steps.get(step as usize - 1).cloned(),
            _ => None,
        };
        step_into(&mut needs, cost);
    }

    let depot = embedded.and_then(|avatar| data.depots.get(&avatar.default_depot));
    for (index, target) in talents.into_iter().enumerate() {
        for from in now.talents[index]..target {
            let cost = match (depot, new) {
                (Some(depot), _) => [&depot.normal, &depot.skill, &depot.burst][index]
                    .upgrades
                    .get(from as usize - 1)
                    .cloned(),
                (None, Some(new)) => new.talent_steps.get(from as usize - 1).cloned(),
                _ => None,
            };
            step_into(&mut needs, cost);
        }
    }

    needs.exp = exp_between(&data.avatar_exp, now.level, level);
    if needs.exp > 0 {
        *needs.items.entry(MORA).or_default() += (needs.exp / 5) as u32;
    }
    needs
}

fn weapon_needs(
    data: &UpgradeData,
    live: &live_data::Upcoming,
    id: u32,
    now: &Now,
    level: u32,
    ascension: u32,
) -> Needs {
    let mut needs = Needs::default();
    let embedded = data.weapons.get(&id);
    let new = live.weapons.iter().find(|w| w.id == id);
    let rarity = embedded
        .map(|weapon| weapon.rarity)
        .or_else(|| new.map(|new| new.rarity))
        .unwrap_or(5);

    for step in now.ascension + 1..=ascension {
        let cost = match (embedded, new) {
            (Some(weapon), _) => weapon
                .promotions
                .get(step as usize)
                .map(|promotion| promotion.cost.clone()),
            (None, Some(new)) => new.ascension_steps.get(step as usize - 1).cloned(),
            _ => None,
        };
        step_into(&mut needs, cost);
    }

    if let Some(table) = data.weapon_exp.get(&rarity) {
        needs.exp = exp_between(table, now.level, level);
    }
    if needs.exp > 0 {
        *needs.items.entry(MORA).or_default() += (needs.exp / 10) as u32;
    }
    needs
}

/// Name, picture and rarity of what a goal is about.
struct Subject {
    name: Text,
    icon: String,
    rarity: u32,
    /// Not out yet.
    upcoming: bool,
}

fn character_subject(data: &UpgradeData, live: &live_data::Upcoming, id: u32) -> Option<Subject> {
    if let Some(avatar) = data.avatars.get(&id) {
        return Some(Subject {
            name: avatar.name.clone(),
            icon: avatar.icon.clone(),
            rarity: avatar.rarity,
            upcoming: false,
        });
    }
    live.characters.iter().find(|c| c.id == id).map(|c| Subject {
        name: c.name.clone(),
        icon: c.icon.clone(),
        rarity: c.rarity,
        upcoming: true,
    })
}

fn weapon_subject(data: &UpgradeData, live: &live_data::Upcoming, id: u32) -> Option<Subject> {
    if let Some(weapon) = data.weapons.get(&id) {
        return Some(Subject {
            name: weapon.name.clone(),
            icon: weapon.icon.clone(),
            rarity: weapon.rarity,
            upcoming: false,
        });
    }
    live.weapons.iter().find(|w| w.id == id).map(|w| Subject {
        name: w.name.clone(),
        icon: w.icon.clone(),
        rarity: w.rarity,
        upcoming: true,
    })
}

/// The items with how many there are and how many are needed, in rows that
/// fit the width, red where there are too few.
fn need_cells(
    ui: &mut Ui,
    icons: &Icons,
    owned: &HashMap<u32, u32>,
    russian: bool,
    items: &BTreeMap<u32, u32>,
    only_missing: bool,
) {
    const CELL: f32 = 72.;
    const GAP: f32 = 8.;
    let data = upgrade_store::data();
    let live = live_data::current();

    let mut ids: Vec<u32> = items
        .iter()
        .filter(|(id, need)| {
            !only_missing || owned.get(id).copied().unwrap_or(0) < **need
        })
        .map(|(id, _)| *id)
        .collect();
    if ids.is_empty() {
        ui.label(RichText::new("—").color(DIM));
        return;
    }
    // Mora first, the rest as the game orders them.
    ids.sort_by_key(|id| (*id != MORA, *id));

    let per_row = (((ui.available_width() + GAP) / (CELL + GAP)).floor() as usize).max(1);
    for row in ids.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, 0.);
            for id in row {
                let need = items[id] as u64;
                let have = owned.get(id).copied().unwrap_or(0) as u64;
                let color = if have >= need { OK } else { BAD };
                let material = data.materials.get(id).or_else(|| live.materials.get(id));
                let name = material
                    .map_or_else(|| format!("#{id}"), |m| m.name.get(russian).to_owned());
                ui.allocate_ui_with_layout(
                    vec2(CELL, 96.),
                    Layout::top_down(Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 2.;
                        let cell = Cell {
                            icon: material.map_or("", |material| material.icon.as_str()),
                            rarity: material.map_or(1, |material| material.rarity.max(1)),
                            stars: false,
                            label: None,
                            selected: false,
                        };
                        let response = item_cell(ui, icons, 56., &cell).on_hover_text(format!(
                            "{name}\n{} / {}",
                            grouped(have),
                            grouped(need)
                        ));
                        material_menu(&response, &[*id], russian);
                        ui.add(
                            egui::Label::new(RichText::new(&name).size(11.).color(DIM)).truncate(),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{}/{}", short(have), short(need)))
                                    .size(12.)
                                    .color(color),
                            )
                            .truncate(),
                        );
                    },
                );
            }
        });
        ui.add_space(6.);
    }
}

// ---------------------------------------------------------------------------
// Materials that are made of lower ones.

/// A chain of materials in which three of one make one of the next.
pub(crate) struct Family {
    /// The lowest rarity first.
    pub(crate) ids: Vec<u32>,
    /// What making one of each costs in mora; the first is not made.
    pub(crate) mora: Vec<u32>,
}

/// The chains there are: those of the data built in, and the new ones that
/// only the upcoming content has.
pub(crate) fn families(data: &UpgradeData, live: &live_data::Upcoming) -> Vec<Family> {
    let made_from: HashMap<u32, (u32, u32)> = data
        .recipes
        .iter()
        .map(|recipe| (recipe.result, (recipe.input, recipe.mora)))
        .collect();
    let next: HashMap<u32, u32> = data
        .recipes
        .iter()
        .map(|recipe| (recipe.input, recipe.result))
        .collect();
    let heads: BTreeSet<u32> = data
        .recipes
        .iter()
        .map(|recipe| recipe.input)
        .filter(|input| !made_from.contains_key(input))
        .collect();
    let mut families = Vec::new();
    for head in heads {
        let mut ids = vec![head];
        let mut mora = vec![0];
        let mut current = head;
        while let Some(&result) = next.get(&current) {
            if ids.len() > 8 {
                break;
            }
            ids.push(result);
            mora.push(made_from[&result].1);
            current = result;
        }
        if ids.len() >= 2 {
            families.push(Family { ids, mora });
        }
    }
    let mut known: HashSet<u32> = families.iter().flat_map(|f| f.ids.iter().copied()).collect();
    for chain in live
        .characters
        .iter()
        .flat_map(|c| &c.chains)
        .chain(live.weapons.iter().flat_map(|w| &w.chains))
    {
        if chain.iter().any(|id| known.contains(id)) {
            continue;
        }
        known.extend(chain.iter().copied());
        families.push(Family {
            ids: chain.clone(),
            // What the new ones cost to craft is not known.
            mora: vec![0; chain.len()],
        });
    }
    families
}

/// What crafting can and cannot do for a family.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Craft {
    /// How many of each are made, in the order of the family.
    pub(crate) crafted: Vec<u64>,
    /// What is still missing of the lowest, which cannot be made.
    pub(crate) farm: u64,
    /// What the crafting costs in mora.
    pub(crate) mora: u64,
}

/// Works from the top down: what is missing of a rarity has to be made of the
/// one below, three for each.  What is left over of a lower rarity cannot be
/// made into a lower one, nor does a higher one help with a lower need.
pub(crate) fn solve(family: &Family, need: impl Fn(u32) -> u64, have: impl Fn(u32) -> u64) -> Craft {
    let count = family.ids.len();
    let mut crafted = vec![0; count];
    let mut demand = 0u64;
    let mut farm = 0;
    for index in (0..count).rev() {
        let id = family.ids[index];
        let short = (need(id) + demand).saturating_sub(have(id));
        if index == 0 {
            farm = short;
        } else {
            crafted[index] = short;
            demand = short * 3;
        }
    }
    let mora = crafted
        .iter()
        .zip(&family.mora)
        .map(|(crafted, mora)| crafted * *mora as u64)
        .sum();
    Craft { crafted, farm, mora }
}

const CELL: f32 = 72.;
const GAP: f32 = 8.;

/// How much of a chain of materials is there, from 0 to 1: of everything that
/// has to be farmed of the lowest rarity for what is needed, how much is not to
/// be farmed any more, what is on hand and crafting being counted.  Counted in
/// the lowest rarity, as three of one rarity make one of the next.
pub(crate) fn family_percent(
    family: &Family,
    need: impl Fn(u32) -> u64 + Copy,
    have: impl Fn(u32) -> u64 + Copy,
) -> f32 {
    let mut required = 0u64;
    let mut weight = 1u64;
    for id in &family.ids {
        required += need(*id) * weight;
        weight *= 3;
    }
    if required == 0 {
        return 1.;
    }
    let farm = solve(family, need, have).farm;
    (required.saturating_sub(farm)) as f32 / required as f32
}

/// The rarity that stands for the chain: the highest one that is needed.
pub(crate) fn chain_top(family: &Family, need: impl Fn(u32) -> u64) -> u32 {
    family
        .ids
        .iter()
        .rev()
        .copied()
        .find(|id| need(*id) > 0)
        .unwrap_or(*family.ids.last().unwrap_or(&0))
}

/// A chain as one picture, with how much of it there is in per cent.
fn percent_cell(
    ui: &mut Ui,
    icons: &Icons,
    russian: bool,
    family: &Family,
    need: impl Fn(u32) -> u64 + Copy,
    have: impl Fn(u32) -> u64 + Copy,
) {
    let data = upgrade_store::data();
    let live = live_data::current();
    let percent = (family_percent(family, need, have) * 100.).floor() as u32;
    let top = chain_top(family, need);
    let material = |id: u32| data.materials.get(&id).or_else(|| live.materials.get(&id));
    let name_of = |id: u32| {
        material(id).map_or_else(|| format!("#{id}"), |m| m.name.get(russian).to_owned())
    };
    let tiers: Vec<String> = family
        .ids
        .iter()
        .filter(|id| need(**id) > 0 || have(**id) > 0)
        .map(|id| format!("{}: {} / {}", name_of(*id), grouped(have(*id)), grouped(need(*id))))
        .collect();
    let color = if percent >= 100 { OK } else { Color32::from_rgb(0xff, 0xc3, 0x4a) };
    ui.allocate_ui_with_layout(vec2(CELL, 96.), Layout::top_down(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 2.;
        let cell = Cell {
            icon: material(top).map_or("", |m| m.icon.as_str()),
            rarity: material(top).map_or(1, |m| m.rarity.max(1)),
            stars: false,
            label: None,
            selected: false,
        };
        let response = item_cell(ui, icons, 56., &cell).on_hover_text(tiers.join("\n"));
        material_menu(&response, &family.ids, russian);
        ui.add(egui::Label::new(RichText::new(name_of(top)).size(11.).color(DIM)).truncate());
        ui.add(
            egui::Label::new(RichText::new(format!("{percent}%")).size(13.).strong().color(color))
                .truncate(),
        );
    });
}

/// One material, with how many there are and how many are needed.
fn need_cell(
    ui: &mut Ui,
    icons: &Icons,
    owned: &HashMap<u32, u32>,
    russian: bool,
    id: u32,
    need: u64,
) {
    let data = upgrade_store::data();
    let live = live_data::current();
    let have = owned.get(&id).copied().unwrap_or(0) as u64;
    let color = if have >= need { OK } else { BAD };
    let material = data.materials.get(&id).or_else(|| live.materials.get(&id));
    let name = material.map_or_else(|| format!("#{id}"), |m| m.name.get(russian).to_owned());
    ui.allocate_ui_with_layout(vec2(CELL, 96.), Layout::top_down(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 2.;
        let cell = Cell {
            icon: material.map_or("", |material| material.icon.as_str()),
            rarity: material.map_or(1, |material| material.rarity.max(1)),
            stars: false,
            label: None,
            selected: false,
        };
        let response = item_cell(ui, icons, 56., &cell).on_hover_text(format!(
            "{name}\n{} / {}",
            grouped(have),
            grouped(need)
        ));
        material_menu(&response, &[id], russian);
        ui.add(egui::Label::new(RichText::new(&name).size(11.).color(DIM)).truncate());
        ui.add(
            egui::Label::new(
                RichText::new(format!("{}/{}", short(have), short(need)))
                    .size(12.)
                    .color(color),
            )
            .truncate(),
        );
    });
}

enum Block {
    Single(u32),
    Family(usize, Craft),
}

/// The materials of the plan like `need_cells`, with the ones that can be
/// crafted from each other in a frame, and under it what crafting makes of
/// what there is: which can be made, and what is still missing of the lowest.
fn need_blocks(
    ui: &mut Ui,
    icons: &Icons,
    owned: &HashMap<u32, u32>,
    russian: bool,
    items: &BTreeMap<u32, u32>,
    only_missing: bool,
    families: &[Family],
    percent: bool,
) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    let data = upgrade_store::data();
    let live = live_data::current();
    let have = |id: u32| owned.get(&id).copied().unwrap_or(0) as u64;
    let need = |id: u32| items.get(&id).copied().unwrap_or(0) as u64;

    // Which family each needed material belongs to.
    let mut in_family: HashMap<u32, usize> = HashMap::new();
    for (index, family) in families.iter().enumerate() {
        if family.ids.iter().any(|id| need(*id) > 0) {
            for id in &family.ids {
                in_family.insert(*id, index);
            }
        }
    }

    let mut blocks: Vec<(u32, Block)> = Vec::new();
    for (id, count) in items {
        if in_family.contains_key(id) {
            continue;
        }
        if only_missing && have(*id) >= *count as u64 {
            continue;
        }
        let key = if *id == MORA { 0 } else { *id };
        blocks.push((key, Block::Single(*id)));
    }
    let mut seen = HashSet::new();
    for index in in_family.values() {
        if !seen.insert(*index) {
            continue;
        }
        let family = &families[*index];
        let craft = solve(family, need, have);
        // A chain that is all there once what can be crafted is crafted has
        // nothing missing to farm, so it is not shown among the missing.
        if only_missing && craft.farm == 0 {
            continue;
        }
        blocks.push((family.ids[0], Block::Family(*index, craft)));
    }
    if blocks.is_empty() {
        ui.label(RichText::new("—").color(DIM));
        return;
    }
    blocks.sort_by_key(|(key, _)| *key);

    // What is shown of a family: the rarities that are needed or on hand.
    let shown = |family: &Family| -> Vec<u32> {
        family
            .ids
            .iter()
            .copied()
            .filter(|id| need(*id) > 0 || have(*id) > 0)
            .collect()
    };
    let width_of = |block: &Block| match block {
        Block::Single(_) => CELL,
        Block::Family(..) if percent => CELL,
        Block::Family(index, _) => {
            let count = shown(&families[*index]).len().max(1) as f32;
            count * CELL + (count - 1.) * GAP + 2. * 9.
        }
    };

    // The materials that are not made of others first, then the chains, each
    // in rows as wide as there is room for; the chains start on a new row.
    let (singles, chains): (Vec<&(u32, Block)>, Vec<&(u32, Block)>) = blocks
        .iter()
        .partition(|(_, block)| matches!(block, Block::Single(_)));
    let room = ui.available_width();
    let mut rows: Vec<Vec<&(u32, Block)>> = Vec::new();
    for group in [singles, chains] {
        let mut current: Vec<&(u32, Block)> = Vec::new();
        let mut used = 0.;
        for block in group {
            let width = width_of(&block.1);
            if used > 0. && used + GAP + width > room {
                rows.push(std::mem::take(&mut current));
                used = 0.;
            }
            used += width + GAP;
            current.push(block);
        }
        if !current.is_empty() {
            rows.push(current);
        }
    }

    for row in rows {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, 0.);
            for (_, block) in row {
                match block {
                    Block::Single(id) => need_cell(ui, icons, owned, russian, *id, need(*id)),
                    Block::Family(index, _) if percent => {
                        percent_cell(ui, icons, russian, &families[*index], need, have);
                    }
                    Block::Family(index, craft) => {
                        let family = &families[*index];
                        let ids = shown(family);
                        let satisfied = family.ids.iter().all(|id| have(*id) >= need(*id));
                        let colour = if satisfied {
                            Color32::from_gray(0x70)
                        } else if craft.farm == 0 {
                            OK
                        } else {
                            BAD
                        };
                        let inner = width_of(block) - 2. * 9.;
                        Frame::new()
                            .stroke(Stroke::new(1.5_f32, colour))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(8))
                            .show(ui, |ui| {
                              // A frame goes on in the layout it is in, which
                              // here is a row; its contents go down.
                              ui.vertical(|ui| {
                                ui.set_width(inner);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing = vec2(GAP, 0.);
                                    for id in &ids {
                                        need_cell(ui, icons, owned, russian, *id, need(*id));
                                    }
                                });
                                if satisfied {
                                    return;
                                }
                                let name_of = |id: u32| {
                                    data.materials
                                        .get(&id)
                                        .or_else(|| live.materials.get(&id))
                                        .map_or_else(
                                            || format!("#{id}"),
                                            |m| {
                                                format!(
                                                    "{} {}",
                                                    stars(m.rarity),
                                                    m.name.get(russian)
                                                )
                                            },
                                        )
                                };
                                let made: Vec<String> = craft
                                    .crafted
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, count)| **count > 0)
                                    .map(|(index, count)| {
                                        format!("{count} × {}", name_of(family.ids[index]))
                                    })
                                    .collect();
                                if !made.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "{}: {}",
                                                tr("Craft", "Скрафтить"),
                                                made.join(", ")
                                            ))
                                            .size(11.)
                                            .color(DIM),
                                        )
                                        .wrap(),
                                    );
                                }
                                if craft.mora > 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}: {}",
                                            tr("Mora for crafting", "Мора на крафт"),
                                            short(craft.mora)
                                        ))
                                        .size(11.)
                                        .color(DIM),
                                    );
                                }
                                let text = if craft.farm == 0 {
                                    tr("Can be crafted", "Можно создать").to_owned()
                                } else {
                                    format!(
                                        "{}: {} × {}",
                                        tr("Missing", "Не хватает"),
                                        grouped(craft.farm),
                                        name_of(family.ids[0])
                                    )
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(text).size(12.).strong().color(colour),
                                    )
                                    .wrap(),
                                );
                              });
                            });
                    }
                }
            }
        });
        ui.add_space(8.);
    }
}

impl Planner {
    pub fn load() -> Self {
        let saved: Saved = file_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            goals: saved.goals,
            percent_mode: saved.percent_mode,
            overlay: crate::overlay::Overlay::new(saved.overlay),
            query: String::new(),
            only_missing: false,
        }
    }

    fn save(&self) {
        let Some(path) = file_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let saved = Saved {
            goals: self.goals.clone(),
            overlay: self.overlay.settings.clone(),
            percent_mode: self.percent_mode,
        };
        let temp = path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(&saved)
            && std::fs::write(&temp, bytes).is_ok()
        {
            let _ = std::fs::rename(&temp, &path);
        }
    }

    /// Whether the floating window is up, or being picked.
    pub fn overlay_active(&self) -> bool {
        self.overlay.is_open() || self.overlay.is_choosing()
    }

    /// The floating window, every frame while it is open, whatever tab is shown.
    pub fn overlay_frame(
        &mut self,
        ctx: &egui::Context,
        icons: &Icons,
        owned: &HashMap<u32, u32>,
        roster: &Roster,
        russian: bool,
    ) {
        let needs = self.needs_total(roster).unwrap_or_default();
        let families = families(upgrade_store::data(), &live_data::current());
        let mut save = self.overlay.choosing_ui(
            ctx,
            icons,
            owned,
            &needs,
            &families,
            self.percent_mode,
            russian,
        );
        save |= self.overlay.frame(
            ctx,
            icons,
            owned,
            &needs,
            &families,
            self.percent_mode,
            russian,
        );
        if save {
            self.save();
        }
    }

    /// What all the goals need in all, or nothing if there are no goals.
    pub fn needs_total(&self, roster: &Roster) -> Option<BTreeMap<u32, u32>> {
        if self.goals.is_empty() {
            return None;
        }
        let data = upgrade_store::data();
        let live = live_data::current();
        let mut total = BTreeMap::new();
        for goal in &self.goals {
            let needs = match goal {
                Goal::Character {
                    id,
                    level,
                    ascension,
                    talents,
                } => {
                    let now = character_now(data, roster, *id);
                    character_needs(data, &live, *id, &now, *level, *ascension, *talents)
                }
                Goal::Weapon {
                    id,
                    guid,
                    level,
                    ascension,
                } => {
                    let now = weapon_now(roster, *guid);
                    weapon_needs(data, &live, *id, &now, *level, *ascension)
                }
            };
            merge(&mut total, &needs.items);
        }
        Some(total)
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        icons: &Icons,
        owned: &HashMap<u32, u32>,
        roster: &Roster,
        language: &mut Language,
    ) {
        let russian = *language == Language::Ru;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = upgrade_store::data();
        let live = live_data::current();
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Planner", "Планировщик")).size(34.).strong());
            ui.add_space(12.);
            // The list to add from opens under its button; the button again, or a
            // click anywhere else, closes it.
            for (kind, label) in [
                (Picker::Character, tr("+ Character", "+ Персонаж")),
                (Picker::Weapon, tr("+ Weapon", "+ Оружие")),
            ] {
                let button = ui.button(RichText::new(label).size(16.));
                if button.clicked() {
                    self.query.clear();
                }
                egui::Popup::from_toggle_button_response(&button)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .align(egui::RectAlign::BOTTOM_START)
                    .show(|ui| {
                        ui.set_width(540.);
                        changed |= self.picker_content(ui, icons, roster, kind, russian);
                    });
            }
            ui.add_space(12.);
            ui.checkbox(&mut self.only_missing, tr("Only what is missing", "Только недостающее"));
            if ui
                .checkbox(
                    &mut self.percent_mode,
                    tr("Per cent for crafted", "Проценты для составных"),
                )
                .on_hover_text(tr(
                    "Materials made of each other show as one picture and how much of it there is, in per cent. Also in the floating window.",
                    "Материалы, которые крафтятся друг из друга, показываются одной иконкой и процентом. Также в плавающем окне.",
                ))
                .changed()
            {
                changed = true;
            }
            if ui
                .button(RichText::new(tr("Floating window", "Плавающее окно")).size(16.))
                .on_hover_text(tr(
                    "A window with what to farm, over the game. Hold Alt to move it.",
                    "Окно с тем, что нужно собрать, поверх игры. Зажмите Alt, чтобы двигать.",
                ))
                .clicked()
                && let Some(total) = self.needs_total(roster)
            {
                self.overlay.start_choosing(&total, owned);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                language_choice(ui, language);
            });
        });
        ui.add_space(6.);

        // The totals of all goals.
        let mut total: BTreeMap<u32, u32> = BTreeMap::new();
        let (mut character_exp, mut weapon_exp) = (0u64, 0u64);
        let mut incomplete = false;
        let mut cards: Vec<Option<Needs>> = Vec::new();
        for goal in &self.goals {
            let needs = match goal {
                Goal::Character {
                    id,
                    level,
                    ascension,
                    talents,
                } => {
                    let now = character_now(data, roster, *id);
                    let needs = character_needs(data, &live, *id, &now, *level, *ascension, *talents);
                    character_exp += needs.exp;
                    needs
                }
                Goal::Weapon {
                    id,
                    guid,
                    level,
                    ascension,
                } => {
                    let now = weapon_now(roster, *guid);
                    let needs = weapon_needs(data, &live, *id, &now, *level, *ascension);
                    weapon_exp += needs.exp;
                    needs
                }
            };
            merge(&mut total, &needs.items);
            incomplete |= needs.incomplete;
            cards.push(Some(needs));
        }

        let body_height = (ui.available_height() - 30.).max(100.);
        let right_width = 420.;
        let left_width = (ui.available_width() - right_width - 12.).max(360.);
        let mut remove = None;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(left_width, body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("planner_goals")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 12.);
                            ui.spacing_mut().item_spacing.y = 10.;
                            if self.goals.is_empty() {
                                ui.label(
                                    RichText::new(tr(
                                        "Add a character or a weapon to plan its upgrades.",
                                        "Добавьте персонажа или оружие, чтобы спланировать прокачку.",
                                    ))
                                    .color(DIM),
                                );
                            }
                            for (index, goal) in self.goals.iter_mut().enumerate() {
                                let needs = cards[index].take().unwrap_or_default();
                                let (edited, removed) = goal_card(
                                    ui, icons, owned, data, &live, roster, goal, &needs, index,
                                    russian,
                                );
                                changed |= edited;
                                if removed {
                                    remove = Some(index);
                                }
                            }
                        });
                },
            );

            ui.add_space(4.);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("planner_total")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 12.);
                            self.totals(
                                ui,
                                icons,
                                owned,
                                russian,
                                &total,
                                character_exp,
                                weapon_exp,
                                incomplete,
                            );
                        });
                },
            );
        });

        if let Some(index) = remove {
            self.goals.remove(index);
            changed = true;
        }

        if changed {
            self.save();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn totals(
        &self,
        ui: &mut Ui,
        icons: &Icons,
        owned: &HashMap<u32, u32>,
        russian: bool,
        total: &BTreeMap<u32, u32>,
        character_exp: u64,
        weapon_exp: u64,
        incomplete: bool,
    ) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        self.experience(ui, owned, russian, character_exp, weapon_exp);
        section(ui, tr("In all, to farm", "Всего нужно"));
        card(ui, |ui| {
            if self.goals.is_empty() {
                ui.label(RichText::new("—").color(DIM));
                return;
            }
            let families = families(upgrade_store::data(), &live_data::current());
            need_blocks(
                ui,
                icons,
                owned,
                russian,
                total,
                self.only_missing,
                &families,
                self.percent_mode,
            );
            // There are no frames to explain when the chains are shown as per cent.
            if !self.percent_mode {
                ui.label(
                    RichText::new(tr(
                        "Materials in a frame can be crafted from the lower ones, three for one.",
                        "Материалы в рамке можно крафтить из низших: три за один.",
                    ))
                    .size(12.)
                    .color(DIM),
                );
            }
            if incomplete {
                ui.label(
                    RichText::new(tr(
                        "For some steps there is no data yet, so the list is not complete.",
                        "По некоторым шагам данных пока нет, поэтому список неполный.",
                    ))
                    .size(12.)
                    .color(BAD),
                );
            }
        });

    }

    fn experience(
        &self,
        ui: &mut Ui,
        owned: &HashMap<u32, u32>,
        russian: bool,
        character_exp: u64,
        weapon_exp: u64,
    ) {
        let has_character = self
            .goals
            .iter()
            .any(|goal| matches!(goal, Goal::Character { .. }));
        let has_weapon = self
            .goals
            .iter()
            .any(|goal| matches!(goal, Goal::Weapon { .. }));
        if !has_character && !has_weapon {
            return;
        }
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = upgrade_store::data();
        let name_of = |id: u32| {
            data.materials
                .get(&id)
                .map_or_else(|| format!("#{id}"), |m| m.name.get(russian).to_owned())
        };
        // What the experience items on the account are worth in all.
        let worth = |items: &[(u32, u64)]| -> (u64, String) {
            let mut total = 0;
            let mut parts = Vec::new();
            for (id, exp) in items.iter().rev() {
                let count = owned.get(id).copied().unwrap_or(0) as u64;
                total += count * exp;
                parts.push(format!("{} × {}", name_of(*id), count));
            }
            (total, parts.join(",  "))
        };
        let (have_character_exp, character_items) = worth(&CHARACTER_BOOKS);
        let (have_weapon_exp, weapon_items) = worth(&WEAPON_ORES);

        section(ui, tr("Experience", "Опыт"));
        card(ui, |ui| {
            for (label, need, have, items, show) in [
                (
                    tr("For characters (hero's wit and the like)", "Персонажам (книги опыта)"),
                    character_exp,
                    have_character_exp,
                    &character_items,
                    has_character,
                ),
                (
                    tr("For weapons (enhancement ore)", "Оружию (руда для улучшения)"),
                    weapon_exp,
                    have_weapon_exp,
                    &weapon_items,
                    has_weapon,
                ),
            ] {
                if !show {
                    continue;
                }
                ui.label(RichText::new(label).size(14.).strong());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tr("Have", "Есть")).size(13.).color(DIM));
                    ui.label(RichText::new(format!("{} exp", grouped(have))).size(15.).strong());
                    ui.label(RichText::new(tr("needed", "нужно")).size(13.).color(DIM));
                    ui.label(RichText::new(format!("{} exp", grouped(need))).size(15.).strong());
                    let enough = have >= need;
                    ui.label(
                        RichText::new(if enough {
                            tr("enough", "хватает").to_owned()
                        } else {
                            format!("{} {} exp", tr("missing", "не хватает"), grouped(need - have))
                        })
                        .size(15.)
                        .strong()
                        .color(if enough { OK } else { BAD }),
                    );
                });
                ui.add(egui::Label::new(RichText::new(items).size(12.).color(DIM)).wrap());
                ui.add_space(8.);
            }
        });
    }

    /// The list to pick from, as pictures.  Returns whether a goal was added.
    fn picker_content(
        &mut self,
        ui: &mut Ui,
        icons: &Icons,
        roster: &Roster,
        kind: Picker,
        russian: bool,
    ) -> bool {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = upgrade_store::data();
        let live = live_data::current();
        let query = self.query.trim().to_lowercase();

        // (what to add, name, picture, rarity, a line under the name)
        struct Entry {
            goal: Goal,
            name: Text,
            icon: String,
            rarity: u32,
            subtitle: String,
        }
        let matches = |name: &Text| {
            query.is_empty()
                || name.en.to_lowercase().contains(&query)
                || name.ru.to_lowercase().contains(&query)
        };
        let mut entries: Vec<Entry> = Vec::new();
        let default_character = |id: u32| Goal::Character {
            id,
            level: 90,
            ascension: 6,
            talents: [10, 10, 10],
        };
        match kind {
            Picker::Character => {
                for c in &live.characters {
                    if matches(&c.name) {
                        entries.push(Entry {
                            goal: default_character(c.id),
                            name: c.name.clone(),
                            icon: c.icon.clone(),
                            rarity: c.rarity,
                            subtitle: format!(
                                "{}   {}",
                                tr("Upcoming", "Скоро"),
                                element(&c.element, russian).0
                            ),
                        });
                    }
                }
                let mut released: Vec<(&u32, &crate::upgrade_data::Avatar)> = data
                    .avatars
                    .iter()
                    .filter(|(id, avatar)| {
                        live.is_official_character(**id)
                            && avatar.promotions.len() == 7
                            && avatar.promotions[1].cost.items.len() > 0
                            && data
                                .depots
                                .get(&avatar.default_depot)
                                .is_some_and(|depot| depot.normal.upgrades.len() == 9)
                    })
                    .collect();
                released.sort_by_key(|(id, avatar)| {
                    (std::cmp::Reverse(avatar.rarity), avatar.name.en.clone(), **id)
                });
                for (id, avatar) in released {
                    if matches(&avatar.name) {
                        let depot = data.depots.get(&avatar.default_depot);
                        let owned = roster
                            .characters
                            .iter()
                            .any(|info| info.avatar_id == *id && info.avatar_type == 1);
                        entries.push(Entry {
                            goal: default_character(*id),
                            name: avatar.name.clone(),
                            icon: avatar.icon.clone(),
                            rarity: avatar.rarity,
                            subtitle: format!(
                                "{}   {}",
                                depot.map_or("", |depot| element(&depot.element, russian).0),
                                if owned { tr("yours", "у вас") } else { "" }
                            ),
                        });
                    }
                }
            }
            Picker::Weapon => {
                // The weapons of the account first.
                let mut mine: Vec<&crate::player_data::Item> = roster
                    .items
                    .iter()
                    .filter(|item| item.has_equip() && item.equip().has_weapon())
                    .collect();
                mine.sort_by_key(|item| {
                    let rarity = data.weapons.get(&item.item_id).map_or(0, |w| w.rarity);
                    (std::cmp::Reverse(rarity), std::cmp::Reverse(item.equip().weapon().level))
                });
                for item in mine {
                    let Some(weapon) = data.weapons.get(&item.item_id) else {
                        continue;
                    };
                    if weapon.rarity >= 3 && matches(&weapon.name) {
                        entries.push(Entry {
                            goal: Goal::Weapon {
                                id: item.item_id,
                                guid: Some(item.guid),
                                level: 90,
                                ascension: 6,
                            },
                            name: weapon.name.clone(),
                            icon: weapon.icon.clone(),
                            rarity: weapon.rarity,
                            subtitle: format!(
                                "{}   {} {}",
                                tr("yours", "у вас"),
                                tr("Lv.", "Ур."),
                                item.equip().weapon().level
                            ),
                        });
                    }
                }
                for w in &live.weapons {
                    if matches(&w.name) {
                        entries.push(Entry {
                            goal: Goal::Weapon {
                                id: w.id,
                                guid: None,
                                level: 90,
                                ascension: 6,
                            },
                            name: w.name.clone(),
                            icon: w.icon.clone(),
                            rarity: w.rarity,
                            subtitle: format!(
                                "{}   {}",
                                tr("Upcoming", "Скоро"),
                                weapon_type_name(&w.weapon_type, russian)
                            ),
                        });
                    }
                }
                let mut released: Vec<(&u32, &crate::upgrade_data::Weapon)> = data
                    .weapons
                    .iter()
                    .filter(|(id, weapon)| {
                        live.is_official_weapon(**id)
                            && weapon.rarity >= 3
                            && weapon.promotions.len() == 7
                            && weapon.promotions[1].cost.items.len() > 0
                    })
                    .collect();
                released.sort_by_key(|(id, weapon)| {
                    (std::cmp::Reverse(weapon.rarity), weapon.name.en.clone(), **id)
                });
                for (id, weapon) in released {
                    if matches(&weapon.name) {
                        entries.push(Entry {
                            goal: Goal::Weapon {
                                id: *id,
                                guid: None,
                                level: 90,
                                ascension: 6,
                            },
                            name: weapon.name.clone(),
                            icon: weapon.icon.clone(),
                            rarity: weapon.rarity,
                            subtitle: weapon_type_name(&weapon.weapon_type, russian).to_owned(),
                        });
                    }
                }
            }
        }

        const PICK_CELL: f32 = 64.;
        const PICK_GAP: f32 = 8.;
        let mut added = None;
        ui.add(
            TextEdit::singleline(&mut self.query)
                .hint_text(tr("Search by name", "Поиск по названию"))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.);
        // Pictures in a grid, like the filter of the materials; a click adds.
        let per_row =
            (((ui.available_width() + PICK_GAP) / (PICK_CELL + PICK_GAP)).floor() as usize).max(1);
        let rows: Vec<&[Entry]> = entries.chunks(per_row).collect();
        // Only the rows in view are drawn, so only their pictures are fetched.
        ScrollArea::vertical()
            .max_height(420.)
            .auto_shrink([false, true])
            .show_rows(ui, PICK_CELL + PICK_GAP, rows.len(), |ui, range| {
                for row in &rows[range] {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = vec2(PICK_GAP, PICK_GAP);
                        for entry in *row {
                            let cell = Cell {
                                icon: &entry.icon,
                                rarity: entry.rarity,
                                stars: false,
                                label: None,
                                selected: false,
                            };
                            if item_cell(ui, icons, PICK_CELL, &cell)
                                .on_hover_text(format!(
                                    "{}\n{}",
                                    entry.name.get(russian),
                                    entry.subtitle
                                ))
                                .clicked()
                            {
                                added = Some(entry.goal.clone());
                            }
                        }
                    });
                    ui.add_space(PICK_GAP);
                }
            });
        if let Some(goal) = added {
            self.goals.push(goal);
            // What was picked is added, and the list closes.
            ui.close();
            return true;
        }
        false
    }
}

/// One goal: what it is, where it stands, the controls and what it needs.
/// Returns whether it was edited, and whether it is to be removed.
#[allow(clippy::too_many_arguments)]
fn goal_card(
    ui: &mut Ui,
    icons: &Icons,
    owned: &HashMap<u32, u32>,
    data: &UpgradeData,
    live: &live_data::Upcoming,
    roster: &Roster,
    goal: &mut Goal,
    needs: &Needs,
    index: usize,
    russian: bool,
) -> (bool, bool) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    let mut edited = false;
    let mut removed = false;

    let (subject, now) = match goal {
        Goal::Character { id, .. } => (
            character_subject(data, live, *id),
            character_now(data, roster, *id),
        ),
        Goal::Weapon { id, guid, .. } => (weapon_subject(data, live, *id), weapon_now(roster, *guid)),
    };
    let Some(subject) = subject else {
        // Something the data does not know any more.
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Unknown item", "Неизвестный объект")).color(DIM));
                if ui.button(egui_material_icons::icons::ICON_CLOSE).clicked() {
                    removed = true;
                }
            });
        });
        return (false, removed);
    };

    card(ui, |ui| {
        ui.horizontal(|ui| {
            let cell = Cell {
                icon: &subject.icon,
                rarity: subject.rarity,
                stars: true,
                label: None,
                selected: false,
            };
            item_cell(ui, icons, 64., &cell);
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(subject.name.get(russian)).size(20.).strong());
                    if subject.upcoming {
                        ui.label(RichText::new(tr("upcoming", "скоро")).size(13.).color(DIM));
                    }
                });
                let state = if now.owned {
                    match goal {
                        Goal::Character { .. } => format!(
                            "{}: {} {}, {} {}, {} {}/{}/{}",
                            tr("Now", "Сейчас"),
                            tr("Lv.", "Ур."),
                            now.level,
                            tr("ascension", "возв."),
                            now.ascension,
                            tr("talents", "таланты"),
                            now.talents[0],
                            now.talents[1],
                            now.talents[2]
                        ),
                        Goal::Weapon { .. } => format!(
                            "{}: {} {}, {} {}",
                            tr("Now", "Сейчас"),
                            tr("Lv.", "Ур."),
                            now.level,
                            tr("ascension", "возв."),
                            now.ascension
                        ),
                    }
                } else {
                    tr(
                        "Not owned: counted from the start",
                        "Нет в аккаунте: считаю с нуля",
                    )
                    .to_owned()
                };
                ui.add(
                    egui::Label::new(RichText::new(state).size(12.).color(DIM)).wrap(),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                if ui
                    .button(egui_material_icons::icons::ICON_CLOSE)
                    .on_hover_text(tr("Remove", "Убрать"))
                    .clicked()
                {
                    removed = true;
                }
            });
        });
        ui.add_space(4.);

        // Controls: drag along the line.  The stars are the ascensions.
        match goal {
            Goal::Character {
                level,
                ascension,
                talents,
                ..
            } => {
                edited |= level_slider(ui, ascension, level, russian);
                ui.add_space(4.);
                for (slot, label) in [
                    (0, tr("Attack", "Атака")),
                    (1, tr("Skill", "Навык")),
                    (2, tr("Burst", "Взрыв")),
                ] {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [52., 30.],
                            egui::Label::new(RichText::new(label).size(13.).color(DIM)),
                        );
                        edited |= talent_slider(ui, &mut talents[slot]);
                    });
                }
            }
            Goal::Weapon {
                level, ascension, ..
            } => {
                edited |= level_slider(ui, ascension, level, russian);
            }
        }

        ui.add_space(4.);
        egui::CollapsingHeader::new(RichText::new(tr("Needs", "Что нужно")).size(14.).strong())
            .id_salt(("goal_needs", index))
            .default_open(true)
            .show(ui, |ui| {
                need_cells(ui, icons, owned, russian, &needs.items, false);
                if needs.exp > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{}: {}",
                            tr("Experience", "Опыт"),
                            format!("{} exp", grouped(needs.exp))
                        ))
                        .size(13.)
                        .color(DIM),
                    );
                }
                if needs.incomplete {
                    ui.label(
                        RichText::new(tr(
                            "No data for some steps.",
                            "По некоторым шагам нет данных.",
                        ))
                        .size(12.)
                        .color(BAD),
                    );
                }
            });
    });
    (edited, removed)
}

/// The levels at which an ascension is done: the first star is at level 20.
const STAR_LEVELS: [u32; 6] = [20, 40, 50, 60, 70, 80];

/// The ascension a level goes with.  Passing a star lights it; on the star it
/// is lit only if `ascension` says so, since a character at level 80 may or
/// may not have taken its sixth ascension yet.
fn ascension_for(level: u32, ascension: u32) -> u32 {
    let passed = STAR_LEVELS.iter().filter(|star| **star < level).count() as u32;
    if STAR_LEVELS.contains(&level) && ascension == passed + 1 {
        passed + 1
    } else {
        passed
    }
}

const GOLD: Color32 = Color32::from_rgb(0xff, 0xc3, 0x4a);
const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3f, 0x66);
const FILL: Color32 = Color32::from_rgb(0x6a, 0xa6, 0xff);

/// Where along the line a value is, and the value at a place along it.
fn track_x(track: egui::Rect, min: u32, max: u32, value: u32) -> f32 {
    track.left() + track.width() * (value - min) as f32 / (max - min) as f32
}

fn track_value(track: egui::Rect, min: u32, max: u32, x: f32) -> u32 {
    let fraction = ((x - track.left()) / track.width()).clamp(0., 1.);
    min + (fraction * (max - min) as f32).round() as u32
}

fn draw_track(ui: &Ui, track: egui::Rect, min: u32, max: u32, value: u32, ticks: &[u32]) {
    let painter = ui.painter();
    painter.line_segment(
        [track.left_center(), track.right_center()],
        egui::Stroke::new(4_f32, TRACK),
    );
    let at = track_x(track, min, max, value);
    painter.line_segment(
        [track.left_center(), egui::pos2(at, track.center().y)],
        egui::Stroke::new(4_f32, FILL),
    );
    for tick in ticks {
        let x = track_x(track, min, max, *tick);
        painter.line_segment(
            [
                egui::pos2(x, track.center().y - 5.),
                egui::pos2(x, track.center().y + 5.),
            ],
            egui::Stroke::new(1.5_f32, Color32::from_gray(0x90)),
        );
    }
    painter.circle(
        egui::pos2(at, track.center().y),
        8.,
        Color32::WHITE,
        egui::Stroke::new(2_f32, FILL),
    );
}

/// The level and the ascension on one line: drag along it, and the stars above
/// it, one for each ascension, light up as they are passed.  Clicking a star
/// goes to it.  Returns whether anything changed.
fn level_slider(ui: &mut Ui, ascension: &mut u32, level: &mut u32, russian: bool) -> bool {
    const HEIGHT: f32 = 66.;
    const PAD: f32 = 14.;
    let width = ui.available_width().clamp(260., 560.);
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, HEIGHT), egui::Sense::click_and_drag());
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD, rect.top() + 36.),
        egui::pos2(rect.right() - PAD, rect.top() + 36.),
    );
    let star_y = rect.top() + 14.;

    let before = (*level, *ascension);
    if let Some(pos) = response.interact_pointer_pos() {
        let on_stars = pos.y < rect.top() + 26.;
        let nearest_star = STAR_LEVELS
            .iter()
            .enumerate()
            .map(|(index, star)| (index, (track_x(track, 1, 90, *star) - pos.x).abs()))
            .filter(|(_, distance)| *distance < 12.)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if response.clicked() && on_stars && nearest_star.is_some() {
            let (index, _) = nearest_star.unwrap();
            let star = index as u32 + 1;
            if *level == STAR_LEVELS[index] && *ascension == star {
                // Clicked again: the ascension is not wanted.
                *ascension = star - 1;
            } else {
                *level = STAR_LEVELS[index];
                *ascension = star;
            }
        } else if response.dragged() || response.clicked() {
            *level = track_value(track, 1, 90, pos.x).max(1);
            *ascension = ascension_for(*level, *ascension);
        }
    }
    *ascension = ascension_for(*level, *ascension);

    draw_track(ui, track, 1, 90, *level, &[]);
    for (index, star) in STAR_LEVELS.iter().enumerate() {
        let lit = (index as u32) < *ascension;
        let x = track_x(track, 1, 90, *star);
        ui.painter().text(
            egui::pos2(x, star_y),
            egui::Align2::CENTER_CENTER,
            "★",
            FontId::proportional(20.),
            if lit { GOLD } else { Color32::from_gray(0x5a) },
        );
        ui.painter().line_segment(
            [
                egui::pos2(x, track.center().y - 5.),
                egui::pos2(x, track.center().y + 5.),
            ],
            egui::Stroke::new(1.5_f32, if lit { GOLD } else { Color32::from_gray(0x90) }),
        );
    }
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 8.),
        egui::Align2::CENTER_CENTER,
        if russian {
            format!("Уровень {}  ·  возвышение {}/6", level, ascension)
        } else {
            format!("Level {}  ·  ascension {}/6", level, ascension)
        },
        FontId::proportional(14.),
        Color32::WHITE,
    );
    (*level, *ascension) != before
}

/// A talent level, 1 to 10, along a line with a place for each.
fn talent_slider(ui: &mut Ui, level: &mut u32) -> bool {
    const PAD: f32 = 12.;
    let width = (ui.available_width() - 8.).clamp(200., 480.);
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, 34.), egui::Sense::click_and_drag());
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD, rect.top() + 10.),
        egui::pos2(rect.right() - PAD, rect.top() + 10.),
    );
    let before = *level;
    if let Some(pos) = response.interact_pointer_pos()
        && (response.dragged() || response.clicked())
    {
        *level = track_value(track, 1, 10, pos.x).clamp(1, 10);
    }
    draw_track(ui, track, 1, 10, *level, &(1..=10).collect::<Vec<u32>>());
    for value in 1..=10u32 {
        ui.painter().text(
            egui::pos2(track_x(track, 1, 10, value), rect.bottom() - 8.),
            egui::Align2::CENTER_CENTER,
            value.to_string(),
            FontId::proportional(12.),
            if value == *level {
                Color32::WHITE
            } else {
                Color32::from_gray(0x90)
            },
        );
    }
    *level != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Now {
        Now {
            level: 1,
            ascension: 0,
            talents: [1; 3],
            owned: false,
        }
    }

    #[test]
    fn a_character_taken_to_the_top_needs_what_the_game_says() {
        let data = upgrade_store::data();
        let live = live_data::Upcoming::default();
        let (id, _) = data
            .avatars
            .iter()
            .find(|(_, avatar)| avatar.name.en == "Venti")
            .unwrap();
        let needs = character_needs(data, &live, *id, &fresh(), 90, 6, [10, 10, 10]);
        assert!(!needs.incomplete);
        assert_eq!(needs.exp, 8_362_650);
        // 420k for the ascensions, three talents of 1.6525M each, and the levels.
        assert_eq!(needs.items[&MORA], 420_000 + 3 * 1_652_500 + 8_362_650 / 5);
        // One Crown of Insight per talent.
        assert_eq!(needs.items[&104319], 3);
    }

    #[test]
    fn nothing_is_needed_when_the_goal_is_reached() {
        let data = upgrade_store::data();
        let live = live_data::Upcoming::default();
        let (id, _) = data.avatars.iter().find(|(_, a)| a.name.en == "Venti").unwrap();
        let now = Now {
            level: 90,
            ascension: 6,
            talents: [10; 3],
            owned: true,
        };
        let needs = character_needs(data, &live, *id, &now, 90, 6, [10, 10, 10]);
        assert!(needs.items.is_empty());
        assert_eq!(needs.exp, 0);
    }

    #[test]
    fn a_five_star_weapon_taken_to_the_top() {
        let data = upgrade_store::data();
        let live = live_data::Upcoming::default();
        let (id, _) = data
            .weapons
            .iter()
            .find(|(_, weapon)| weapon.name.en == "Aquila Favonia")
            .unwrap();
        let needs = weapon_needs(data, &live, *id, &fresh(), 90, 6);
        assert!(!needs.incomplete);
        assert_eq!(needs.exp, 9_064_450);
        assert_eq!(needs.items[&MORA], 225_000 + 906_445);
    }

    #[test]
    fn stars_light_up_as_they_are_passed() {
        assert_eq!(ascension_for(1, 0), 0);
        assert_eq!(ascension_for(19, 3), 0);
        // On a star, only if it was taken.
        assert_eq!(ascension_for(20, 0), 0);
        assert_eq!(ascension_for(20, 1), 1);
        assert_eq!(ascension_for(21, 1), 1);
        assert_eq!(ascension_for(70, 4), 4);
        assert_eq!(ascension_for(80, 6), 6);
        // The top needs all six.
        assert_eq!(ascension_for(90, 0), 6);
    }

    fn gems() -> Family {
        Family {
            ids: vec![1, 2, 3, 4],
            mora: vec![0, 300, 900, 2700],
        }
    }

    #[test]
    fn everything_missing_goes_down_to_the_lowest_rarity() {
        let need = |id: u32| if id == 4 { 6 } else { 0 };
        let craft = solve(&gems(), need, |_| 0);
        // Six of the top need 18 below, 54 below that, and 162 of the lowest.
        assert_eq!(craft.crafted, vec![0, 54, 18, 6]);
        assert_eq!(craft.farm, 162);
        assert_eq!(craft.mora, 54 * 300 + 18 * 900 + 6 * 2700);
    }

    #[test]
    fn what_is_on_hand_in_a_higher_rarity_does_not_help_a_lower_need() {
        // Ten of the lowest are needed; five of the top are on hand.
        let craft = solve(&gems(), |id| if id == 1 { 10 } else { 0 }, |id| if id == 4 { 5 } else { 0 });
        assert_eq!(craft.farm, 10);
        assert!(craft.crafted.iter().all(|count| *count == 0));
    }

    #[test]
    fn lower_rarities_on_hand_are_crafted_up() {
        // Six of the top are needed and 18 of the one below are on hand.
        let craft = solve(&gems(), |id| if id == 4 { 6 } else { 0 }, |id| if id == 3 { 18 } else { 0 });
        assert_eq!(craft.farm, 0);
        assert_eq!(craft.crafted, vec![0, 0, 0, 6]);
        assert_eq!(craft.mora, 6 * 2700);
    }

    #[test]
    fn the_chains_of_the_game_are_found() {
        let list = families(upgrade_store::data(), &live_data::Upcoming::default());
        let gems = list.iter().find(|f| f.ids.contains(&104162)).unwrap();
        assert_eq!(gems.ids, vec![104161, 104162, 104163, 104164]);
        assert_eq!(gems.mora, vec![0, 300, 900, 2700]);
        // Talent books are crafted the same way.
        let books = list.iter().find(|f| f.ids.contains(&104366)).unwrap();
        assert_eq!(books.ids, vec![104365, 104366, 104367]);
        assert_eq!(books.mora, vec![0, 175, 550]);
    }

    #[test]
    fn numbers_are_shortened_and_grouped() {
        assert_eq!(short(23_813_567), "23.8M");
        assert_eq!(short(1_500_000), "1.5M");
        assert_eq!(short(12_500), "12.5k");
        assert_eq!(short(9_999), "9999");
        assert_eq!(grouped(23_813_567), "23 813 567");
        assert_eq!(grouped(12), "12");
    }
}
