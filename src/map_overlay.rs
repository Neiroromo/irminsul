//! The map over the game.
//!
//! A key (Caps Lock at first) shows the map in the middle of the screen, and
//! hides it.  The map is see-through, and more so toward the edges, which fade
//! out until there is nothing there, so that what is near the middle is clear
//! and the rest of the screen is the game's.  It follows the player.  Clicks go
//! through it to the game.
//!
//! The key held while the map is shown is for the map itself: it can be moved
//! by dragging it, made bigger or smaller by dragging its edges, and zoomed with
//! the wheel.  The map can be black and white.
//!
//! Where the player is has places under or over the ground (caves, ruins,
//! towers) the map has floors of.  The map shows the floors of the level the
//! player is on (found by `levels`); while the key is held, the buttons of the
//! levels are shown, 0 (the surface) farthest right, and a click on one shows
//! that level, until the player goes to another floor.
//!
//! The map is drawn by the program in memory (`map_render`) and shown by a
//! window that takes such pictures (`overlay_window`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui::{Context, RichText, Ui, Vec2, vec2};
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::atlas::WORLD;
use crate::chests::Snapshot;
use crate::hotkey::{self, Edge};
use crate::map_render::{self, FloorPicture, Icon, IconLook, LevelsView, MarkDraw, Params};
use crate::marker_icons::MarkerIcons;
use crate::markers;
use crate::map_tab::{Look, Matched, official_on_map, on_map};
use crate::map_tiles::{MapInfo, MapTiles};
use crate::official_chests;
use crate::overlay_window::{MIN_SIZE, OverlayWindow};

/// A key held this long, while the map is shown, is for moving the map.
const HOLD: Duration = Duration::from_millis(350);
const MIN_SCALE: f32 = 0.05;
const MAX_SCALE: f32 = 2.;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MapOverlaySettings {
    pub enabled: bool,
    /// The key, as the number Windows gives it.
    pub key: u32,
    /// The size of the window, in pixels.
    pub size: [f32; 2],
    /// Where the window is on the screen; the middle if not known.
    pub pos: Option<[f32; 2]>,
    /// Pixels of the screen to a pixel of the full size map.
    pub scale: f32,
    /// How seen the middle is: 1 is all of it.
    pub opacity: f32,
    /// How far in from the edge the fading begins: 0 is none, 1 is from the
    /// middle.
    pub vignette: f32,
    /// Drawn only while the game is the window in front: not over Irminsul or
    /// any other window.
    pub only_over_game: bool,
    /// The markers are drawn over the big map of the game itself, when it is open.
    pub on_game_map: bool,
    /// How big the markers are there: 1 is the usual size.
    pub game_map_size: f32,
    /// The enemies of the kinds chosen on the map that are near the player are listed at the
    /// right of the game, and within how many metres.
    pub nearby_mobs: bool,
    pub nearby_metres: f32,
    /// Where the list is, from the top left of the window of the game; where it is at first
    /// (at the right, under what is told of the changes) if not moved.
    pub nearby_pos: Option<[i32; 2]>,
}

impl Default for MapOverlaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            key: hotkey::DEFAULT_KEY,
            size: [560., 560.],
            pos: None,
            scale: 0.45,
            opacity: 0.85,
            vignette: 0.55,
            only_over_game: false,
            on_game_map: false,
            game_map_size: 1.,
            nearby_mobs: true,
            nearby_metres: 80.,
            nearby_pos: None,
        }
    }
}

/// `IRMINSUL_MAP_OVERLAY` replaces the file, for trying things out.
fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_MAP_OVERLAY") {
        return Some(path.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("map_overlay.json"))
}

impl MapOverlaySettings {
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, bytes) {
                    tracing::warn!("unable to store the settings of the map over the game: {e}");
                }
            }
            Err(e) => tracing::warn!("unable to store the settings of the map over the game: {e}"),
        }
    }
}

/// The size of the main screen, in pixels.
#[cfg(windows)]
fn screen_size() -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    if width > 0 && height > 0 { (width, height) } else { (1920, 1080) }
}

#[cfg(not(windows))]
fn screen_size() -> (i32, i32) {
    (1920, 1080)
}

fn icon_look(look: Look) -> IconLook {
    match look {
        Look::Closed => IconLook::Closed,
        Look::Opened => IconLook::Opened,
        Look::Likely => IconLook::Likely,
        Look::Faint => IconLook::Faint,
    }
}

/// A setting that changes how the map looks was changed: the picture is made
/// again.
static LOOK_CHANGED: AtomicBool = AtomicBool::new(true);

/// The markers chosen on the map of the program, which this map shows too.
pub struct SharedMarkers<'a> {
    pub enabled: &'a std::collections::BTreeSet<u32>,
    /// The kinds that can be marked as collected: the others are never shown as collected.
    pub markable: &'a std::collections::BTreeSet<u32>,
    pub store: &'a markers::Store,
    /// Not the ones that are collected (nor the chests that are opened).
    pub hide_done: bool,
    /// The chests of the map not found yet are shown (faint).
    pub show_faint: bool,
    /// Draw the circle that is looked at around the player.
    pub show_range: bool,
    /// Changes when what is shown does.
    pub version: u64,
    /// The traffic of the game can not be read (the key of the connection was not found):
    /// nothing here is right, so nothing is shown but that.
    pub key_lost: bool,
    /// The monsters alive around the player, as the game sent them.
    pub mobs: &'a crate::mob_watch::MobsSeen,
}

/// What the picture shown was made from.
#[derive(Clone, Copy, PartialEq)]
struct Made {
    center: (i32, i32),
    scale: i32,
    size: (i32, i32),
    editing: bool,
    data: u64,
    tiles: usize,
    map: u32,
    unknown: bool,
    marks: u64,
    icons_there: usize,
    /// The level, how many there are, and how many floor pictures are there.
    level: usize,
    max_level: usize,
    floors: usize,
    /// The way the player faces (whole degrees, -1 if not told) and the circle drawn
    /// around them (metres, 0 if none).
    turn: i32,
    range: i32,
}

