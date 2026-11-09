//! The map tab: the map of the world, with the chests that have been found on
//! it, each where it is, and each as what kind it is.  The ones the account
//! has opened are grey.  The chests of the official map that are not found yet
//! can be shown too, faint.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::Ordering;

use egui::{
    Color32, Context, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

use crate::achievement_data::Language;
use crate::atlas::{self, WORLD};
use crate::achievement_tab::DIM;
use crate::chests::{Chest, Snapshot, kind_name, kinds, tier_name};
use crate::coverage::{self, Coverage};
use crate::map_overlay::MapOverlay;
use crate::marker_icons::MarkerIcons;
use crate::markers::{self, Label, Point, State as MarkerState, Store};
use crate::map_tiles::{
    MapInfo, MapTiles, TILE, TileKey, all_keys, estimate_level_mb, grid, keys_around, keys_in,
};
use crate::official_chests::{self, Official};
use crate::roster_tab::language_choice;

const MIN_SCALE: f32 = 0.02;
const MAX_SCALE: f32 = 2.;
/// Below this zoom the chests that are not found are dots, not chests.
const DOT_SCALE: f32 = 0.12;
const VOID: Color32 = Color32::from_rgb(0x0f, 0x15, 0x1c);
const OPENED: Color32 = Color32::from_rgb(0x70, 0x74, 0x7e);
/// How many tiles around the chests found are fetched, when that is asked for.
const AROUND: i32 = 2;
const TIERS: usize = 5;

/// Where a chest found is on the full size map.
pub(crate) fn on_map(info: &MapInfo, position: [f32; 3]) -> Vec2 {
    let (x, y) = official_chests::game_to_map(position);
    vec2(info.origin[0] + x, info.origin[1] + y)
}

/// Where a chest of the official map is on the full size map.
pub(crate) fn official_on_map(info: &MapInfo, chest: &Official) -> Vec2 {
    vec2(info.origin[0] + chest.x, info.origin[1] + chest.y)
}

/// The colour of a kind of chest: as the game colours them, near enough.
fn tier_color(tier: u8) -> Color32 {
    match tier {
        1 => Color32::from_rgb(0xe8, 0xea, 0xee),
        2 => Color32::from_rgb(0x6f, 0xe0, 0x7a),
        3 => Color32::from_rgb(0xb8, 0x82, 0xff),
        4 => Color32::from_rgb(0xff, 0xc2, 0x4a),
        _ => Color32::from_rgb(0x66, 0xd9, 0xe8),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Closed,
    Opened,
    /// Not sent though the place was walked by, so it is opened, most likely.
    Likely,
    /// On the official map, not found yet, and the place not walked by.
    Faint,
}

/// What is under the pointer.
enum Hover<'a> {
    Found(&'a Chest, Look),
    Faint(&'a Official, Look),
    Marker(Point, &'a Label, MarkerState),
}

/// What of the filters is kept between runs.
#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
struct Filters {
    enabled: Vec<u32>,
    only_closed: bool,
    show_faint: bool,
    show_explored: bool,
    /// Do not draw the circle around the player that is looked at.
    hide_range: bool,
    /// The radius of the radar (metres); 0 is the usual.
    radar: u32,
    /// The map that was shown (0: the world).
    map: u32,
    /// The kinds that can be marked as collected; not there: the usual ones.
    markable: Option<Vec<u32>>,
}

/// The kinds that can be marked as collected as long as the user has not chosen: what is taken
/// once and is never there again, but not the waypoints, the statues and the domains, which are
/// not taken at all.
fn default_markable(data: &markers::Data) -> BTreeSet<u32> {
    data.labels
        .iter()
        .filter(|label| label.parent != 0 && label.can_be_marked() && data.group_of(label.id) != 1)
        .map(|label| label.id)
        .collect()
}

/// `IRMINSUL_MAP_FILTERS` replaces the file, for trying things out.
fn filters_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_MAP_FILTERS") {
        return Some(path.into());
    }
    eframe::storage_dir(crate::APP_ID).map(|dir| dir.join("map_filters.json"))
}

fn load_filters() -> Option<Filters> {
    let text = std::fs::read_to_string(filters_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// A colour for a kind of marker, told by its number, for when it has no
/// picture (yet) or is too small for one.
pub(crate) fn label_color(id: u32) -> Color32 {
    let hue = (id.wrapping_mul(2654435761) >> 8) as f32 / (1u32 << 24) as f32;
    egui::ecolor::Hsva::new(hue, 0.65, 0.95, 1.).into()
}

/// A time of the clock, as it is told to the user.
fn clock(seconds: u64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// Which of the chests found are which of the official map, and what each
/// is, for the account in use.
pub(crate) struct Matched {
    pub(crate) version_of: u64,
    /// Whether each chest of the map is found (or has a chest found beside it).
    pub(crate) found: Vec<bool>,
    /// What each chest found looks like.
    pub(crate) of_found: Vec<Look>,
    /// What each chest of the map that is not found looks like.
    pub(crate) of_official: Vec<Look>,
    /// How many are opened, most likely.
    likely: usize,
}

impl Matched {
    pub(crate) fn version(&self) -> u64 {
        self.version_of
    }
}

pub struct MapTab {
    tiles: MapTiles,
    /// The place in the middle of the view, on the full size map.
    center: Vec2,
    /// Points of the screen to a pixel of the full size map.
    scale: f32,
    /// The view was placed where the chests are.
    placed: bool,
    only_closed: bool,
    show_faint: bool,
    /// Draw the circle around the player that is looked at.
    show_range: bool,
    /// How far around the player (metres) what the game does not send is looked at.
    radar: u32,
    /// Darken the places that were not walked by.
    show_explored: bool,
    /// The darkening, as a picture, and the places it was made from.
    fog: Option<(std::sync::Arc<Coverage>, egui::TextureHandle)>,
    /// Which tiers are shown: 1 to 5.
    shown: [bool; TIERS],
    matched: Option<Matched>,
    /// The levels of detail to fetch (0 is the closest).
    levels: [bool; 5],
    /// What of the maps is fetched ahead, and which maps (for whole ones).
    scope: Scope,
    download_maps: BTreeSet<u32>,
    /// The place the Region list was last set to (from the origin of the map).
    region_rect: Option<[f32; 4]>,
    /// The names of the maps being fetched, for telling what is going on.
    downloading: String,
    /// Fetch also while the game is running.
    during_game: bool,
    /// The map over the game, which a key shows.
    overlay: MapOverlay,
    /// The kinds of marker that are shown.
    enabled: BTreeSet<u32>,
    /// The domains that are shown, if only some are (the others are not shown).
    only_domains: Option<HashSet<u32>>,
    /// The kinds of marker that can be marked as collected: what is not here is always shown
    /// and a click on it does not mark it.
    markable: BTreeSet<u32>,
    /// The window where the user chooses them, and what was searched in it.
    marking_open: bool,
    marking_search: String,
    /// The marker of a domain whose description is open.
    domain_popup: Option<u32>,
    /// The pictures of the items and artifacts in that description.
    item_icons: crate::icons::Icons,
    /// The route mode: what is chosen to farm and the route planned.
    route: crate::route_ui::RouteState,
    search: String,
    /// What was changed in the filters, to be kept.
    filters_dirty: bool,
    /// What the account has collected.
    markers: Store,
    icons: MarkerIcons,
    /// The pictures of the floors of underground places.
    floor_images: MarkerIcons,
    /// The map that is shown (see `atlas`), the underground place on it, if
    /// one is, and the floor of it that is shown (0 is the surface).
    map: u32,
    /// A place to put in view when the map is next drawn (left, top, right,
    /// bottom, from the origin of the map).
    pending_view: Option<[f32; 4]>,
    /// The region that was last chosen, as told on its button.
    region: String,
    /// A thing to tell the user over the map, and when it was said.
    notice: Option<(String, std::time::Instant)>,
    /// The window of the settings is open.
    settings_open: bool,
    /// What the game said of the chests of the markers that was last applied.
    chest_marks_applied: Option<(Option<u32>, u64)>,
    /// Where the player was last told to be (for the settings).
    last_player: Option<crate::PlayerPlace>,
    /// The level shown: 0 is the surface, 1 the first floor of every
    /// underground place in view, 2 the second, and so on.
    level: usize,
    /// The level that is shown in fact (there may be fewer in view), and the
    /// floors of it, as of the last time the map was drawn.
    level_now: usize,
    shown_floors: HashSet<u32>,
    /// How many of each kind are collected, and what that was made for.
    done_counts: Option<((u64, u64, usize), HashMap<u32, usize>)>,
    /// The tiles that are on the disk, and when that was looked at.
    on_disk: Option<(HashMap<u32, HashSet<TileKey>>, std::time::Instant)>,
    /// The tiles around the chests found for what is chosen, and what it was
    /// chosen by.  Only made when that is wanted.
    plan: Option<((u64, [bool; 5]), Vec<TileKey>)>,
}

/// Where to look to see these places: a single one is centred and closely zoomed; those that
/// are all of one region show the region; the rest are shown all together.
fn view_for(
    places: &[[f32; 2]],
    def: &crate::atlas::MapDef,
    region: &mut String,
    region_rect: &mut Option<[f32; 4]>,
    russian: bool,
) -> Option<[f32; 4]> {
    let first = places.first()?;
    if places.len() == 1 {
        return Some([first[0] - 20., first[1] - 20., first[0] + 20., first[1] + 20.]);
    }
    let inside = |anchor: &crate::atlas::Anchor, at: &[f32; 2]| {
        anchor.rect[0] <= at[0] && at[0] <= anchor.rect[2] && anchor.rect[1] <= at[1] && at[1] <= anchor.rect[3]
    };
    let anchors: Vec<&crate::atlas::Anchor> = def.top_anchors().collect();
    // The regions that have one of the places, the smallest that has all of them first.
    let common = anchors
        .iter()
        .filter(|anchor| places.iter().all(|at| inside(anchor, at)))
        .min_by(|a, b| {
            let area = |rect: &[f32; 4]| (rect[2] - rect[0]) * (rect[3] - rect[1]);
            area(&a.rect).total_cmp(&area(&b.rect))
        });
    if let Some(anchor) = common {
        *region = anchor.name(russian).to_owned();
        *region_rect = Some(anchor.rect);
        return Some(anchor.rect);
    }
    let mut rect = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for at in places {
        rect = [rect[0].min(at[0]), rect[1].min(at[1]), rect[2].max(at[0]), rect[3].max(at[1])];
    }
    let pad = ((rect[2] - rect[0]).max(rect[3] - rect[1]) * 0.05).max(30.);
    Some([rect[0] - pad, rect[1] - pad, rect[2] + pad, rect[3] + pad])
}

/// The kinds of marker for the statues and the waypoints.
const TELEPORTS: [u32; 2] = [2, 3];

/// What a list of materials asked to see on the map: where an item is got.
pub struct ShowRequest {
    /// The kinds of marker to show.
    pub labels: Vec<u32>,
    /// The domains to show (their markers); the other domains are not shown.
    pub domains: Vec<u32>,
    /// What to tell over the map (the days of the domains ...).
    pub note: String,
}

static SHOW_REQUEST: std::sync::Mutex<Option<ShowRequest>> = std::sync::Mutex::new(None);

/// Asks for the map to be opened where these items are got.
pub fn request_show(request: ShowRequest, ctx: &Context) {
    if let Ok(mut slot) = SHOW_REQUEST.lock() {
        *slot = Some(request);
    }
    ctx.request_repaint();
}

static ROUTE_QUEUE: std::sync::Mutex<Vec<(u32, bool)>> = std::sync::Mutex::new(Vec::new());
static ROUTE_CHOSEN: std::sync::Mutex<std::collections::BTreeSet<u32>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Asks for items to be added to (or taken off) the route being made on the map.
pub fn request_route(items: &[u32], add: bool, ctx: &Context) {
    if let Ok(mut queue) = ROUTE_QUEUE.lock() {
        queue.extend(items.iter().map(|item| (*item, add)));
    }
    if let Ok(mut chosen) = ROUTE_CHOSEN.lock() {
        for item in items {
            if add {
                chosen.insert(*item);
            } else {
                chosen.remove(item);
            }
        }
    }
    ctx.request_repaint();
}

/// Whether an item is in the route being made.
pub fn route_has(item: u32) -> bool {
    ROUTE_CHOSEN.lock().is_ok_and(|chosen| chosen.contains(&item))
}

/// The items asked to be added or taken off, once.
pub fn take_route_requests() -> Vec<(u32, bool)> {
    ROUTE_QUEUE.lock().map(|mut queue| std::mem::take(&mut *queue)).unwrap_or_default()
}

/// What was asked for, once.
pub fn take_request() -> Option<ShowRequest> {
    SHOW_REQUEST.lock().ok().and_then(|mut request| request.take())
}

impl MapTab {
    pub fn new(ctx: &Context) -> Self {
        let mut tab = Self::made(ctx);
        if let Some(filters) = load_filters() {
            tab.enabled = filters.enabled.into_iter().collect();
            tab.only_closed = filters.only_closed;
            if let Some(markable) = filters.markable {
                tab.markable = markable.into_iter().collect();
            }
            tab.show_faint = filters.show_faint;
            tab.show_explored = filters.show_explored;
            tab.show_range = !filters.hide_range;
            if filters.radar != 0 {
                tab.radar = filters.radar.clamp(crate::chest_watch::RADAR_LEAST, crate::chest_watch::RADAR_MOST);
            }
            crate::chest_watch::set_radar(tab.radar);
            if filters.map != 0 && filters.map != WORLD {
                tab.switch_map(filters.map, &markers::data());
            }
        }
        // For trying things out: start in an underground place (and on a floor of it).
        if let Some(id) = std::env::var("IRMINSUL_MAP_SITE").ok().and_then(|text| text.parse::<u32>().ok()) {
            let data = markers::data();
            if let Some(site) = data.atlas.map(tab.map).and_then(|def| def.site(id)) {
                let index = std::env::var("IRMINSUL_MAP_FLOOR")
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                    .unwrap_or(site.default);
                let floor = &site.floors[index.min(site.floors.len() - 1)];
                tab.level = index.min(site.floors.len() - 1) + 1;
                tab.pending_view = Some(floor.rect);
            }
        }
        // For trying things out: start looking at a place (left, top, right, bottom).
        if let Ok(text) = std::env::var("IRMINSUL_MAP_GOTO") {
            let numbers: Vec<f32> = text.split(',').filter_map(|part| part.trim().parse().ok()).collect();
            if let [left, top, right, bottom] = numbers[..] {
                tab.pending_view = Some([left, top, right, bottom]);
            }
        }
        // The markers are fetched again, in the background, when they are old.
        markers::refresh_in_background(ctx, false);
        tab
    }

    fn made(ctx: &Context) -> Self {
        Self {
            tiles: MapTiles::new(ctx),
            center: Vec2::ZERO,
            scale: 0.1,
            placed: false,
            only_closed: false,
            show_faint: true,
            show_range: true,
            radar: crate::chest_watch::NEAR_METRES as u32,
            show_explored: true,
            fog: None,
            shown: [true; TIERS],
            matched: None,
            levels: [false, false, true, true, true],
            scope: Scope::Maps,
            download_maps: BTreeSet::from([WORLD]),
            region_rect: None,
            downloading: String::new(),
            during_game: false,
            overlay: MapOverlay::new(ctx),
            enabled: markers::DEFAULT_SHOWN.into_iter().collect(),
            only_domains: None,
            markable: default_markable(&markers::data()),
            marking_open: false,
            marking_search: String::new(),
            domain_popup: None,
            item_icons: crate::icons::Icons::new(ctx),
            route: crate::route_ui::RouteState::new(),
            search: String::new(),
            filters_dirty: false,
            markers: Store::default(),
            icons: MarkerIcons::new(ctx),
            floor_images: MarkerIcons::floors(ctx),
            map: WORLD,
            level: 0,
            level_now: 0,
            shown_floors: HashSet::from([0]),
            pending_view: None,
            region: String::new(),
            notice: None,
            settings_open: false,
            chest_marks_applied: None,
            last_player: None,
            done_counts: None,
            on_disk: None,
            plan: None,
        }
    }

    /// Shows only the given kinds of marker, and the teleports (a request of the materials
    /// and the planner: where an item is got), on the map of the world.
    /// Applies what the menus of the materials asked of the route.
    pub fn route_requests(&mut self, requests: Vec<(u32, bool)>) {
        if requests.is_empty() {
            return;
        }
        for (item, add) in requests {
            if add {
                self.route.items.insert(item);
            } else {
                self.route.items.remove(&item);
            }
        }
        self.route.resolve(&markers::data());
    }

    /// Keeps what the menus of the materials know of the route up to date.
    pub fn route_mirror(&self) {
        if let Ok(mut chosen) = ROUTE_CHOSEN.lock() {
            *chosen = self.route.items.clone();
        }
    }

    pub fn show_only(&mut self, request: ShowRequest, russian: bool) {
        let ShowRequest { labels, domains, note } = request;
        let data = markers::data();
        if self.map != WORLD {
            self.switch_map(WORLD, &data);
        }
        self.level = 0;
        self.search.clear();
        self.enabled = labels.iter().copied().chain(TELEPORTS).collect();
        if !domains.is_empty() {
            self.enabled.insert(markers::DOMAIN_LABEL);
        }
        self.only_domains = (!domains.is_empty()).then(|| domains.iter().copied().collect());
        // The view: a single marker is centred and zoomed to the most; markers of one region
        // show that region; markers of several regions are all in view.
        let mut places: Vec<[f32; 2]> = Vec::new();
        for label in &labels {
            places.extend(data.points_in(*label, WORLD, 0).map(|point| [point.x, point.y]));
        }
        if !domains.is_empty() {
            places.extend(
                data.points_in(markers::DOMAIN_LABEL, WORLD, 0)
                    .filter(|point| domains.contains(&point.id))
                    .map(|point| [point.x, point.y]),
            );
        }
        self.pending_view = data.atlas.map(WORLD).and_then(|def| view_for(&places, def, &mut self.region, &mut self.region_rect, russian));
        self.placed = true;
        if !note.is_empty() {
            self.notice = Some((note, std::time::Instant::now()));
        }
    }

    /// Whether this is the surface of the world: where the chests, the places
    /// walked by and the player are.
    fn tracked(&self) -> bool {
        self.map == WORLD && self.level_now == 0
    }

    /// Shows another map.
    fn switch_map(&mut self, id: u32, data: &markers::Data) {
        let Some(def) = data.atlas.map(id) else {
            return;
        };
        self.map = id;
        self.level = 0;
        self.region.clear();
        self.region_rect = None;
        self.tiles.set_map(def.info());
        self.on_disk = None;
        self.plan = None;
        self.filters_dirty = true;
        if id == WORLD {
            self.placed = false;
        } else {
            self.pending_view = Some(def.bounds());
            self.placed = true;
        }
    }

    /// Puts a place of the map (left, top, right, bottom) in view.
    fn view_rect(&mut self, rect: [f32; 4], size: Vec2, origin: [f32; 2]) {
        let (width, height) = ((rect[2] - rect[0]).max(50.), (rect[3] - rect[1]).max(50.));
        self.center = vec2(origin[0] + (rect[0] + rect[2]) / 2., origin[1] + (rect[1] + rect[3]) / 2.);
        self.scale = (size.x / (width * 1.15)).min(size.y / (height * 1.15)).clamp(MIN_SCALE, MAX_SCALE);
    }

    /// What is chosen to be seen, next to the title: the map, the region of
    /// it to go to, and the underground place with its floors.
    fn view_bar(&mut self, ui: &mut Ui, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = markers::data();
        let Some(def) = data.atlas.map(self.map) else {
            return;
        };
        let mut switch: Option<u32> = None;
        let mut go: Option<[f32; 4]> = None;
        let mut region: Option<String> = None;
        let mut site_pick: Option<u32> = None;

        egui::ComboBox::from_id_salt("map_choice")
            .selected_text(RichText::new(def.name(russian)).size(15.))
            .width(140.)
            .show_ui(ui, |ui| {
                for other in &data.atlas.maps {
                    if ui.selectable_label(other.id == self.map, other.name(russian)).clicked() {
                        switch = Some(other.id);
                    }
                }
            })
            .response
            .on_hover_text(tr("Which map", "Какая карта"));

        // Regions: as the official map lists them, by the part of the world.
        if def.top_anchors().next().is_some() {
            let shown = if self.region.is_empty() { tr("Region", "Регион").to_owned() } else { self.region.clone() };
            egui::ComboBox::from_id_salt("map_region")
                .selected_text(RichText::new(shown).size(15.))
                .width(210.)
                .height(520.)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(false, tr("Whole map", "Вся карта")).clicked() {
                        go = Some(def.bounds());
                        region = Some(String::new());
                    }
                    let mut areas: Vec<u32> = def.top_anchors().map(|anchor| anchor.area).collect();
                    areas.sort_unstable();
                    areas.dedup();
                    for area in areas {
                        let anchors: Vec<&atlas::Anchor> = def.top_anchors().filter(|anchor| anchor.area == area).collect();
                        let indent = if let Some(name) = atlas::region_name(area, russian) {
                            if ui.selectable_label(false, RichText::new(name).strong()).clicked() {
                                go = atlas::union(anchors.iter().map(|anchor| anchor.rect));
                                region = Some(name.to_owned());
                            }
                            "    "
                        } else {
                            ""
                        };
                        for anchor in anchors {
                            if ui
                                .selectable_label(false, format!("{indent}{}", anchor.name(russian)))
                                .clicked()
                            {
                                go = Some(anchor.rect);
                                region = Some(anchor.name(russian).to_owned());
                            }
                        }
                    }
                })
                .response
                .on_hover_text(tr("Go to a region", "Перейти к региону"));
        }

        // Underground places: a list to go to (their floors are the levels,
        // chosen with the buttons on the map).
        if !def.sites.is_empty() {
            egui::ComboBox::from_id_salt("map_site")
                .selected_text(RichText::new(tr("Underground", "Подземелья")).size(15.))
                .width(210.)
                .height(520.)
                .show_ui(ui, |ui| {
                    let mut areas: Vec<u32> = def.sites.iter().map(|site| site.area).collect();
                    areas.sort_unstable();
                    areas.dedup();
                    for area in areas {
                        let mut sites: Vec<(String, &atlas::Site)> = def
                            .sites
                            .iter()
                            .filter(|site| site.area == area)
                            .map(|site| (site.name(russian), site))
                            .collect();
                        sites.sort_by(|a, b| a.0.cmp(&b.0));
                        let heading = atlas::region_name(area, russian)
                            .unwrap_or(if def.id == WORLD { tr("Elsewhere", "Прочее") } else { tr("Underground", "Подземелья") });
                        ui.label(RichText::new(heading).strong().color(DIM));
                        for (name, site) in sites {
                            let floors = if site.floors.len() > 1 { format!("  ({})", site.floors.len()) } else { String::new() };
                            if ui.selectable_label(false, format!("{name}{floors}")).clicked() {
                                site_pick = Some(site.id);
                            }
                        }
                    }
                })
                .response
                .on_hover_text(tr("Go to an underground place", "Перейти к подземелью"));
        }

        if let Some(id) = switch
            && id != self.map
        {
            self.switch_map(id, &data);
        }
        if let Some(rect) = go {
            self.pending_view = Some(rect);
            self.region_rect = Some(rect);
        }
        if let Some(label) = region {
            self.region = label;
        }
        if let Some(site) = site_pick.and_then(|id| def.site(id)) {
            // Shown at its first floor, which is level 1 or so.
            self.level = site.default + 1;
            self.pending_view = site.extent().or(Some(site.first_floor().rect));
        }
    }

    /// Puts the whole of what is found in view, or the middle of the world if
    /// nothing is.
    fn fit(&mut self, snapshot: &Snapshot, size: Vec2) {
        let info = self.tiles.info();
        let places: Vec<Vec2> = snapshot
            .chests
            .iter()
            .filter_map(|chest| chest.position.map(|position| on_map(&info, position)))
            .collect();
        if places.is_empty() {
            self.center = vec2(info.origin[0] + 500., info.origin[1] + 500.);
            self.scale = 0.12;
            return;
        }
        let (mut min, mut max) = (places[0], places[0]);
        for place in &places {
            min = min.min(*place);
            max = max.max(*place);
        }
        self.center = (min + max) / 2.;
        let span = (max - min) + vec2(400., 400.);
        self.scale = (size.x / span.x).min(size.y / span.y).clamp(MIN_SCALE, 0.6);
    }

    /// Tells which of the chests found are which of the official map, and what
    /// each is for the account, again if what is found or walked has changed.
    fn match_found(&mut self, snapshot: &Snapshot) {
        if self
            .matched
            .as_ref()
            .is_some_and(|matched| matched.version_of == snapshot.version)
        {
            return;
        }
        let official = official_chests::all();
        let found: Vec<(u8, Option<[f32; 3]>)> = snapshot
            .chests
            .iter()
            .map(|chest| (chest.tier(), chest.position))
            .collect();
        let matches = official_chests::match_found(&found);
        let mut flags = vec![false; official.len()];
        for index in matches.iter().flatten() {
            flags[*index] = true;
        }
        // A chest found that is not told to be one of the map, but is by one
        // that is not told to be any, is that one, off by a little more.
        for (index, chest) in snapshot.chests.iter().enumerate() {
            let (Some(position), None) = (chest.position, matches[index]) else {
                continue;
            };
            let (x, y) = official_chests::game_to_map(position);
            let nearest = official
                .iter()
                .enumerate()
                .filter(|(at, other)| !flags[*at] && other.tier == chest.tier())
                .map(|(at, other)| (at, (other.x - x).hypot(other.y - y)))
                .filter(|(_, distance)| *distance <= 30.)
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((at, _)) = nearest {
                flags[at] = true;
            }
        }

        let walked = &snapshot.coverage;
        let mut likely = 0;
        let of_found: Vec<Look> = snapshot
            .chests
            .iter()
            .map(|chest| {
                if chest.opened_at.is_some() {
                    Look::Opened
                } else if !chest.seen
                    && let Some(position) = chest.position
                    && {
                        let (x, y) = official_chests::game_to_map(position);
                        walked.is_covered(x, y)
                    }
                {
                    // Walked by, and the game did not send it to this account.
                    likely += 1;
                    Look::Likely
                } else {
                    Look::Closed
                }
            })
            .collect();
        let of_official: Vec<Look> = official
            .iter()
            .enumerate()
            .map(|(index, chest)| {
                if flags[index] {
                    Look::Closed
                } else if walked.is_covered(chest.x, chest.y) {
                    likely += 1;
                    Look::Likely
                } else {
                    Look::Faint
                }
            })
            .collect();
        self.matched = Some(Matched {
            version_of: snapshot.version,
            found: flags,
            of_found,
            of_official,
            likely,
        });
    }

    /// The darkening of the places not walked by, as a picture the size of the
    /// grid, made again when the places change.
    fn update_fog(&mut self, ctx: &Context, coverage: &std::sync::Arc<Coverage>) {
        if self
            .fog
            .as_ref()
            .is_some_and(|(made, _)| std::sync::Arc::ptr_eq(made, coverage) || **made == **coverage)
        {
            return;
        }
        let dark = Color32::from_rgba_unmultiplied(4, 8, 16, 150);
        let mut pixels = vec![dark; coverage::COLUMNS * coverage::ROWS];
        for row in 0..coverage::ROWS {
            for column in 0..coverage::COLUMNS {
                if coverage.cell_covered(column, row) {
                    pixels[row * coverage::COLUMNS + column] = Color32::TRANSPARENT;
                }
            }
        }
        let image = egui::ColorImage {
            size: [coverage::COLUMNS, coverage::ROWS],
            pixels,
            source_size: vec2(coverage::COLUMNS as f32, coverage::ROWS as f32),
        };
        let texture = ctx.load_texture("walked", image, egui::TextureOptions::LINEAR);
        self.fog = Some((coverage.clone(), texture));
    }

    /// The map over the game: call every frame, whatever tab is shown.
    pub fn overlay_frame(
        &mut self,
        ctx: &Context,
        snapshot: &Snapshot,
        marks: &crate::chest_watch::ChestMarks,
        player: Option<crate::PlayerPlace>,
        language: Language,
    ) {
        self.match_found(snapshot);
        self.markers.use_account(snapshot.uid);
        self.apply_chest_marks(marks);
        // What is chosen on the map here is on the map over the game too.
        let mut version = self.markers.version ^ u64::from(self.only_closed) ^ (u64::from(self.show_faint) << 1) ^ (self.markable.len() as u64) << 20;
        version ^= u64::from(self.show_range) << 2;
        for label in &self.enabled {
            version = version.rotate_left(5) ^ u64::from(*label);
        }
        let shown = crate::map_overlay::SharedMarkers {
            enabled: &self.enabled,
            markable: &self.markable,
            store: &self.markers,
            hide_done: self.only_closed,
            show_faint: self.show_faint,
            show_range: self.show_range,
            version,
        };
        self.overlay.frame(
            ctx,
            snapshot,
            player,
            self.matched.as_ref(),
            &shown,
            language == Language::Ru,
        );
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        snapshot: &Snapshot,
        marks: &crate::chest_watch::ChestMarks,
        player: Option<crate::PlayerPlace>,
        language: &mut Language,
    ) {
        self.tiles.poll();
        self.icons.poll();
        self.floor_images.poll();
        self.match_found(snapshot);
        self.markers.use_account(snapshot.uid);
        self.apply_chest_marks(marks);
        let russian = *language == Language::Ru;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };

        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Map", "Карта")).size(34.).strong());
            ui.add_space(8.);
            self.view_bar(ui, russian);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                language_choice(ui, language);
                self.settings_button(ui, russian);
                self.locate_button(ui, player, russian);
                self.route_button(ui, russian);
                let progress = self.tiles.progress();
                if progress.running.load(Ordering::Relaxed) {
                    ui.label(
                        RichText::new(format!(
                            "{} {}/{}",
                            tr("Tiles:", "Тайлы:"),
                            progress.done.load(Ordering::Relaxed),
                            progress.total.load(Ordering::Relaxed)
                        ))
                        .size(13.)
                        .color(DIM),
                    );
                }
            });
        });
        let russian = *language == Language::Ru;
        ui.add_space(4.);

        // The filters on the left, as on the official map, and the map on the right.
        let mut fit_now = false;
        let mut route_action = crate::route_ui::Action::None;
        self.route.poll();
        egui::SidePanel::left("map_filters")
            .resizable(true)
            .default_width(330.)
            .width_range(250.0..=520.0)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 0,
                right: 8,
                top: 0,
                bottom: 0,
            }))
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.route.active {
                            self.item_icons.poll();
                            let data = markers::data();
                            route_action = self.route.panel(ui, &self.item_icons, &data, data.atlas.map(self.map), self.map, russian);
                        } else {
                            fit_now = self.filters_ui(ui, snapshot, russian);
                        }
                    });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                self.map_ui(ui, snapshot, player, russian, fit_now);
            });
        self.route_action(ui.ctx(), route_action, player, russian);
        self.settings_window(ui.ctx(), snapshot, russian);
        self.marking_window(ui.ctx(), russian);
        self.domain_window(ui.ctx(), russian);
        self.markers.save();
        if self.filters_dirty {
            self.filters_dirty = false;
            self.save_filters();
        }
    }

    fn save_filters(&self) {
        let Some(path) = filters_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let filters = Filters {
            enabled: self.enabled.iter().copied().collect(),
            only_closed: self.only_closed,
            show_faint: self.show_faint,
            show_explored: self.show_explored,
            hide_range: !self.show_range,
            radar: self.radar,
            map: self.map,
            markable: Some(self.markable.iter().copied().collect()),
        };
        if let Ok(bytes) = serde_json::to_vec(&filters) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Whether the chests of a kind (1 to 5) are shown.
    fn tier_shown(&self, tier: u8) -> bool {
        markers::CHEST_LABELS
            .get(tier as usize - 1)
            .is_some_and(|label| self.enabled.contains(label))
    }

    /// The panel of filters: what is shown on the map.  Returns whether the map
    /// is to be put so that what is found is in view.
    fn filters_ui(&mut self, ui: &mut Ui, snapshot: &Snapshot, russian: bool) -> bool {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = markers::data();
        let now = markers::unix_now();
        let mut fit_now = false;
        let mut changed = false;
        let (map, tracked) = (self.map, self.tracked());
        let floors_shown = self.shown_floors.clone();

        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(tr("Search markers", "Поиск меток"))
                .desired_width(ui.available_width()),
        );
        ui.add_space(4.);
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("Chests found", "Найденные сундуки")).clicked() {
                fit_now = true;
            }
            if ui
                .button(tr("Marks", "Отметки"))
                .on_hover_text(tr(
                    "Choose which kinds of marker can be marked as collected",
                    "Выбрать, какие метки можно отмечать собранными",
                ))
                .clicked()
            {
                self.marking_open = !self.marking_open;
            }
            if ui.button(tr("Hide all", "Скрыть всё")).clicked() {
                self.only_domains = None;
                self.enabled.clear();
                changed = true;
            }
            if ui.button(tr("Defaults", "По умолчанию")).clicked() {
                self.only_domains = None;
                self.enabled = markers::DEFAULT_SHOWN.into_iter().collect();
                changed = true;
            }
        });

        egui::CollapsingHeader::new(RichText::new(tr("Display", "Отображение")).size(15.).strong())
            .default_open(true)
            .show(ui, |ui| {
                changed |= ui
                    .checkbox(
                        &mut self.only_closed,
                        RichText::new(tr("Hide opened and collected", "Скрывать открытые и собранные")).size(14.),
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.show_range,
                        RichText::new(tr("Circle around the player that is followed", "Круг вокруг игрока, где идёт отслеживание"))
                            .size(14.),
                    )
                    .on_hover_text(tr(
                        "How far around the player the program looks at chests and what is not sent by the game.",
                        "На каком расстоянии от игрока программа проверяет сундуки и то, что игра не присылает.",
                    ))
                    .changed();
                // How far around the player what the game does not send is looked at.
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr("Radar radius, m", "Радиус радара, м")).size(14.));
                    let slider = egui::Slider::new(&mut self.radar, crate::chest_watch::RADAR_LEAST..=crate::chest_watch::RADAR_MOST);
                    if ui.add(slider).changed() {
                        crate::chest_watch::set_radar(self.radar);
                        changed = true;
                    }
                });
                ui.label(
                    RichText::new(tr(
                        "The game sends objects about 100-130 m around you (up to ~580 m when you enter a place). The wider the radar, the more is marked done without walking up to it, but a wrong guess is more likely. What the game sends later, at any distance, is put back. Oculi are looked at only close (about 20 m).",
                        "Игра присылает объекты в радиусе примерно 100–130 м вокруг вас (до ~580 м при входе в локацию). Чем шире радар, тем больше отмечается выполненным без подхода, но тем выше шанс ошибки. Что игра пришлёт позже, на любом расстоянии, возвращается обратно. Окулы проверяются только вблизи (около 20 м).",
                    ))
                    .size(12.)
                    .weak(),
                );
                // These are of what is followed: the surface of the world.
                if tracked {
                    changed |= ui
                        .checkbox(
                            &mut self.show_faint,
                            RichText::new(tr("Show chests not found yet", "Показывать ненайденные сундуки")).size(14.),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut self.show_explored,
                            RichText::new(tr("Darken places not walked by", "Затемнять не пройденное")).size(14.),
                        )
                        .changed();
                }
            });

        // How many of each kind are collected, made again when that changes.
        let stamp = (self.markers.version, now / 60, data.points.len());
        if self.done_counts.as_ref().is_none_or(|(made, _)| *made != stamp) {
            self.done_counts = Some((stamp, self.markers.done_counts(&data, now)));
        }
        let done_counts = self.done_counts.as_ref().map(|(_, counts)| counts.clone()).unwrap_or_default();

        let needle = self.search.trim().to_lowercase();
        let searching = !needle.is_empty();
        ui.add_space(6.);
        for group in data.groups() {
            let kinds: Vec<&Label> = data
                .children(group.id)
                .filter(|label| data.points_on(label.id, map, &floors_shown).next().is_some())
                .filter(|label| {
                    !searching
                        || label.en.to_lowercase().contains(&needle)
                        || label.ru.to_lowercase().contains(&needle)
                })
                .collect();
            if kinds.is_empty() {
                continue;
            }
            let shown = kinds.iter().filter(|label| self.enabled.contains(&label.id)).count();
            let header = format!("{}  ({shown}/{})", group.name(russian), kinds.len());
            let mut header = egui::CollapsingHeader::new(RichText::new(header).size(15.).strong())
                .id_salt(("map_group", group.id))
                .default_open(group.id == 13);
            if searching {
                header = header.open(Some(true));
            }
            header.show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button(tr("All", "Все")).clicked() {
                        for label in &kinds {
                            self.enabled.insert(label.id);
                        }
                        changed = true;
                    }
                    if ui.small_button(tr("None", "Ни одной")).clicked() {
                        for label in &kinds {
                            self.enabled.remove(&label.id);
                        }
                        changed = true;
                    }
                });
                let width = ((ui.available_width() - 6.) / 2.).max(110.);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6., 6.);
                    for label in &kinds {
                        let total = data.points_on(label.id, map, &floors_shown).count();
                        let done = if tracked && markers::CHEST_LABELS.contains(&label.id) {
                            snapshot.chests.iter().filter(|chest| chest.tier() == chest_tier(label.id) && chest.opened_at.is_some()).count()
                        } else {
                            done_counts.get(&label.id).copied().filter(|_| self.markable.contains(&label.id)).unwrap_or(0)
                        };
                        let count = if done > 0 { format!("{done}/{total}") } else { total.to_string() };
                        let texture = self.icons.get(label.id, &label.icon);
                        let selected = self.enabled.contains(&label.id);
                        let response = chip(
                            ui,
                            width,
                            selected,
                            texture.as_ref(),
                            label_color(label.id),
                            label.name(russian),
                            &count,
                        );
                        let response = if label.resin {
                            response.on_hover_text(tr(
                                "A boss fought for resin: it is there again at once, as long as there is resin",
                                "Босс за смолу: появляется снова сразу, пока хватает смолы",
                            ))
                        } else if label.refresh > 0 {
                            response.on_hover_text(format!(
                                "{}: {} {}",
                                tr("Comes back after", "Возвращается через"),
                                label.refresh,
                                tr("h", "ч")
                            ))
                        } else {
                            response
                        };
                        if response.clicked() {
                            if !self.enabled.remove(&label.id) {
                                self.enabled.insert(label.id);
                            }
                            changed = true;
                        }
                    }
                });
            });
        }

        // Where the markers come from, and that they are kept up to date.
        ui.add_space(8.);
        ui.separator();
        let status = markers::refresh_status();
        let updated = if data.stamp == 0 {
            tr("what the program has", "из программы").to_owned()
        } else {
            clock(data.stamp)
        };
        ui.label(
            RichText::new(format!(
                "{}: {} · {} {}",
                tr("Markers", "Метки"),
                updated,
                data.points.iter().filter(|point| u32::from(point.map) == map && floors_shown.contains(&u32::from(point.floor))).count(),
                tr("on the map", "на карте")
            ))
            .size(12.)
            .color(DIM),
        );
        ui.horizontal(|ui| {
            if status.running {
                ui.label(RichText::new(tr("Updating…", "Обновляется…")).size(12.).color(DIM));
            } else if ui.small_button(tr("Update now", "Обновить сейчас")).clicked() {
                markers::refresh_in_background(ui.ctx(), true);
            }
            if let Some(error) = &status.error {
                ui.label(RichText::new(error.chars().take(60).collect::<String>()).size(11.).color(Color32::from_rgb(0xff, 0x8a, 0x8a)));
            }
        });
        if changed {
            self.filters_dirty = true;
        }
        fit_now
    }

    /// The map, with what is shown on it.
    fn map_ui(
        &mut self,
        ui: &mut Ui,
        snapshot: &Snapshot,
        player: Option<crate::PlayerPlace>,
        russian: bool,
        fit_now: bool,
    ) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let (opened, known) = snapshot.totals();
        let on_map_total = official_chests::all().len();
        let likely = self.matched.as_ref().map_or(0, |matched| matched.likely);
        let hint = tr(
            "Scroll to zoom, drag to move, click a marker to mark it.",
            "Колесо — масштаб, перетаскивание — сдвиг, клик по метке — отметить.",
        );
        // Chests are followed on the surface of the world only.
        let tracked = self.tracked();
        let line = if tracked {
            format!(
                "{}: {known} {} {on_map_total}   ·   {}: {opened}   ·   {}: {likely}   ·   {hint}",
                tr("Chests found", "Найдено сундуков"),
                tr("of", "из"),
                tr("opened", "открыто"),
                tr("most likely opened", "вероятно открыто"),
            )
        } else {
            format!(
                "{}   ·   {hint}",
                tr(
                    "Chests, the places walked by and the player are followed on the surface of Teyvat.",
                    "Сундуки, пройденные места и игрок отслеживаются на поверхности Тейвата."
                )
            )
        };
        ui.label(RichText::new(line).size(13.).color(DIM));
        ui.add_space(4.);

        let area = ui.available_rect_before_wrap();
        if area.width() < 50. || area.height() < 50. {
            return;
        }
        let response = ui.allocate_rect(area, Sense::click_and_drag());

        if fit_now && self.map != WORLD {
            self.switch_map(WORLD, &markers::data());
        }
        // A place that was chosen to go to.
        if let Some(rect) = self.pending_view.take() {
            let origin = self.tiles.info().origin;
            self.view_rect(rect, area.size(), origin);
            self.placed = true;
        }
        // Placed once there is something to place it by, or when asked.
        if fit_now || (!self.placed && (!snapshot.chests.is_empty() || ui.ctx().input(|i| i.time) > 3.)) {
            self.fit(snapshot, area.size());
            self.placed = true;
        }

        // Moving and zooming.
        if response.dragged() {
            self.center -= response.drag_delta() / self.scale;
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll * 0.0025).exp() * pinch;
            if (factor - 1.).abs() > 1e-4
                && let Some(pointer) = response.hover_pos()
            {
                let under = self.center + (pointer - area.center()) / self.scale;
                self.scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
                self.center = under - (pointer - area.center()) / self.scale;
            }
        }

        let info = self.tiles.info();
        let data = markers::data();
        let origin = vec2(info.origin[0], info.origin[1]);

        // The underground places in view, and so how many levels there are: the
        // most floors any of them has.  A level is one floor of each of them.
        let view = Rect::from_center_size(self.center.to_pos2(), area.size() / self.scale);
        let mut visible: Vec<(&atlas::Site, Rect)> = Vec::new();
        let mut max_level = 0usize;
        if let Some(def) = data.atlas.map(self.map) {
            for site in &def.sites {
                let Some(extent) = site.extent() else {
                    continue;
                };
                let rect = Rect::from_min_max(
                    pos2(origin.x + extent[0], origin.y + extent[1]),
                    pos2(origin.x + extent[2], origin.y + extent[3]),
                );
                if rect.intersects(view) {
                    max_level = max_level.max(site.floors.len());
                    visible.push((site, rect));
                }
            }
        }
        let level = self.level.min(max_level);
        self.level_now = level;
        self.shown_floors = if level == 0 {
            HashSet::from([0])
        } else {
            visible.iter().filter_map(|(site, _)| site.floors.get(level - 1)).map(|floor| floor.id).collect()
        };
        let tracked = self.map == WORLD && level == 0;
        let painter = ui.painter_at(area);
        painter.rect_filled(area, CornerRadius::same(6), VOID);
        self.draw_tiles(&painter, area, &info);

        // The places that were not walked by are darker, once there are some that
        // were.
        if tracked && self.show_explored && !snapshot.coverage.is_empty() {
            self.update_fog(ui.ctx(), &snapshot.coverage);
            if let Some((_, texture)) = &self.fog {
                let top_left = area.center()
                    + (vec2(info.origin[0] + coverage::X0, info.origin[1] + coverage::Y0) - self.center)
                        * self.scale;
                let size = vec2(coverage::COLUMNS as f32, coverage::ROWS as f32) * coverage::CELL * self.scale;
                painter.image(
                    texture.id(),
                    Rect::from_min_size(top_left, size),
                    Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                    Color32::WHITE,
                );
            }
        }

        let to_screen = |place: Vec2| -> Pos2 { area.center() + (place - self.center) * self.scale };
        let on_screen = area.expand(12.);
        let mut hoverable: Vec<(Pos2, Hover)> = Vec::new();

        // Underground: the map dimmed, and the floor of this level of each place in
        // view on it.
        if level > 0 {
            painter.rect_filled(area, CornerRadius::same(6), Color32::from_black_alpha(200));
            for (site, _) in &visible {
                let Some(floor) = site.floors.get(level - 1) else {
                    continue;
                };
                if floor.rect[2] <= floor.rect[0] {
                    continue;
                }
                let rect = Rect::from_two_pos(
                    to_screen(origin + vec2(floor.rect[0], floor.rect[1])),
                    to_screen(origin + vec2(floor.rect[2], floor.rect[3])),
                );
                // Not the pictures of places too small to see: they are not fetched.
                if !rect.intersects(area) || rect.width() < 10. {
                    continue;
                }
                match self.floor_images.get(floor.id, &floor.url) {
                    Some(texture) => {
                        painter.image(
                            texture.id(),
                            rect,
                            Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        painter.rect_stroke(
                            rect,
                            CornerRadius::ZERO,
                            Stroke::new(1.0_f32, Color32::from_white_alpha(60)),
                            StrokeKind::Inside,
                        );
                    }
                }
            }
        }

        // The buttons of the levels, at the top right: 0 farthest right.
        let level_buttons: Vec<(Rect, usize)> = (0..=max_level.min(12))
            .map(|number| {
                let left = area.right() - 10. - 32. - number as f32 * 36.;
                (Rect::from_min_size(pos2(left, area.top() + 10.), vec2(32., 32.)), number)
            })
            .collect();

        // The markers of the kinds that are chosen (the chests are below).
        let now = markers::unix_now();
        let dots = self.scale < 0.07;
        // In the route mode only what is chosen to farm (and the teleports) is shown.
        let route_on = self.route.active;
        let enabled: BTreeSet<u32> = if route_on {
            let mut set = self.route.labels.clone();
            if self.route.teleports {
                set.extend(TELEPORTS);
            }
            if !self.route.domains.is_empty() {
                set.insert(markers::DOMAIN_LABEL);
            }
            set
        } else {
            self.enabled.clone()
        };
        for &label_id in &enabled {
            // The chests of the surface of the world are drawn below, as they are
            // followed; all others are markers like the rest.
            if tracked && markers::CHEST_LABELS.contains(&label_id) {
                continue;
            }
            let Some(label) = data.label(label_id) else {
                continue;
            };
            let only_domains = if route_on {
                (!self.route.labels.contains(&markers::DOMAIN_LABEL)).then(|| self.route.domains.clone())
            } else {
                self.only_domains.clone()
            }
            .filter(|_| label_id == markers::DOMAIN_LABEL);
            let mut points = data
                .points_on(label_id, self.map, &self.shown_floors)
                .filter(|point| only_domains.as_ref().is_none_or(|set| set.contains(&point.id)))
                .peekable();
            if points.peek().is_none() {
                continue;
            }
            let texture = if dots { None } else { self.icons.get(label_id, &label.icon) };
            let color = label_color(label_id);
            // The chests of the maps that are not followed are drawn as the others are: as chests.
            let chest_tier = markers::CHEST_LABELS.iter().position(|chest| *chest == label_id).map(|index| index as u8 + 1);
            for point in points {
                let at = to_screen(vec2(info.origin[0] + point.x, info.origin[1] + point.y));
                if !on_screen.contains(at) {
                    continue;
                }
                // What can not be marked is always as it is: never done, never hidden.
                let state = if self.markable.contains(&label_id) {
                    self.markers.state(point.id, label.refresh, now)
                } else {
                    MarkerState::Available
                };
                let done = matches!(state, MarkerState::Done { .. });
                if done && self.only_closed {
                    continue;
                }
                if let Some(tier) = chest_tier {
                    if dots {
                        painter.circle_filled(at, 2., tier_color(tier).gamma_multiply(if done { 0.35 } else { 0.8 }));
                    } else {
                        draw_chest(&painter, at, tier, if done { Look::Opened } else { Look::Closed }, 1.);
                        hoverable.push((at, Hover::Marker(*point, label, state)));
                    }
                    continue;
                }
                let strength = if done { 0.35 } else { 1. };
                if dots {
                    painter.circle_filled(at, 1.8, color.gamma_multiply(strength));
                } else {
                    match &texture {
                        Some(texture) => {
                            painter.image(
                                texture.id(),
                                Rect::from_center_size(at, vec2(20., 20.)),
                                Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                                Color32::WHITE.gamma_multiply(strength),
                            );
                        }
                        None => {
                            painter.circle_filled(at, 5., color.gamma_multiply(strength));
                            painter.circle_stroke(at, 5., Stroke::new(1.0_f32, Color32::from_black_alpha(160)));
                        }
                    }
                    if done {
                        painter.circle_stroke(at, 10., Stroke::new(1.5_f32, Color32::from_rgb(0x4a, 0xde, 0x80).gamma_multiply(0.8)));
                    }
                    hoverable.push((at, Hover::Marker(*point, label, state)));
                }
            }
        }

        // The chests of the official map that are not found: faint, below; or,
        // if the place was walked by, most likely opened.
        if let Some(matched) = self.matched.as_ref().filter(|_| tracked && !self.route.active) {
            let dots = self.scale < DOT_SCALE;
            for (index, chest) in official_chests::all().iter().enumerate() {
                if matched.found[index] || !self.tier_shown(chest.tier) {
                    continue;
                }
                let look = matched.of_official[index];
                if look == Look::Faint && !self.show_faint {
                    continue;
                }
                if look == Look::Likely && self.only_closed {
                    continue;
                }
                let at = to_screen(official_on_map(&info, chest));
                if !on_screen.contains(at) {
                    continue;
                }
                if dots {
                    let color = if look == Look::Likely { OPENED } else { tier_color(chest.tier) };
                    painter.circle_filled(at, 2., color.gamma_multiply(0.55));
                } else {
                    draw_chest(&painter, at, chest.tier, look, 1.);
                    hoverable.push((at, Hover::Faint(chest, look)));
                }
            }
        }

        // The chests found.  The opened ones below, so the ones to go to are
        // not hidden by them.
        let mut found: Vec<(&Chest, Pos2, Look)> = Vec::new();
        for (index, chest) in snapshot.chests.iter().enumerate().take(if tracked && !self.route.active { usize::MAX } else { 0 }) {
            let Some(position) = chest.position else {
                continue;
            };
            let look = self
                .matched
                .as_ref()
                .and_then(|matched| matched.of_found.get(index).copied())
                .unwrap_or(Look::Closed);
            let tier = chest.tier();
            if !(1..=TIERS as u8).contains(&tier) || !self.tier_shown(tier) {
                continue;
            }
            if self.only_closed && matches!(look, Look::Opened | Look::Likely) {
                continue;
            }
            let at = to_screen(on_map(&info, position));
            if on_screen.contains(at) {
                found.push((chest, at, look));
            }
        }
        found.sort_by_key(|(_, _, look)| std::cmp::Reverse(*look != Look::Closed));
        for (chest, at, look) in &found {
            draw_chest(&painter, *at, chest.tier(), *look, 1.);
            hoverable.push((*at, Hover::Found(chest, *look)));
        }

        // The route, over the markers.
        crate::route_ui::draw(&self.route, &painter, area, &to_screen, origin, &self.shown_floors, self.map);

        // The buttons of the levels.
        let pointer_now = ui.ctx().input(|i| i.pointer.hover_pos());
        for (rect, number) in &level_buttons {
            let lit = pointer_now.is_some_and(|pointer| rect.contains(pointer));
            let current = *number == level;
            let amber = Color32::from_rgb(0xff, 0xc2, 0x4a);
            painter.rect_filled(
                *rect,
                CornerRadius::same(7),
                if current { amber } else if lit { Color32::from_rgb(0x2c, 0x3a, 0x54) } else { Color32::from_rgb(0x1c, 0x26, 0x38) },
            );
            painter.rect_stroke(*rect, CornerRadius::same(7), Stroke::new(1.5_f32, amber), StrokeKind::Inside);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                number.to_string(),
                egui::FontId::proportional(16.),
                if current { Color32::BLACK } else { Color32::WHITE },
            );
            if lit {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.text(
                    rect.center_bottom() + vec2(0., 6.),
                    egui::Align2::CENTER_TOP,
                    if *number == 0 {
                        tr("Surface", "Поверхность").to_owned()
                    } else {
                        format!("{} {number}", tr("Underground, level", "Подземелье, уровень"))
                    },
                    egui::FontId::proportional(13.),
                    Color32::WHITE,
                );
            }
        }

        // The player.
        if let Some(player) = player
            && player.map == self.map
            && level == 0
        {
            let at = to_screen(origin + vec2(player.at[0], player.at[1]));
            if self.show_range {
                // Where what is followed is looked at.
                let radius = player.range * player.scale * self.scale;
                painter.circle_stroke(at, radius, Stroke::new(2.0_f32, Color32::from_rgba_unmultiplied(0x20, 0xc0, 0xff, 200)));
            }
            if on_screen.contains(at) {
                draw_player(&painter, at, player.facing());
            }
        }
        painter.rect_stroke(
            area,
            CornerRadius::same(6),
            Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
            StrokeKind::Inside,
        );

        // What is under the pointer: a marker found before one that is not.
        let mut clicked_marker: Option<u32> = None;
        let mut refused: Option<String> = None;
        let mut stood_by: Option<Point> = None;
        let clicked_at = if response.clicked() { response.interact_pointer_pos() } else { None };
        if let Some(pointer) = response.hover_pos() {
            let nearest = hoverable
                .iter()
                .rev()
                .filter(|(at, _)| at.distance(pointer) <= 11.)
                .min_by(|a, b| {
                    a.0.distance(pointer)
                        .partial_cmp(&b.0.distance(pointer))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some((_, what)) = nearest {
                // Shift and a click on a marker: the player stands by it.
                let shift = ui.input(|input| input.modifiers.shift);
                if response.clicked()
                    && shift
                    && let Hover::Marker(point, ..) = what
                {
                    stood_by = Some(*point);
                } else if response.clicked()
                    && let Hover::Marker(point, label, _) = what
                {
                    // A domain tells what it gives; of the others only the kinds the user
                    // chose (see "Marks") are marked.
                    if markers::domain_described(point.id) {
                        self.domain_popup = Some(point.id);
                    } else if self.markable.contains(&label.id) {
                        clicked_marker = Some(point.id);
                    } else {
                        refused = Some(tr(
                            "Marking of this kind is off: turn it on with the button \"Marks\" in the filters.",
                            "Отметка этого вида выключена: включите её в фильтрах, кнопка «Отметки».",
                        ).to_owned());
                    }
                }
                response.on_hover_ui_at_pointer(|ui| match what {
                    Hover::Found(chest, look) => chest_tooltip(ui, chest, *look, russian),
                    Hover::Faint(chest, look) => {
                        ui.label(RichText::new(tier_name(chest.tier, russian)).strong().size(15.));
                        ui.label(if *look == Look::Likely {
                            tr(
                                "Most likely opened: the place was walked by, and the game did not send it",
                                "Вероятно открыт: место пройдено, а игра его не прислала",
                            )
                        } else {
                            tr("Not found yet", "Ещё не найден")
                        });
                    }
                    Hover::Marker(point, label, state) => {
                        ui.label(RichText::new(label.name(russian)).strong().size(15.));
                        match state {
                            MarkerState::Available => {
                                if markers::domain_described(point.id) {
                                    ui.label(tr("Click: what it gives and when.", "Клик: что здесь фармить и когда."));
                                } else if self.markable.contains(&label.id) {
                                    ui.label(tr("Not collected. Click to mark it.", "Не собрано. Клик — отметить."));
                                }
                            }
                            MarkerState::Done { at, back } => {
                                ui.label(
                                    RichText::new(format!("{} {}", tr("Collected", "Собрано"), clock(*at)))
                                        .color(Color32::from_rgb(0x4a, 0xde, 0x80)),
                                );
                                if let Some(back) = back {
                                    ui.label(format!("{} {}", tr("Comes back", "Вернётся"), clock(*back)));
                                }
                                ui.label(RichText::new(tr("Click to take the mark off.", "Клик — снять отметку.")).size(12.).color(DIM));
                            }
                        }
                        if player.is_some_and(|player| player.scene != 3) {
                            ui.label(
                                RichText::new(tr(
                                    "Shift + click: I am standing here (sets where this place lies on the map)",
                                    "Shift + клик: я стою здесь (задаёт, где это место лежит на карте)",
                                ))
                                .size(12.)
                                .color(DIM),
                            );
                        }
                        ui.label(RichText::new(format!("#{}", point.id)).size(11.).color(DIM));
                    }
                });
            }
        }
        if let Some(at) = clicked_at
            && let Some((_, number)) = level_buttons.iter().find(|(rect, _)| rect.contains(at))
        {
            self.level = *number;
            clicked_marker = None;
        }
        if let Some(point) = clicked_marker {
            self.markers.toggle(point, now);
            self.done_counts = None;
        }
        if let Some(text) = refused {
            self.notice = Some((text, std::time::Instant::now()));
        }
        if let Some(point) = stood_by {
            self.notice = Some((
                match player {
                    Some(player) if player.scene != 3 => {
                        crate::scenes::calibrate(player.scene, self.map, [point.x, point.y], player.position);
                        tr(
                            "Done: the place you are in is put on the map by this marker. It takes a few seconds.",
                            "Готово: место, где вы находитесь, привязано к карте по этой метке. Займёт несколько секунд.",
                        )
                    }
                    Some(_) => tr(
                        "The world is placed exactly already.",
                        "Основной мир уже привязан точно.",
                    ),
                    None => tr(
                        "Where you are is not known yet: go into the game first.",
                        "Где вы находитесь, пока неизвестно: сначала зайдите в игру.",
                    ),
                }
                .to_owned(),
                std::time::Instant::now(),
            ));
        }
        if let Some((text, at)) = &self.notice
            && at.elapsed() < std::time::Duration::from_secs(8)
        {
            painter.text(
                pos2(area.center().x, area.top() + 24.),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(15.),
                Color32::from_rgb(0xff, 0xc2, 0x4a),
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }

        let (loaded, missing) = self.tiles.counts();
        if loaded == 0 && missing > 0 {
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                tr(
                    "The map could not be loaded. Check the connection.",
                    "Карту не удалось загрузить. Проверьте подключение.",
                ),
                egui::FontId::proportional(16.),
                Color32::from_white_alpha(170),
            );
        }
    }

    /// The tiles in view: the coarsest level first, always, so there is
    /// something while the finer ones come; the one that fits the zoom over it.
    fn draw_tiles(&self, painter: &egui::Painter, area: Rect, info: &MapInfo) {
        let wanted = (-self.scale.log2()).floor().clamp(0., info.max_level as f32) as u32;
        let mut levels = vec![info.max_level];
        if wanted < info.max_level {
            levels.push(wanted);
        }
        for level in levels {
            let span = TILE * 2f32.powi(level as i32);
            let min = self.center + (area.min - area.center()) / self.scale;
            let max = self.center + (area.max - area.center()) / self.scale;
            let (columns, rows) = grid(info, level);
            let first_x = ((min.x / span).floor() as i32).max(0);
            let last_x = ((max.x / span).floor() as i32).min(columns as i32 - 1);
            let first_y = ((min.y / span).floor() as i32).max(0);
            let last_y = ((max.y / span).floor() as i32).min(rows as i32 - 1);
            for y in first_y..=last_y {
                for x in first_x..=last_x {
                    let key = TileKey {
                        level,
                        x: x as u32,
                        y: y as u32,
                    };
                    let Some(texture) = self.tiles.get(key) else {
                        continue;
                    };
                    let top_left = area.center() + (vec2(x as f32, y as f32) * span - self.center) * self.scale;
                    // A little over, so the seams between tiles do not show.
                    let tile = Rect::from_min_size(top_left, Vec2::splat(span * self.scale))
                        .expand(0.5);
                    painter.image(
                        texture.id(),
                        tile,
                        Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                        Color32::WHITE,
                    );
                }
            }
        }
    }

    /// The button of the map's settings, and what opens under it: fetching the
    /// tiles of the map in the background.
    /// The button that switches the route mode on and off.
    fn route_button(&mut self, ui: &mut Ui, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let button = ui
            .add(egui::Button::new(RichText::new(format!("{} {}", egui_material_icons::icons::ICON_ROUTE, tr("Route", "Маршрут"))).size(15.)).selected(self.route.active))
            .on_hover_text(tr("Plan a route over the places to farm", "Построить маршрут по местам фарма"));
        if button.clicked() {
            self.route.active = !self.route.active;
            if self.route.active {
                self.route.resolve(&markers::data());
            }
        }
    }

    /// Does what the route panel asked.
    fn route_action(&mut self, ctx: &Context, action: crate::route_ui::Action, player: Option<crate::PlayerPlace>, russian: bool) {
        use crate::route_ui::Action;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        match action {
            Action::None => {}
            Action::Focus(rect) => {
                self.pending_view = Some(rect);
            }
            Action::Generate => {
                let data = markers::data();
                self.route.resolve(&data);
                let now = markers::unix_now();
                let stops = crate::route_ui::targets(&self.route, &data, self.map, &|point, label| {
                    self.markable.contains(&label.id) && matches!(self.markers.state(point.id, label.refresh, now), MarkerState::Done { .. })
                });
                if stops.is_empty() {
                    self.route.message = tr("There is nothing to visit on this map for this choice.", "На этой карте для такого выбора нет точек.").to_owned();
                    return;
                }
                if stops.len() > crate::route::MAX_STOPS {
                    self.route.message = format!(
                        "{} {} ({} {})",
                        tr("Too many places:", "Слишком много точек:"),
                        stops.len(),
                        tr("at most", "не больше"),
                        crate::route::MAX_STOPS
                    );
                    return;
                }
                let waypoints = if self.route.teleports { crate::route_ui::waypoints(&data, self.map, &TELEPORTS) } else { Vec::new() };
                let start = player
                    .filter(|player| player.map != 0 && player.map == self.map)
                    .map(|player| crate::route::Spot { id: 0, label: 0, at: player.at, floor: player.floor as u16 });
                self.route.start(ctx, self.map, stops, waypoints, start);
            }
        }
    }

    fn settings_button(&mut self, ui: &mut Ui, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let button = ui.button(
            RichText::new(format!(
                "{} {}",
                egui_material_icons::icons::ICON_SETTINGS,
                tr("Map settings", "Настройки карты")
            ))
            .size(15.),
        );
        if button.clicked() {
            self.settings_open = !self.settings_open;
        }
    }

    /// Brings the chests of the markers to the state the game has them in: what it
    /// says is opened is collected, and what it says is closed is not.
    fn apply_chest_marks(&mut self, marks: &crate::chest_watch::ChestMarks) {
        if marks.version == 0 || self.chest_marks_applied == Some((marks.uid, marks.version)) || marks.uid != self.markers.account() {
            return;
        }
        self.chest_marks_applied = Some((marks.uid, marks.version));
        for (id, at) in &marks.opened {
            // Collected anew if the game says it was done after what is marked.
            if self.markers.collected_at(*id).is_none_or(|marked| marked < *at) {
                self.markers.collect(*id, *at);
            }
        }
        for id in &marks.closed {
            self.markers.uncollect(*id);
        }
        self.done_counts = None;
    }

    /// The button that shows where the player is: the map they are on, the floor,
    /// and the place in the middle.  It works while the game is running and says where
    /// the player is.
    fn locate_button(&mut self, ui: &mut Ui, player: Option<crate::PlayerPlace>, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        self.last_player = player;
        let known = player.is_some_and(|player| player.map != 0);
        let response = ui
            .add_enabled(
                known,
                egui::Button::new(
                    RichText::new(format!("{} {}", egui_material_icons::icons::ICON_MY_LOCATION, tr("Where am I", "Где я"))).size(15.),
                ),
            )
            .on_hover_text(if known {
                tr("Show the place of the player in the game now", "Показать, где игрок в игре сейчас")
            } else if player.is_some() {
                tr(
                    "The game says where the player is, but this place is not on a map the program has yet",
                    "Игра сообщает, где игрок, но этого места пока нет ни на одной карте программы",
                )
            } else {
                tr(
                    "Works when the game is running and the program gets its data (go into the game)",
                    "Работает, когда игра запущена и программа получает её данные (зайдите в игру)",
                )
            });
        if response.clicked()
            && let Some(player) = player
        {
            self.go_to_player(player);
        }
    }

    /// Puts the map, the floor and the place of the player in view.
    fn go_to_player(&mut self, player: crate::PlayerPlace) {
        let data = markers::data();
        if player.map == 0 || data.atlas.map(player.map).is_none() {
            return;
        }
        if player.map != self.map {
            self.switch_map(player.map, &data);
        }
        // The level: the number of the floor among those of its place.
        self.level = if player.floor == 0 {
            0
        } else {
            data.atlas
                .map(player.map)
                .and_then(|def| def.site_of_floor(player.floor))
                .and_then(|site| site.floors.iter().position(|floor| floor.id == player.floor))
                .map_or(0, |index| index + 1)
        };
        let at = player.at;
        self.pending_view = Some([at[0] - 160., at[1] - 160., at[0] + 160., at[1] + 160.]);
        self.placed = true;
    }

    /// The settings of the map, in a window of their own: as wide as the app's
    /// window lets, in as many columns as fit, and what is taller than the window
    /// scrolls, so that it is always all in it.
    /// The window where the user chooses which kinds of marker can be marked as collected.
    fn marking_window(&mut self, ctx: &Context, russian: bool) {
        if !self.marking_open {
            return;
        }
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = markers::data();
        let mut changed = false;
        let modal = egui::Modal::new(egui::Id::new("marking_window")).show(ctx, |ui| {
            ui.set_width(400.);
            ui.label(RichText::new(tr("What can be marked", "Что можно отмечать")).size(20.).strong());
            ui.label(
                RichText::new(tr(
                    "A marked kind gets a click that marks a marker as collected (it is greyed out, or hidden with \"Only not collected\"). The others are always shown. Only what is not coming back is here.",
                    "Для выбранного вида клик по метке отмечает её собранной (она тускнеет или скрывается при «Только несобранные»). Остальные всегда показаны. Здесь только то, что не появляется снова.",
                ))
                .size(12.)
                .color(DIM),
            );
            ui.add_space(4.);
            ui.add(
                egui::TextEdit::singleline(&mut self.marking_search)
                    .hint_text(tr("Search", "Поиск"))
                    .desired_width(ui.available_width()),
            );
            ui.horizontal(|ui| {
                if ui.button(tr("Defaults", "По умолчанию")).clicked() {
                    self.markable = default_markable(&data);
                    changed = true;
                }
                if ui.button(tr("None", "Ни одной")).clicked() {
                    self.markable.clear();
                    changed = true;
                }
            });
            let needle = self.marking_search.trim().to_lowercase();
            egui::ScrollArea::vertical().max_height(420.).auto_shrink([false, true]).show(ui, |ui| {
                for group in data.groups() {
                    let kinds: Vec<&markers::Label> = data
                        .children(group.id)
                        .filter(|label| label.refresh == 0 && !label.resin && data.points_of(label.id).iter().next().is_some())
                        .filter(|label| needle.is_empty() || label.en.to_lowercase().contains(&needle) || label.ru.to_lowercase().contains(&needle))
                        .collect();
                    if kinds.is_empty() {
                        continue;
                    }
                    let on = kinds.iter().filter(|label| self.markable.contains(&label.id)).count();
                    let mut header = egui::CollapsingHeader::new(RichText::new(format!("{}  ({on}/{})", group.name(russian), kinds.len())).strong())
                        .id_salt(("marking_group", group.id));
                    if !needle.is_empty() {
                        header = header.open(Some(true));
                    }
                    header.show(ui, |ui| {
                        for label in kinds {
                            let mut marked = self.markable.contains(&label.id);
                            if ui.checkbox(&mut marked, label.name(russian)).changed() {
                                if marked {
                                    self.markable.insert(label.id);
                                } else {
                                    self.markable.remove(&label.id);
                                }
                                changed = true;
                            }
                        }
                    });
                }
            });
        });
        if changed {
            self.done_counts = None;
            self.save_filters();
        }
        if modal.should_close() {
            self.marking_open = false;
        }
    }

    /// The description of a domain, opened by a click on it: what it gives (as pictures) and on
    /// which days.  Closed by Escape or by a press outside the window, like every popup.
    fn domain_window(&mut self, ctx: &Context, russian: bool) {
        let Some(point) = self.domain_popup else {
            return;
        };
        let artifacts = markers::artifact_domain(point);
        let daily = markers::domain_info(point);
        let story = markers::story_domain(point);
        if artifacts.is_none() && daily.is_none() && story.is_none() {
            self.domain_popup = None;
            return;
        }
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        const RU: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];
        const EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let names = if russian { RU } else { EN };
        let today = markers::game_weekday();
        let upgrade = crate::upgrade_store::data();
        let live = crate::live_data::current();
        self.item_icons.poll();
        let icons = &self.item_icons;
        let green = Color32::from_rgb(0x4a, 0xde, 0x80);
        let modal = egui::Modal::new(egui::Id::new("domain_window")).show(ctx, |ui| {
            ui.set_width(460.);
            if let Some(story) = story {
                let trounce = story.kind == "trounce";
                ui.label(
                    RichText::new(if trounce {
                        tr("Trounce domain (weekly boss)", "Подземелье наказания (недельный босс)")
                    } else {
                        tr("One-time domain", "Разовое подземелье")
                    })
                    .size(20.)
                    .strong(),
                );
                ui.label(RichText::new(if russian { &story.ru } else { &story.en }).size(15.).strong());
                if trounce {
                    if !story.boss.is_empty() {
                        ui.label(RichText::new(&story.boss).size(13.).color(DIM));
                    }
                    ui.add_space(4.);
                    ui.label(
                        RichText::new(tr(
                            "Once a week for resin: the materials below, a gem and a billet. First clear: one of each character level-up material.",
                            "Раз в неделю за смолу: материалы ниже, самоцвет и заготовка. За первое прохождение: по одному материалу возвышения персонажа каждого вида.",
                        ))
                        .size(12.)
                        .color(DIM),
                    );
                } else {
                    ui.label(
                        RichText::new(tr(
                            "Rewards of the first completion only; later it gives nothing.",
                            "Награда только за первое прохождение; дальше ничего не даёт.",
                        ))
                        .size(12.)
                        .color(DIM),
                    );
                }
                ui.add_space(6.);
                let list = if trounce { &story.drops } else { &story.rewards };
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6., 6.);
                    for reward in list {
                        let cell = crate::roster_tab::Cell {
                            icon: &reward.icon,
                            rarity: reward.rank.max(1),
                            stars: false,
                            label: (reward.qty > 1).then(|| (reward.qty.to_string(), Color32::WHITE)),
                            selected: false,
                        };
                        crate::roster_tab::item_cell(ui, icons, 52., &cell)
                            .on_hover_text(if russian { &reward.ru } else { &reward.en });
                    }
                });
                if list.is_empty() {
                    ui.label(RichText::new(tr("No data on the rewards.", "Нет данных о наградах.")).color(DIM));
                }
                ui.add_space(6.);
            }
            if let Some(sets) = artifacts {
                ui.label(RichText::new(tr("Artifact domain", "Подземелье артефактов")).size(20.).strong());
                ui.label(RichText::new(tr("Artifact sets it gives:", "Какие наборы артефактов здесь выпадают:")).size(12.).color(DIM));
                ui.add_space(6.);
                for set in sets {
                    ui.horizontal(|ui| {
                        let cell = crate::roster_tab::Cell { icon: &set.icon, rarity: 5, stars: false, label: None, selected: false };
                        crate::roster_tab::item_cell(ui, icons, 52., &cell);
                        ui.label(RichText::new(if russian { &set.ru } else { &set.en }).size(15.).strong());
                    });
                }
            }
            if let Some((talents, city, days)) = &daily {
                let nation = match city {
                    1 => tr("Mondstadt", "Мондштадт"),
                    2 => tr("Liyue", "Ли Юэ"),
                    3 => tr("Inazuma", "Инадзума"),
                    4 => tr("Sumeru", "Сумеру"),
                    5 => tr("Fontaine", "Фонтейн"),
                    6 => tr("Natlan", "Натлан"),
                    7 => tr("Nod-Krai", "Нод-Край"),
                    _ => tr("Snezhnaya", "Снежная"),
                };
                ui.label(
                    RichText::new(if *talents {
                        tr("Domain of talent books", "Подземелье книг талантов")
                    } else {
                        tr("Domain of weapon materials", "Подземелье материалов оружия")
                    })
                    .size(20.)
                    .strong(),
                );
                ui.label(RichText::new(nation).size(15.));
                ui.label(
                    RichText::new(tr(
                        "Which material is got changes with the day of the week (the day changes at 4:00); on Sunday all of them are open.",
                        "Какой материал падает, зависит от дня недели (день меняется в 4:00); в воскресенье открыты все.",
                    ))
                    .size(12.)
                    .color(DIM),
                );
                ui.add_space(6.);
                for day in days {
                    let open_today = day.days.contains(&today);
                    let list = day.days.iter().filter_map(|d| names.get(*d as usize)).copied().collect::<Vec<_>>().join(", ");
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(list).strong().color(if open_today { green } else { DIM }));
                        ui.label(RichText::new(if russian { &day.ru } else { &day.en }).strong());
                        if open_today {
                            ui.label(RichText::new(tr("(today)", "(сегодня)")).color(green));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6., 6.);
                        for id in &day.items {
                            let Some(material) = upgrade.materials.get(id).or_else(|| live.materials.get(id)) else {
                                continue;
                            };
                            let cell = crate::roster_tab::Cell {
                                icon: &material.icon,
                                rarity: material.rarity.max(1),
                                stars: false,
                                label: None,
                                selected: false,
                            };
                            crate::roster_tab::item_cell(ui, icons, 52., &cell).on_hover_text(material.name.get(russian));
                        }
                    });
                    ui.add_space(6.);
                }
            }
        });
        if modal.should_close() {
            self.domain_popup = None;
        }
    }

    fn settings_window(&mut self, ctx: &Context, snapshot: &Snapshot, russian: bool) {
        if !self.settings_open {
            return;
        }
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let screen = ctx.screen_rect();
        let width = (screen.width() - 64.).max(320.);
        let columns = settings_columns(width);
        let gap = 18.;
        let column = ((width - gap * (columns as f32 - 1.)) / columns as f32).clamp(300., 440.);
        let inner = columns as f32 * column + gap * (columns as f32 - 1.);
        // What goes in each column: the tiles, the map over the game, the minimap.
        let groups: Vec<Vec<u8>> = match columns {
            3 => vec![vec![0], vec![1], vec![2]],
            2 => vec![vec![0], vec![1, 2]],
            _ => vec![vec![0, 1, 2]],
        };
        let modal = egui::Modal::new(egui::Id::new("map_settings_modal")).show(ctx, |ui| {
                ui.label(RichText::new(tr("Map settings", "Настройки карты")).size(20.).strong());
                ui.add_space(4.);
                egui::ScrollArea::vertical()
                    .max_height((screen.height() - 190.).max(240.))
                    .auto_shrink([true, true])
                    .show(ui, |ui| {
                        ui.set_max_width(inner + 16.);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for group in &groups {
                                ui.vertical(|ui| {
                                    ui.set_width(column);
                                    for (index, section) in group.iter().enumerate() {
                                        if index > 0 {
                                            ui.add_space(8.);
                                            ui.separator();
                                        }
                                        match section {
                                            0 => self.settings_ui(ui, snapshot, russian),
                                            1 => {
                                                self.overlay.settings_ui(ui, ctx, russian);
                                                // What was pinned by hand for the place the player is in.
                                                if let Some(player) = self.last_player.filter(|player| player.scene != 3)
                                                    && ui
                                                        .button(tr(
                                                            "Take back what was pinned for this place",
                                                            "Сбросить ручную привязку этого места",
                                                        ))
                                                        .on_hover_text(tr(
                                                            "Shift + clicks on markers say where the player stands; this takes them back.",
                                                            "Shift + клики по меткам говорят, где стоит игрок; это их отменяет.",
                                                        ))
                                                        .clicked()
                                                {
                                                    crate::scenes::reset(player.scene);
                                                }
                                            }
                                            _ => self.overlay.minimap_ui(ui, ctx, russian),
                                        }
                                    }
                                });
                            }
                        });
                    });
            });
        if modal.should_close() {
            self.settings_open = false;
        }
    }

    fn settings_ui(&mut self, ui: &mut Ui, snapshot: &Snapshot, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = markers::data();
        ui.label(RichText::new(tr("Fetch the tiles of the maps", "Скачать тайлы карт")).size(18.).strong());
        ui.label(
            RichText::new(tr(
                "Each map (Teyvat, Enkanomiya, ...) has tiles of its own, at five zoom levels: the \
                 closer the zoom, the more tiles. They are fetched as you look at a map anyway, and \
                 kept on the disk; here you can fetch them ahead, so a map shows at once, even \
                 without a connection. Only what is missing is fetched.",
                "У каждой карты (Тейват, Энканомия, ...) свои тайлы, в пяти масштабах: чем ближе \
                 масштаб, тем их больше. Они и так подгружаются при просмотре и хранятся на диске; \
                 здесь их можно скачать заранее, чтобы карта открывалась сразу, даже без интернета. \
                 Скачивается только то, чего ещё нет.",
            ))
            .size(12.)
            .color(DIM),
        );
        ui.add_space(6.);

        let running = self.tiles.progress().running.load(Ordering::Relaxed);
        let infos: Vec<(MapInfo, String)> = data
            .atlas
            .maps
            .iter()
            .map(|def| {
                let info = if def.id == self.map { self.tiles.info() } else { def.info() };
                (info, def.name(russian).to_owned())
            })
            .collect();

        // What is on the disk, for each map, looked at again now and then.
        let looked = self
            .on_disk
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < std::time::Duration::from_secs(4));
        if !looked {
            let found = infos.iter().map(|(info, _)| (info.map, MapTiles::on_disk_of(&info.version))).collect();
            self.on_disk = Some((found, std::time::Instant::now()));
        }
        let on_disk = self.on_disk.as_ref().map(|(found, _)| found.clone()).unwrap_or_default();
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(4));

        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Zoom levels:", "Масштабы:")).size(14.));
                for level in (0..5usize).rev() {
                    let name = match level {
                        4 => tr("farthest", "самый дальний"),
                        3 => tr("far", "дальний"),
                        2 => tr("medium", "средний"),
                        1 => tr("near", "близкий"),
                        _ => tr("closest, full size", "самый близкий, полный размер"),
                    };
                    ui.checkbox(&mut self.levels[level], level.to_string()).on_hover_text(name);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut self.scope, Scope::Maps, tr("Whole maps", "Карты целиком"));
                ui.radio_value(&mut self.scope, Scope::Region, tr("The region chosen", "Выбранный регион"))
                    .on_hover_text(tr(
                        "The place of the map that the Region list was last set to.",
                        "Часть карты, к которой в списке «Регион» перешли последний раз.",
                    ));
                ui.radio_value(&mut self.scope, Scope::Chests, tr("Around the chests found", "Вокруг найденных сундуков"))
                    .on_hover_text(tr("Teyvat only.", "Только Тейват."));
            });
        });
        let levels: Vec<u32> = (0..5u32).filter(|level| self.levels[*level as usize]).collect();

        // What each scope comes to, for each map.
        struct Row {
            index: usize,
            total: usize,
            have: usize,
            /// The tiles, when they are not all of the levels of the map.
            keys: Option<Vec<TileKey>>,
        }
        if self.scope == Scope::Chests {
            let key = (snapshot.version, self.levels);
            if self.plan.as_ref().is_none_or(|(planned, _)| *planned != key)
                && let Some((world, _)) = infos.iter().find(|(info, _)| info.map == WORLD)
            {
                let places: Vec<[f32; 2]> = snapshot
                    .chests
                    .iter()
                    .filter_map(|chest| chest.position)
                    .map(|position| {
                        let place = on_map(world, position);
                        [place.x, place.y]
                    })
                    .collect();
                self.plan = Some((key, keys_around(world, &levels, &places, AROUND)));
            }
        }
        let mut rows: Vec<Row> = Vec::new();
        for (index, (info, _)) in infos.iter().enumerate() {
            let there = on_disk.get(&info.map);
            let counted = |keys: &[TileKey]| keys.iter().filter(|key| there.is_some_and(|set| set.contains(key))).count();
            let row = match self.scope {
                Scope::Maps => {
                    let total: usize = levels
                        .iter()
                        .map(|level| {
                            let (columns, rows) = grid(info, *level);
                            (columns * rows) as usize
                        })
                        .sum();
                    let have = there.map_or(0, |set| set.iter().filter(|key| levels.contains(&key.level)).count());
                    Row { index, total, have: have.min(total), keys: None }
                }
                Scope::Chests if info.map == WORLD => {
                    let keys = self.plan.as_ref().map(|(_, keys)| keys.clone()).unwrap_or_default();
                    Row { index, total: keys.len(), have: counted(&keys), keys: Some(keys) }
                }
                Scope::Region if info.map == self.map && self.region_rect.is_some() => {
                    let keys = keys_in(info, &levels, self.region_rect.unwrap_or_default());
                    Row { index, total: keys.len(), have: counted(&keys), keys: Some(keys) }
                }
                _ => Row { index, total: 0, have: 0, keys: Some(Vec::new()) },
            };
            rows.push(row);
        }
        if self.scope == Scope::Region && self.region_rect.is_none() {
            ui.label(
                RichText::new(tr(
                    "No region is chosen: pick one in the Region list above the map.",
                    "Регион не выбран: выберите его в списке «Регион» над картой.",
                ))
                .size(12.)
                .color(DIM),
            );
        }

        ui.add_space(4.);
        let mut total_missing = 0;
        let mut total_mb = 0.;
        ui.add_enabled_ui(!running, |ui| {
            for row in &rows {
                let (info, name) = &infos[row.index];
                let missing = row.total - row.have;
                let mb: f32 = levels
                    .iter()
                    .map(|level| {
                        let share = if row.total == 0 { 0. } else { missing as f32 / row.total as f32 };
                        // About: the share still missing of the tiles of that level.
                        let count = match &row.keys {
                            Some(keys) => keys.iter().filter(|key| key.level == *level).count(),
                            None => {
                                let (columns, rows) = grid(info, *level);
                                (columns * rows) as usize
                            }
                        };
                        estimate_level_mb(*level, count) * share
                    })
                    .sum();
                let text = if row.total == 0 {
                    "—".to_owned()
                } else if missing == 0 {
                    format!("{} / {} · {}", row.have, row.total, tr("all on the disk", "всё на диске"))
                } else {
                    format!("{} / {} · {} {} ≈ {}", row.have, row.total, missing, tr("to fetch", "скачать"), size_text(mb))
                };
                ui.horizontal(|ui| {
                    // Only whole maps are chosen one by one; the others are for the map
                    // that they are for.
                    let mut included = row.total > 0
                        && (self.scope != Scope::Maps || self.download_maps.contains(&info.map));
                    if self.scope == Scope::Maps {
                        if ui.checkbox(&mut included, RichText::new(name.as_str()).size(14.)).changed() {
                            if included {
                                self.download_maps.insert(info.map);
                            } else {
                                self.download_maps.remove(&info.map);
                            }
                        }
                    } else {
                        ui.label(RichText::new(name.as_str()).size(14.));
                    }
                    ui.label(RichText::new(text).size(13.).color(DIM));
                    if included {
                        total_missing += missing;
                        total_mb += mb;
                    }
                });
            }
        });
        ui.add_space(6.);

        let progress = self.tiles.progress().clone();
        ui.label(format!(
            "{}: {} {} ≈ {}",
            tr("Still to fetch", "Осталось скачать"),
            total_missing,
            tr("tiles", "плиток"),
            size_text(total_mb)
        ));
        ui.checkbox(
            &mut self.during_game,
            RichText::new(tr("Also while the game is running", "Также пока запущена игра")).size(14.),
        )
        .on_hover_text(tr(
            "By default the fetching waits for the game to end, so that it does not \
             compete with it for the connection.",
            "По умолчанию скачивание ждёт, пока игра закроется, чтобы не мешать ей \
             своим трафиком.",
        ));
        ui.horizontal(|ui| {
            if running {
                if ui.button(tr("Stop", "Остановить")).clicked() {
                    self.tiles.stop_download();
                }
                if progress.paused.load(Ordering::Relaxed) {
                    ui.label(
                        RichText::new(tr("Waiting for the game to end", "Ждёт, пока игра закроется"))
                            .size(13.)
                            .color(DIM),
                    );
                }
            } else if ui
                .add_enabled(total_missing > 0, egui::Button::new(tr("Fetch in the background", "Скачать в фоне")))
                .clicked()
            {
                let mut jobs: Vec<(MapInfo, Vec<TileKey>)> = Vec::new();
                let mut names: Vec<String> = Vec::new();
                for row in &mut rows {
                    let (info, name) = &infos[row.index];
                    let included = row.total > 0 && (self.scope != Scope::Maps || self.download_maps.contains(&info.map));
                    if !included {
                        continue;
                    }
                    let wanted = row.keys.take().unwrap_or_else(|| all_keys(info, &levels));
                    // What is on the disk is not fetched again.
                    let there = MapTiles::on_disk_of(&info.version);
                    let keys: Vec<TileKey> = wanted.into_iter().filter(|key| !there.contains(key)).collect();
                    if !keys.is_empty() {
                        names.push(name.clone());
                        jobs.push((info.clone(), keys));
                    }
                }
                self.downloading = names.join(", ");
                self.tiles.start_download(jobs, self.during_game);
                self.on_disk = None;
            }
        });

        let total = progress.total.load(Ordering::Relaxed);
        if total > 0 {
            let done = progress.done.load(Ordering::Relaxed);
            if !self.downloading.is_empty() {
                ui.label(
                    RichText::new(format!("{}: {}", tr("Fetching", "Скачивается"), self.downloading))
                        .size(13.)
                        .color(DIM),
                );
            }
            ui.add(
                // Not an infinite width: a window that is infinitely wide has its
                // place worked out as infinity times zero, which is no number, and
                // the next time the pointer is over it egui panics.
                egui::ProgressBar::new(done as f32 / total as f32)
                    .text(format!("{done} / {total}"))
                    .desired_width(ui.available_width()),
            );
            ui.label(
                RichText::new(format!(
                    "{}: {}   ·   {}: {}   ·   {}: {}",
                    tr("Fetched", "Скачано"),
                    size_text(progress.bytes.load(Ordering::Relaxed) as f32 / 1_048_576.),
                    tr("Already had", "Уже было"),
                    progress.had.load(Ordering::Relaxed),
                    tr("Not available", "Недоступно"),
                    progress.failed.load(Ordering::Relaxed),
                ))
                .size(12.)
                .color(DIM),
            );
            if !running && done < total {
                ui.label(
                    RichText::new(tr("Stopped. What was fetched stays.", "Остановлено. Скачанное сохранено."))
                        .size(12.)
                        .color(DIM),
                );
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Every tile of the maps that are chosen.
    Maps,
    /// The tiles of the region that was chosen on the map now shown.
    Region,
    /// The tiles around the chests found (Teyvat).
    Chests,
}

/// In how many columns the settings are laid out in a window this wide: as many
/// as fit (a column is at least about 380 wide), at most three.
fn settings_columns(width: f32) -> usize {
    if width >= 1180. {
        3
    } else if width >= 780. {
        2
    } else {
        1
    }
}

fn size_text(megabytes: f32) -> String {
    if megabytes < 1. {
        format!("{:.0} KB", megabytes * 1024.)
    } else {
        format!("{megabytes:.1} MB")
    }
}

/// A small chest: a box with a lid, in the colour of its kind, grey if it is
/// opened, only an outline if it is not found yet.
/// `fade` is how much of it is seen: 1 is all (the map over the game lets the
/// chests fade out toward its edges with the map).
pub(crate) fn draw_chest(painter: &egui::Painter, at: Pos2, tier: u8, look: Look, fade: f32) {
    let half = match tier {
        4 | 5 => 8.,
        3 => 7.5,
        _ => 7.,
    };
    let body = Rect::from_center_size(at, vec2(half * 2., half * 1.6));
    let color = tier_color(tier).gamma_multiply(fade);
    let edge = Color32::from_rgb(0x1b, 0x1d, 0x27).gamma_multiply(fade);
    match look {
        Look::Faint => {
            painter.rect_filled(
                body,
                CornerRadius::same(3),
                Color32::from_rgba_unmultiplied(0x15, 0x1a, 0x24, 150).gamma_multiply(fade),
            );
            painter.rect_stroke(
                body,
                CornerRadius::same(3),
                Stroke::new(1.2_f32, color.gamma_multiply(0.7)),
                StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    pos2(body.left(), body.top() + body.height() * 0.38),
                    pos2(body.right(), body.top() + body.height() * 0.38),
                ],
                Stroke::new(1.0_f32, color.gamma_multiply(0.7)),
            );
        }
        Look::Closed | Look::Opened | Look::Likely => {
            let (fill, alpha) = match look {
                Look::Opened => (OPENED.gamma_multiply(fade), 0.8),
                Look::Likely => (OPENED.gamma_multiply(fade), 0.5),
                _ => (color, 1.),
            };
            painter.rect_filled(body, CornerRadius::same(3), fill.gamma_multiply(alpha));
            painter.rect_stroke(
                body,
                CornerRadius::same(3),
                Stroke::new(1.5_f32, edge),
                StrokeKind::Outside,
            );
            // The lid, and the lock.
            painter.line_segment(
                [
                    pos2(body.left(), body.top() + body.height() * 0.38),
                    pos2(body.right(), body.top() + body.height() * 0.38),
                ],
                Stroke::new(1.3_f32, edge),
            );
            painter.circle_filled(pos2(at.x, body.top() + body.height() * 0.38), 1.6, edge);
        }
    }
}

