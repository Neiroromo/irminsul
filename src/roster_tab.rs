//! The characters, weapons and materials of the account in use.
//!
//! What a player has comes from the packets; names, what upgrades cost and how
//! weapon stats grow come from the game's data files (`upgrade_data`); the
//! pictures come from `icons`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use egui::{
    Align, Color32, CornerRadius, FontId, Grid, Layout, Rect, Response, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, TextEdit, Ui, pos2, vec2,
};

use crate::achievement_data::Language;
use crate::achievement_tab::{CARD, CARD_SELECTED, DIM, PRIMOGEM, elided};
use crate::icons::Icons;
use crate::live_data::{self, Named, NewCharacter, NewWeapon, Upcoming};
use crate::player_data::{AvatarInfo, Item, Roster};
use crate::upgrade_data::{self as ud, Step, UpgradeData};
use crate::upgrade_store;

pub(crate) const OK: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
pub(crate) const BAD: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub(crate) const MORA: u32 = 202;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Characters,
    Weapons,
    /// Not in the game yet, or not in the data built into the program.
    Upcoming,
}

/// A character or a weapon, by its id in the game.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum NeedFor {
    Character(u32),
    Weapon(u32),
}

/// Facts about the roster worked out once and used until it changes.
struct Cache {
    roster: Arc<Roster>,
    /// How many of each material there are.
    owned: HashMap<u32, u32>,
    by_guid: HashMap<u64, usize>,
    /// Indices of the characters shown, in the order shown.
    characters: Vec<usize>,
    /// Indices of the weapons, in the order shown.
    weapons: Vec<usize>,
    /// The character each weapon (by guid) is on.
    equipped_on: HashMap<u64, u32>,
}

pub struct RosterTab {
    icons: Icons,
    view: View,
    query: String,
    selected_character: Option<u64>,
    selected_weapon: Option<u64>,
    /// Whether it is a weapon, and its id.
    selected_upcoming: Option<(bool, u32)>,
    cache: Option<Cache>,

    material_query: String,
    only_needed: bool,
    selected_material: Option<u32>,
    /// The characters and weapons the materials are narrowed to.
    material_filter: BTreeSet<NeedFor>,
    filter_query: String,
    usage: Usage,

    planner: crate::planner_tab::Planner,
}

// ---------------------------------------------------------------------------
// Names and numbers.

pub(crate) fn rarity_color(rarity: u32) -> Color32 {
    match rarity {
        5 => Color32::from_rgb(0xff, 0xc3, 0x4a),
        4 => Color32::from_rgb(0xc0, 0x7b, 0xff),
        3 => Color32::from_rgb(0x6a, 0xa6, 0xff),
        _ => Color32::from_gray(0xaa),
    }
}

/// The colours the game puts behind an item of a rarity, top and bottom.
fn rarity_gradient(rarity: u32) -> (Color32, Color32) {
    let rgb = Color32::from_rgb;
    match rarity {
        5 => (rgb(0x9a, 0x6a, 0x2e), rgb(0xe0, 0xa2, 0x4c)),
        4 => (rgb(0x6a, 0x4b, 0x9a), rgb(0xa7, 0x7f, 0xd8)),
        3 => (rgb(0x44, 0x64, 0x9f), rgb(0x6a, 0x9a, 0xd8)),
        2 => (rgb(0x3f, 0x6b, 0x5a), rgb(0x6f, 0xa6, 0x8a)),
        _ => (rgb(0x5b, 0x5f, 0x6e), rgb(0x8c, 0x90, 0xa0)),
    }
}

/// A count for a small place: from a thousand on, rounded and shortened
/// (1.2k, 23.8M).
pub(crate) fn compact(count: u64) -> String {
    let trimmed = |value: f64, suffix: &str| {
        let text = format!("{value:.1}");
        format!("{}{suffix}", text.trim_end_matches('0').trim_end_matches('.'))
    };
    if count >= 1_000_000 {
        trimmed(count as f64 / 1_000_000., "M")
    } else if count >= 1_000 {
        trimmed(count as f64 / 1_000., "k")
    } else {
        count.to_string()
    }
}

pub(crate) fn stars(rarity: u32) -> String {
    "★".repeat(rarity as usize)
}

/// The game's name for an element, how it is shown, and its colour.
pub(crate) fn element(game_name: &str, russian: bool) -> (&'static str, Color32) {
    let (en, ru, color) = match game_name {
        "Fire" => ("Pyro", "Пиро", (0xff, 0x6b, 0x4a)),
        "Water" => ("Hydro", "Гидро", (0x4a, 0xa3, 0xff)),
        "Wind" => ("Anemo", "Анемо", (0x4a, 0xde, 0xbd)),
        "Electric" => ("Electro", "Электро", (0xc0, 0x7b, 0xff)),
        "Grass" => ("Dendro", "Дендро", (0x8a, 0xd6, 0x4a)),
        "Ice" => ("Cryo", "Крио", (0x9f, 0xdf, 0xff)),
        "Rock" => ("Geo", "Гео", (0xff, 0xcc, 0x4a)),
        _ => ("", "", (0xaa, 0xaa, 0xaa)),
    };
    (
        if russian { ru } else { en },
        Color32::from_rgb(color.0, color.1, color.2),
    )
}

pub(crate) fn weapon_type_name(game_name: &str, russian: bool) -> &'static str {
    match (game_name, russian) {
        ("WEAPON_SWORD_ONE_HAND", false) => "Sword",
        ("WEAPON_SWORD_ONE_HAND", true) => "Меч",
        ("WEAPON_CLAYMORE", false) => "Claymore",
        ("WEAPON_CLAYMORE", true) => "Двуручный меч",
        ("WEAPON_POLE", false) => "Polearm",
        ("WEAPON_POLE", true) => "Копьё",
        ("WEAPON_BOW", false) => "Bow",
        ("WEAPON_BOW", true) => "Лук",
        ("WEAPON_CATALYST", false) => "Catalyst",
        ("WEAPON_CATALYST", true) => "Катализатор",
        (_, false) => "Weapon",
        (_, true) => "Оружие",
    }
}

/// The artifact slots in the order the game shows them.
const SLOTS: [(&str, &str, &str); 5] = [
    ("EQUIP_BRACER", "Flower of Life", "Цветок жизни"),
    ("EQUIP_NECKLACE", "Plume of Death", "Перо смерти"),
    ("EQUIP_SHOES", "Sands of Eon", "Пески времени"),
    ("EQUIP_RING", "Goblet of Eonothem", "Кубок пространства"),
    ("EQUIP_DRESS", "Circlet of Logos", "Корона разума"),
];

/// A property the game names `FIGHT_PROP_...`: how it is shown, and whether
/// its value is a percentage.
fn prop(name: &str, russian: bool) -> (String, bool) {
    let short = name.trim_start_matches("FIGHT_PROP_");
    let (en, ru, percent) = match short {
        "HP" | "BASE_HP" => ("HP", "ОЗ", false),
        "HP_PERCENT" => ("HP%", "ОЗ%", true),
        "ATTACK" | "BASE_ATTACK" => ("ATK", "АТК", false),
        "ATTACK_PERCENT" => ("ATK%", "АТК%", true),
        "DEFENSE" | "BASE_DEFENSE" => ("DEF", "ЗЩТ", false),
        "DEFENSE_PERCENT" => ("DEF%", "ЗЩТ%", true),
        "ELEMENT_MASTERY" => ("Elemental Mastery", "Мастерство стихий", false),
        "CRITICAL" => ("Crit Rate", "Шанс крит. попадания", true),
        "CRITICAL_HURT" => ("Crit DMG", "Крит. урон", true),
        "CHARGE_EFFICIENCY" => ("Energy Recharge", "Восстановление энергии", true),
        "HEAL_ADD" => ("Healing Bonus", "Бонус лечения", true),
        "PHYSICAL_ADD_HURT" => ("Physical DMG Bonus", "Бонус физ. урона", true),
        "FIRE_ADD_HURT" => ("Pyro DMG Bonus", "Бонус Пиро урона", true),
        "WATER_ADD_HURT" => ("Hydro DMG Bonus", "Бонус Гидро урона", true),
        "WIND_ADD_HURT" => ("Anemo DMG Bonus", "Бонус Анемо урона", true),
        "ELEC_ADD_HURT" => ("Electro DMG Bonus", "Бонус Электро урона", true),
        "GRASS_ADD_HURT" => ("Dendro DMG Bonus", "Бонус Дендро урона", true),
        "ICE_ADD_HURT" => ("Cryo DMG Bonus", "Бонус Крио урона", true),
        "ROCK_ADD_HURT" => ("Geo DMG Bonus", "Бонус Гео урона", true),
        _ => (short, short, false),
    };
    (if russian { ru } else { en }.to_owned(), percent)
}

pub(crate) fn format_value(value: f32, percent: bool) -> String {
    if percent {
        format!("{:.1}%", value * 100.)
    } else {
        format!("{:.0}", value)
    }
}

/// The numbers of the character window: (property id in the packets, the
/// game's name for it, shown only when above zero).
const STATS: [(u32, &str, bool); 14] = [
    (2000, "FIGHT_PROP_HP", false),
    (2001, "FIGHT_PROP_ATTACK", false),
    (2002, "FIGHT_PROP_DEFENSE", false),
    (28, "FIGHT_PROP_ELEMENT_MASTERY", false),
    (20, "FIGHT_PROP_CRITICAL", false),
    (22, "FIGHT_PROP_CRITICAL_HURT", false),
    (23, "FIGHT_PROP_CHARGE_EFFICIENCY", false),
    (26, "FIGHT_PROP_HEAL_ADD", true),
    (30, "FIGHT_PROP_PHYSICAL_ADD_HURT", true),
    (40, "FIGHT_PROP_FIRE_ADD_HURT", true),
    (41, "FIGHT_PROP_ELEC_ADD_HURT", true),
    (42, "FIGHT_PROP_WATER_ADD_HURT", true),
    (43, "FIGHT_PROP_GRASS_ADD_HURT", true),
    (44, "FIGHT_PROP_WIND_ADD_HURT", true),
];
const MORE_STATS: [(u32, &str, bool); 2] = [
    (45, "FIGHT_PROP_ROCK_ADD_HURT", true),
    (46, "FIGHT_PROP_ICE_ADD_HURT", true),
];

