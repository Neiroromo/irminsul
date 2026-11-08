//! What state the chests of the markers of the maps (other than the world) are in
//! in the game, so the map shows them as the game has them.
//!
//! The game sends the chests that are closed when the player is near (about a
//! hundred metres), and not the ones that are opened.  So the state of a chest
//! marker is told from that:
//!
//! * a chest of the game at the marker (the same kind, by where it is): it is
//!   closed, whatever was marked;
//! * a chest of the game that says it is opened (the state 102), or that the player
//!   opens: it is opened;
//! * the player walks by a marker, near enough for its chest to have been sent, and
//!   none was: it is opened.
//!
//! What is told is kept for each account, and given to the maps, which mark the
//! chests collected, or not, to be as in the game.  The world has its own way of
//! this (`chests`, with the places walked by).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use auto_artifactarium::{GadgetInteraction, SceneGadget};

use crate::atlas::WORLD;
use crate::chests::kinds;
use crate::markers::{self, CHEST_LABELS, Data};
use crate::profiles;
use crate::scenes::SceneMap;

/// A marker is looked at for being opened when the player is this near it (metres) at
/// most: within where the game sends chests.  Where the game is seen to send things
/// less far in a scene, it is that much less (but not under `LEAST_METRES`).
pub const NEAR_METRES: f32 = 90.;
/// The radius of the radar the user can choose (metres), from and to.
pub const RADAR_LEAST: u32 = 40;
pub const RADAR_MOST: u32 = 130;
static RADAR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(NEAR_METRES as u32);

/// How far around the player (metres) the markers are looked at, at most, as the
/// user chose.
pub fn radar() -> f32 {
    RADAR.load(std::sync::atomic::Ordering::Relaxed) as f32
}

pub fn set_radar(metres: u32) {
    RADAR.store(metres.clamp(RADAR_LEAST, RADAR_MOST), std::sync::atomic::Ordering::Relaxed);
}

const LEAST_METRES: f32 = 40.;
/// Before it is seen how far the game sends things in a scene.
const UNSEEN_METRES: f32 = 50.;
/// The farthest thing seen sent is this much farther than what is looked at.
const REACH_MARGIN: f32 = 20.;
/// The player must be in a scene this long before what was not sent counts.
const SETTLE: Duration = Duration::from_secs(10);
/// A chest of the game is the marker that is this near it (pixels of the map).
const MATCH: f32 = 14.;
const CELL: f32 = 16.;

struct ChestMarker {
    id: u32,
    x: f32,
    y: f32,
    tier: u8,
    floor: u32,
}

/// What the maps are given: the chests the game says are opened (and since when),
/// and those it says are closed.
#[derive(Clone, Debug, Default)]
pub struct ChestMarks {
    pub version: u64,
    pub uid: Option<u32>,
    pub opened: Vec<(u32, u64)>,
    pub closed: Vec<u32>,
}

pub struct ChestWatch {
    uid: Option<u32>,
    /// Marker: opened since (seconds), or `None` if closed.
    state: HashMap<u32, Option<u64>>,
    version: u64,
    dirty: bool,
    saved_at: Instant,
    data: Option<std::sync::Arc<Data>>,
    cells: HashMap<(u32, i32, i32), Vec<ChestMarker>>,
    /// In this connection: markers whose chest was sent, the marker of each chest of
    /// the game (by entity), how many times in a row a marker was walked by with no
    /// chest, the scenes that sent anything, and when the player came into the one
    /// they are in.
    seen: HashSet<u32>,
    entities: HashMap<u32, u32>,
    near: HashMap<u32, u8>,
    loaded: HashSet<u32>,
    scene: Option<(u32, Instant)>,
    /// Where the player was last told to be, and the farthest (metres across) that
    /// anything was seen to be sent, by scene.
    last: Option<(u32, [f32; 3], Instant)>,
    reach: HashMap<u32, f32>,
}

fn path(uid: u32) -> Option<PathBuf> {
    Some(profiles::profiles_dir().ok()?.join("chest_marks").join(format!("{uid}.json")))
}