/// The player: a ring with a dot.
fn draw_player(painter: &egui::Painter, at: Pos2, facing: Option<[f32; 2]>) {
    if let Some([dx, dy]) = facing {
        // An arrow that points the way the player faces.
        let (d, n) = (vec2(dx, dy), vec2(-dy, dx));
        let shape = |grow: f32| {
            vec![
                at + d * 12. * grow,
                at - d * 8. * grow + n * 8.5 * grow,
                at - d * 3. * grow,
                at - d * 8. * grow - n * 8.5 * grow,
            ]
        };
        painter.add(egui::Shape::convex_polygon(shape(1.3), Color32::WHITE, Stroke::NONE));
        painter.add(egui::Shape::convex_polygon(shape(1.), Color32::from_rgb(0x20, 0xc0, 0xff), Stroke::NONE));
        return;
    }
    painter.circle_filled(at, 9., Color32::from_rgba_unmultiplied(0x20, 0xc0, 0xff, 90));
    painter.circle_stroke(at, 9., Stroke::new(2.0_f32, Color32::WHITE));
    painter.circle_filled(at, 4., Color32::from_rgb(0x20, 0xc0, 0xff));
}

fn chest_tooltip(ui: &mut Ui, chest: &Chest, look: Look, russian: bool) {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    ui.label(RichText::new(kind_name(chest.gadget, russian)).strong().size(15.));
    if kinds().get(&chest.gadget).is_some_and(|kind| kind.locked) {
        ui.label(tr("Locked: opens after something is done", "Запертый: открывается после действия"));
    }
    match (&chest.opened_at, look) {
        (Some(at), _) => ui.label(
            RichText::new(format!("{} {}", tr("Opened", "Открыт"), short_time(at))).color(OPENED),
        ),
        (None, Look::Likely) => ui.label(
            RichText::new(tr(
                "Most likely opened: the place was walked by, and the game did not send it",
                "Вероятно открыт: место пройдено, а игра его не прислала",
            ))
            .color(OPENED),
        ),
        (None, _) => ui.label(tr("Not opened", "Не открыт")),
    };
    if let Some([x, _, z]) = chest.position {
        ui.label(
            RichText::new(format!("x {x:.0}   z {z:.0}"))
                .size(12.)
                .color(DIM),
        );
    }
}