pub(crate) fn level_of(info: &AvatarInfo) -> u32 {
    info.prop_map.get(&4001).map_or(1, |prop| prop.val as u32)
}

pub(crate) fn ascension_of(info: &AvatarInfo) -> u32 {
    info.prop_map.get(&1002).map_or(0, |prop| prop.val as u32)
}

pub(crate) fn depot_of<'a>(data: &'a UpgradeData, info: &AvatarInfo) -> Option<&'a ud::Depot> {
    data.depots.get(&info.skill_depot_id).or_else(|| {
        let avatar = data.avatars.get(&info.avatar_id)?;
        data.depots.get(&avatar.default_depot)
    })
}

pub(crate) fn weapon_refinement(weapon: &Item) -> u32 {
    weapon
        .equip()
        .weapon()
        .affix_map
        .values()
        .next()
        .copied()
        .unwrap_or(0)
        + 1
}

pub(crate) fn is_weapon(item: &Item) -> bool {
    item.has_equip() && item.equip().has_weapon()
}

// ---------------------------------------------------------------------------
// What upgrades cost.

pub(crate) fn add_step(totals: &mut BTreeMap<u32, u32>, step: &Step) {
    if step.mora > 0 {
        *totals.entry(MORA).or_default() += step.mora;
    }
    for cost in &step.items {
        *totals.entry(cost.id).or_default() += cost.count;
    }
}

pub(crate) fn talent_level(info: &AvatarInfo, talent: &ud::Talent) -> (u32, u32) {
    (
        info.skill_level_map
            .get(&talent.skill_id)
            .copied()
            .unwrap_or(1),
        info.proud_skill_extra_level_map
            .get(&talent.skill_id)
            .copied()
            .unwrap_or(0),
    )
}

pub(crate) fn merge(into: &mut BTreeMap<u32, u32>, from: &BTreeMap<u32, u32>) {
    for (id, count) in from {
        *into.entry(*id).or_default() += count;
    }
}

// ---------------------------------------------------------------------------
// What each character and weapon upgrades with, and the lists to pick from.

/// A character or weapon of the game, for picking.
pub(crate) struct Entry {
    pub who: NeedFor,
    pub name: ud::Text,
    pub icon: String,
    pub rarity: u32,
}

pub(crate) fn character_catalog(data: &UpgradeData, live: &live_data::Upcoming) -> Vec<Entry> {
    let mut entries: Vec<Entry> = live
        .characters
        .iter()
        .map(|c| Entry {
            who: NeedFor::Character(c.id),
            name: c.name.clone(),
            icon: c.icon.clone(),
            rarity: c.rarity,
        })
        .collect();
    let mut released: Vec<Entry> = data
        .avatars
        .iter()
        .filter(|(id, avatar)| {
            live.is_official_character(**id)
                && avatar.promotions.len() == 7
                && !avatar.promotions[1].cost.items.is_empty()
                && data
                    .depots
                    .get(&avatar.default_depot)
                    .is_some_and(|depot| depot.normal.upgrades.len() == 9)
        })
        .map(|(id, avatar)| Entry {
            who: NeedFor::Character(*id),
            name: avatar.name.clone(),
            icon: avatar.icon.clone(),
            rarity: avatar.rarity,
        })
        .collect();
    released.sort_by_key(|e| (std::cmp::Reverse(e.rarity), e.name.en.clone()));
    entries.extend(released);
    entries
}

pub(crate) fn weapon_catalog(data: &UpgradeData, live: &live_data::Upcoming) -> Vec<Entry> {
    let mut entries: Vec<Entry> = live
        .weapons
        .iter()
        .map(|w| Entry {
            who: NeedFor::Weapon(w.id),
            name: w.name.clone(),
            icon: w.icon.clone(),
            rarity: w.rarity,
        })
        .collect();
    let mut released: Vec<Entry> = data
        .weapons
        .iter()
        .filter(|(id, weapon)| {
            live.is_official_weapon(**id)
                && weapon.rarity >= 3
                && weapon.promotions.len() == 7
                && !weapon.promotions[1].cost.items.is_empty()
        })
        .map(|(id, weapon)| Entry {
            who: NeedFor::Weapon(*id),
            name: weapon.name.clone(),
            icon: weapon.icon.clone(),
            rarity: weapon.rarity,
        })
        .collect();
    released.sort_by_key(|e| (std::cmp::Reverse(e.rarity), e.name.en.clone()));
    entries.extend(released);
    entries
}

/// The materials each character and weapon is upgraded with, and the other way.
#[derive(Default)]
struct Usage {
    /// Tells when what it was made from has changed.
    key: String,
    by_entity: HashMap<NeedFor, BTreeSet<u32>>,
    by_material: HashMap<u32, Vec<NeedFor>>,
}

fn build_usage(data: &UpgradeData, live: &live_data::Upcoming) -> Usage {
    let mut usage = Usage {
        key: format!("{}-{}", live.version, live.fetched_at),
        ..Default::default()
    };
    let steps_items = |steps: &[Step], set: &mut BTreeSet<u32>| {
        for step in steps {
            set.extend(step.items.iter().map(|cost| cost.id));
        }
    };
    for entry in character_catalog(data, live) {
        let NeedFor::Character(id) = entry.who else {
            continue;
        };
        let mut set = BTreeSet::new();
        if let Some(avatar) = data.avatars.get(&id) {
            for promotion in &avatar.promotions {
                steps_items(std::slice::from_ref(&promotion.cost), &mut set);
            }
            if let Some(depot) = data.depots.get(&avatar.default_depot) {
                for talent in [&depot.normal, &depot.skill, &depot.burst] {
                    steps_items(&talent.upgrades, &mut set);
                }
            }
        } else if let Some(new) = live.characters.iter().find(|c| c.id == id) {
            set.extend(new.ascension.keys().chain(new.talent_costs.keys()).copied());
        }
        set.remove(&MORA);
        usage.by_entity.insert(entry.who, set);
    }
    for entry in weapon_catalog(data, live) {
        let NeedFor::Weapon(id) = entry.who else {
            continue;
        };
        let mut set = BTreeSet::new();
        if let Some(weapon) = data.weapons.get(&id) {
            for promotion in &weapon.promotions {
                steps_items(std::slice::from_ref(&promotion.cost), &mut set);
            }
        } else if let Some(new) = live.weapons.iter().find(|w| w.id == id) {
            set.extend(new.ascension.keys().copied());
        }
        set.remove(&MORA);
        usage.by_entity.insert(entry.who, set);
    }
    for (who, set) in &usage.by_entity {
        for id in set {
            usage.by_material.entry(*id).or_default().push(*who);
        }
    }
    usage
}

// ---------------------------------------------------------------------------
// The cache.

fn build_cache(roster: &Arc<Roster>, russian: bool) -> Cache {
    let data = upgrade_store::data();

    let mut owned: HashMap<u32, u32> = HashMap::new();
    // The mora is not among the items.
    if let Some(mora) = roster.mora {
        owned.insert(MORA, mora.min(u32::MAX as u64) as u32);
    }
    let mut by_guid = HashMap::new();
    let mut weapons = Vec::new();
    for (index, item) in roster.items.iter().enumerate() {
        by_guid.insert(item.guid, index);
        if item.has_material() {
            *owned.entry(item.item_id).or_default() += item.material().count;
        } else if is_weapon(item) {
            weapons.push(index);
        }
    }
    weapons.sort_by_key(|index| {
        let item = &roster.items[*index];
        let rarity = data
            .weapons
            .get(&item.item_id)
            .map_or(0, |weapon| weapon.rarity);
        (
            std::cmp::Reverse(rarity),
            std::cmp::Reverse(item.equip().weapon().level),
            weapon_name(data, item.item_id, russian),
        )
    });

    let mut characters: Vec<usize> = roster
        .characters
        .iter()
        .enumerate()
        .filter(|(_, info)| info.avatar_type == 1 && data.avatars.contains_key(&info.avatar_id))
        .map(|(index, _)| index)
        .collect();
    characters.sort_by_key(|index| {
        let info = &roster.characters[*index];
        let rarity = data
            .avatars
            .get(&info.avatar_id)
            .map_or(0, |avatar| avatar.rarity);
        (
            std::cmp::Reverse(level_of(info)),
            std::cmp::Reverse(rarity),
            info.avatar_id,
        )
    });

    let mut equipped_on = HashMap::new();
    for index in &characters {
        let info = &roster.characters[*index];
        for guid in &info.equip_guid_list {
            equipped_on.insert(*guid, info.avatar_id);
        }
    }

    Cache {
        roster: roster.clone(),
        owned,
        by_guid,
        characters,
        weapons,
        equipped_on,
    }
}

fn weapon_name(data: &UpgradeData, id: u32, russian: bool) -> String {
    data.weapons.get(&id).map_or_else(
        || format!("#{id}"),
        |weapon| weapon.name.get(russian).to_owned(),
    )
}

fn avatar_name(data: &UpgradeData, id: u32, russian: bool) -> String {
    data.avatars.get(&id).map_or_else(
        || format!("#{id}"),
        |avatar| avatar.name.get(russian).to_owned(),
    )
}

// ---------------------------------------------------------------------------
// Drawing.