pub struct MapOverlay {
    pub settings: MapOverlaySettings,
    tiles: MapTiles,
    /// The window, made the first time the map is wanted.
    window: Option<OverlayWindow>,
    visible: bool,
    /// When the key went down, while it is down.
    pressed_at: Option<Instant>,
    /// The key is held while the map is shown: the map can be moved.
    editing: bool,
    /// The place in the middle of the map, on the full size map.
    center: Option<Vec2>,
    dirty: bool,
    /// A key is being chosen.
    choosing_key: bool,
    made: Option<Made>,
    /// The pictures of the floors, in memory.
    floor_images: MarkerIcons,
    /// The pictures of the kinds of marker, in memory.
    mark_images: MarkerIcons,
    /// The minimap of the game, which tells the floor the player is on.
    pub watcher: crate::minimap::Watcher,
    /// `IRMINSUL_FAKE_PLAYER` (x, height, z, floor): a player, for trying things out.
    fake_player: Option<crate::PlayerPlace>,
    /// The level the user chose, instead of the one the player is on.
    manual_level: Option<usize>,
    /// Where the player was last time (scene, floor), to see them move.
    last_place: Option<(u32, u32)>,
    /// `IRMINSUL_MAP_OVERLAY_DEBUG`: shown from the start, whatever is in front,
    /// for looking at it without the keyboard.
    debug: bool,
    /// The markers over the big map of the game itself.
    game_map: crate::game_map::GameMap,
    /// What the picture of those markers was made from (the last number: 0 for the markers,
    /// more for the sign that the place is being looked for).
    game_made: Option<(u64, u64, u64, usize, i32, u32)>,
    /// The panel of the settings of those markers is shown (the key is held over the big
    /// map of the game): where the window of the map was before, and what the panel shows.
    panel_saved: Option<[i32; 4]>,
    panel_made: Option<crate::game_map_panel::PanelState>,
    /// The key was pressed while the traffic can not be read: that is told until then.
    no_key_until: Option<Instant>,
    no_key_shown: bool,
    /// The list of the enemies near: its window, and what it shows.
    near_window: Option<crate::game_map::MarksWindow>,
    near_made: Option<(Vec<crate::game_map_panel::NearRow>, [i32; 2], bool)>,
    /// The list was being moved the last time it was looked at.
    near_moving: bool,
}

