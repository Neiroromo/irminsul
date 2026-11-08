//! Which floor of the world the player is on: the surface, or a floor of an
//! underground place (or of a place above the ground).
//!
//! The game says where the player is (the scene, and x, height, z) and what is
//! around them, but not which floor of the map that is.  It is worked out from
//! two things.
//!
//! 1. What is around.  Each thing the game puts into view (a chest, a plant, a
//!    waypoint) has a place, and the official map has a marker at that place,
//!    which belongs to a floor, or to the surface.  The things that are near the
//!    player, at about the player's height, are the ones of the floor the player
//!    is on: a cave under a hill sends its things too, but they are far below.
//! 2. The height.  Each time a thing is told to be of a floor, its height is
//!    kept, so there comes to be a height for each floor, and the player's
//!    height tells the floor when nothing is around.
//!
//! Only the floors whose picture covers the place the player is at are looked
//! at; where none does, the player is on the surface.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use auto_artifactarium::SceneGadget;
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::markers::{self, Data};
use crate::scenes::SceneMap;

/// Cells of the index of markers, in pixels of the map.
const CELL: f32 = 16.;
/// How far a thing can be from a marker to be that one.
const MATCH_RADIUS: f32 = 8.;
/// Things nearer than this (across), and not higher or lower than the next, count.
const NEAR_ACROSS: f32 = 130.;
const NEAR_HEIGHT: f32 = 25.;
/// How many things are kept.
const KEEP: usize = 800;
/// A floor whose things are spread over more than this much of height (the middle
/// eight tenths of them) is not a layer under or over the surface.
const SPREAD: f32 = 150.;
/// Heights this near are the same height, for what is on a floor.
const NEAR_BAND: f32 = 12.;
/// The surface of a map has its heights kept under this number and the map's.
const SURFACE: u32 = 1_000_000;
/// A change of height from one report to the next that is a lift, or a fall:
/// the player is on another floor, and it is not waited for to be sure.
/// What is told of is forgotten after this long.
const FORGET: Duration = Duration::from_secs(15 * 60);
/// How much a floor needs of what is around to be taken over the surface.
const NEEDED: f32 = 2.5;
/// How many heights are kept for a floor.
const HEIGHTS: usize = 40;
const SAVE_EVERY: Duration = Duration::from_secs(30);

struct Marker {
    x: f32,
    y: f32,
    floor: u32,
    /// A teleport waypoint (or a statue of the seven): what a teleport goes to.
    waypoint: bool,
}

/// A teleport that was gone to: where it is on the map, at what height, and the
/// floor the waypoint of the map that it is by is on.  The player who is near it
/// across and at about its height is on its floor; far above or below it, or far
/// off, it says nothing.
struct Anchor {
    map: u32,
    floor: u32,
    at: [f32; 2],
    height: f32,
}

/// The floor the minimap of the game says the player is on (see `minimap`): the
/// map, the floor, and when.
static VISION: std::sync::Mutex<Option<(u32, u32, Instant)>> = std::sync::Mutex::new(None);

/// What the minimap says, taken for this long.
const VISION_FOR: Duration = Duration::from_secs(8);

pub fn set_vision(map: u32, floor: u32) {
    if let Ok(mut slot) = VISION.lock() {
        *slot = Some((map, floor, Instant::now()));
    }
}

fn vision_of(map: u32) -> Option<u32> {
    let slot = VISION.lock().ok()?;
    slot.filter(|(of, _, at)| *of == map && at.elapsed() <= VISION_FOR).map(|(_, floor, _)| floor)
}

/// Near an anchor across (in pixels of the map), the player is on its floor...
const ANCHOR_ACROSS: f32 = 80.;
/// ...if they are within this much of its height (metres).
const ANCHOR_HEIGHT: f32 = 25.;
/// Right by an anchor, but this far above or below it, is another floor.
const ANCHOR_OTHER: f32 = 60.;

/// A thing that was put into view, and what floor it is of.
struct Seen {
    map: u32,
    at: [f32; 2],
    height: f32,
    floor: u32,
    when: Instant,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Heights {
    samples: Vec<f32>,
}

impl Heights {
    fn add(&mut self, height: f32, most: usize) {
        self.samples.push(height);
        if self.samples.len() > most {
            self.samples.remove(0);
        }
    }