pub(crate) fn section(ui: &mut Ui, title: &str) {
    ui.add_space(8.);
    ui.label(RichText::new(title).size(16.).strong());
    ui.add_space(2.);
}

pub(crate) fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_max_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// What goes on a picture cell, besides the picture.
pub(crate) struct Cell<'a> {
    pub icon: &'a str,
    pub rarity: u32,
    /// Shows the rarity as stars, like the game does.
    pub stars: bool,
    /// Text in the bottom right corner, like the number of items.
    pub label: Option<(String, Color32)>,
    pub selected: bool,
}

fn text_with_shadow(
    ui: &Ui,
    pos: egui::Pos2,
    align: egui::Align2,
    text: &str,
    size: f32,
    color: Color32,
) {
    let painter = ui.painter();
    let font = FontId::proportional(size);
    painter.text(
        pos + vec2(1., 1.),
        align,
        text,
        font.clone(),
        Color32::from_black_alpha(200),
    );
    painter.text(pos, align, text, font, color);
}

/// The square the game shows an item in: the colour of its rarity behind the
/// picture, the stars at the bottom, the number in the corner.
fn paint_cell(ui: &Ui, icons: &Icons, rect: Rect, cell: &Cell, hovered: bool) {
    paint_cell_scaled(ui, icons, rect, cell, hovered, 1.);
}

/// A card with the text at `text_scale` of its usual size.  The darkened band
/// under the text grows with it, and gets darker, so that it can be read.
pub(crate) fn paint_cell_scaled(
    ui: &Ui,
    icons: &Icons,
    rect: Rect,
    cell: &Cell,
    hovered: bool,
    text_scale: f32,
) {
    let painter = ui.painter();
    let radius = (rect.width() * 0.1).round().clamp(3., 10.) as u8;
    let (top, bottom) = rarity_gradient(cell.rarity);

    // The gradient is drawn in strips.  The corners are two caps, each as high
    // as two radii, so that they curve exactly like the frame and the band
    // below do; the strips between are plain and cover the caps' inner halves.
    let r = radius as f32;
    let at = |y: f32| top.lerp_to_gamma(bottom, ((y - rect.top()) / rect.height()).clamp(0., 1.));
    painter.rect_filled(
        Rect::from_min_max(rect.left_top(), pos2(rect.right(), rect.top() + 2. * r)),
        CornerRadius {
            nw: radius,
            ne: radius,
            sw: 0,
            se: 0,
        },
        at(rect.top() + r),
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2. * r), rect.right_bottom()),
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: radius,
            se: radius,
        },
        at(rect.bottom() - r),
    );
    let strips = 12;
    let (low, high) = (rect.top() + r, rect.bottom() - r);
    for strip in 0..strips {
        let y0 = low + (high - low) * strip as f32 / strips as f32;
        let y1 = low + (high - low) * (strip + 1) as f32 / strips as f32;
        painter.rect_filled(
            Rect::from_min_max(pos2(rect.left(), y0), pos2(rect.right(), y1 + 0.5)),
            0,
            at((y0 + y1) / 2.),
        );
    }

    if let Some(texture) = icons.get(cell.icon) {
        let side = rect.width() * 0.84;
        let center = rect.center() - vec2(0., rect.height() * 0.05);
        painter.image(
            texture.id(),
            Rect::from_center_size(center, vec2(side, side)),
            Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
            Color32::WHITE,
        );
    }

    // Under the stars and the count the picture is darkened, to read them.
    let has_stars = cell.stars && cell.rarity > 0;
    if has_stars || cell.label.is_some() {
        let text_scale = text_scale.clamp(0.6, 1.7);
        let band = rect.height() * (0.26 * text_scale).min(0.46);
        painter.rect_filled(
            Rect::from_min_max(pos2(rect.left(), rect.bottom() - band), rect.right_bottom()),
            CornerRadius {
                nw: 0,
                ne: 0,
                sw: radius,
                se: radius,
            },
            Color32::from_black_alpha((150. + 70. * (text_scale - 1.)).clamp(100., 235.) as u8),
        );
        let middle = rect.bottom() - band / 2.;
        let star_text = if has_stars { stars(cell.rarity) } else { String::new() };
        let count_text = cell.label.as_ref().map_or("", |(text, _)| text.as_str());
        // One size for both.  They must fit side by side; if they do not, both
        // are made smaller, again and again, until they do.
        let pad = rect.width() * 0.06;
        let room = rect.width() - 2. * pad - rect.width() * 0.04;
        let width_at = |size: f32| {
            let measure = |text: &str| {
                if text.is_empty() {
                    0.
                } else {
                    painter
                        .layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::WHITE)
                        .size()
                        .x
                }
            };
            measure(&star_text) + measure(count_text)
        };
        let mut size = band * 0.45;
        while size > 6. && width_at(size) > room {
            size *= 0.92;
        }
        if has_stars {
            text_with_shadow(
                ui,
                pos2(rect.left() + pad, middle),
                egui::Align2::LEFT_CENTER,
                &star_text,
                size,
                Color32::from_rgb(0xff, 0xdd, 0x6a),
            );
        }
        if let Some((text, color)) = &cell.label {
            text_with_shadow(
                ui,
                pos2(rect.right() - pad, middle),
                egui::Align2::RIGHT_CENTER,
                text,
                size,
                *color,
            );
        }
    }

    if cell.selected {
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius),
            Stroke::new(2_f32, Color32::WHITE),
            StrokeKind::Outside,
        );
    } else if hovered {
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius),
            Stroke::new(1.5_f32, Color32::from_white_alpha(170)),
            StrokeKind::Outside,
        );
    }
}

pub(crate) fn item_cell(ui: &mut Ui, icons: &Icons, size: f32, cell: &Cell) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    paint_cell(ui, icons, rect, cell, response.hovered());
    response
}

/// The menu of the right button on a material (or on a chain of them: any of its items that
/// the map knows a place of): "Show on the map", or that this cannot be shown.  Where the
/// material is got in a domain that is not open today, it says so.
pub(crate) fn material_menu(response: &Response, items: &[u32], russian: bool) {
    let found = crate::markers::data().where_is(items);
    let today = crate::markers::game_weekday();
    let note = domain_note(&found, today, russian);
    let data = crate::markers::data();
    let routable: Vec<u32> = items.iter().copied().filter(|item| data.routable(*item)).collect();
    response.context_menu(|ui| {
        route_entry(ui, &routable, russian);
        if found.is_empty() {
            ui.label(
                RichText::new(if russian {
                    "Нет возможности отобразить на карте"
                } else {
                    "It can not be shown on the map"
                })
                .color(DIM),
            );
            return;
        }
        if ui.button(if russian { "Показать на карте" } else { "Show on the map" }).clicked() {
            crate::map_tab::request_show(
                crate::map_tab::ShowRequest {
                    labels: found.labels.clone(),
                    domains: found.points.clone(),
                    note: note.clone(),
                },
                ui.ctx(),
            );
            ui.close();
        }
        if !found.domains.is_empty() && !found.open_on(today) {
            ui.label(
                RichText::new(if russian {
                    "Сегодня может быть недоступно (не тот день)"
                } else {
                    "May not be open today (wrong day)"
                })
                .color(Color32::from_rgb(0xff, 0xc2, 0x4a)),
            );
        }
    });
}

/// The entry "Add to the route" of the menu of a material: it is not offered when the item is not
/// got in the open world (flour, what is crafted, bought ...), and it takes the item off when it is in.
fn route_entry(ui: &mut Ui, routable: &[u32], russian: bool) {
    if routable.is_empty() {
        ui.add_enabled(false, egui::Button::new(if russian { "Добавить в маршрут" } else { "Add to the route" }))
            .on_disabled_hover_text(if russian {
                "Нельзя: этого предмета нет в открытом мире (его не собрать по меткам карты)"
            } else {
                "Not possible: this item is not got in the open world (no markers of it on the map)"
            });
        return;
    }
    let all_in = routable.iter().all(|item| crate::map_tab::route_has(*item));
    let label = match (all_in, russian) {
        (true, true) => "Убрать из маршрута",
        (true, false) => "Take off the route",
        (false, true) => "Добавить в маршрут",
        (false, false) => "Add to the route",
    };
    if ui.button(label).clicked() {
        crate::map_tab::request_route(routable, !all_in, ui.ctx());
        ui.close();
    }
}

/// The button "Show on the map" of the details of a material (disabled with a note when the map
/// knows no place of it).
fn show_on_map_button(ui: &mut Ui, items: &[u32], russian: bool) {
    let found = crate::markers::data().where_is(items);
    let today = crate::markers::game_weekday();
    if found.is_empty() {
        ui.add_enabled(false, egui::Button::new(if russian { "Показать на карте" } else { "Show on the map" }))
            .on_disabled_hover_text(if russian { "Нет возможности отобразить на карте" } else { "It can not be shown on the map" });
        return;
    }
    if ui.button(if russian { "Показать на карте" } else { "Show on the map" }).clicked() {
        crate::map_tab::request_show(
            crate::map_tab::ShowRequest {
                labels: found.labels.clone(),
                domains: found.points.clone(),
                note: domain_note(&found, today, russian),
            },
            ui.ctx(),
        );
    }
    if !found.domains.is_empty() && !found.open_on(today) {
        ui.label(
            RichText::new(if russian {
                "Сегодня может быть недоступно (не тот день)"
            } else {
                "May not be open today (wrong day)"
            })
            .size(12.)
            .color(Color32::from_rgb(0xff, 0xc2, 0x4a)),
        );
    }
}