impl MapOverlay {
    pub fn new(ctx: &Context) -> Self {
        let tiles = MapTiles::new(ctx);
        tiles.keep_in_memory();
        let debug = std::env::var_os("IRMINSUL_MAP_OVERLAY_DEBUG").is_some();
        Self {
            settings: MapOverlaySettings::load(),
            tiles,
            window: None,
            visible: debug,
            pressed_at: None,
            editing: std::env::var("IRMINSUL_MAP_OVERLAY_DEBUG").is_ok_and(|value| value == "edit"),
            center: None,
            dirty: false,
            choosing_key: false,
            made: None,
            floor_images: MarkerIcons::floors_in_memory(ctx),
            mark_images: MarkerIcons::icons_in_memory(ctx),
            watcher: crate::minimap::Watcher::new(),
            fake_player: std::env::var("IRMINSUL_FAKE_PLAYER").ok().and_then(|text| {
                let n: Vec<f32> = text.split(',').filter_map(|part| part.trim().parse().ok()).collect();
                // x, height, z, floor; and, for another map, its number and the
                // place on it.
                (n.len() == 4 || n.len() == 7).then(|| {
                    let (x, y) = crate::official_chests::game_to_map([n[0], n[1], n[2]]);
                    let other = n.len() == 7;
                    crate::PlayerPlace {
                        scene: if other { 6 } else { 3 },
                        position: [n[0], n[1], n[2]],
                        floor: n[3] as u32,
                        map: if other { n[4] as u32 } else { WORLD },
                        at: if other { [n[5], n[6]] } else { [x, y] },
                        scale: 1.,
                        yaw: std::env::var("IRMINSUL_FAKE_YAW").ok().and_then(|text| text.parse().ok()),
                        range: 90.,
                        count: 1,
                    }
                })
            }),
            manual_level: None,
            last_place: None,
            debug,
            game_map: crate::game_map::GameMap::new(),
            game_made: None,
            panel_saved: None,
            panel_made: None,
            no_key_until: None,
            no_key_shown: false,
            near_window: None,
            near_made: None,
            near_moving: false,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// What the key did since the last frame: a tap shows or hides the map, and
    /// the key held while it is shown is for moving it.
    fn handle_key(&mut self, ctx: &Context) {
        hotkey::configure(self.settings.enabled, self.settings.key, ctx);
        if !self.settings.enabled {
            self.visible = false;
            self.editing = false;
            self.pressed_at = None;
            let _ = hotkey::take_events();
            return;
        }
        for event in hotkey::take_events() {
            match event.edge {
                Edge::Down => self.pressed_at = Some(event.at),
                Edge::Up => {
                    if self.editing {
                        self.editing = false;
                    } else if self.pressed_at.is_some() {
                        self.visible = !self.visible;
                    }
                    self.pressed_at = None;
                }
            }
        }
        // Held while the map is up: the map is to be moved.
        if let Some(at) = self.pressed_at
            && self.visible
            && !self.editing
            && at.elapsed() >= HOLD
            && hotkey::is_down()
        {
            self.editing = true;
        }
        if self.pressed_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
    }

    /// The place and size the window is made with.
    fn first_rect(&self) -> [i32; 4] {
        let (screen_w, screen_h) = screen_size();
        let size = [
            (self.settings.size[0] as i32).max(MIN_SIZE),
            (self.settings.size[1] as i32).max(MIN_SIZE),
        ];
        let pos = self.settings.pos.map_or(
            [(screen_w - size[0]) / 2, (screen_h - size[1]) / 2],
            |pos| [pos[0] as i32, pos[1] as i32],
        );
        [pos[0], pos[1], size[0], size[1]]
    }

    /// Call every frame.
    pub fn frame(
        &mut self,
        ctx: &Context,
        snapshot: &Snapshot,
        player: Option<crate::PlayerPlace>,
        matched: Option<&Matched>,
        shared: &SharedMarkers,
        russian: bool,
    ) {
        let player = player.or(self.fake_player);
        self.handle_key(ctx);
        self.game_map_frame(ctx, snapshot, player, matched, shared, russian);
        self.nearby_frame(ctx, player, shared, russian);
        // The traffic of the game can not be read: no map (it would be wrong), only that
        // said when the key is pressed.
        if shared.key_lost {
            if self.visible || self.editing || self.pressed_at.is_some() {
                self.no_key_until = Some(Instant::now() + Duration::from_secs(5));
            }
            self.visible = false;
            self.editing = false;
            self.close_panel();
            if let Some(window) = &self.window {
                window.set_visible(false);
                window.set_edit(false);
            }
            self.made = None;
            match self.no_key_until {
                Some(until) if Instant::now() < until => {
                    if !self.no_key_shown {
                        let (picture, width, height) = crate::game_map_panel::no_key(russian);
                        let (screen_w, screen_h) = screen_size();
                        let game = self.game_map.game_rect().unwrap_or([0, 0, screen_w, screen_h]);
                        let x = game[0] + (game[2] - width as i32) / 2;
                        let y = game[1] + (game[3] as f32 * 0.2) as i32;
                        self.game_map.show([x, y, width as i32, height as i32], &picture);
                        self.no_key_shown = true;
                    }
                    ctx.request_repaint_after(Duration::from_millis(200));
                }
                _ => {
                    if self.no_key_shown {
                        self.game_map.hide();
                        self.no_key_shown = false;
                    }
                    self.no_key_until = None;
                }
            }
            return;
        }
        if self.no_key_shown {
            self.game_map.hide();
            self.no_key_shown = false;
            self.no_key_until = None;
        }
        // While the big map of the game is open, this map is not shown: it is closed.  The
        // key held then shows the settings of the markers over the game's map.
        if self.game_map.map_open() {
            self.visible = false;
            self.editing = false;
            self.panel_frame(ctx, russian);
            return;
        }
        self.close_panel();
        // Out of sight when something else than the game (or us) is in front;
        // it comes back by itself.
        // The map of a place that is not known is not shown (that of the last place
        // would be taken for it); the overlay is, with the player in the middle and
        // a question mark, so that it is seen that it is not that it is not working.
        let unknown_place = player.is_some_and(|player| player.map == 0);
        let show = self.settings.enabled
            && self.visible
            && (self.debug
                || if self.settings.only_over_game {
                    hotkey::front() == hotkey::FRONT_GAME
                } else {
                    hotkey::front() != hotkey::FRONT_OTHER
                });
        if self.window.is_none() {
            if !show {
                return;
            }
            self.window = Some(OverlayWindow::new(self.first_rect()));
        }
        let first_rect = self.first_rect();
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_visible(show);
        window.set_edit(show && self.editing);
        if !show {
            self.made = None;
            return;
        }
        self.tiles.poll();
        self.floor_images.poll();
        self.mark_images.poll();
        // The map of the place the player is in.
        if let Some(player) = player.filter(|player| player.map != 0)
            && player.map != self.tiles.info().map
            && let Some(def) = markers::data().atlas.map(player.map)
        {
            self.tiles.set_map(def.info());
            self.center = None;
            self.manual_level = None;
            LOOK_CHANGED.store(true, Ordering::Relaxed);
        }
        let info = self.tiles.info();

        // The wheel, while the map is moved, is the zoom.
        let wheel = window.take_wheel();
        if wheel != 0 {
            self.settings.scale =
                (self.settings.scale * (wheel as f32 / 120. * 0.18).exp()).clamp(MIN_SCALE, MAX_SCALE);
            self.dirty = true;
        }

        // It follows the player: smoothly, but at once if it is far.
        let target = player
            .filter(|player| player.map == info.map)
            .map(|player| vec2(info.origin[0] + player.at[0], info.origin[1] + player.at[1]));
        match (target, &mut self.center) {
            (Some(target), Some(center)) => {
                let gap = target - *center;
                *center = if gap.length() > 600. { target } else { *center + gap * 0.3 };
            }
            (Some(target), None) => self.center = Some(target),
            (None, None) => {
                self.center = Some(vec2(info.origin[0] + 500., info.origin[1] + 500.));
            }
            (None, Some(_)) => (),
        }
        let center = self.center.unwrap_or(Vec2::ZERO);

        // Where the window is and how big: what it was made with the first time,
        // after that what the user made of it.
        let mut rect = window.rect();
        if rect[2] < MIN_SIZE || rect[3] < MIN_SIZE {
            rect = first_rect;
            window.set_rect(rect);
        }
        let (width, height) = (rect[2], rect[3]);

        // The levels: the floors of the places in view, one level being one floor
        // of each.  The player's own level is where they are, unless one was
        // chosen.
        let data = markers::data();
        // The minimap of the game, held up to the pictures of the floors.
        self.watcher.frame(ctx, player, &data, &self.tiles, &self.floor_images, &info);
        let origin = vec2(info.origin[0], info.origin[1]);
        let scale = self.settings.scale;
        let half = vec2(width as f32 / 2. / scale, height as f32 / 2. / scale);
        let (view_min, view_max) = (center - half, center + half);
        let mut sites: Vec<&crate::atlas::Site> = Vec::new();
        let mut max_level = 0usize;
        let world = data.atlas.map(info.map);
        if let Some(def) = world {
            for site in &def.sites {
                let Some(extent) = site.extent() else {
                    continue;
                };
                let (low, high) = (origin + vec2(extent[0], extent[1]), origin + vec2(extent[2], extent[3]));
                if high.x >= view_min.x && low.x <= view_max.x && high.y >= view_min.y && low.y <= view_max.y {
                    max_level = max_level.max(site.floors.len());
                    sites.push(site);
                }
            }
        }
        let place = player.map(|player| (player.scene, player.floor));
        if place != self.last_place {
            // The player went to another floor: what was chosen is not kept.
            self.manual_level = None;
            self.last_place = place;
        }
        let auto = player
            .filter(|player| player.map == info.map && player.floor != 0)
            .and_then(|player| {
                world
                    .and_then(|def| def.site_of_floor(player.floor))
                    .and_then(|site| site.floors.iter().position(|floor| floor.id == player.floor))
                    .map(|index| index + 1)
            })
            .unwrap_or(0);
        if self.editing {
            // The buttons of the levels, and after them the one of the markers over the
            // game's own map.
            let mut buttons = map_render::level_buttons(width as usize, max_level);
            let levels = buttons.len();
            buttons.push(map_render::sync_button());
            buttons.push(map_render::mobs_button());
            window.set_buttons(buttons);
            if let Some(number) = window.take_click() {
                if number == levels {
                    self.settings.on_game_map = !self.settings.on_game_map;
                    self.dirty = true;
                } else if number == levels + 1 {
                    self.settings.nearby_mobs = !self.settings.nearby_mobs;
                    self.dirty = true;
                } else {
                    self.manual_level = Some(number);
                }
                LOOK_CHANGED.store(true, Ordering::Relaxed);
            }
        } else {
            window.set_buttons(Vec::new());
        }
        let level = self.manual_level.unwrap_or(auto).min(max_level);
        let mut pictures: Vec<FloorPicture> = Vec::new();
        let mut shown_floors: std::collections::HashSet<u32> = std::collections::HashSet::new();
        if level > 0 {
            for site in &sites {
                let Some(floor) = site.floors.get(level - 1) else {
                    continue;
                };
                shown_floors.insert(floor.id);
                if floor.rect[2] <= floor.rect[0] {
                    continue;
                }
                if let Some(image) = self.floor_images.image(floor.id, &floor.url) {
                    pictures.push(FloorPicture {
                        rect: [
                            origin.x + floor.rect[0],
                            origin.y + floor.rect[1],
                            origin.x + floor.rect[2],
                            origin.y + floor.rect[3],
                        ],
                        image,
                    });
                }
            }
        }

        // What the map needs of its tiles, so they come; and how many are there.
        let mut tiles_there = 0;
        for key in map_render::needed_tiles(
            &info,
            [center.x, center.y],
            self.settings.scale,
            width as f32,
            height as f32,
        ) {
            if self.tiles.image(key).is_some() {
                tiles_there += 1;
            }
        }
        let made = Made {
            center: ((center.x * 2.) as i32, (center.y * 2.) as i32),
            scale: (self.settings.scale * 1000.) as i32,
            size: (width, height),
            editing: self.editing,
            data: snapshot.version ^ matched.map_or(0, |matched| matched.version().rotate_left(7)),
            tiles: tiles_there,
            map: info.map,
            unknown: unknown_place,
            marks: shared.version,
            icons_there: self.icons_there(&data, shared),
            level,
            max_level,
            floors: pictures.len(),
            turn: player.and_then(|player| player.yaw).map_or(-1, |yaw| yaw.round() as i32),
            range: if shared.show_range { player.map_or(0, |player| player.range as i32) } else { 0 },
        };
        if self.made != Some(made) || LOOK_CHANGED.swap(false, Ordering::Relaxed) {
            let unknown = unknown_place;
            let icons = if unknown {
                Vec::new()
            } else if level == 0 {
                // The chests that are followed are those of the world; the chests of the other
                // maps are drawn as chests too, the same way, from what is marked.
                if info.map == WORLD {
                    self.icons(&info, snapshot, matched, shared)
                } else {
                    self.floor_icons(&info, &data, &std::collections::HashSet::from([0u32]), shared)
                }
            } else {
                self.floor_icons(&info, &data, &shown_floors, shared)
            };
            let marks = if unknown {
                Vec::new()
            } else {
                self.marks(
                    &info,
                    &data,
                    if level == 0 { None } else { Some(&shown_floors) },
                    shared,
                    (view_min, view_max),
                    true,
                )
            };
            let tiles = &self.tiles;
            let lookup = |key| if unknown { None } else { tiles.image(key) };
            let picture = map_render::render(&Params {
                width: width as usize,
                height: height as usize,
                center: [center.x, center.y],
                scale: self.settings.scale,
                opacity: self.settings.opacity,
                vignette: self.settings.vignette,
                edit: self.editing,
                info: &info,
                tiles: &lookup,
                icons: &icons,
                player: if unknown {
                    Some([center.x, center.y])
                } else {
                    player
                        .filter(|player| player.map == info.map)
                        .map(|player| [info.origin[0] + player.at[0], info.origin[1] + player.at[1]])
                },
                facing: if unknown { None } else { player.and_then(|player| player.facing()) },
                range: if unknown || !shared.show_range {
                    None
                } else {
                    player.filter(|player| player.map == info.map).map(|player| player.range * player.scale)
                },
                unknown,
                floors: if unknown { &[] } else { &pictures },
                markers: &marks,
                dots: self.settings.scale < 0.12,
                dim: if level > 0 { 0.72 } else { 0. },
                levels: (max_level > 0 && !unknown).then_some(LevelsView {
                    current: level,
                    max: max_level,
                    buttons: self.editing,
                }),
                sync: self.editing.then_some(self.settings.on_game_map),
                mobs: self.editing.then_some(self.settings.nearby_mobs),
            });
            window.present(&picture, width as usize, height as usize);
            self.made = Some(made);
        }

        // Where it is and how big, kept when the key is let go.
        if window.take_changed() {
            let place = window.rect();
            self.settings.size = [place[2] as f32, place[3] as f32];
            self.settings.pos = Some([place[0] as f32, place[1] as f32]);
            self.dirty = true;
        }
        if self.dirty && !self.editing {
            self.dirty = false;
            self.settings.save();
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    /// The enemies of the kinds chosen on the map that are alive near the player, listed at the
    /// right of the game (where what changed is told, under it): the picture of the kind, the
    /// name, how far, and how much higher or lower.
    fn nearby_frame(&mut self, ctx: &Context, player: Option<crate::PlayerPlace>, shared: &SharedMarkers, russian: bool) {
        use crate::game_map_panel::NearRow;
        let mut rows: Vec<NearRow> = Vec::new();
        let shown = self.settings.nearby_mobs
            && !shared.key_lost
            && !self.game_map.map_open()
            && (self.debug || hotkey::front() == hotkey::FRONT_GAME);
        if let (true, Some(player)) = (shown, player) {
            let data = markers::data();
            // The nearest of each kind of monster, and how many of it are near.
            let mut kinds: Vec<(u32, u32, f32, f32, usize, [f32; 2])> = Vec::new();
            for mob in &shared.mobs.alive {
                let Some(label) = mob.label.filter(|label| shared.enabled.contains(label)) else {
                    continue;
                };
                if mob.scene != player.scene {
                    continue;
                }
                let across = (mob.position[0] - player.position[0]).hypot(mob.position[2] - player.position[2]);
                let up = mob.position[1] - player.position[1];
                if across.hypot(up) > self.settings.nearby_metres {
                    continue;
                }
                // One line for the variants of a monster (the ids differ in the last digits).
                let family = if mob.monster / 1_000_000 == 28 { mob.monster } else { mob.monster / 100 * 100 };
                match kinds.iter_mut().find(|kind| kind.0 == family) {
                    Some(kind) => {
                        kind.4 += 1;
                        if across < kind.2 {
                            (kind.2, kind.3, kind.5) = (across, up, [mob.position[0] - player.position[0], mob.position[2] - player.position[2]]);
                        }
                    }
                    None => kinds.push((family, label, across, up, 1, [mob.position[0] - player.position[0], mob.position[2] - player.position[2]])),
                }
            }
            kinds.sort_by(|a, b| a.2.total_cmp(&b.2));
            for (family, label, across, up, count, [dx, dz]) in kinds.into_iter().take(6) {
                let kind = data.label(label);
                // The way to it from the way the player faces (0 is along z, 90 along x, as the
                // game tells it), in steps, so that the list is not drawn anew all the time.
                let turn = player.yaw.filter(|_| across >= 2.).map(|yaw| {
                    let bearing = dx.atan2(dz).to_degrees();
                    let turn = (bearing - yaw).rem_euclid(360.);
                    ((turn / 10.).round() as i32 * 10) % 360
                });
                rows.push(NearRow {
                    icon: kind.and_then(|kind| self.mark_images.image(kind.id, &kind.icon)),
                    name: crate::mob_watch::name_of(family, russian)
                        .map(str::to_owned)
                        .or_else(|| kind.map(|kind| kind.name(russian).to_owned()))
                        .unwrap_or_default(),
                    count,
                    metres: across.round() as i32,
                    up: up.round() as i32,
                    turn,
                });
            }
            if !rows.is_empty() {
                self.mark_images.poll();
            }
        }
        // While the key of the map over the game is held, the list can be moved with the mouse
        // (it is shown then even with nothing in it, to be put where it is wanted).
        let moving = shown && (self.editing || (self.pressed_at.is_some_and(|at| at.elapsed() >= HOLD) && hotkey::is_down()));
        if self.near_moving && !moving {
            self.settings.save();
        }
        self.near_moving = moving;
        if rows.is_empty() && !moving {
            if self.near_made.take().is_some()
                && let Some(window) = &self.near_window
            {
                window.set_movable(false);
                window.hide();
            }
            return;
        }
        let (screen_w, screen_h) = screen_size();
        let game = self.game_map.game_rect().unwrap_or([0, 0, screen_w, screen_h]);
        let (picture, width, height) = crate::game_map_panel::nearby(&rows, russian, moving);
        let window = self.near_window.get_or_insert_with(crate::game_map::MarksWindow::new);
        // Where it was moved to is where it stays.
        if moving
            && self.near_made.as_ref().is_some_and(|made| made.2)
            && let Some(at) = window.position()
            && self.near_made.as_ref().is_some_and(|made| made.1 != at)
        {
            self.settings.nearby_pos = Some([at[0] - game[0], at[1] - game[1]]);
        }
        let place = match self.settings.nearby_pos {
            // Not out of the window of the game.
            Some(offset) => [
                game[0] + offset[0].clamp(0, (game[2] - width as i32).max(0)),
                game[1] + offset[1].clamp(0, (game[3] - height as i32).max(0)),
            ],
            None => [game[0] + game[2] - width as i32 - 24, game[1] + (game[3] as f32 * 0.36) as i32],
        };
        let made = (rows, place, moving);
        if self.near_made.as_ref() != Some(&made) {
            window.show([place[0], place[1], width as i32, height as i32], &picture);
            self.near_made = Some(made);
        }
        window.set_movable(moving);
        ctx.request_repaint_after(Duration::from_millis(if moving { 40 } else { 250 }));
    }

    /// What the thread that looks at the game is doing, in words.
    fn game_map_status(&self, russian: bool) -> String {
        use crate::game_map::Status;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let status = match self.game_map.status() {
            Status::Off => tr("starting", "запускается"),
            Status::NoGame => tr("the game is not in front", "игра не на переднем плане"),
            Status::MapClosed => tr("the game's map is not open", "карта в игре не открыта"),
            Status::MapOpen => tr("the game's map is open", "карта в игре открыта"),
            Status::Looking => tr("looking for the place of the map", "ищу место на карте"),
            Status::Following => tr("following the map", "слежу за картой"),
        };
        let alike = self.game_map.synced().map_or(String::new(), |synced| format!(" ({:.0}%)", synced.score * 100.));
        format!("{}: {status}{alike}", tr("Now", "Сейчас"))
    }

    /// Puts the window of the map back as it was before the panel of the settings was shown
    /// in it.
    fn close_panel(&mut self) {
        let Some(saved) = self.panel_saved.take() else {
            return;
        };
        if let Some(window) = &self.window {
            window.set_edit(false);
            window.set_visible(false);
            window.set_buttons(Vec::new());
            window.set_rect(saved);
            let _ = window.take_changed();
            let _ = window.take_click();
        }
        self.panel_made = None;
        self.made = None;
        LOOK_CHANGED.store(true, Ordering::Relaxed);
    }

    /// The big map of the game is open: while the key is held, the settings of the markers
    /// over it are shown at the right of the game, in the middle; nothing else of this map.
    fn panel_frame(&mut self, ctx: &Context, russian: bool) {
        use crate::game_map_panel::{self as panel, PANEL_HEIGHT, PANEL_WIDTH, PanelState};
        let held = self.pressed_at.is_some_and(|at| at.elapsed() >= HOLD) && hotkey::is_down();
        let game = self.game_map.game_rect();
        let (true, Some(game)) = (held, game) else {
            self.close_panel();
            if let Some(window) = &self.window {
                window.set_visible(false);
                window.set_edit(false);
            }
            self.made = None;
            if self.pressed_at.is_some() {
                ctx.request_repaint_after(Duration::from_millis(40));
            }
            return;
        };
        if self.window.is_none() {
            self.window = Some(OverlayWindow::new(self.first_rect()));
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if self.panel_saved.is_none() {
            self.panel_saved = Some(window.rect());
            self.panel_made = None;
            let x = game[0] + game[2] - PANEL_WIDTH as i32 - (game[2] as f32 * 0.085) as i32;
            let y = game[1] + (game[3] - PANEL_HEIGHT as i32) / 2;
            window.set_rect([x, y, PANEL_WIDTH as i32, PANEL_HEIGHT as i32]);
            let _ = window.take_click();
        }
        window.set_buttons(panel::panel_buttons());
        window.set_visible(true);
        window.set_edit(true);
        let mut changed = false;
        if let Some(button) = window.take_click() {
            match button {
                panel::BUTTON_TOGGLE => self.settings.on_game_map = !self.settings.on_game_map,
                panel::BUTTON_SMALLER => self.settings.game_map_size = ((self.settings.game_map_size - 0.1) * 10.).round().max(5.) / 10.,
                panel::BUTTON_BIGGER => self.settings.game_map_size = ((self.settings.game_map_size + 0.1) * 10.).round().min(25.) / 10.,
                panel::BUTTON_MOBS => self.settings.nearby_mobs = !self.settings.nearby_mobs,
                _ => (),
            }
            changed = true;
        }
        let _ = window.take_changed();
        let _ = window.take_wheel();
        let state = PanelState {
            on: self.settings.on_game_map,
            mobs: self.settings.nearby_mobs,
            size: self.settings.game_map_size,
            status: self.game_map_status(russian),
            russian,
        };
        if self.panel_made.as_ref() != Some(&state) {
            window.present(&panel::panel(&state), PANEL_WIDTH, PANEL_HEIGHT);
            self.panel_made = Some(state);
        }
        if changed {
            self.settings.save();
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    /// The markers over the big map of the game itself: tells the thread that looks at the
    /// game what it needs, and draws the markers where it found them to be.
    fn game_map_frame(
        &mut self,
        ctx: &Context,
        snapshot: &Snapshot,
        player: Option<crate::PlayerPlace>,
        matched: Option<&Matched>,
        shared: &SharedMarkers,
        russian: bool,
    ) {
        // Not when the traffic of the game can not be read: where the player is and what is
        // collected is not known then.
        let enabled = self.settings.on_game_map && !shared.key_lost;
        // The game is looked at also for closing this map when its own is opened, and for
        // where to put the list of the enemies near.
        let watch = enabled || self.settings.enabled || self.settings.nearby_mobs;
        if !watch && self.game_made.is_none() && self.game_map.status() == crate::game_map::Status::Off {
            return;
        }
        let data = markers::data();
        let world = data.atlas.map(WORLD).map(|def| def.info());
        let place = world.as_ref().and_then(|info| {
            player.filter(|player| player.map == WORLD).map(|player| [info.origin[0] + player.at[0], info.origin[1] + player.at[1]])
        });
        let map = world.clone().and_then(|info| crate::map_tiles::version_dir(&info.version).map(|dir| (dir, info)));
        self.game_map.update(ctx, watch, enabled, place, map);
        let (Some(synced), Some(info), true) = (self.game_map.synced(), world, enabled) else {
            // The place is being looked for (or the map is on its way): no markers, which
            // would be at wrong places, but a sign that it is being done.
            let looking = enabled && self.game_map.status() == crate::game_map::Status::Looking;
            match self.game_map.game_rect().filter(|_| looking) {
                Some(game) => {
                    let phase = (ctx.input(|input| input.time) * 3.) as u32 % 3 + 1;
                    let made = (0, 0, 0, 0, 0, phase);
                    if self.game_made != Some(made) {
                        let (picture, width, height) = crate::game_map_panel::looking(phase - 1, russian);
                        let x = game[0] + (game[2] - width as i32) / 2;
                        let y = game[1] + (game[3] as f32 * 0.115) as i32;
                        self.game_map.show([x, y, width as i32, height as i32], &picture);
                        self.game_made = Some(made);
                    }
                    ctx.request_repaint_after(Duration::from_millis(200));
                }
                None => {
                    // (Unless it tells that the traffic can not be read.)
                    if !self.no_key_shown {
                        self.game_map.hide();
                    }
                    self.game_made = None;
                }
            }
            return;
        };
        self.mark_images.poll();
        let (width, height) = (synced.rect[2] as f32, synced.rect[3] as f32);
        let view = synced.view;
        let scale = view.zoom * height;
        // The tiles the view is held up to, so that they come if they are not there (those
        // of the world: the map shown here may be another).
        if self.tiles.info().map == WORLD {
            self.tiles.poll();
            for rows in [540., 240.] {
                let small = view.zoom * rows;
                for key in map_render::needed_tiles(&info, view.center, small, width / height * rows, rows) {
                    let _ = self.tiles.image(key);
                }
            }
        }
        let made = (
            self.game_map.version(),
            snapshot.version ^ matched.map_or(0, |matched| matched.version().rotate_left(7)),
            shared.version,
            self.icons_there(&data, shared),
            (self.settings.game_map_size * 100.) as i32,
            0,
        );
        if self.game_made == Some(made) {
            return;
        }
        let center = vec2(view.center[0], view.center[1]);
        let half = vec2(width / 2. / scale, height / 2. / scale);
        let marks = self.marks(&info, &data, None, shared, (center - half, center + half), true);
        let icons = self.icons(&info, snapshot, matched, shared);
        // Smaller the farther out the map is zoomed, as the signs of the game are fewer
        // there: from 11 (all the way out) to 24 (all the way in) for a window 1080 high.
        let near = ((view.zoom / 0.00036).ln() / (0.0022f32 / 0.00036).ln()).clamp(0., 1.);
        let size = (height / 1080. * (11. + 13. * near) * self.settings.game_map_size).clamp(8., 96.);
        // Too many to tell apart: dots.
        let dots = marks.len() > 1200;
        let picture = map_render::render_marks(
            width as usize,
            height as usize,
            &move |at| view.to_screen(at, width, height),
            &marks,
            &icons,
            size,
            dots,
            1.,
        );
        self.game_map.show(synced.rect, &picture);
        self.game_made = Some(made);
    }

    /// How many of the pictures of the chosen kinds are there (they are asked for
    /// here), so that the map is drawn again as they come.
    fn icons_there(&self, data: &markers::Data, shared: &SharedMarkers) -> usize {
        shared
            .enabled
            .iter()
            .filter_map(|id| data.label(*id))
            .filter(|label| self.mark_images.image(label.id, &label.icon).is_some())
            .count()
    }

    /// The markers of the chosen kinds that are in view, on the surface or on the
    /// floors that are shown.  The chests are drawn as chests, apart.
    fn marks(
        &self,
        info: &MapInfo,
        data: &markers::Data,
        floors: Option<&std::collections::HashSet<u32>>,
        shared: &SharedMarkers,
        view: (Vec2, Vec2),
        chests_apart: bool,
    ) -> Vec<MarkDraw> {
        let surface = std::collections::HashSet::from([0u32]);
        let floors = floors.unwrap_or(&surface);
        let now = markers::unix_now();
        let (low, high) = (view.0 - vec2(24., 24.), view.1 + vec2(24., 24.));
        let mut out = Vec::new();
        // The chests are drawn as chests where they are followed (the surface of the
        // world, and the floors); elsewhere (Enkanomiya and the like) they are
        // markers like the rest.
        for id in shared.enabled.iter().filter(|id| !chests_apart || !markers::CHEST_LABELS.contains(id)) {
            let Some(label) = data.label(*id) else {
                continue;
            };
            let image = self.mark_images.image(label.id, &label.icon);
            let color = crate::map_tab::label_color(label.id).to_array();
            for point in data.points_on(label.id, info.map, floors) {
                let at = [info.origin[0] + point.x, info.origin[1] + point.y];
                if at[0] < low.x || at[0] > high.x || at[1] < low.y || at[1] > high.y {
                    continue;
                }
                let done = shared.markable.contains(id)
                    && matches!(shared.store.state(point.id, label.refresh, now), markers::State::Done { .. });
                if done && shared.hide_done {
                    continue;
                }
                out.push(MarkDraw {
                    at,
                    image: image.clone(),
                    color: [color[0], color[1], color[2]],
                    done,
                });
                if out.len() >= 6000 {
                    return out;
                }
            }
        }
        out
    }

    /// The chests of the floors that are shown (they are not followed, so they are
    /// all as not found).
    fn floor_icons(
        &self,
        info: &MapInfo,
        data: &markers::Data,
        floors: &std::collections::HashSet<u32>,
        shared: &SharedMarkers,
    ) -> Vec<Icon> {
        let mut icons = Vec::new();
        for (index, label) in markers::CHEST_LABELS.iter().enumerate() {
            if !shared.enabled.contains(label) {
                continue;
            }
            let now = markers::unix_now();
            for point in data.points_on(*label, info.map, floors) {
                // As marked: opened (or collected) ones are grey, or not there if they are hidden.
                let done = matches!(shared.store.state(point.id, 0, now), markers::State::Done { .. });
                if done && shared.hide_done {
                    continue;
                }
                icons.push(Icon {
                    at: [info.origin[0] + point.x, info.origin[1] + point.y],
                    tier: index as u8 + 1,
                    look: if done { IconLook::Opened } else { IconLook::Closed },
                });
            }
        }
        icons
    }

    /// The chests to draw, on the full size map.
    fn icons(&self, info: &MapInfo, snapshot: &Snapshot, matched: Option<&Matched>, shared: &SharedMarkers) -> Vec<Icon> {
        let mut icons = Vec::new();
        let Some(matched) = matched else {
            return icons;
        };
        // As on the map of the program: the kinds of chest that are chosen, the
        // opened ones only if they are not hidden, the ones not found only if they
        // are shown.
        let tier_shown = |tier: u8| {
            markers::CHEST_LABELS
                .get((tier as usize).wrapping_sub(1))
                .is_some_and(|label| shared.enabled.contains(label))
        };
        for (index, chest) in official_chests::all().iter().enumerate() {
            if matched.found[index] || !tier_shown(chest.tier) {
                continue;
            }
            let look = matched.of_official[index];
            if (look == Look::Likely && shared.hide_done) || (look == Look::Faint && !shared.show_faint) {
                continue;
            }
            let at = official_on_map(info, chest);
            icons.push(Icon {
                at: [at.x, at.y],
                tier: chest.tier,
                look: icon_look(look),
            });
        }
        for (index, chest) in snapshot.chests.iter().enumerate() {
            let Some(position) = chest.position else {
                continue;
            };
            let look = matched.of_found.get(index).copied().unwrap_or(Look::Closed);
            if !tier_shown(chest.tier()) || (shared.hide_done && matches!(look, Look::Opened | Look::Likely)) {
                continue;
            }
            let at = on_map(info, position);
            icons.push(Icon {
                at: [at.x, at.y],
                tier: chest.tier(),
                look: icon_look(look),
            });
        }
        icons
    }

    /// The settings of the map over the game, in the settings of the map.
    pub fn settings_ui(&mut self, ui: &mut Ui, ctx: &Context, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        ui.label(RichText::new(tr("Map over the game", "Карта поверх игры")).size(18.).strong());
        ui.label(
            RichText::new(tr(
                "A tap of the key shows the map in the middle of the screen, and hides it. Hold \
                 the key while the map is shown, and it can be moved, made bigger or smaller by \
                 its edges, and zoomed with the wheel. Where there are places under or over the \
                 ground, the map shows the floor the player is on (found from what is around \
                 them and their height), and while the key is held, buttons of the levels appear \
                 at the top right (0 is the surface): a click shows that level.",
                "Нажатие клавиши показывает карту посередине экрана и скрывает её. Если зажать \
                 клавишу, пока карта открыта, её можно двигать, менять размер за края и \
                 масштабировать колесом. Там, где есть места под землёй или над ней, карта \
                 показывает этаж, на котором игрок (определяется по окружению и высоте), а пока \
                 клавиша зажата, справа вверху появляются кнопки уровней (0 — поверхность): \
                 клик показывает этот уровень.",
            ))
            .size(12.)
            .color(crate::achievement_tab::DIM),
        );
        let mut changed = false;
        changed |= ui
            .checkbox(
                &mut self.settings.enabled,
                RichText::new(tr("Turn it on", "Включить")).size(14.),
            )
            .changed();

        ui.horizontal(|ui| {
            ui.label(tr("Key", "Клавиша"));
            if self.choosing_key {
                ui.label(
                    RichText::new(tr("Press a key (Esc: cancel)…", "Нажмите клавишу (Esc: отмена)…")).size(14.),
                );
                if let Some(key) = hotkey::pressed_key() {
                    if key != 0x1b {
                        self.settings.key = key;
                        changed = true;
                    }
                    self.choosing_key = false;
                }
                ctx.request_repaint_after(Duration::from_millis(30));
            } else if ui
                .button(RichText::new(hotkey::key_name(self.settings.key)).size(14.))
                .on_hover_text(tr(
                    "Click, then press the key to use",
                    "Нажмите, затем клавишу, которую хотите использовать",
                ))
                .clicked()
            {
                self.choosing_key = true;
            }
            if ui.small_button(tr("Caps Lock", "Caps Lock")).clicked() {
                self.settings.key = hotkey::DEFAULT_KEY;
                self.choosing_key = false;
                changed = true;
            }
        });

        changed |= ui
            .add(
                egui::Slider::new(&mut self.settings.opacity, 0.2..=1.)
                    .text(tr("Opacity", "Непрозрачность"))
                    .custom_formatter(|value, _| format!("{:.0}%", value * 100.)),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.settings.vignette, 0.0..=1.)
                    .text(tr("Fading at the edges", "Затухание к краям"))
                    .custom_formatter(|value, _| format!("{:.0}%", value * 100.)),
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.settings.only_over_game,
                RichText::new(tr("Only over the game", "Только поверх игры")).size(14.),
            )
            .on_hover_text(tr(
                "Not drawn while Irminsul (or any other window) is in front.",
                "Не рисуется, пока на переднем плане Irminsul (или любое другое окно).",
            ))
            .changed();
        ui.label(
            RichText::new(tr(
                "What is shown on this map (the kinds of marker, the kinds of chest, what is                  collected or opened) is chosen on the map of the program, and is the same here.",
                "Что показано на этой карте (виды меток, виды сундуков, собранное и открытое)                  выбирается на карте в программе и здесь то же самое.",
            ))
            .size(12.)
            .color(crate::achievement_tab::DIM),
        );
        ui.add_space(6.);
        ui.label(RichText::new(tr("Markers over the game's own map", "Отметки поверх карты самой игры")).size(16.).strong());
        ui.label(
            RichText::new(tr(
                "When the big map of the game is open, the markers chosen here are drawn over it, where they are, and follow it as it is moved and zoomed. The program looks at the screen for this (only while the game is in front; nothing of the game is read, nothing is kept or sent). The surface of the world only. It can also be turned on and off with the button M at the top left of the map over the game, while its key is held.",
                "Когда в игре открыта большая карта, выбранные здесь отметки рисуются поверх неё на своих местах и следуют за ней при сдвиге и масштабировании. Для этого программа смотрит на экран (только пока игра на переднем плане; из игры ничего не читается, ничего не сохраняется и не отправляется). Только поверхность мира. Включать и выключать можно и кнопкой M слева вверху карты поверх игры, пока зажата её клавиша.",
            ))
            .size(12.)
            .color(crate::achievement_tab::DIM),
        );
        changed |= ui
            .checkbox(&mut self.settings.on_game_map, RichText::new(tr("Draw the markers over the game's map", "Рисовать отметки поверх карты игры")).size(14.))
            .changed();
        if self.settings.on_game_map {
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.settings.game_map_size, 0.5..=2.5)
                        .text(tr("Size of the markers", "Размер отметок"))
                        .custom_formatter(|value, _| format!("{:.0}%", value * 100.)),
                )
                .changed();
            ui.label(RichText::new(self.game_map_status(russian)).size(12.).color(crate::achievement_tab::DIM));
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        ui.add_space(6.);
        ui.label(RichText::new(tr("Enemies near", "Мобы рядом")).size(16.).strong());
        ui.label(
            RichText::new(tr(
                "The game sends the enemies that are alive around you. Those of the kinds chosen on the map are listed at the right of the game while they are near: how far, and how much higher or lower. An enemy that is killed marks its marker of the map as done (for a day); a marker you stand by with no enemy there is marked too. The button E at the top left of the map over the game, while its key is held, turns the list on and off.",
                "Игра присылает живых противников вокруг вас. Те из них, чьи виды выбраны на карте, показываются справа поверх игры, пока они рядом: расстояние и насколько выше или ниже. Убитый противник отмечает свою метку на карте выполненной (на сутки); метка, у которой вы стоите и где противников нет, тоже отмечается. Кнопка E слева вверху карты поверх игры, пока зажата её клавиша, включает и выключает список.",
            ))
            .size(12.)
            .color(crate::achievement_tab::DIM),
        );
        changed |= ui
            .checkbox(&mut self.settings.nearby_mobs, RichText::new(tr("List the enemies near", "Показывать мобов рядом")).size(14.))
            .changed();
        if self.settings.nearby_mobs {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(tr(
                        "Hold the key of the map over the game and drag the list to move it.",
                        "Зажмите клавишу карты поверх игры и перетащите список мышью, чтобы передвинуть.",
                    ))
                    .size(12.)
                    .color(crate::achievement_tab::DIM),
                );
                if self.settings.nearby_pos.is_some() && ui.small_button(tr("Put it back", "Вернуть на место")).clicked() {
                    self.settings.nearby_pos = None;
                    changed = true;
                }
            });
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.settings.nearby_metres, 20.0..=250.)
                        .text(tr("Within, m", "В радиусе, м"))
                        .custom_formatter(|value, _| format!("{value:.0}")),
                )
                .changed();
        }
        if ui.button(tr("Reset the place and size", "Сбросить положение и размер")).clicked() {
            let defaults = MapOverlaySettings::default();
            self.settings.size = defaults.size;
            self.settings.pos = None;
            self.settings.scale = defaults.scale;
            if let Some(window) = &self.window {
                window.set_rect(self.first_rect());
            }
            changed = true;
        }
        if changed {
            LOOK_CHANGED.store(true, Ordering::Relaxed);
            self.settings.save();
        }
    }