    /// What the height of the player says of whether they are on this floor: its
    /// things are often at about that height (a good deal more), or never.  Floors
    /// are layers one over another, each with its own heights (and the surface
    /// too), so this is how they are told apart when the same place is on more
    /// than one of them.
    fn says(&self, height: f32) -> f32 {
        let n = self.samples.len();
        if n < 4 {
            return 0.;
        }
        let near = self.samples.iter().filter(|sample| (**sample - height).abs() <= NEAR_BAND).count() as f32 / n as f32;
        if near >= 0.25 {
            2.
        } else if near >= 0.1 {
            1.
        } else if near < 0.03 && n >= 8 {
            -2.
        } else {
            0.
        }
    }

    /// The floor is not a layer of its own: its things are at all heights (the
    /// markers of a whole region, such as a lake, that lie over the surface), so
    /// the height says nothing and the floor can't be told from the surface.
    fn is_spread(&self) -> bool {
        if self.samples.len() < 20 {
            return false;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let (low, high) = (sorted[sorted.len() / 10], sorted[sorted.len() * 9 / 10]);
        high - low > SPREAD
    }
}

pub struct FloorLocator {
    data: Option<Arc<Data>>,
    /// The markers of every map, by map and place.
    cells: HashMap<(u32, i32, i32), Vec<Marker>>,
    /// The teleports that were gone to, with their floors, and how each scene
    /// lay on its map when they were worked out.
    anchors: Vec<Anchor>,
    seeded: HashMap<u32, SceneMap>,
    /// The floor of the teleport that was just gone to, till the next report.
    arrival_floor: Option<u32>,
    /// Places of things that came into view since the player's place was last
    /// told (the game's own coordinates), worked out when it is told, as which
    /// scene they are of is known then.
    pending: Vec<([f32; 3], u32)>,
    recent: VecDeque<Seen>,
    heights: HashMap<u32, Heights>,
    current: u32,
    last_height: Option<f32>,
    /// The floor that seems to have taken over, and for how many reports.
    challenger: Option<(u32, u8)>,
    dirty: bool,
    saved_at: Instant,
}

/// Where the heights of the floors are kept.  `IRMINSUL_FLOOR_HEIGHTS` replaces it.
fn heights_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_FLOOR_HEIGHTS") {
        return Some(path.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("floor_heights.json"))
}

impl FloorLocator {
    pub fn new() -> Self {
        let heights = heights_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            data: None,
            cells: HashMap::new(),
            anchors: Vec::new(),
            seeded: HashMap::new(),
            arrival_floor: None,
            pending: Vec::new(),
            recent: VecDeque::new(),
            heights,
            current: 0,
            last_height: None,
            challenger: None,
            dirty: false,
            saved_at: Instant::now(),
        }
    }

    /// The index of the markers of the maps, made again when the markers are.
    fn index(&mut self) -> Arc<Data> {
        let data = markers::data();
        if self.data.as_ref().is_some_and(|made| Arc::ptr_eq(made, &data)) {
            return data;
        }
        self.cells.clear();
        for point in data.points.iter() {
            self.cells
                .entry((u32::from(point.map), (point.x / CELL).floor() as i32, (point.y / CELL).floor() as i32))
                .or_default()
                .push(Marker {
                    x: point.x,
                    y: point.y,
                    floor: u32::from(point.floor),
                    waypoint: point.label == 2 || point.label == 3,
                });
        }
        self.data = Some(data.clone());
        data
    }

    /// The floor of the waypoint of the map that a place is by (within 25 pixels),
    /// and so the floor that a teleport to it puts the player on.
    fn waypoint_floor(&self, map: u32, at: [f32; 2]) -> Option<u32> {
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let mut best: Option<(f32, u32)> = None;
        for gy in cy - 2..=cy + 2 {
            for gx in cx - 2..=cx + 2 {
                for marker in self.cells.get(&(map, gx, gy)).into_iter().flatten().filter(|marker| marker.waypoint) {
                    let distance = (marker.x - at[0]).hypot(marker.y - at[1]);
                    if distance <= 25. && best.is_none_or(|(nearest, _)| distance < nearest) {
                        best = Some((distance, marker.floor));
                    }
                }
            }
        }
        best.map(|(_, floor)| floor)
    }