/// What is told over the map for domains: which ones, on which days, and a warning when none is
/// open today.
fn domain_note(found: &crate::markers::Where, today: u8, russian: bool) -> String {
    if found.domains.is_empty() {
        return String::new();
    }
    const RU: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];
    const EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let names = if russian { RU } else { EN };
    let days = |days: &[u8]| days.iter().filter_map(|day| names.get(*day as usize)).copied().collect::<Vec<_>>().join(", ");
    let mut lines: Vec<String> = found
        .domains
        .iter()
        .map(|domain| format!("{} ({})", if russian { &domain.ru } else { &domain.en }, days(&domain.days)))
        .collect();
    lines.dedup();
    lines.truncate(4);
    if !found.open_on(today) {
        lines.insert(
            0,
            if russian {
                format!("Сегодня ({}) это в подземелье может быть недоступно: не тот день.", names[today as usize])
            } else {
                format!("It may not be available in the domain today ({}): wrong day.", names[today as usize])
            },
        );
    }
    lines.join("
")
}

/// A clickable card of a list, with the picture of what it is of.  Returns
/// whether it was clicked.
pub(crate) fn list_card(
    ui: &mut Ui,
    icons: &Icons,
    selected: bool,
    cell: &Cell,
    title: &str,
    subtitle: &str,
) -> bool {
    let width = ui.available_width() - 12.;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 58.), Sense::click());
    let fill = if selected {
        CARD_SELECTED
    } else if response.hovered() {
        CARD.gamma_multiply(1.3)
    } else {
        CARD
    };
    ui.painter().rect_filled(rect, CornerRadius::same(12), fill);

    let picture = Rect::from_min_size(rect.min + vec2(7., 7.), vec2(44., 44.));
    paint_cell(ui, icons, picture, cell, false);

    let galley = elided(
        ui,
        title,
        FontId::proportional(16.),
        Color32::WHITE,
        width - 70.,
    );
    ui.painter()
        .galley(rect.left_top() + vec2(60., 9.), galley, Color32::WHITE);
    ui.painter().text(
        rect.left_top() + vec2(60., 34.),
        egui::Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(13.),
        Color32::from_gray(0xd0),
    );
    response.clicked()
}

/// What an upgrade needs, as the items with how many there are of how many
/// are needed, in red where there is not enough.
/// The materials something upgrades with, as pictures with their names, in
/// rows that fit the width.  How many are needed is left out on purpose: that
/// is for the planner.
fn cost_cells(
    ui: &mut Ui,
    icons: &Icons,
    _cache: &Cache,
    russian: bool,
    costs: &BTreeMap<u32, u32>,
) {
    const CELL: f32 = 64.;
    const GAP: f32 = 8.;
    let data = upgrade_store::data();
    let live = live_data::current();
    if costs.is_empty() {
        ui.label(RichText::new("—").color(DIM));
        return;
    }
    // Mora first, the rest as the game orders them.
    let mut ids: Vec<u32> = costs.keys().copied().collect();
    ids.sort_by_key(|id| (*id != MORA, *id));

    let per_row = (((ui.available_width() + GAP) / (CELL + GAP)).floor() as usize).max(1);
    for row in ids.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, 0.);
            for id in row {
                let material = data.materials.get(id).or_else(|| live.materials.get(id));
                let name = material
                    .map_or_else(|| format!("#{id}"), |m| m.name.get(russian).to_owned());
                ui.allocate_ui_with_layout(
                    vec2(CELL, 72.),
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
                        let response = item_cell(ui, icons, 52., &cell).on_hover_text(&name);
                        material_menu(&response, &[*id], russian);
                        ui.add(
                            egui::Label::new(RichText::new(&name).size(11.).color(DIM)).truncate(),
                        );
                    },
                );
            }
        });
        ui.add_space(4.);
    }
}

impl RosterTab {
    pub fn new(ctx: &egui::Context) -> Self {
        live_data::start(ctx);
        Self {
            icons: Icons::new(ctx),
            // For looking at the other views without a click.
            view: match std::env::var("IRMINSUL_START_VIEW").as_deref() {
                Ok("weapons") => View::Weapons,
                Ok("upcoming") => View::Upcoming,
                _ => View::Characters,
            },
            selected_upcoming: None,
            query: String::new(),
            selected_character: None,
            selected_weapon: None,
            cache: None,
            material_query: String::new(),
            only_needed: false,
            selected_material: None,
            material_filter: BTreeSet::new(),
            filter_query: String::new(),
            usage: Usage::default(),
            planner: crate::planner_tab::Planner::load(),
        }
    }

    fn ensure_cache(&mut self, roster: &Arc<Roster>, russian: bool) {
        self.icons.poll();
        let stale = self
            .cache
            .as_ref()
            .is_none_or(|cache| !Arc::ptr_eq(&cache.roster, roster));
        if stale {
            self.cache = Some(build_cache(roster, russian));
        }
    }

    /// The pictures, for what is shown outside the tabs.
    pub fn icons(&self) -> &Icons {
        &self.icons
    }

    /// The floating window of the planner, which is shown over everything.
    pub fn overlay_ui(&mut self, ctx: &egui::Context, roster: &Arc<Roster>, language: Language) {
        if !self.planner.overlay_active() {
            return;
        }
        let russian = language == Language::Ru;
        self.ensure_cache(roster, russian);
        let Some(cache) = &self.cache else {
            return;
        };
        self.planner
            .overlay_frame(ctx, &self.icons, &cache.owned, roster, russian);
    }

    /// The planner tab.
    pub fn planner_ui(&mut self, ui: &mut Ui, roster: &Arc<Roster>, language: &mut Language) {
        let russian = *language == Language::Ru;
        self.ensure_cache(roster, russian);
        let Some(cache) = &self.cache else {
            return;
        };
        self.planner.ui(ui, &self.icons, &cache.owned, roster, language);
    }