impl ChestWatch {
    pub fn new() -> Self {
        Self {
            uid: None,
            state: HashMap::new(),
            version: 1,
            dirty: false,
            saved_at: Instant::now(),
            data: None,
            cells: HashMap::new(),
            seen: HashSet::new(),
            entities: HashMap::new(),
            near: HashMap::new(),
            loaded: HashSet::new(),
            scene: None,
            last: None,
            reach: HashMap::new(),
        }
    }

    /// The account whose chests these are.
    pub fn use_account(&mut self, uid: Option<u32>) {
        if self.uid == uid {
            return;
        }
        self.save(true);
        self.uid = uid;
        self.state.clear();
        if let Some(path) = uid.and_then(path)
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(book) = serde_json::from_str::<HashMap<u32, u64>>(&text)
        {
            self.state = book.into_iter().map(|(id, at)| (id, (at > 0).then_some(at))).collect();
        }
        self.version += 1;
    }

    /// A new connection to the game: what was sent in the one before says nothing.
    pub fn start_session(&mut self) {
        self.seen.clear();
        self.entities.clear();
        self.near.clear();
        self.loaded.clear();
        self.scene = None;
        self.last = None;
        self.reach.clear();
    }

    /// How far around the player (metres) the markers are looked at in a scene: as far
    /// as the game is seen to send things, but not farther than `NEAR_METRES`.
    pub fn range(&self, scene: u32) -> f32 {
        match self.reach.get(&scene) {
            Some(reach) => (reach - REACH_MARGIN).clamp(LEAST_METRES, radar().max(LEAST_METRES)),
            None => UNSEEN_METRES,
        }
    }