    /// The player came by a teleport to this place: its floor is where they are,
    /// and the height they are at is noted for it.
    pub fn note_arrival(&mut self, scene_map: SceneMap, position: [f32; 3]) {
        self.index();
        if let Some(floor) = self.keep_anchor(scene_map, position) {
            self.arrival_floor = Some(floor);
        }
    }

    /// The places teleports have put the player at in this scene before: they are
    /// anchors too (worked out again if how the scene lies on the map changed).
    pub fn seed_anchors(&mut self, scene_map: SceneMap, places: &[[f32; 3]]) {
        if self.seeded.get(&scene_map.map) == Some(&scene_map) {
            return;
        }
        self.index();
        self.seeded.insert(scene_map.map, scene_map);
        self.anchors.retain(|anchor| anchor.map != scene_map.map);
        for place in places {
            self.keep_anchor(scene_map, *place);
        }
    }

    fn keep_anchor(&mut self, scene_map: SceneMap, position: [f32; 3]) -> Option<u32> {
        let at = scene_map.to_map(position);
        let floor = self.waypoint_floor(scene_map.map, at)?;
        let (key, most) = if floor != 0 { (floor, HEIGHTS) } else { (SURFACE + scene_map.map, 4 * HEIGHTS) };
        if !self.anchors.iter().any(|anchor| {
            anchor.map == scene_map.map && (anchor.at[0] - at[0]).hypot(anchor.at[1] - at[1]) < 3. && (anchor.height - position[1]).abs() < 3.
        }) {
            self.anchors.push(Anchor { map: scene_map.map, floor, at, height: position[1] });
            // What height this floor is at is known by this one.
            self.heights.entry(key).or_default().add(position[1], most);
            self.dirty = true;
        }
        Some(floor)
    }

    /// The floor of the marker a place is, if it is clearly one: the nearest
    /// within reach, and no marker of another floor nearly as near.
    fn floor_at(&self, map: u32, at: [f32; 2]) -> Option<u32> {
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let mut best: Option<(f32, u32)> = None;
        let mut other: Option<f32> = None;
        let mut found: Vec<(f32, u32)> = Vec::new();
        for gy in cy - 1..=cy + 1 {
            for gx in cx - 1..=cx + 1 {
                for marker in self.cells.get(&(map, gx, gy)).into_iter().flatten() {
                    let distance = (marker.x - at[0]).hypot(marker.y - at[1]);
                    if distance <= MATCH_RADIUS {
                        found.push((distance, marker.floor));
                    }
                }
            }
        }
        for (distance, floor) in found {
            match best {
                None => best = Some((distance, floor)),
                Some((nearest, nearest_floor)) if distance < nearest => {
                    if nearest_floor != floor {
                        other = Some(other.map_or(nearest, |o: f32| o.min(nearest)));
                    }
                    best = Some((distance, floor));
                }
                Some((_, nearest_floor)) => {
                    if nearest_floor != floor {
                        other = Some(other.map_or(distance, |o: f32| o.min(distance)));
                    }
                }
            }
        }
        let (distance, floor) = best?;
        // Another floor's marker nearly as near: it can't be told.
        if other.is_some_and(|other| other < distance * 1.2 + 0.5) {
            return None;
        }
        Some(floor)
    }

    /// Things came into view.  Where they are on a map is worked out when the
    /// player's place is next told.
    pub fn observe(&mut self, gadgets: &[SceneGadget]) {
        for gadget in gadgets {
            if let Some(position) = gadget.position {
                self.pending.push((position, 0));
            }
        }
        if self.pending.len() > 4 * KEEP {
            let drop = self.pending.len() - 2 * KEEP;
            self.pending.drain(..drop);
        }
    }