    pub fn characters_ui(&mut self, ui: &mut Ui, roster: &Arc<Roster>, language: &mut Language) {
        let russian = *language == Language::Ru;
        self.ensure_cache(roster, russian);
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let live = live_data::current();
        let Some(cache) = &self.cache else {
            return;
        };

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr("Characters", "Персонажи"))
                    .size(34.)
                    .strong(),
            );
            ui.add_space(12.);
            ui.selectable_value(
                &mut self.view,
                View::Characters,
                RichText::new(format!(
                    "{} ({})",
                    tr("Characters", "Персонажи"),
                    cache.characters.len()
                ))
                .size(16.),
            );
            ui.selectable_value(
                &mut self.view,
                View::Weapons,
                RichText::new(format!(
                    "{} ({})",
                    tr("Weapons", "Оружие"),
                    cache.weapons.len()
                ))
                .size(16.),
            );
            let upcoming = live.characters.len() + live.weapons.len();
            ui.selectable_value(
                &mut self.view,
                View::Upcoming,
                RichText::new(format!("{} ({upcoming})", tr("Upcoming", "Скоро"))).size(16.),
            );
            ui.add_space(12.);
            ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text(tr("Search", "Поиск"))
                    .desired_width(220.),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                language_choice(ui, language);
            });
        });
        ui.add_space(8.);

        if cache.characters.is_empty() && cache.weapons.is_empty() && live.is_empty() {
            ui.label(
                RichText::new(tr(
                    "No data yet. Log in to the game while capture is running, or pick an \
                     account stored earlier on the Export tab.",
                    "Данных пока нет. Войдите в игру при включённом захвате или выберите \
                     сохранённый аккаунт на вкладке Export.",
                ))
                .color(DIM),
            );
            return;
        }

        let body_height = (ui.available_height() - 30.).max(100.);
        let list_width = 300.;
        let data = upgrade_store::data();
        let query = self.query.trim().to_lowercase();
        let view = self.view;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(list_width, body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("roster_list")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.;
                            match view {
                                View::Characters => self.character_list(ui, data, &query, russian),
                                View::Weapons => self.weapon_list(ui, data, &query, russian),
                                View::Upcoming => self.upcoming_list(ui, &live, &query, russian),
                            }
                        });
                },
            );

            ui.add_space(4.);
            let width = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(width, body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    let mut area = ScrollArea::vertical()
                        .id_salt("roster_detail")
                        .auto_shrink([false, false]);
                    // For looking at the lower part without scrolling.
                    if let Some(offset) = std::env::var("IRMINSUL_SCROLL")
                        .ok()
                        .and_then(|offset| offset.parse::<f32>().ok())
                    {
                        area = area.vertical_scroll_offset(offset);
                    }
                    area.show(ui, |ui| {
                        ui.set_width(ui.available_width() - 12.);
                        match view {
                            View::Characters => self.character_detail(ui, data, russian),
                            View::Weapons => self.weapon_detail(ui, data, russian),
                            View::Upcoming => self.upcoming_detail(ui, &live, russian),
                        }
                    });
                },
            );
        });
    }

    fn character_list(&mut self, ui: &mut Ui, data: &UpgradeData, query: &str, russian: bool) {
        let Some(cache) = &self.cache else {
            return;
        };
        let mut clicked = None;
        for index in &cache.characters {
            let info = &cache.roster.characters[*index];
            let Some(avatar) = data.avatars.get(&info.avatar_id) else {
                continue;
            };
            let name = avatar.name.get(russian);
            if !query.is_empty()
                && !name.to_lowercase().contains(query)
                && !avatar.name.en.to_lowercase().contains(query)
            {
                continue;
            }
            let ascension = ascension_of(info);
            let max_level = avatar
                .promotions
                .get(ascension as usize)
                .map_or(90, |promotion| promotion.max_level);
            let subtitle = format!(
                "{} {}/{}   C{}",
                if russian { "Ур." } else { "Lv." },
                level_of(info),
                max_level,
                info.talent_id_list.len()
            );
            let selected = self.selected_character == Some(info.guid);
            let cell = Cell {
                icon: &avatar.icon,
                rarity: avatar.rarity,
                stars: false,
                label: None,
                selected: false,
            };
            if list_card(ui, &self.icons, selected, &cell, name, &subtitle) {
                clicked = Some(info.guid);
            }
        }
        if clicked.is_some() {
            self.selected_character = clicked;
        }
    }

    fn weapon_list(&mut self, ui: &mut Ui, data: &UpgradeData, query: &str, russian: bool) {
        let Some(cache) = &self.cache else {
            return;
        };
        let mut clicked = None;
        for index in &cache.weapons {
            let item = &cache.roster.items[*index];
            let Some(weapon) = data.weapons.get(&item.item_id) else {
                continue;
            };
            let name = weapon.name.get(russian);
            if !query.is_empty()
                && !name.to_lowercase().contains(query)
                && !weapon.name.en.to_lowercase().contains(query)
            {
                continue;
            }
            let state = item.equip().weapon();
            let on = cache
                .equipped_on
                .get(&item.guid)
                .map(|id| avatar_name(data, *id, russian))
                .unwrap_or_default();
            let subtitle = format!(
                "{} {}   R{}   {}",
                if russian { "Ур." } else { "Lv." },
                state.level,
                weapon_refinement(item),
                on
            );
            let selected = self.selected_weapon == Some(item.guid);
            let cell = Cell {
                icon: &weapon.icon,
                rarity: weapon.rarity,
                stars: false,
                label: None,
                selected: false,
            };
            if list_card(ui, &self.icons, selected, &cell, name, &subtitle) {
                clicked = Some(item.guid);
            }
        }
        if clicked.is_some() {
            self.selected_weapon = clicked;
        }
    }

    fn upcoming_list(&mut self, ui: &mut Ui, live: &Upcoming, query: &str, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        if live.is_empty() {
            ui.label(
                RichText::new(tr(
                    "Nothing new yet. This is checked in the background.",
                    "Пока ничего нового. Проверка идёт в фоне.",
                ))
                .color(DIM),
            );
            return;
        }
        let matches = |name: &crate::upgrade_data::Text| {
            query.is_empty()
                || name.en.to_lowercase().contains(query)
                || name.ru.to_lowercase().contains(query)
        };
        let mut clicked = None;
        for character in live.characters.iter().filter(|c| matches(&c.name)) {
            let (element_name, _) = element(&character.element, russian);
            let subtitle = format!(
                "{element_name}  {}",
                weapon_type_name(&character.weapon_type, russian)
            );
            let cell = Cell {
                icon: &character.icon,
                rarity: character.rarity,
                stars: false,
                label: None,
                selected: false,
            };
            let selected = self.selected_upcoming == Some((false, character.id));
            if list_card(
                ui,
                &self.icons,
                selected,
                &cell,
                character.name.get(russian),
                &subtitle,
            ) {
                clicked = Some((false, character.id));
            }
        }
        for weapon in live.weapons.iter().filter(|w| matches(&w.name)) {
            let cell = Cell {
                icon: &weapon.icon,
                rarity: weapon.rarity,
                stars: false,
                label: None,
                selected: false,
            };
            let selected = self.selected_upcoming == Some((true, weapon.id));
            if list_card(
                ui,
                &self.icons,
                selected,
                &cell,
                weapon.name.get(russian),
                weapon_type_name(&weapon.weapon_type, russian),
            ) {
                clicked = Some((true, weapon.id));
            }
        }
        if clicked.is_some() {
            self.selected_upcoming = clicked;
        }
    }

    fn upcoming_detail(&self, ui: &mut Ui, live: &Upcoming, russian: bool) {
        let Some(cache) = &self.cache else {
            return;
        };
        let selected = self.selected_upcoming.or_else(|| {
            live.characters
                .first()
                .map(|c| (false, c.id))
                .or_else(|| live.weapons.first().map(|w| (true, w.id)))
        });
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        ui.add(egui::Label::new(
            RichText::new(format!(
                "{} {}.  {}",
                tr("Data of version", "Данные версии"),
                live.version,
                tr(
                    "Taken from a public database of leaked game data, so it can change.",
                    "Взято из публичной базы утёкших данных игры, поэтому может измениться.",
                )
            ))
            .size(12.)
            .color(DIM),
        ).wrap());
        match selected {
            Some((false, id)) => {
                if let Some(character) = live.characters.iter().find(|c| c.id == id) {
                    upcoming_character(ui, &self.icons, cache, character, russian);
                }
            }
            Some((true, id)) => {
                if let Some(weapon) = live.weapons.iter().find(|w| w.id == id) {
                    upcoming_weapon(ui, &self.icons, cache, weapon, russian);
                }
            }
            None => {}
        }
    }

    fn selected_character<'a>(&self, cache: &'a Cache) -> Option<&'a AvatarInfo> {
        let by_guid = self.selected_character.and_then(|guid| {
            cache
                .characters
                .iter()
                .find(|i| cache.roster.characters[**i].guid == guid)
        });
        let index = by_guid.or_else(|| cache.characters.first())?;
        Some(&cache.roster.characters[*index])
    }

    fn selected_weapon<'a>(&self, cache: &'a Cache) -> Option<&'a Item> {
        let by_guid = self.selected_weapon.and_then(|guid| {
            cache
                .weapons
                .iter()
                .find(|i| cache.roster.items[**i].guid == guid)
        });
        let index = by_guid.or_else(|| cache.weapons.first())?;
        Some(&cache.roster.items[*index])
    }

    fn character_detail(&self, ui: &mut Ui, data: &UpgradeData, russian: bool) {
        let Some(cache) = &self.cache else {
            return;
        };
        let Some(info) = self.selected_character(cache) else {
            return;
        };
        let Some(avatar) = data.avatars.get(&info.avatar_id) else {
            return;
        };
        let icons = &self.icons;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let depot = depot_of(data, info);
        let ascension = ascension_of(info);
        let max_ascension = avatar.promotions.len().saturating_sub(1) as u32;
        let max_level = avatar
            .promotions
            .get(ascension as usize)
            .map_or(90, |promotion| promotion.max_level);
        let (element_name, element_color) = depot.map_or(("", Color32::GRAY), |depot| {
            element(&depot.element, russian)
        });

        card(ui, |ui| {
            ui.horizontal(|ui| {
                let cell = Cell {
                    icon: &avatar.icon,
                    rarity: avatar.rarity,
                    stars: true,
                    label: None,
                    selected: false,
                };
                item_cell(ui, icons, 96., &cell);
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(avatar.name.get(russian)).size(26.).strong());
                        ui.label(RichText::new(element_name).size(16.).color(element_color));
                        ui.label(
                            RichText::new(weapon_type_name(&avatar.weapon_type, russian))
                                .size(14.)
                                .color(DIM),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        for text in [
                            format!(
                                "{} {}/{}",
                                tr("Level", "Уровень"),
                                level_of(info),
                                max_level
                            ),
                            format!(
                                "{} {ascension}/{max_ascension}",
                                tr("Ascension", "Возвышение")
                            ),
                            format!(
                                "{} {}",
                                tr("Friendship", "Дружба"),
                                info.fetter_info.exp_level
                            ),
                            format!(
                                "{} {}/6",
                                tr("Constellations", "Созвездия"),
                                info.talent_id_list.len()
                            ),
                        ] {
                            ui.label(RichText::new(text).size(15.));
                            ui.add_space(10.);
                        }
                    });
                });
            });
        });

        // Stats.
        section(ui, tr("Attributes", "Характеристики"));
        card(ui, |ui| {
            Grid::new("character_stats")
                .num_columns(2)
                .spacing([40., 4.])
                .show(ui, |ui| {
                    for (id, name, hide_zero) in STATS.iter().chain(MORE_STATS.iter()) {
                        let value = info.fight_prop_map.get(id).copied().unwrap_or(0.);
                        if *hide_zero && value <= 0.0001 {
                            continue;
                        }
                        let (label, percent) = prop(name, russian);
                        ui.label(label);
                        ui.label(RichText::new(format_value(value, percent)).strong());
                        ui.end_row();
                    }
                });
        });

        // Talents, passives and constellations.  What they say is not in the
        // built-in data; it is fetched once and kept.
        let details = live_data::details(info.avatar_id);
        if let Some(depot) = depot {
            section(ui, tr("Talents", "Таланты"));
            card(ui, |ui| {
                for (index, (kind, talent)) in [
                    (tr("Normal Attack", "Обычная атака"), &depot.normal),
                    (tr("Elemental Skill", "Элементальный навык"), &depot.skill),
                    (tr("Elemental Burst", "Взрыв стихии"), &depot.burst),
                ]
                .into_iter()
                .enumerate()
                {
                    let (level, extra) = talent_level(info, talent);
                    let mut text = format!(
                        "{}   {} {}",
                        talent.name.get(russian),
                        tr("Lv.", "Ур."),
                        level + extra
                    );
                    if extra > 0 {
                        text.push_str(&format!(" (+{extra})"));
                    }
                    let body = details
                        .as_ref()
                        .and_then(|details| details.talents.get(index))
                        .map_or("", |talent| talent.description.get(russian));
                    ui.label(RichText::new(kind).size(12.).color(DIM));
                    expandable(
                        ui,
                        ("talent", info.avatar_id, index),
                        RichText::new(text).size(15.).strong(),
                        body,
                    );
                    ui.add_space(4.);
                }
                if details.is_none() {
                    ui.label(
                        RichText::new(tr(
                            "Descriptions are being loaded.",
                            "Описания загружаются.",
                        ))
                        .size(12.)
                        .color(DIM),
                    );
                }
            });

            if let Some(passives) = details
                .as_ref()
                .map(|details| &details.passives)
                .filter(|passives| !passives.is_empty())
            {
                section(ui, tr("Passive talents", "Пассивные таланты"));
                card(ui, |ui| {
                    for (index, passive) in passives.iter().enumerate() {
                        named_block(ui, ("passive", info.avatar_id, index), "", passive, russian);
                    }
                });
            }

            section(ui, tr("Constellations", "Созвездия"));
            card(ui, |ui| {
                let unlocked = info.talent_id_list.len();
                for (index, name) in depot.constellations.iter().enumerate() {
                    let on = index < unlocked;
                    let body = details
                        .as_ref()
                        .and_then(|details| details.constellations.get(index))
                        .map_or("", |constellation| constellation.description.get(russian));
                    expandable(
                        ui,
                        ("constellation", info.avatar_id, index),
                        RichText::new(format!("C{}  {}", index + 1, name.get(russian)))
                            .size(14.)
                            .color(if on { Color32::WHITE } else { DIM }),
                        body,
                    );
                }
            });
        }

        // Equipment.
        section(ui, tr("Weapon", "Оружие"));
        let equipped: Vec<&Item> = info
            .equip_guid_list
            .iter()
            .filter_map(|guid| cache.by_guid.get(guid))
            .map(|index| &cache.roster.items[*index])
            .collect();
        card(ui, |ui| match equipped.iter().find(|item| is_weapon(item)) {
            Some(item) => weapon_summary(ui, icons, data, item, russian),
            None => {
                ui.label(RichText::new(tr("No weapon", "Нет оружия")).color(DIM));
            }
        });

        section(ui, tr("Artifacts", "Артефакты"));
        card(ui, |ui| {
            for (slot, slot_en, slot_ru) in SLOTS {
                let artifact = equipped.iter().find(|item| {
                    item.has_equip()
                        && item.equip().has_reliquary()
                        && data
                            .artifacts
                            .get(&item.item_id)
                            .is_some_and(|a| a.slot == slot)
                });
                ui.label(
                    RichText::new(if russian { slot_ru } else { slot_en })
                        .size(13.)
                        .color(DIM),
                );
                match artifact {
                    Some(item) => artifact_summary(ui, icons, data, item, russian),
                    None => {
                        ui.label(RichText::new("—").color(DIM));
                    }
                }
                ui.add_space(6.);
            }
        });

        // What it upgrades with.  How many is for the planner.
        section(ui, tr("Upgrade materials", "Материалы для прокачки"));
        card(ui, |ui| {
            let mut ascension = BTreeMap::new();
            for promotion in &avatar.promotions {
                add_step(&mut ascension, &promotion.cost);
            }
            ui.label(
                RichText::new(tr("Ascension", "Возвышение"))
                    .size(13.)
                    .color(DIM),
            );
            cost_cells(ui, icons, cache, russian, &ascension);
            if let Some(depot) = depot {
                let mut talents = BTreeMap::new();
                for talent in [&depot.normal, &depot.skill, &depot.burst] {
                    for step in &talent.upgrades {
                        add_step(&mut talents, step);
                    }
                }
                ui.add_space(6.);
                ui.label(RichText::new(tr("Talents", "Таланты")).size(13.).color(DIM));
                cost_cells(ui, icons, cache, russian, &talents);
            }
        });
    }

    fn weapon_detail(&self, ui: &mut Ui, data: &UpgradeData, russian: bool) {
        let Some(cache) = &self.cache else {
            return;
        };
        let Some(item) = self.selected_weapon(cache) else {
            return;
        };
        let icons = &self.icons;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };

        card(ui, |ui| weapon_summary(ui, icons, data, item, russian));

        let Some(weapon) = data.weapons.get(&item.item_id) else {
            return;
        };
        section(ui, tr("Equipped on", "Установлено на"));
        card(ui, |ui| match cache.equipped_on.get(&item.guid) {
            Some(id) => {
                ui.horizontal(|ui| {
                    let avatar = data.avatars.get(id);
                    let cell = Cell {
                        icon: avatar.map_or("", |avatar| avatar.icon.as_str()),
                        rarity: avatar.map_or(1, |avatar| avatar.rarity),
                        stars: false,
                        label: None,
                        selected: false,
                    };
                    item_cell(ui, icons, 48., &cell);
                    ui.label(RichText::new(avatar_name(data, *id, russian)).size(16.));
                });
            }
            None => {
                ui.label(RichText::new(tr("Not equipped", "Не установлено")).color(DIM));
            }
        });

        section(ui, tr("Ascension materials", "Материалы возвышения"));
        card(ui, |ui| {
            let mut costs = BTreeMap::new();
            for promotion in &weapon.promotions {
                add_step(&mut costs, &promotion.cost);
            }
            cost_cells(ui, icons, cache, russian, &costs);
        });
    }

    /// The materials, as the game shows its inventory.  What the planner
    /// needs of them shows only while something is being planned.
    pub fn materials_ui(&mut self, ui: &mut Ui, roster: &Arc<Roster>, language: &mut Language) {
        let russian = *language == Language::Ru;
        self.ensure_cache(roster, russian);
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = upgrade_store::data();
        let live = live_data::current();
        let key = format!("{}-{}", live.version, live.fetched_at);
        if self.usage.key != key {
            self.usage = build_usage(data, &live);
        }
        let plan = self.planner.needs_total(roster);
        let Some(cache) = &self.cache else {
            return;
        };

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr("Materials", "Материалы"))
                    .size(34.)
                    .strong(),
            );
            ui.add_space(12.);
            ui.add(
                TextEdit::singleline(&mut self.material_query)
                    .hint_text(tr("Search", "Поиск"))
                    .desired_width(180.),
            );
            let characters = self
                .material_filter
                .iter()
                .filter(|who| matches!(who, NeedFor::Character(_)))
                .count();
            let weapons = self.material_filter.len() - characters;
            // The lists to narrow to open under their buttons; the button again, or a
            // click anywhere else, closes them.
            let characters_button = ui.button(
                RichText::new(format!("{} ({characters})", tr("Characters", "Персонажи"))).size(15.),
            );
            if characters_button.clicked() {
                self.filter_query.clear();
            }
            egui::Popup::from_toggle_button_response(&characters_button)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .align(egui::RectAlign::BOTTOM_START)
                .show(|ui| {
                    ui.set_width(540.);
                    let entries = character_catalog(data, &live);
                    filter_grid(
                        ui,
                        &self.icons,
                        &entries,
                        &mut self.material_filter,
                        &mut self.filter_query,
                        russian,
                    );
                });
            let weapons_button = ui.button(
                RichText::new(format!("{} ({weapons})", tr("Weapons", "Оружие"))).size(15.),
            );
            if weapons_button.clicked() {
                self.filter_query.clear();
            }
            egui::Popup::from_toggle_button_response(&weapons_button)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .align(egui::RectAlign::BOTTOM_START)
                .show(|ui| {
                    ui.set_width(540.);
                    let entries = weapon_catalog(data, &live);
                    filter_grid(
                        ui,
                        &self.icons,
                        &entries,
                        &mut self.material_filter,
                        &mut self.filter_query,
                        russian,
                    );
                });
            if plan.is_some() {
                ui.checkbox(
                    &mut self.only_needed,
                    tr("Only what is planned", "Только из планировщика"),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                language_choice(ui, language);
            });
        });

        // What the list is narrowed to, as pictures; a click takes one away.
        if !self.material_filter.is_empty() {
            let characters = character_catalog(data, &live);
            let weapons = weapon_catalog(data, &live);
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6., 6.);
                for who in &self.material_filter {
                    let entry = characters
                        .iter()
                        .chain(weapons.iter())
                        .find(|entry| entry.who == *who);
                    let Some(entry) = entry else {
                        continue;
                    };
                    let cell = Cell {
                        icon: &entry.icon,
                        rarity: entry.rarity,
                        stars: false,
                        label: None,
                        selected: false,
                    };
                    if item_cell(ui, &self.icons, 36., &cell)
                        .on_hover_text(format!("{}\n{}", entry.name.get(russian), tr("Click to remove", "Нажмите, чтобы убрать")))
                        .clicked()
                    {
                        remove = Some(*who);
                    }
                }
                if ui.button(tr("Clear", "Сбросить")).clicked() {
                    remove = None;
                    self.material_filter.clear();
                }
            });
            if let Some(who) = remove {
                self.material_filter.remove(&who);
            }
        }
        ui.add_space(6.);

        if cache.owned.is_empty() && self.material_filter.is_empty() {
            ui.label(
                RichText::new(tr(
                    "No data yet. Log in to the game while capture is running.",
                    "Данных пока нет. Войдите в игру при включённом захвате.",
                ))
                .color(DIM),
            );
            return;
        }

        // Group by what the game calls the kind of material.
        let query = self.material_query.trim().to_lowercase();
        let narrowed: BTreeSet<u32> = self
            .material_filter
            .iter()
            .filter_map(|who| self.usage.by_entity.get(who))
            .flatten()
            .copied()
            .collect();
        let narrowed_to_filter = !self.material_filter.is_empty();
        let mut groups: BTreeMap<(usize, String), Vec<(u32, &ud::Material)>> = BTreeMap::new();
        for (id, material) in data.materials.iter().chain(live.materials.iter()) {
            let have = cache.owned.get(id).copied().unwrap_or(0);
            let need = plan.as_ref().map_or(0, |plan| plan.get(id).copied().unwrap_or(0));
            if narrowed_to_filter {
                if !narrowed.contains(id) {
                    continue;
                }
            } else if have == 0 && need == 0 {
                continue;
            }
            if self.only_needed && plan.is_some() && need == 0 {
                continue;
            }
            if !query.is_empty()
                && !material.name.en.to_lowercase().contains(&query)
                && !material.name.ru.to_lowercase().contains(&query)
            {
                continue;
            }
            groups
                .entry((
                    category_order(&material.category.en),
                    material.category.en.clone(),
                ))
                .or_default()
                .push((*id, material));
        }

        let body_height = (ui.available_height() - 30.).max(100.);
        let detail_width = 330.;
        let list_width = (ui.available_width() - detail_width - 8.).max(300.);
        let mut clicked = None;
        let icons = &self.icons;
        let selected_material = self.selected_material;
        let usage = &self.usage;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(list_width, body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("materials")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 12.);
                            ui.spacing_mut().item_spacing.y = 10.;
                            if groups.is_empty() {
                                ui.label(
                                    RichText::new(tr("Nothing found", "Ничего не найдено"))
                                        .color(DIM),
                                );
                            }
                            for (_, mut materials) in groups {
                                materials.sort_by_key(|(id, material)| {
                                    (std::cmp::Reverse(material.rarity), *id)
                                });
                                let title = materials[0].1.category.get(russian).to_owned();
                                card(ui, |ui| {
                                    ui.label(RichText::new(title).size(16.).strong());
                                    ui.add_space(4.);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing = vec2(8., 8.);
                                        for (id, material) in &materials {
                                            let have = cache.owned.get(id).copied().unwrap_or(0);
                                            let need = plan.as_ref().map_or(0, |plan| {
                                                plan.get(id).copied().unwrap_or(0)
                                            });
                                            let cell = Cell {
                                                icon: &material.icon,
                                                rarity: material.rarity.max(1),
                                                stars: true,
                                                label: Some((
                                                    compact(have as u64),
                                                    if need > have { BAD } else { Color32::WHITE },
                                                )),
                                                selected: selected_material == Some(*id),
                                            };
                                            let response = item_cell(ui, icons, 76., &cell)
                                                .on_hover_text(material.name.get(russian));
                                            material_menu(&response, &[*id], russian);
                                            if response.clicked() {
                                                clicked = Some(*id);
                                            }
                                        }
                                    });
                                });
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
                        .id_salt("material_detail")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 12.);
                            material_detail(
                                ui,
                                icons,
                                cache,
                                data,
                                &live,
                                usage,
                                plan.as_ref(),
                                selected_material,
                                russian,
                            );
                        });
                },
            );
        });
        if clicked.is_some() {
            self.selected_material = clicked;
        }

    }
}