/// `2026-11-07T23:39:03+03:00` as `2026-11-07 23:39`.
fn short_time(time: &str) -> String {
    time.chars()
        .take(16)
        .map(|c| if c == 'T' { ' ' } else { c })
        .collect()
}

/// The kind of chest (1 to 5) a label of the map is.
fn chest_tier(label: u32) -> u8 {
    markers::CHEST_LABELS
        .iter()
        .position(|chest| *chest == label)
        .map_or(0, |at| at as u8 + 1)
}

/// A kind of marker as a button: its picture (or a colour), its name, how many
/// there are.
fn chip(
    ui: &mut Ui,
    width: f32,
    selected: bool,
    picture: Option<&egui::TextureHandle>,
    color: Color32,
    name: &str,
    count: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 40.), Sense::click());
    let painter = ui.painter_at(rect);
    let fill = if selected {
        Color32::from_rgba_unmultiplied(0x2e, 0x3d, 0x7c, 230)
    } else if response.hovered() {
        Color32::from_gray(46)
    } else {
        Color32::from_gray(30)
    };
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    if selected {
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0_f32, Color32::from_rgb(0x6a, 0x8a, 0xff)),
            StrokeKind::Inside,
        );
    }
    let icon = Rect::from_center_size(pos2(rect.left() + 22., rect.center().y), vec2(28., 28.));
    let tint = if selected { Color32::WHITE } else { Color32::WHITE.gamma_multiply(0.6) };
    match picture {
        Some(texture) => {
            painter.image(texture.id(), icon, Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)), tint);
        }
        None => {
            painter.circle_filled(icon.center(), 7., color.gamma_multiply(if selected { 1. } else { 0.6 }));
        }
    }
    // The name, and under it how many there are; the name is cut if it is longer
    // than the room.
    let text_left = rect.left() + 42.;
    let room = Rect::from_min_max(pos2(text_left, rect.top()), pos2(rect.right() - 6., rect.bottom()));
    painter.with_clip_rect(room).text(
        pos2(text_left, rect.top() + 14.),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.),
        if selected { Color32::WHITE } else { Color32::from_white_alpha(190) },
    );
    painter.text(
        pos2(text_left, rect.top() + 29.),
        egui::Align2::LEFT_CENTER,
        count,
        egui::FontId::proportional(11.),
        Color32::from_white_alpha(140),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_of_chest_are_told_by_their_label() {
        assert_eq!(chest_tier(17), 1);
        assert_eq!(chest_tier(269), 5);
        assert_eq!(chest_tier(5), 0);
    }

    #[test]
    fn a_kind_of_marker_has_a_colour_of_its_own() {
        assert_ne!(label_color(5), label_color(6));
        assert_eq!(label_color(5), label_color(5));
    }

    #[test]
    fn a_chest_of_the_game_is_put_where_it_is_on_the_map() {
        let info = MapInfo::default();
        // The chest that was opened first: x 2468.0, z -1234.3.
        let place = on_map(&info, [2468.0, 206.3, -1234.3]);
        assert!((place.x - (24207. + 1234.3 - 750.)).abs() < 0.01);
        assert!((place.y - (16411. - 2468. + 2321.)).abs() < 0.01);
        // More x is further up the map.
        let more_x = on_map(&info, [2568.0, 206.3, -1234.3]);
        assert!(more_x.y < place.y);
    }

    #[test]
    fn the_chest_that_was_found_is_within_the_map() {
        let info = MapInfo::default();
        let place = on_map(&info, [2468.0, 206.3, -1234.3]);
        assert!(place.x > 0. && place.x < info.size[0]);
        assert!(place.y > 0. && place.y < info.size[1]);
    }

    #[test]
    fn the_sizes_are_told_in_the_unit_that_fits() {
        assert_eq!(size_text(0.25), "256 KB");
        assert_eq!(size_text(65.04), "65.0 MB");
    }

    #[test]
    fn the_settings_are_laid_out_in_the_columns_that_fit() {
        assert_eq!(settings_columns(1500.), 3);
        assert_eq!(settings_columns(1180.), 3);
        assert_eq!(settings_columns(1000.), 2);
        assert_eq!(settings_columns(780.), 2);
        assert_eq!(settings_columns(600.), 1);
        assert_eq!(settings_columns(320.), 1);
        // However many, they are never wider than the window (the width of a column
        // is worked out as it is in the popup).
        for width in [320., 500., 780., 1000., 1180., 1900.] {
            let columns = settings_columns(width) as f32;
            let column = ((width - 18. * (columns - 1.)) / columns).clamp(300., 440.);
            if width >= 340. {
                assert!(columns * column + 18. * (columns - 1.) <= width + 0.5, "{width}");
            }
        }
    }
}