    /// Keeps the things that are at a marker, with the floor of it, and notes
    /// its height for the floor.
    fn take_in(&mut self, map: u32, scene_map: SceneMap) {
        self.index();
        let pending = std::mem::take(&mut self.pending);
        for (position, _) in pending {
            let at = scene_map.to_map(position);
            let Some(floor) = self.floor_at(map, at) else {
                continue;
            };
            // The same thing told again.
            if self.recent.iter().rev().take(64).any(|seen| {
                seen.map == map
                    && (seen.at[0] - at[0]).abs() < 0.5
                    && (seen.at[1] - at[1]).abs() < 0.5
                    && (seen.height - position[1]).abs() < 1.
            }) {
                continue;
            }
            // The heights things are at on each floor, and on the surface.
            let (key, most) = if floor != 0 { (floor, HEIGHTS) } else { (SURFACE + map, 4 * HEIGHTS) };
            self.heights.entry(key).or_default().add(position[1], most);
            self.dirty = true;
            self.recent.push_back(Seen {
                map,
                at,
                height: position[1],
                floor,
                when: Instant::now(),
            });
            while self.recent.len() > KEEP || self.recent.front().is_some_and(|seen| seen.when.elapsed() > FORGET) {
                self.recent.pop_front();
            }
        }
        self.save_now_and_then();
    }