/// Pictures to pick several from, in a grid; the search narrows them.
fn filter_grid(
    ui: &mut Ui,
    icons: &Icons,
    entries: &[Entry],
    chosen: &mut BTreeSet<NeedFor>,
    query: &mut String,
    russian: bool,
) {
    const CELL: f32 = 64.;
    const GAP: f32 = 8.;
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    ui.add(
        TextEdit::singleline(query)
            .hint_text(tr("Search by name", "Поиск по названию"))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.);
    let needle = query.trim().to_lowercase();
    let shown: Vec<&Entry> = entries
        .iter()
        .filter(|entry| {
            needle.is_empty()
                || entry.name.en.to_lowercase().contains(&needle)
                || entry.name.ru.to_lowercase().contains(&needle)
        })
        .collect();
    let per_row = (((ui.available_width() + GAP) / (CELL + GAP)).floor() as usize).max(1);
    let rows: Vec<&[&Entry]> = shown.chunks(per_row).collect();
    // Only the rows in view are drawn, so only their pictures are fetched.
    ScrollArea::vertical()
        .max_height(420.)
        .auto_shrink([false, true])
        .show_rows(ui, CELL + GAP, rows.len(), |ui, range| {
            for row in &rows[range] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(GAP, GAP);
                    for entry in *row {
                        let cell = Cell {
                            icon: &entry.icon,
                            rarity: entry.rarity,
                            stars: false,
                            label: None,
                            selected: chosen.contains(&entry.who),
                        };
                        if item_cell(ui, icons, CELL, &cell)
                            .on_hover_text(entry.name.get(russian))
                            .clicked()
                            && !chosen.remove(&entry.who)
                        {
                            chosen.insert(entry.who);
                        }
                    }
                });
                ui.add_space(GAP);
            }
        });
}