    fn save(&mut self, force: bool) {
        if !self.dirty || (!force && self.saved_at.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.dirty = false;
        self.saved_at = Instant::now();
        let Some(path) = self.uid.and_then(path) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let book: HashMap<u32, u64> = self.state.iter().map(|(id, at)| (*id, at.unwrap_or(0))).collect();
        if let Ok(bytes) = serde_json::to_vec(&book) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// The index of the chest markers of the maps (other than the world).
    fn index(&mut self) -> std::sync::Arc<Data> {
        let data = markers::data();
        if self.data.as_ref().is_some_and(|made| std::sync::Arc::ptr_eq(made, &data)) {
            return data;
        }
        self.cells.clear();
        for point in data.points.iter().filter(|point| u32::from(point.map) != WORLD) {
            let Some(tier) = CHEST_LABELS.iter().position(|label| *label == point.label) else {
                continue;
            };
            self.cells
                .entry((u32::from(point.map), (point.x / CELL).floor() as i32, (point.y / CELL).floor() as i32))
                .or_default()
                .push(ChestMarker {
                    id: point.id,
                    x: point.x,
                    y: point.y,
                    tier: tier as u8 + 1,
                    floor: u32::from(point.floor),
                });
        }
        self.data = Some(data.clone());
        data
    }

    fn set(&mut self, id: u32, opened: Option<u64>) {
        if self.state.get(&id) != Some(&opened) {
            self.state.insert(id, opened);
            self.version += 1;
            self.dirty = true;
        }
    }

    /// The marker of a kind of chest at a place of a map: the nearest within reach,
    /// and not another nearly as near.
    fn marker_at(&self, map: u32, at: [f32; 2], tier: u8) -> Option<u32> {
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let mut found: Vec<(f32, u32)> = Vec::new();
        for gy in cy - 1..=cy + 1 {
            for gx in cx - 1..=cx + 1 {
                for marker in self.cells.get(&(map, gx, gy)).into_iter().flatten() {
                    let distance = (marker.x - at[0]).hypot(marker.y - at[1]);
                    if marker.tier == tier && distance <= MATCH {
                        found.push((distance, marker.id));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        match found.as_slice() {
            [] => None,
            [(_, id)] => Some(*id),
            [(first, id), (second, _), ..] => (*second >= *first * 1.5 || *first < 3.).then_some(*id),
        }
    }

    /// Things came into view in a scene.  The chests among them are the chests of
    /// the markers.
    pub fn gadgets(&mut self, scene: u32, scene_map: SceneMap, gadgets: &[SceneGadget]) {
        if scene_map.map == WORLD {
            return;
        }
        self.index();
        self.loaded.insert(scene);
        // How far from the player the game sends things (anything it sends tells).
        if let Some((was, at, when)) = self.last
            && was == scene
            && when.elapsed() < Duration::from_secs(20)
        {
            for gadget in gadgets {
                if let Some(position) = gadget.position {
                    let distance = (position[0] - at[0]).hypot(position[2] - at[2]);
                    if distance < 250. {
                        let reach = self.reach.entry(scene).or_insert(0.);
                        *reach = reach.max(distance);
                    }
                }
            }
        }
        let now = markers::unix_now();
        for gadget in gadgets {
            let Some(kind) = kinds().get(&gadget.gadget_id).filter(|kind| kind.tier > 0) else {
                continue;
            };
            let Some(position) = gadget.position else {
                continue;
            };
            let Some(id) = self.marker_at(scene_map.map, scene_map.to_map(position), kind.tier) else {
                continue;
            };
            self.entities.insert(gadget.entity_id, id);
            self.seen.insert(id);
            self.near.remove(&id);
            // Sent as opened (some scenes do), or as closed, which it is.
            self.set(id, (gadget.state == Some(102)).then_some(now));
        }
        self.save(false);
    }

    /// A chest was opened by the player.
    pub fn interaction(&mut self, event: &GadgetInteraction) {
        if event.kind == 3
            && !event.begins
            && let Some(id) = self.entities.get(&event.entity_id).copied()
        {
            self.set(id, Some(markers::unix_now()));
            self.save(false);
        }
    }

    /// The player is here (in a scene known to be on a map, on this floor of it).
    pub fn player_at(&mut self, scene: u32, scene_map: SceneMap, position: [f32; 3], floor: u32) {
        if scene_map.map == WORLD {
            return;
        }
        self.last = Some((scene, position, Instant::now()));
        if self.scene.is_none_or(|(was, _)| was != scene) {
            self.scene = Some((scene, Instant::now()));
            self.near.clear();
        }
        let settled = self.scene.is_some_and(|(_, since)| since.elapsed() >= SETTLE);
        if !settled || !self.loaded.contains(&scene) {
            return;
        }
        self.index();
        let at = scene_map.to_map(position);
        let radius = self.range(scene) * scene_map.s;
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let reach = (radius / CELL).ceil() as i32 + 1;
        let mut here: Vec<(u32, u8)> = Vec::new();
        for gy in cy - reach..=cy + reach {
            for gx in cx - reach..=cx + reach {
                for marker in self.cells.get(&(scene_map.map, gx, gy)).into_iter().flatten() {
                    // Only markers of the floor the player is on: a chest of another floor
                    // over or under is not sent.
                    let distance = (marker.x - at[0]).hypot(marker.y - at[1]);
                    if marker.floor == floor && distance <= radius {
                        // Far (where the radar is wide) it takes one report more.
                        here.push((marker.id, if distance <= 0.6 * radius.min(50. * scene_map.s) { 2 } else { 3 }));
                    }
                }
            }
        }
        let mut changed = false;
        for (id, needed) in &here {
            if self.seen.contains(id) {
                continue;
            }
            let times = self.near.entry(*id).or_insert(0);
            *times = times.saturating_add(1);
            // Twice in a row (three if far): not a chest on its way.
            if *times >= *needed && !matches!(self.state.get(id), Some(Some(_))) {
                changed = true;
                let now = markers::unix_now();
                self.set(*id, Some(now));
            }
        }
        // What is not near any more starts again.
        self.near.retain(|id, _| here.iter().any(|(near, _)| near == id));
        if changed {
            self.save(false);
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// What the maps are given.
    pub fn snapshot(&self) -> ChestMarks {
        let mut opened = Vec::new();
        let mut closed = Vec::new();
        for (id, state) in &self.state {
            match state {
                Some(at) => opened.push((*id, *at)),
                None => closed.push(*id),
            }
        }
        ChestMarks { version: self.version, uid: self.uid, opened, closed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chest marker of a map that is not the world, with the scene put on it.
    fn some_marker(data: &Data) -> (u32, f32, f32, u8, u32) {
        let point = data
            .points
            .iter()
            .find(|point| u32::from(point.map) == 7 && point.floor == 0 && point.label == CHEST_LABELS[0])
            .expect("a common chest in Enkanomiya");
        (u32::from(point.map), point.x, point.y, 1, point.id)
    }

    fn game_of(scene_map: SceneMap, x: f32, y: f32) -> [f32; 3] {
        [(scene_map.b - y) / scene_map.s, 100., (scene_map.a - x) / scene_map.s]
    }

    fn chest(entity: u32, position: [f32; 3], state: Option<u32>) -> SceneGadget {
        SceneGadget {
            entity_id: entity,
            gadget_id: 70_211_002,
            group_id: 155_000_000 + entity,
            config_id: entity,
            position: Some(position),
            state,
        }
    }

    #[test]
    fn a_chest_the_game_does_not_send_by_a_marker_walked_by_is_opened() {
        let data = markers::data();
        let (map, x, y, _tier, id) = some_marker(&data);
        let scene_map = SceneMap { map, a: 198.5, b: 267., s: 1. };
        let mut watch = ChestWatch::new();
        watch.uid = None;
        // The scene sent things (not that chest), and the player has been in it a while.
        watch.gadgets(5, scene_map, &[chest(1, game_of(scene_map, x + 500., y + 500.), None)]);
        watch.scene = Some((5, Instant::now() - Duration::from_secs(60)));
        let here = game_of(scene_map, x + 10., y);
        watch.player_at(5, scene_map, here, 0);
        assert!(!matches!(watch.state.get(&id), Some(Some(_))), "once is not enough");
        watch.player_at(5, scene_map, here, 0);
        assert!(matches!(watch.state.get(&id), Some(Some(_))), "walked by twice, not sent: opened");
        // Sent as closed later: it is closed.
        watch.gadgets(5, scene_map, &[chest(2, game_of(scene_map, x + 2., y - 1.), None)]);
        assert_eq!(watch.state.get(&id), Some(&None));
    }

    #[test]
    fn a_chest_that_was_sent_is_closed_and_one_that_is_opened_is_opened() {
        let data = markers::data();
        let (map, x, y, _tier, id) = some_marker(&data);
        let scene_map = SceneMap { map, a: 198.5, b: 267., s: 1. };
        let mut watch = ChestWatch::new();
        watch.gadgets(5, scene_map, &[chest(1, game_of(scene_map, x, y), None)]);
        assert_eq!(watch.state.get(&id), Some(&None));
        // Walked by, with the chest sent: closed, not opened.
        watch.scene = Some((5, Instant::now() - Duration::from_secs(60)));
        for _ in 0..3 {
            watch.player_at(5, scene_map, game_of(scene_map, x + 5., y), 0);
        }
        assert_eq!(watch.state.get(&id), Some(&None));
        // Opened by the player.
        watch.interaction(&GadgetInteraction { gadget_id: 70_211_002, entity_id: 1, kind: 3, begins: false });
        assert!(matches!(watch.state.get(&id), Some(Some(_))));
        // Sent as opened (state 102).
        let mut other = ChestWatch::new();
        other.gadgets(5, scene_map, &[chest(1, game_of(scene_map, x, y), Some(102))]);
        assert!(matches!(other.state.get(&id), Some(Some(_))));
    }

    #[test]
    fn the_world_is_not_looked_at() {
        let scene_map = SceneMap { map: WORLD, a: -750., b: 2321., s: 1. };
        let mut watch = ChestWatch::new();
        watch.gadgets(3, scene_map, &[chest(1, [0., 0., 0.], None)]);
        assert!(watch.state.is_empty());
        watch.player_at(3, scene_map, [0., 0., 0.], 0);
        assert!(watch.state.is_empty());
    }

    #[test]
    fn how_far_is_looked_at_follows_how_far_the_game_is_seen_to_send() {
        let scene_map = SceneMap { map: 7, a: 198.5, b: 267., s: 1. };
        let mut watch = ChestWatch::new();
        assert_eq!(watch.range(5), UNSEEN_METRES);
        let here = [0., 100., 0.];
        watch.scene = Some((5, Instant::now() - Duration::from_secs(60)));
        watch.player_at(5, scene_map, here, 0);
        // A thing 75 m off was sent: 55 m is looked at.
        watch.gadgets(5, scene_map, &[chest(1, [75., 100., 0.], None)]);
        assert_eq!(watch.range(5), 55.);
        // One 200 m off: no more than the most.
        watch.gadgets(5, scene_map, &[chest(2, [200., 100., 0.], None)]);
        assert_eq!(watch.range(5), NEAR_METRES);
    }
}