    fn save_now_and_then(&mut self) {
        if !self.dirty || self.saved_at.elapsed() < SAVE_EVERY {
            return;
        }
        self.dirty = false;
        self.saved_at = Instant::now();
        let Some(path) = heights_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(&self.heights) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// The floor the player is on (0 is the surface), for where the game says
    /// the player is.
    pub fn locate(&mut self, map: u32, scene_map: SceneMap, position: [f32; 3]) -> u32 {
        // What came into view is of this scene, now that it is known which.
        self.take_in(map, scene_map);
        let data = self.index();
        let Some(def) = data.atlas.map(map) else {
            self.current = 0;
            self.challenger = None;
            return 0;
        };
        let [mx, my] = scene_map.to_map(position);

        // The floors whose picture covers the place.
        let mut candidates: Vec<u32> = Vec::new();
        for site in &def.sites {
            for floor in &site.floors {
                let rect = floor.rect;
                // Not the floors that are no layer of their own: unless a teleport to a
                // waypoint of the floor was gone to, and the player is by it and at its
                // height.  (Its things at all heights are then the surface's, matched
                // by mistake; that the player is by its waypoint is not.)
                if self.heights.get(&floor.id).is_some_and(Heights::is_spread)
                    && !self.anchors.iter().any(|anchor| {
                        anchor.map == map
                            && anchor.floor == floor.id
                            && (anchor.at[0] - mx).hypot(anchor.at[1] - my) <= ANCHOR_ACROSS
                            && (anchor.height - position[1]).abs() <= ANCHOR_HEIGHT
                    })
                {
                    continue;
                }
                if rect[2] > rect[0] && mx >= rect[0] && mx <= rect[2] && my >= rect[1] && my <= rect[3] {
                    candidates.push(floor.id);
                }
            }
        }
        if candidates.is_empty() {
            self.current = 0;
            self.challenger = None;
            return 0;
        }

        let mut score: HashMap<u32, f32> = HashMap::new();
        score.insert(0, 1.);
        for floor in &candidates {
            score.insert(*floor, 0.);
        }
        // What is around, at about the player's height.
        for seen in self.recent.iter().filter(|seen| seen.map == map && seen.when.elapsed() <= FORGET) {
            let across = (seen.at[0] - mx).hypot(seen.at[1] - my);
            if across > NEAR_ACROSS || (seen.height - position[1]).abs() > NEAR_HEIGHT {
                continue;
            }
            if let Some(total) = score.get_mut(&seen.floor) {
                *total += 1. / (1. + across / 30.);
            }
        }
        // The heights things are at on each floor, and on the surface.
        for floor in candidates.iter().copied().chain([0]) {
            let key = if floor == 0 { SURFACE + map } else { floor };
            if let Some(says) = self.heights.get(&key).map(|heights| heights.says(position[1]))
                && let Some(total) = score.get_mut(&floor)
            {
                *total += says;
            }
        }
        // The teleports gone to: by one, and at its height, is its floor.  Far above or
        // below one that is right by is another.
        for anchor in self.anchors.iter().filter(|anchor| anchor.map == map) {
            let across = (anchor.at[0] - mx).hypot(anchor.at[1] - my);
            let up = (anchor.height - position[1]).abs();
            let Some(total) = score.get_mut(&anchor.floor) else {
                continue;
            };
            if across <= ANCHOR_ACROSS && up <= ANCHOR_HEIGHT {
                // By it, and at its height: that is where the player is, whatever
                // else is told; the nearer, the surer.
                *total += 4. * (1. - 0.5 * across / ANCHOR_ACROSS);
            } else if across <= 30. && up >= ANCHOR_OTHER {
                *total -= 1.5;
            }
        }
        // What the minimap of the game shows is the picture of the floor the player is
        // on: that, when it is clear, is what counts most.
        if let Some(floor) = vision_of(map)
            && let Some(total) = score.get_mut(&floor)
        {
            *total += 5.;
        }
        // A lift or a fall: another floor, and it is not waited for to be sure.
        let jumped = self.last_height.is_some_and(|last| (last - position[1]).abs() > 25.);
        self.last_height = Some(position[1]);

        let mut best = score
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal).then(b.0.cmp(a.0)))
            .map(|(floor, _)| *floor)
            .unwrap_or(0);
        // A floor needs plenty of what is around it to be taken over the surface.
        if best != 0 && score[&best] < NEEDED {
            best = 0;
        }
        // Just teleported: the floor of the waypoint, if the player is where it is
        // (by it, at about its height), and not waited for to be sure.
        if let Some(floor) = self.arrival_floor.take()
            && score.contains_key(&floor)
            && self.anchors.iter().any(|anchor| {
                anchor.map == map
                    && anchor.floor == floor
                    && (anchor.at[0] - mx).hypot(anchor.at[1] - my) <= ANCHOR_ACROSS
                    && (anchor.height - position[1]).abs() <= ANCHOR_HEIGHT
            })
        {
            best = floor;
            self.current = floor;
            self.challenger = None;
            tracing::info!("the player teleported to a waypoint of floor {floor}");
        }
        let here = score.contains_key(&self.current);
        if best == self.current {
            self.challenger = None;
        } else if !here || jumped || score[&best] - score[&self.current] >= 2. {
            // The old floor can't be the one any more, or it is clear.
            self.current = best;
            self.challenger = None;
            tracing::info!("the player is on floor {best}");
        } else {
            // Told apart only after it holds for a second report.
            let count = match self.challenger {
                Some((floor, count)) if floor == best => count + 1,
                _ => 1,
            };
            if count >= 2 {
                self.current = best;
                self.challenger = None;
                tracing::info!("the player is on floor {best}");
            } else {
                self.challenger = Some((best, count));
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD_ID: u32 = crate::atlas::WORLD;

    /// The game's place of a place of the map.
    fn game(map_x: f32, map_y: f32, height: f32) -> [f32; 3] {
        [2321. - map_y, height, -(map_x + 750.)]
    }

    fn gadget(entity: u32, position: [f32; 3]) -> SceneGadget {
        SceneGadget {
            entity_id: entity,
            gadget_id: 70_000_000 + entity,
            group_id: 100_000_000 + entity,
            config_id: entity,
            position: Some(position),
            state: None,
        }
    }

    /// The game's place of a place of a map, in a scene shifted so.
    fn game_in(scene_map: SceneMap, map_x: f32, map_y: f32, height: f32) -> [f32; 3] {
        // The map's x is minus z plus a, its y minus x plus b.
        [scene_map.b - map_y, height, scene_map.a - map_x]
    }

    /// With things at the markers of a floor around, the player is on it.
    fn the_player_is_found_on_the_floor_of_a_map(map: u32, scene_map: SceneMap) {
        let data = markers::data();
        let def = data.atlas.map(map).unwrap();
        // A floor with a number of markers of its own: a layer, not a region.
        let floor = def
            .sites
            .iter()
            .flat_map(|site| site.floors.iter())
            .map(|floor| {
                let count = data.points.iter().filter(|point| u32::from(point.floor) == floor.id).count();
                (floor, count)
            })
            .filter(|(_, count)| (20..=400).contains(count))
            .max_by_key(|(_, count)| *count)
            .map(|(floor, _)| floor.id)
            .unwrap();
        let points: Vec<_> = data.points.iter().filter(|point| u32::from(point.floor) == floor).collect();
        let mut locator = FloorLocator::new();
        locator.heights.clear();
        let depth = -80.;
        let things: Vec<SceneGadget> = points
            .iter()
            .take(30)
            .enumerate()
            .map(|(index, point)| gadget(index as u32 + 1, game_in(scene_map, point.x, point.y, depth)))
            .collect();
        locator.observe(&things);
        let at = &points[0];
        let near = game_in(scene_map, at.x, at.y, depth);
        let found = locator.locate(map, scene_map, near);
        assert!(!locator.recent.is_empty(), "things at markers are kept");
        assert_eq!(found, floor, "the floor of what is around, on map {map}");

        // Where no floor's picture is, the player is on the surface.
        let mut far = FloorLocator::new();
        far.heights.clear();
        assert_eq!(far.locate(map, scene_map, game_in(scene_map, -20_000., -20_000., 200.)), 0);
    }

    #[test]
    fn the_player_is_on_the_floor_of_what_is_around() {
        // The world, and the Ancient Sacred Mountain (where the floors are in rooms
        // of the mountain, and the player arrives high up or inside).
        the_player_is_found_on_the_floor_of_a_map(WORLD_ID, SceneMap { map: WORLD_ID, a: -750., b: 2321., s: 1. });
        the_player_is_found_on_the_floor_of_a_map(36, SceneMap { map: 36, a: -152., b: 225., s: 1. });
    }

    #[test]
    fn a_thing_between_two_floors_is_not_told() {
        let locator = FloorLocator::new();
        // Nothing indexed: nothing is told.
        assert_eq!(locator.floor_at(2, [0., 0.]), None);
    }

    /// A teleport to a waypoint that is on a floor puts the player on that floor,
    /// and keeps them there near it and at its height; not far above it.
    #[test]
    fn a_teleport_puts_the_player_on_the_floor_of_its_waypoint() {
        let data = markers::data();
        // A waypoint of a floor of the Ancient Sacred Mountain.
        let waypoint = data
            .points
            .iter()
            .find(|point| point.map == 36 && (point.label == 2 || point.label == 3) && point.floor != 0)
            .expect("a waypoint on a floor");
        let floor = u32::from(waypoint.floor);
        let scene_map = SceneMap { map: 36, a: -152., b: 225., s: 1. };
        let there = game_in(scene_map, waypoint.x, waypoint.y, 60.);

        let mut locator = FloorLocator::new();
        locator.heights.clear();
        locator.note_arrival(scene_map, [there[0] + 2., there[1], there[2] + 2.]);
        // At once, the floor of the waypoint.
        assert_eq!(locator.locate(36, scene_map, there), floor);
        // A few steps on, the same height: still.
        let near = game_in(scene_map, waypoint.x + 15., waypoint.y, 62.);
        assert_eq!(locator.locate(36, scene_map, near), floor);
        // Right over it, but a hundred metres up: not that floor.
        let above = game_in(scene_map, waypoint.x, waypoint.y, 160.);
        assert_ne!(locator.locate(36, scene_map, above), floor);
    }

    /// A floor whose things were noted at all heights (matched by mistake) is
    /// still the floor of a waypoint that is gone to and stood by.
    #[test]
    fn a_floor_with_heights_all_over_is_found_by_its_waypoint() {
        let data = markers::data();
        let waypoint = data
            .points
            .iter()
            .find(|point| point.map == 36 && (point.label == 2 || point.label == 3) && point.floor != 0)
            .expect("a waypoint on a floor");
        let floor = u32::from(waypoint.floor);
        let scene_map = SceneMap { map: 36, a: -152., b: 225., s: 1. };
        let there = game_in(scene_map, waypoint.x, waypoint.y, 210.);
        let mut locator = FloorLocator::new();
        locator.heights.clear();
        // Heights from 3 to 860, as were noted for the floor and the surface.
        for height in 0..40 {
            locator.heights.entry(floor).or_default().add(3. + height as f32 * 22., 40);
            locator.heights.entry(SURFACE + 36).or_default().add(25. + height as f32 * 21., 160);
        }
        // Known from the teleports of the scene before, not from this one.
        locator.seed_anchors(scene_map, &[there]);
        assert_eq!(locator.locate(36, scene_map, [there[0] + 3., 210., there[2] + 2.]), floor);
    }
}