/// The details of the material that was clicked.  What the planner needs of
/// it is shown only while something is planned.
#[allow(clippy::too_many_arguments)]
fn material_detail(
    ui: &mut Ui,
    icons: &Icons,
    cache: &Cache,
    data: &UpgradeData,
    live: &live_data::Upcoming,
    usage: &Usage,
    plan: Option<&BTreeMap<u32, u32>>,
    selected: Option<u32>,
    russian: bool,
) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    let Some((id, material)) = selected
        .and_then(|id| Some((id, data.materials.get(&id).or_else(|| live.materials.get(&id))?)))
    else {
        ui.label(
            RichText::new(tr(
                "Click a material to see what it is needed for.",
                "Нажмите на материал, чтобы увидеть, для чего он нужен.",
            ))
            .color(DIM),
        );
        return;
    };
    let have = cache.owned.get(&id).copied().unwrap_or(0);

    card(ui, |ui| {
        ui.horizontal(|ui| {
            let cell = Cell {
                icon: &material.icon,
                rarity: material.rarity.max(1),
                stars: true,
                label: Some((compact(have as u64), Color32::WHITE)),
                selected: false,
            };
            item_cell(ui, icons, 96., &cell);
            ui.vertical(|ui| {
                ui.label(RichText::new(material.name.get(russian)).size(20.).strong());
                ui.label(
                    RichText::new(stars(material.rarity.max(1)))
                        .size(15.)
                        .color(rarity_color(material.rarity)),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(material.category.get(russian))
                            .size(13.)
                            .color(DIM),
                    )
                    .wrap(),
                );
            });
        });
        ui.add_space(6.);
        Grid::new("material_numbers")
            .num_columns(2)
            .spacing([30., 4.])
            .show(ui, |ui| {
                ui.label(tr("Have", "Есть"));
                ui.label(RichText::new(have.to_string()).strong());
                ui.end_row();
                // Only with something in the planner.
                let need = plan.map_or(0, |plan| plan.get(&id).copied().unwrap_or(0));
                if need > 0 {
                    ui.label(tr("Needed", "Нужно"));
                    ui.label(RichText::new(need.to_string()).strong());
                    ui.end_row();
                    ui.label(tr("Missing", "Не хватает"));
                    let missing = need.saturating_sub(have);
                    ui.label(
                        RichText::new(missing.to_string())
                            .strong()
                            .color(if missing > 0 { BAD } else { OK }),
                    );
                    ui.end_row();
                }
            });
        ui.add_space(6.);
        show_on_map_button(ui, &[id], russian);
    });

    // Who upgrades with it, as pictures: the ones on the account first.
    if let Some(users) = usage.by_material.get(&id) {
        let characters = character_catalog(data, live);
        let weapons = weapon_catalog(data, live);
        let mine_characters: HashSet<u32> = cache
            .roster
            .characters
            .iter()
            .filter(|info| info.avatar_type == 1)
            .map(|info| info.avatar_id)
            .collect();
        let mine_weapons: HashSet<u32> = cache
            .roster
            .items
            .iter()
            .filter(|item| is_weapon(item))
            .map(|item| item.item_id)
            .collect();
        for (title, entries, mine) in [
            (tr("Characters", "Персонажи"), characters, mine_characters),
            (tr("Weapons", "Оружие"), weapons, mine_weapons),
        ] {
            let mut list: Vec<&Entry> = entries
                .iter()
                .filter(|entry| users.contains(&entry.who))
                .collect();
            if list.is_empty() {
                continue;
            }
            let owned_of = |entry: &Entry| match entry.who {
                NeedFor::Character(id) | NeedFor::Weapon(id) => mine.contains(&id),
            };
            // Stable: the order by rarity stays within each part.
            list.sort_by_key(|entry| !owned_of(entry));
            section(ui, &format!("{title} ({})", list.len()));
            card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6., 6.);
                    for entry in list.iter().take(80) {
                        let cell = Cell {
                            icon: &entry.icon,
                            rarity: entry.rarity,
                            stars: false,
                            label: None,
                            selected: false,
                        };
                        item_cell(ui, icons, 40., &cell).on_hover_text(entry.name.get(russian));
                    }
                });
                if list.len() > 80 {
                    ui.label(
                        RichText::new(format!("… {} {}", list.len() - 80, tr("more", "ещё")))
                            .size(12.)
                            .color(DIM),
                    );
                }
            });
        }
    }
}

/// How the database names a stat, in a language, and its value.
fn live_stat(name: &str, value: f32, russian: bool) -> (String, String) {
    let (base, percent) = match name.strip_suffix('%') {
        Some(base) => (base, true),
        None => (name, false),
    };
    let label = match (base, russian) {
        ("hp" | "HP", false) => "HP",
        ("hp" | "HP", true) => "ОЗ",
        ("atk" | "ATK", false) => "ATK",
        ("atk" | "ATK", true) => "АТК",
        ("def" | "DEF", false) => "DEF",
        ("def" | "DEF", true) => "ЗЩТ",
        ("CRIT Rate", true) => "Шанс крит. попадания",
        ("CRIT DMG", true) => "Крит. урон",
        ("Energy Recharge", true) => "Восстановление энергии",
        ("Elemental Mastery", true) => "Мастерство стихий",
        ("Healing Bonus", true) => "Бонус лечения",
        (other, _) => other,
    };
    let value = if percent {
        format!("{value:.1}%")
    } else {
        format!("{value:.0}")
    };
    (label.to_owned(), value)
}

fn stats_grid(ui: &mut Ui, id: &str, stats: &[(String, f32)], russian: bool) {
    Grid::new(id)
        .num_columns(2)
        .spacing([40., 4.])
        .show(ui, |ui| {
            for (name, value) in stats {
                let (label, value) = live_stat(name, *value, russian);
                ui.label(label);
                ui.label(RichText::new(value).strong());
                ui.end_row();
            }
        });
}