    /// The settings of the floor told by the minimap of the game.
    pub fn minimap_ui(&mut self, ui: &mut Ui, ctx: &Context, russian: bool) {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        ui.label(RichText::new(tr("The floor by the minimap", "Этаж по миникарте игры")).size(16.).strong());
        ui.label(
            RichText::new(tr(
                "The minimap of the game shows the picture of the floor the player is on. With this                  on, the program looks at the circle of the minimap on the screen (only it, and only                  while the game is in front), holds the pictures of the floors that may be here up to                  it, and takes the one that is alike. Nothing is kept or sent. Set the circle so                  that the picture below is just the minimap.",
                "Миникарта игры показывает картинку того этажа, где находится игрок. Если включить,                  программа смотрит на круг миникарты на экране (только на него и только пока игра на                  переднем плане), сравнивает с ним картинки этажей, которые могут быть здесь, и берёт                  тот, что похож. Ничего не сохраняется и не отправляется. Подберите круг так, чтобы                  картинка ниже была в точности миникартой.",
            ))
            .size(12.)
            .color(crate::achievement_tab::DIM),
        );
        let mut minimap_changed = ui
            .checkbox(
                &mut self.watcher.settings.enabled,
                RichText::new(tr("Tell the floor by the minimap", "Определять этаж по миникарте")).size(14.),
            )
            .changed();
        if self.watcher.settings.enabled {
            let settings = &mut self.watcher.settings;
            minimap_changed |= ui
                .add(
                    egui::Slider::new(&mut settings.x, 0.0..=0.5)
                        .text(tr("Middle across", "Центр по горизонтали"))
                        .custom_formatter(|value, _| format!("{:.1}%", value * 100.)),
                )
                .changed();
            minimap_changed |= ui
                .add(
                    egui::Slider::new(&mut settings.y, 0.0..=0.5)
                        .text(tr("Middle down", "Центр по вертикали"))
                        .custom_formatter(|value, _| format!("{:.1}%", value * 100.)),
                )
                .changed();
            minimap_changed |= ui
                .add(
                    egui::Slider::new(&mut settings.radius, 0.03..=0.25)
                        .text(tr("Radius", "Радиус"))
                        .custom_formatter(|value, _| format!("{:.1}%", value * 100.)),
                )
                .changed();
            if let Some(preview) = &self.watcher.preview {
                let texture = ctx.load_texture("minimap-preview", preview.clone(), egui::TextureOptions::LINEAR);
                ui.add(egui::Image::new(&texture).fit_to_exact_size(vec2(150., 150.)));
            }
            ui.label(RichText::new(self.watcher.report.text.clone()).size(12.));
            for (floor, share) in &self.watcher.report.alike {
                ui.label(
                    RichText::new(format!(
                        "{}: {share:.2}",
                        if *floor == 0 { tr("surface", "поверхность").to_owned() } else { format!("{} {floor}", tr("floor", "этаж")) }
                    ))
                    .size(12.)
                    .color(crate::achievement_tab::DIM),
                );
            }
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if minimap_changed {
            self.watcher.settings.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_survive_being_stored() {
        let mut settings = MapOverlaySettings::default();
        settings.enabled = true;
        settings.key = 0x41;
        let text = serde_json::to_string(&settings).unwrap();
        let back: MapOverlaySettings = serde_json::from_str(&text).unwrap();
        assert!(back.enabled);
        // A file of before, with settings that are not there any more, is read.
        assert_eq!(back.key, 0x41);
        // A file with only some of the settings takes the others as they are.
        let old: MapOverlaySettings = serde_json::from_str(r#"{"enabled":true,"black_white":true,"show_chests":false}"#).unwrap();
        assert!(old.enabled);
        let partial: MapOverlaySettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(partial.key, hotkey::DEFAULT_KEY);
        assert_eq!(partial.opacity, 0.85);
    }

    #[test]
    fn a_look_maps_to_the_look_of_a_picture() {
        assert_eq!(icon_look(Look::Opened), IconLook::Opened);
        assert_eq!(icon_look(Look::Faint), IconLook::Faint);
    }
}