/// A header that opens to the text under it.  Without text it is just the
/// header.
fn expandable(ui: &mut Ui, id: impl std::hash::Hash, header: RichText, body: &str) {
    if body.is_empty() {
        ui.label(header);
        return;
    }
    egui::CollapsingHeader::new(header)
        .id_salt(id)
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(body)
                        .size(13.)
                        .color(Color32::from_gray(0xd0)),
                )
                .wrap(),
            );
        });
}

/// A name that opens to what it says.
fn named_block(ui: &mut Ui, id: impl std::hash::Hash, prefix: &str, item: &Named, russian: bool) {
    expandable(
        ui,
        id,
        RichText::new(format!("{prefix}{}", item.name.get(russian)))
            .size(15.)
            .strong(),
        item.description.get(russian),
    );
}

fn upcoming_character(
    ui: &mut Ui,
    icons: &Icons,
    cache: &Cache,
    character: &NewCharacter,
    russian: bool,
) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    let (element_name, element_color) = element(&character.element, russian);

    card(ui, |ui| {
        ui.horizontal(|ui| {
            let cell = Cell {
                icon: &character.icon,
                rarity: character.rarity,
                stars: true,
                label: None,
                selected: false,
            };
            item_cell(ui, icons, 96., &cell);
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(character.name.get(russian)).size(26.).strong());
                    ui.label(RichText::new(element_name).size(16.).color(element_color));
                    ui.label(
                        RichText::new(weapon_type_name(&character.weapon_type, russian))
                            .size(14.)
                            .color(DIM),
                    );
                });
                ui.label(
                    RichText::new(tr("Not released yet", "Ещё не вышел"))
                        .size(14.)
                        .color(PRIMOGEM),
                );
            });
        });
    });

    section(ui, tr("Stats at level 90", "Характеристики на 90 уровне"));
    card(ui, |ui| stats_grid(ui, "upcoming_stats", &character.stats, russian));

    section(ui, tr("Upgrade materials", "Материалы для прокачки"));
    card(ui, |ui| {
        ui.label(RichText::new(tr("Ascension", "Возвышение")).size(13.).color(DIM));
        cost_cells(ui, icons, cache, russian, &character.ascension);
        ui.add_space(6.);
        ui.label(RichText::new(tr("Talents", "Таланты")).size(13.).color(DIM));
        cost_cells(ui, icons, cache, russian, &character.talent_costs);
    });

    section(ui, tr("Talents", "Таланты"));
    card(ui, |ui| {
        for (index, talent) in character.talents.iter().enumerate() {
            named_block(ui, ("up_talent", character.id, index), "", talent, russian);
        }
    });
    if !character.passives.is_empty() {
        section(ui, tr("Passive talents", "Пассивные таланты"));
        card(ui, |ui| {
            for (index, passive) in character.passives.iter().enumerate() {
                named_block(ui, ("up_passive", character.id, index), "", passive, russian);
            }
        });
    }
    if !character.constellations.is_empty() {
        section(ui, tr("Constellations", "Созвездия"));
        card(ui, |ui| {
            for (index, constellation) in character.constellations.iter().enumerate() {
                named_block(
                    ui,
                    ("up_constellation", character.id, index),
                    &format!("C{}  ", index + 1),
                    constellation,
                    russian,
                );
            }
        });
    }
}

fn upcoming_weapon(ui: &mut Ui, icons: &Icons, cache: &Cache, weapon: &NewWeapon, russian: bool) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    card(ui, |ui| {
        ui.horizontal(|ui| {
            let cell = Cell {
                icon: &weapon.icon,
                rarity: weapon.rarity,
                stars: true,
                label: None,
                selected: false,
            };
            item_cell(ui, icons, 96., &cell);
            ui.vertical(|ui| {
                ui.label(RichText::new(weapon.name.get(russian)).size(24.).strong());
                ui.label(
                    RichText::new(weapon_type_name(&weapon.weapon_type, russian))
                        .size(14.)
                        .color(DIM),
                );
                ui.label(
                    RichText::new(tr("Not released yet", "Ещё не вышло"))
                        .size(14.)
                        .color(PRIMOGEM),
                );
            });
        });
    });

    section(ui, tr("Stats at level 90", "Характеристики на 90 уровне"));
    card(ui, |ui| {
        stats_grid(ui, "upcoming_weapon_stats", &weapon.stats, russian)
    });

    section(ui, tr("Ascension materials", "Материалы возвышения"));
    card(ui, |ui| cost_cells(ui, icons, cache, russian, &weapon.ascension));

    section(ui, tr("Passive", "Пассивный эффект"));
    card(ui, |ui| named_block(ui, ("up_weapon", weapon.id), "", &weapon.passive, russian));
}

pub(crate) fn language_choice(ui: &mut Ui, language: &mut Language) {
    egui::ComboBox::from_id_salt("roster_language")
        .width(60.)
        .selected_text(match language {
            Language::Ru => "RU",
            Language::En => "EN",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(language, Language::Ru, "RU");
            ui.selectable_value(language, Language::En, "EN");
        });
}

/// Kinds of material in the order they are shown.
fn category_order(category: &str) -> usize {
    const ORDER: [&str; 7] = [
        "Common Currency",
        "Character Level-Up Material",
        "Character Ascension Material",
        "Character Talent Material",
        "Weapon Ascension Material",
        "Character and Weapon Enhancement Material",
        "Refinement Material",
    ];
    ORDER
        .iter()
        .position(|kind| *kind == category)
        .unwrap_or(ORDER.len())
}

fn weapon_summary(ui: &mut Ui, icons: &Icons, data: &UpgradeData, item: &Item, russian: bool) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    let state = item.equip().weapon();
    let Some(weapon) = data.weapons.get(&item.item_id) else {
        ui.label(format!("#{}", item.item_id));
        return;
    };
    let max_level = weapon
        .promotions
        .get(state.promote_level as usize)
        .map_or(90, |promotion| promotion.max_level);
    let (attack, sub) = data.weapon_stats(weapon, state.level, state.promote_level);

    ui.horizontal(|ui| {
        let cell = Cell {
            icon: &weapon.icon,
            rarity: weapon.rarity,
            stars: true,
            label: Some((format!("Lv.{}", state.level), Color32::WHITE)),
            selected: false,
        };
        item_cell(ui, icons, 84., &cell);
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(weapon.name.get(russian)).size(20.).strong());
                ui.label(
                    RichText::new(weapon_type_name(&weapon.weapon_type, russian))
                        .size(13.)
                        .color(DIM),
                );
            });
            ui.horizontal_wrapped(|ui| {
                for text in [
                    format!("{} {}/{}", tr("Level", "Уровень"), state.level, max_level),
                    format!(
                        "{} {}/{}",
                        tr("Ascension", "Возвышение"),
                        state.promote_level,
                        weapon.promotions.len().saturating_sub(1)
                    ),
                    format!(
                        "{} {}",
                        tr("Refinement", "Уровень пробуждения"),
                        weapon_refinement(item)
                    ),
                ] {
                    ui.label(RichText::new(text).size(14.));
                    ui.add_space(10.);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} {:.0}", tr("Base ATK", "Базовая АТК"), attack));
                if !weapon.sub_prop.is_empty() {
                    let (label, percent) = prop(&weapon.sub_prop, russian);
                    ui.add_space(10.);
                    ui.label(format!("{label} {}", format_value(sub, percent)));
                }
            });
        });
    });
}

fn artifact_summary(ui: &mut Ui, icons: &Icons, data: &UpgradeData, item: &Item, russian: bool) {
    let reliquary = item.equip().reliquary();
    let Some(artifact) = data.artifacts.get(&item.item_id) else {
        ui.label(format!("#{}", item.item_id));
        return;
    };
    let set = data
        .sets
        .get(&artifact.set)
        .map_or("", |set| set.get(russian));
    // The packet counts levels from 1.
    let level = reliquary.level.saturating_sub(1);

    ui.horizontal(|ui| {
        let cell = Cell {
            icon: &artifact.icon,
            rarity: artifact.rarity,
            stars: true,
            label: Some((format!("+{level}"), Color32::WHITE)),
            selected: false,
        };
        item_cell(ui, icons, 64., &cell);
        ui.vertical(|ui| {
            ui.label(RichText::new(set).size(15.).strong());

            if let Some(main) = data.main_props.get(&reliquary.main_prop_id) {
                let (label, percent) = prop(main, russian);
                let value = data.main_value(artifact.rarity, main, level);
                ui.label(
                    RichText::new(format!(
                        "{label}  {}",
                        value.map_or_else(|| "?".to_owned(), |value| format_value(value, percent))
                    ))
                    .size(14.)
                    .color(PRIMOGEM),
                );
            }

            // The values of one stat add up across the rolls that went into it.
            let mut substats: Vec<(String, f32, bool)> = Vec::new();
            for id in &reliquary.append_prop_id_list {
                let Some(affix) = data.affixes.get(id) else {
                    continue;
                };
                let (label, percent) = prop(&affix.prop, russian);
                match substats.iter_mut().find(|(name, _, _)| *name == label) {
                    Some(existing) => existing.1 += affix.value,
                    None => substats.push((label, affix.value, percent)),
                }
            }
            ui.horizontal_wrapped(|ui| {
                for (label, value, percent) in substats {
                    ui.label(
                        RichText::new(format!("{label} {}", format_value(value, percent)))
                            .size(13.),
                    );
                    ui.add_space(8.);
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::compact;

    #[test]
    fn counts_from_a_thousand_are_shortened() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1000), "1k");
        assert_eq!(compact(1234), "1.2k");
        assert_eq!(compact(45_600), "45.6k");
        assert_eq!(compact(23_813_567), "23.8M");
    }
}
