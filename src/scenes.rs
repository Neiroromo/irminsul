//! Which map of the official interactive map a scene of the game is, and where
//! on it a place of the scene is.
//!
//! The world (scene 3) and every other place the game has (the Chasm
//! underground, Enkanomiya, the Sea of Bygone Eras and the rest) are scenes of
//! their own, each with its own coordinates, and the game does not say which map
//! of the official map each is.  What it does is put things into view: chests,
//! plants, waypoints, each at a place of the scene.  The official map has
//! markers at the same places, on its own map.  So for a scene that is not
//! known, the places of what comes into view are laid over the markers of each
//! map, shifted every way, until many of them fall on markers: that map, and that
//! shift, are the scene's.  (The directions are the same in all the maps: the
//! map's x is minus the game's z, and its y minus the game's x.)
//!
//! What is learned is kept, so the next time the scene is known at once.

use std::collections::HashMap;
use std::path::PathBuf;

use auto_artifactarium::SceneGadget;
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::atlas::WORLD;
use crate::markers;

/// How near to a marker a thing is to be on it, in pixels of the map (the
/// things that are on a marker are within about one of it, the ones that are not, anywhere in a wider ring).
const NEAR: f32 = 1.5;
/// The size of the places votes are gathered in.
const BIN: f32 = 2.;
/// How many things a scene gathers before it is looked at, and how many more
/// before it is looked at again.
const FIRST_LOOK: usize = 25;
const MORE: usize = 25;
const KEEP: usize = 600;
/// At most this many looks for a scene (a place that is on no map is not looked
/// at for ever).
const LOOKS: usize = 6;

/// Where a scene is on a map: the map, and how its places are shifted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneMap {
    pub map: u32,
    pub a: f32,
    pub b: f32,
    /// How many pixels of the map are a metre of the scene: 1 for most maps (as in
    /// the world), but not all (the Frost Moon has about 0.57).
    #[serde(default = "one")]
    pub s: f32,
}

fn one() -> f32 {
    1.
}

impl SceneMap {
    /// A place of the scene (x, height, z of the game) on the map, from the
    /// origin of the map.
    pub fn to_map(&self, position: [f32; 3]) -> [f32; 2] {
        [-self.s * position[2] + self.a, -self.s * position[0] + self.b]
    }
}

/// What is known without looking: the world, and the Chasm underground (which
/// was found from what was put into view there).
fn built_in() -> HashMap<u32, SceneMap> {
    HashMap::from([
        (3, SceneMap { map: WORLD, a: -750., b: 2321., s: 1. }),
        // Found from what the game put into view, and checked by where the player
        // arrives after a teleport (7 to 17 metres from a waypoint, as it is
        // everywhere): the log of 2026-11-08.
        (5, SceneMap { map: 7, a: 198.5, b: 267.0, s: 1. }),
        (6, SceneMap { map: 9, a: 545.0, b: 606.0, s: 1. }),
        (11, SceneMap { map: 34, a: 4000.7, b: 1445.9, s: 1. }),
        (101, SceneMap { map: 36, a: -152.0, b: 225.0, s: 1. }),
        // The Temple of Space, from waypoints pinned by hand (see `calibrate`).
        // (Its scale is 0.947: from three waypoints the user pinned, and all five
        // waypoints that were teleported to come out within 25 metres of one.)
        (103, SceneMap { map: 37, a: 391.3, b: 157.5, s: 0.947 }),
        // The Frost Moon is not at the scale of one: found from three teleports that
        // the user pinned to their waypoints by hand (the game's places against the
        // waypoints' places on the map are 0.574 of a pixel to a metre, to 4 pixels
        // at worst), and 109 of 374 things on markers at that scale (about 15 by
        // chance).
        (102, SceneMap { map: 40, a: 3973.0, b: -352.1, s: 0.5742 }),
    ])
}

/// What map a scene is most likely, from the order the places were gone to in
/// the log of 2026-11-08 (Enkanomiya, the Chasm, the Sea of Bygone Eras, the
/// Ancient Sacred Mountain, the Temple of Space, the Frost Moon went with the
/// scenes 5, 6, 11, 101, 103, 102).  Only a hint: it says which map to look at
/// first, and is not taken for true.
fn hint(scene: u32) -> Option<u32> {
    match scene {
        5 => Some(7),
        11 => Some(34),
        101 => Some(36),
        103 => Some(37),
        102 => Some(40),
        _ => None,
    }
}

/// A place of a scene that the user said: they stood by a marker and said so.
/// Asked for from the map (another thread), made when the player's place is next
/// told.
enum Request {
    Set(u32, u32, Point),
    Reset(u32),
}

/// A place that the user pinned: the game says the player is at `position`, and
/// the player is at `marker` of the map.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Point {
    marker: [f32; 2],
    position: [f32; 3],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UserScene {
    map: u32,
    points: Vec<Point>,
}

impl UserScene {
    /// How the scene lies on the map, from the places that were pinned.  One place
    /// gives the shift (at the scale `base`); two or more that are far apart give
    /// the scale too (by least squares), if they agree with each other.
    fn fit(&self, base: f32) -> Option<SceneMap> {
        let last = self.points.last()?;
        let alone = SceneMap {
            map: self.map,
            a: last.marker[0] + base * last.position[2],
            b: last.marker[1] + base * last.position[0],
            s: base,
        };
        // Far enough apart to tell the scale: some pair of places 150 metres apart.
        let apart = self.points.iter().any(|first| {
            self.points
                .iter()
                .any(|second| (first.position[0] - second.position[0]).hypot(first.position[2] - second.position[2]) >= 150.)
        });
        if self.points.len() < 2 || !apart {
            return Some(alone);
        }
        // The map's x is minus s times z plus a, its y minus s times x plus b:
        // normal equations of the three unknowns (s, a, b).
        let mut matrix = [[0f64; 3]; 3];
        let mut vector = [0f64; 3];
        for point in &self.points {
            for (row, y) in [
                ([-f64::from(point.position[2]), 1., 0.], f64::from(point.marker[0])),
                ([-f64::from(point.position[0]), 0., 1.], f64::from(point.marker[1])),
            ] {
                for i in 0..3 {
                    for j in 0..3 {
                        matrix[i][j] += row[i] * row[j];
                    }
                    vector[i] += row[i] * y;
                }
            }
        }
        for i in 0..3 {
            let pivot = matrix[i][i];
            if pivot.abs() < 1e-9 {
                return Some(alone);
            }
            for j in i..3 {
                matrix[i][j] /= pivot;
            }
            vector[i] /= pivot;
            for k in 0..3 {
                if k != i {
                    let factor = matrix[k][i];
                    for j in i..3 {
                        matrix[k][j] -= factor * matrix[i][j];
                    }
                    vector[k] -= factor * vector[i];
                }
            }
        }
        let fitted = SceneMap {
            map: self.map,
            s: vector[0] as f32,
            a: vector[1] as f32,
            b: vector[2] as f32,
        };
        // The places must agree (to 30 pixels) and the scale be one that can be
        // (a click on the wrong marker is not taken).
        let agree = self.points.iter().all(|point| {
            let at = fitted.to_map(point.position);
            (at[0] - point.marker[0]).hypot(at[1] - point.marker[1]) <= 30.
        });
        if agree && (0.2..=5.).contains(&fitted.s) { Some(fitted) } else { Some(alone) }
    }
}

fn requests() -> &'static std::sync::Mutex<Vec<Request>> {
    static REQUESTS: std::sync::OnceLock<std::sync::Mutex<Vec<Request>>> = std::sync::OnceLock::new();
    REQUESTS.get_or_init(Default::default)
}

/// The player stands by this marker of this map (its place on the map), in this
/// scene, and the game says they are at this place: so that is how the scene lies
/// on the map.  Good to a few metres.
pub fn calibrate(scene: u32, map: u32, marker: [f32; 2], position: [f32; 3]) {
    if let Ok(mut list) = requests().lock() {
        list.push(Request::Set(scene, map, Point { marker, position }));
    }
}

/// Takes back what was said for a scene.
pub fn reset(scene: u32) {
    if let Ok(mut list) = requests().lock() {
        list.push(Request::Reset(scene));
    }
}

/// Where what the user said is kept.  `IRMINSUL_SCENE_CALIBRATIONS` replaces it.
fn calibrations_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_SCENE_CALIBRATIONS") {
        return Some(path.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("scene_calibrations.json"))
}

/// Where the points teleported to are kept.  `IRMINSUL_TELEPORTS` replaces it.
fn teleports_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_TELEPORTS") {
        return Some(path.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("teleport_points.json"))
}

/// Where what was learned is kept.  `IRMINSUL_SCENE_MAPS` replaces it.
/// What was gathered of a scene that is not known, kept in the log folder, so
/// that it can be looked at (which map it is, and how its places are shifted)
/// when the program could not tell.
fn dump_pool(scene: u32, pool: &[[f32; 3]], player: [f32; 3], hint: Option<u32>, look: usize) {
    let Ok(dir) = crate::log_dir() else {
        return;
    };
    let text = serde_json::json!({
        "scene": scene,
        "hint": hint,
        "look": look,
        "player": player,
        "things": pool,
    });
    let _ = std::fs::write(dir.join(format!("scene-{scene}-things.json")), text.to_string());
}

fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_SCENE_MAPS") {
        return Some(path.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("scene_maps.json"))
}

pub struct Scenes {
    /// The scene the player is in, as far as is told: by where they came in, and by
    /// where they are said to be.
    now: Option<u32>,
    known: HashMap<u32, SceneMap>,
    /// The point of a scene the player was last told to teleport to, and the place
    /// each point put them at (scene, point, place of the game): the points are the
    /// waypoints, so these are waypoints of the scene, with their places in its
    /// coordinates.
    teleport_to: Option<(u32, u32)>,
    teleports: HashMap<u32, HashMap<u32, [f32; 3]>>,
    /// What the user said (the places they pinned), and how the scenes lie from
    /// them; it is before all else.
    user_points: HashMap<u32, UserScene>,
    user: HashMap<u32, SceneMap>,
    /// What came into view, for each scene that is not known.
    pools: HashMap<u32, Vec<[f32; 3]>>,
    /// What came into view since the player's place was last told.
    pending: Vec<[f32; 3]>,
    looked_at: HashMap<u32, (usize, usize)>,
    last_scene: Option<u32>,
    /// Where the player was first told to be, each time they came into a scene.
    arrival: HashMap<u32, [f32; 3]>,
    /// What a look found, which is taken only when the next look finds it again
    /// (a shift that was by chance is not found again).
    candidates: HashMap<u32, (u32, f32, f32)>,
    /// What is learned is kept in a file.
    persist: bool,
}

/// Where the player is, as far as it can be told.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub map: u32,
    pub at: [f32; 2],
    pub scale: f32,
}

impl Scenes {
    pub fn new() -> Self {
        let mut known = built_in();
        if let Some(path) = path()
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(learned) = serde_json::from_str::<HashMap<u32, SceneMap>>(&text)
        {
            known.extend(learned);
        }
        // What was checked is not replaced by what was learned.
        known.extend(built_in());
        let text = calibrations_path().and_then(|path| std::fs::read_to_string(path).ok());
        let user_points: HashMap<u32, UserScene> = text
            .as_deref()
            .and_then(|text| serde_json::from_str::<HashMap<u32, UserScene>>(text).ok())
            .unwrap_or_default();
        let teleports: HashMap<u32, HashMap<u32, [f32; 3]>> = teleports_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let user: HashMap<u32, SceneMap> = user_points
            .iter()
            .filter_map(|(scene, user)| {
                let base = known.get(scene).map_or(1., |found| found.s);
                user.fit(base).map(|found| (*scene, found))
            })
            .collect();
        Self {
            now: None,
            known,
            teleport_to: None,
            teleports,
            user_points,
            user,
            arrival: HashMap::new(),
            candidates: HashMap::new(),
            pools: HashMap::new(),
            pending: Vec::new(),
            looked_at: HashMap::new(),
            last_scene: None,
            persist: true,
        }
    }

    /// Without the file: what is known is what the program has.
    #[cfg(test)]
    fn in_memory() -> Self {
        Self {
            known: built_in(),
            user: HashMap::new(),
            user_points: HashMap::new(),
            teleports: HashMap::new(),
            persist: false,
            ..Self::new()
        }
    }

    /// The scene the player is in, if it is known.
    pub fn scene_now(&self) -> Option<u32> {
        self.now
    }

    pub fn map_of(&self, scene: u32) -> Option<SceneMap> {
        self.user.get(&scene).or_else(|| self.known.get(&scene)).copied()
    }

    /// Does what the user asked for (see `calibrate`); says what it did.
    fn apply_requests(&mut self) -> Option<String> {
        let list = std::mem::take(&mut *requests().lock().ok()?);
        let mut said = Vec::new();
        for request in list {
            match request {
                Request::Set(scene, map, point) => {
                    // How far the program had the player from the marker pinned: a pin is
                    // for when it is wrong, but a long way off is likely a wrong marker.
                    let before = self.map_of(scene).filter(|found| found.map == map).map(|found| {
                        let at = found.to_map(point.position);
                        (at[0] - point.marker[0]).hypot(at[1] - point.marker[1])
                    });
                    if let Some(apart) = before.filter(|apart| *apart > 60.) {
                        said.push(format!(
                            "the marker pinned is {apart:.0} pixels from where the program had the player"
                        ));
                    }
                    // A place pinned again at the same spot is the one before; places of
                    // another map are not kept with these.
                    let user = self.user_points.entry(scene).or_default();
                    if user.map != map {
                        user.points.clear();
                        user.map = map;
                    }
                    user.points.retain(|other| {
                        (other.position[0] - point.position[0]).hypot(other.position[2] - point.position[2]) > 5.
                    });
                    user.points.push(point);
                    let base = self.known.get(&scene).map_or(1., |found| found.s);
                    let found = user.fit(base);
                    if let Some(found) = found {
                        said.push(format!(
                            "scene {scene} set by hand to map {} (shift {:.1}, {:.1}, scale {:.3}, from {} places)",
                            found.map,
                            found.a,
                            found.b,
                            found.s,
                            self.user_points.get(&scene).map_or(0, |user| user.points.len())
                        ));
                        self.user.insert(scene, found);
                    }
                    self.pools.remove(&scene);
                    self.candidates.remove(&scene);
                }
                Request::Reset(scene) => {
                    self.user_points.remove(&scene);
                    if self.user.remove(&scene).is_some() {
                        said.push(format!("what was set by hand for scene {scene} is taken back"));
                    }
                }
            }
        }
        if said.is_empty() {
            return None;
        }
        if self.persist
            && let Some(path) = calibrations_path()
        {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(bytes) = serde_json::to_vec(&self.user_points) {
                let _ = std::fs::write(path, bytes);
            }
        }
        Some(said.join("; "))
    }

    /// The game says the player is to teleport to this point of this scene (a
    /// waypoint: the same point has the same number every time).
    pub fn note_teleport(&mut self, scene: u32, point: u32) {
        self.teleport_to = Some((scene, point));
    }

    /// The player came into this scene at this place.  If they came by a teleport,
    /// the place is that of the point, and kept; when there are enough of them
    /// and the scene is not on a map yet, the waypoints are laid on the map's.
    /// Says what it did, if it did something.
    pub fn note_arrival(&mut self, scene: u32, position: [f32; 3]) -> Option<String> {
        self.now = Some(scene);
        let (target_scene, point) = self.teleport_to.take()?;
        if target_scene != scene {
            return None;
        }
        let known = self.teleports.entry(scene).or_default();
        let new = known.insert(point, position) != Some(position);
        let mut said = format!("teleport to point {point} of scene {scene}: arrives at ({:.1}, {:.1}, {:.1})", position[0], position[1], position[2]);
        if new && self.persist {
            if let Some(path) = teleports_path() {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(bytes) = serde_json::to_vec(&self.teleports) {
                    let _ = std::fs::write(path, bytes);
                }
            }
        }
        if new && self.map_of(scene).is_none() {
            let places: Vec<[f32; 3]> = self.teleports.get(&scene).map(|points| points.values().copied().collect()).unwrap_or_default();
            if let Some((found, matched)) = from_waypoints(&places) {
                said.push_str(&format!(
                    "; scene {scene} is map {} (scale {:.3}, shift {:.1}, {:.1}): {matched} of {} waypoints match",
                    found.map,
                    found.s,
                    found.a,
                    found.b,
                    places.len()
                ));
                self.known.insert(scene, found);
                self.pools.remove(&scene);
                self.save();
            }
        }
        Some(said)
    }

    /// The places teleports have put the player at in this scene.
    pub fn teleport_places(&self, scene: u32) -> Vec<[f32; 3]> {
        self.teleports.get(&scene).map(|points| points.values().copied().collect()).unwrap_or_default()
    }

    /// Things came into view.
    pub fn note_gadgets(&mut self, gadgets: &[SceneGadget]) {
        for gadget in gadgets {
            if let Some(position) = gadget.position {
                self.pending.push(position);
            }
        }
        if self.pending.len() > 4 * KEEP {
            let drop = self.pending.len() - 2 * KEEP;
            self.pending.drain(..drop);
        }
    }

    /// The game said where the player is.  Returns where that is on a map, if
    /// the scene is one of a map; and a line to say, if something was learned.
    pub fn locate(&mut self, scene: u32, position: [f32; 3]) -> (Option<Placed>, Option<String>) {
        let things = std::mem::take(&mut self.pending);
        let applied = self.apply_requests();
        let mut said = None;
        self.now = Some(scene);
        if self.last_scene != Some(scene) {
            self.arrival.insert(scene, position);
        }
        if self.map_of(scene).is_none() {
            // What came into view since the last report is of this scene (or of the
            // one before it, if the player just came; the few of those do not
            // matter to the vote).
            let pool = self.pools.entry(scene).or_default();
            for thing in things {
                if !pool.iter().rev().take(40).any(|other| *other == thing) {
                    pool.push(thing);
                }
            }
            if pool.len() > KEEP {
                let drop = pool.len() - KEEP;
                pool.drain(..drop);
            }
            let count = pool.len();
            let (looks, seen) = self.looked_at.get(&scene).copied().unwrap_or((0, 0));
            let due = if looks == 0 { count >= FIRST_LOOK } else { count >= seen + MORE };
            if due && looks < LOOKS {
                self.looked_at.insert(scene, (looks + 1, count));
                let pool = self.pools.get(&scene).cloned().unwrap_or_default();
                // The map the scene is thought to be is looked at first; if it is not
                // that, all of them.
                let arrival = self.arrival.get(&scene).copied();
                let (mut result, mut report) = learn(&pool, hint(scene), arrival);
                if result.is_none() && hint(scene).is_some() {
                    let (all, all_report) = learn(&pool, None, arrival);
                    result = all;
                    report = format!("{report} | all maps: {all_report}");
                }
                dump_pool(scene, &pool, position, hint(scene), looks + 1);
                // Taken when a look before found the same.
                if let Some((found, _)) = result {
                    let again = self
                        .candidates
                        .get(&scene)
                        .is_some_and(|(map, a, b)| *map == found.map && (a - found.a).hypot(b - found.b) <= 3.);
                    if !again {
                        self.candidates.insert(scene, (found.map, found.a, found.b));
                        said = Some(format!(
                            "scene {scene} looks like map {} (shift {:.1}, {:.1}): taken if the next look finds the same",
                            found.map, found.a, found.b
                        ));
                        result = None;
                    }
                }
                match result {
                    Some((found, hits)) => {
                        said = Some(format!(
                            "scene {scene} is map {}: {hits} of {} things are on its markers (shift {:.1}, {:.1}); {report}",
                            found.map,
                            pool.len(),
                            found.a,
                            found.b
                        ));
                        self.known.insert(scene, found);
                        self.pools.remove(&scene);
                        self.save();
                    }
                    None => {
                        if said.is_none() {
                            said = Some(format!(
                                "scene {scene} is on no map yet ({} things looked at, look {}); {report}",
                                pool.len(),
                                looks + 1
                            ));
                        }
                    }
                }
            }
        }
        if self.last_scene != Some(scene) {
            self.last_scene = Some(scene);
            let note = format!(
                "the player is in scene {scene}{}",
                self.map_of(scene).map_or(" (not on a map known)".to_owned(), |found| format!(" (map {})", found.map))
            );
            said = Some(said.map_or(note.clone(), |text| format!("{note}; {text}")));
        }
        let placed = self.map_of(scene).map(|found| Placed {
            map: found.map,
            at: found.to_map(position),
            scale: found.s,
        });
        let said = match (applied, said) {
            (Some(first), Some(second)) => Some(format!("{first}; {second}")),
            (first, second) => first.or(second),
        };
        (placed, said)
    }

    fn save(&self) {
        if !self.persist {
            return;
        }
        let Some(path) = path() else {
            return;
        };
        let built = built_in();
        let learned: HashMap<&u32, &SceneMap> = self.known.iter().filter(|(scene, found)| built.get(scene) != Some(found)).collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(&learned) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

/// A shift that puts many of the things on markers of a map.
struct Fit {
    map: u32,
    hits: usize,
    a: f32,
    b: f32,
    /// How far the place the player came in at is from a waypoint of the map, at
    /// this shift (None if not known where the player came in).
    from_waypoint: Option<f32>,
}

/// The ways the axes of a scene could be laid on those of a map: the first is the
/// way they are in the game's maps (the map's x is minus z, its y minus x); the
/// others are the turns and mirrors of it.
fn laid(way: usize, thing: &[f32; 3]) -> [f32; 2] {
    let (x, z) = (thing[0], thing[2]);
    match way {
        0 => [-z, -x],
        1 => [z, x],
        2 => [x, z],
        3 => [-x, -z],
        4 => [x, -z],
        5 => [-x, z],
        6 => [z, -x],
        _ => [-z, x],
    }
}

/// At most this many things are worked with (every few of the rest are left
/// out), to keep the looking short.
const MOST: usize = 300;
/// How far from a waypoint the player can be told to be when they came in: after
/// a teleport they are some metres from it, and a few seconds go before they are
/// told where they are.
const FROM_WAYPOINT: f32 = 20.;

/// How a scene lies on a map, from the places teleports have put the player at
/// (each is a waypoint of the scene): the waypoints of a map that these places fit.
/// At least four places, not all near one another; the layout of the axes is
/// that of all the maps, and the scale is looked for (it is not always one).
/// Taken only when all (or all but one of many) fit within 25 pixels of a
/// waypoint of the map, and no other map does as well.
fn from_waypoints(places: &[[f32; 3]]) -> Option<(SceneMap, usize)> {
    if places.len() < 4 {
        return None;
    }
    let data = markers::data();
    let mut results: Vec<(usize, f32, SceneMap)> = Vec::new();
    for def in data.atlas.maps.iter().filter(|def| def.id != WORLD) {
        let waypoints: Vec<[f32; 2]> = data
            .points
            .iter()
            .filter(|point| u32::from(point.map) == def.id && (point.label == 2 || point.label == 3))
            .map(|point| [point.x, point.y])
            .collect();
        if waypoints.len() < 3 {
            continue;
        }
        if let Some(found) = fit_waypoints(def.id, places, &waypoints) {
            results.push(found);
        }
    }
    results.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
    #[cfg(test)]
    for (m, e, f) in &results {
        eprintln!("map {} matched {m} error {e:.1} scale {:.3} shift {:.1},{:.1}", f.map, f.s, f.a, f.b);
    }
    let (matched, _, found) = *results.first()?;
    // Nearly all of them, and no other map nearly as good.
    // (Three places are not enough: with the scale not known, a pair of places fits
    // some pair of waypoints on any map, and the third lands on a waypoint by
    // chance now and then.  With four, that is rare.)
    let enough = matched >= 4 && (matched == places.len() || (places.len() >= 6 && matched + 1 >= places.len()));
    let rival = results.get(1).is_some_and(|(other, _, _)| *other >= matched);
    if !enough || rival {
        return None;
    }
    // Worked out again from the waypoints the places are at (least squares), which
    // is nearer than the first guess, from two of them.
    let waypoints: Vec<[f32; 2]> = data
        .points
        .iter()
        .filter(|point| u32::from(point.map) == found.map && (point.label == 2 || point.label == 3))
        .map(|point| [point.x, point.y])
        .collect();
    let pinned = UserScene {
        map: found.map,
        points: places
            .iter()
            .filter_map(|place| {
                let at = found.to_map(*place);
                waypoints
                    .iter()
                    .copied()
                    .min_by(|a, b| {
                        (a[0] - at[0]).hypot(a[1] - at[1]).partial_cmp(&(b[0] - at[0]).hypot(b[1] - at[1])).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .filter(|w| (w[0] - at[0]).hypot(w[1] - at[1]) <= 25.)
                    .map(|marker| Point { marker, position: *place })
            })
            .collect(),
    };
    Some((pinned.fit(found.s).unwrap_or(found), matched))
}

/// The scale and shift (s, a, b) that put most of the places on waypoints of one
/// map: tried from every pair of waypoints for the two places farthest apart.
fn fit_waypoints(map: u32, places: &[[f32; 3]], waypoints: &[[f32; 2]]) -> Option<(usize, f32, SceneMap)> {
    // The two places that are the farthest apart.
    let (mut first, mut second, mut far) = (0, 1, 0f32);
    for i in 0..places.len() {
        for j in i + 1..places.len() {
            let gap = (places[i][0] - places[j][0]).hypot(places[i][2] - places[j][2]);
            if gap > far {
                (first, second, far) = (i, j, gap);
            }
        }
    }
    if far < 150. {
        return None;
    }
    // Where a place is before the scale: the map's x is minus z, its y minus x.
    let flat = |place: &[f32; 3]| [-place[2], -place[0]];
    let (u0, u1) = (flat(&places[first]), flat(&places[second]));
    let u = [u1[0] - u0[0], u1[1] - u0[1]];
    let mut best: Option<(usize, f32, SceneMap)> = None;
    for w0 in waypoints {
        for w1 in waypoints {
            let v = [w1[0] - w0[0], w1[1] - w0[1]];
            let s = (v[0] * u[0] + v[1] * u[1]) / (u[0] * u[0] + u[1] * u[1]);
            if !(0.3..=3.).contains(&s) {
                continue;
            }
            // The same way (not turned) at that scale.
            if (v[0] - s * u[0]).hypot(v[1] - s * u[1]) > 30. {
                continue;
            }
            let (a, b) = (w0[0] - s * u0[0], w0[1] - s * u0[1]);
            let found = SceneMap { map, a, b, s };
            let mut matched = 0;
            let mut error = 0.;
            for place in places {
                let at = found.to_map(*place);
                let nearest = waypoints.iter().map(|w| (w[0] - at[0]).hypot(w[1] - at[1])).fold(f32::MAX, f32::min);
                if nearest <= 25. {
                    matched += 1;
                    error += nearest;
                }
            }
            if best.as_ref().is_none_or(|(most, least, _)| matched > *most || (matched == *most && error < *least)) {
                best = Some((matched, error, found));
            }
        }
    }
    best
}

/// The map and the shift that most of the things are on the markers of.  Only
/// the map `only` is looked at, if one is given.  `arrival` is where the player
/// came into the scene.  The second is a line that says what was found, for the
/// timeline.
///
/// Many things are on markers at shifts that are only chance (about a tenth of
/// the things, at the best of many shifts), so how many are on markers is not
/// enough.  What settles it is the way the player came in: by a teleport, to a
/// waypoint.  A shift is taken when the place they came in at is by a waypoint
/// at that shift.
fn learn(things: &[[f32; 3]], only: Option<u32>, arrival: Option<[f32; 3]>) -> (Option<(SceneMap, usize)>, String) {
    let data = markers::data();
    let things: Vec<[f32; 3]> = if things.len() > MOST {
        (0..MOST).map(|index| things[index * things.len() / MOST]).collect()
    } else {
        things.to_vec()
    };
    let mut fits: Vec<Fit> = Vec::new();
    for def in data.atlas.maps.iter().filter(|def| def.id != WORLD && only.is_none_or(|id| id == def.id)) {
        let points: Vec<[f32; 2]> = data
            .points
            .iter()
            .filter(|point| u32::from(point.map) == def.id)
            .map(|point| [point.x, point.y])
            .collect();
        if points.len() < 20 {
            continue;
        }
        let waypoints: Vec<[f32; 2]> = data
            .points
            .iter()
            .filter(|point| u32::from(point.map) == def.id && (point.label == 2 || point.label == 3))
            .map(|point| [point.x, point.y])
            .collect();
        for (hits, a, b) in best_shifts(&things, &points) {
            let from_waypoint = arrival.map(|arrival| {
                let (x, y) = (-arrival[2] + a, -arrival[0] + b);
                waypoints
                    .iter()
                    .map(|waypoint| (waypoint[0] - x).hypot(waypoint[1] - y))
                    .fold(f32::MAX, f32::min)
            });
            fits.push(Fit { map: def.id, hits, a, b, from_waypoint });
        }
    }
    fits.sort_by_key(|fit| std::cmp::Reverse(fit.hits));
    let report: Vec<String> = fits
        .iter()
        .take(3)
        .map(|fit| {
            format!(
                "map {}: {} on markers at ({:.1}, {:.1}){}",
                fit.map,
                fit.hits,
                fit.a,
                fit.b,
                fit.from_waypoint.map_or(String::new(), |distance| format!(", {distance:.0} m from a waypoint"))
            )
        })
        .collect();
    let report = report.join("; ");
    // Enough on markers, and the player came in at a waypoint of it; or, when it is
    // not known how they came in, very many on markers and clearly more than the
    // next place on any map.
    // With the map given (a guess from where it is in the order) less is enough;
    // among all the maps, a good deal more, as chance finds some place that is near a
    // waypoint on one of them.
    let (least, rate) = if only.is_some() { (7, 14) } else { (22, 18) };
    let enough = |fit: &&Fit| fit.hits >= least && fit.hits * 100 >= things.len() * rate;
    let chosen = match arrival {
        Some(_) => fits
            .iter()
            .filter(enough)
            .filter(|fit| fit.from_waypoint.is_some_and(|distance| distance <= FROM_WAYPOINT))
            .max_by_key(|fit| fit.hits),
        None => fits
            .first()
            .filter(|top| top.hits >= 25 && enough(top))
            .filter(|top| fits.iter().skip(1).all(|other| other.hits * 10 * 13 <= top.hits * 100 || other.map == top.map && (other.a - top.a).hypot(other.b - top.b) < 8.)),
    };
    (
        chosen.map(|fit| (SceneMap { map: fit.map, a: fit.a, b: fit.b, s: 1. }, fit.hits)),
        report,
    )
}

/// The shifts (a, b) at which most of the things are on a marker (the best few,
/// not near one another), with how many are.
fn best_shifts(things: &[[f32; 3]], points: &[[f32; 2]]) -> Vec<(usize, f32, f32)> {
    // Votes: each thing, laid on each marker, says how it would be shifted.
    let mut votes: HashMap<(i32, i32), u16> = HashMap::new();
    for thing in things {
        let place = laid(0, thing);
        for point in points {
            let (a, b) = (point[0] - place[0], point[1] - place[1]);
            *votes.entry(((a / BIN).round() as i32, (b / BIN).round() as i32)).or_default() += 1;
        }
    }
    let mut top: Vec<((i32, i32), u16)> = votes.into_iter().collect();
    top.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    top.truncate(40);

    // A grid of the markers, for telling how many things are on one.
    let mut cells: HashMap<(i32, i32), Vec<[f32; 2]>> = HashMap::new();
    for point in points {
        cells.entry(((point[0] / 8.).floor() as i32, (point[1] / 8.).floor() as i32)).or_default().push(*point);
    }
    // How many things are on a marker at this shift, and how far they are from
    // it altogether (of two shifts with as many, the nearer is the better).
    let score = |a: f32, b: f32| -> (usize, f32) {
        let mut hits = 0;
        let mut apart = 0.;
        for thing in things {
            let place = laid(0, thing);
            let (x, y) = (place[0] + a, place[1] + b);
            let (cx, cy) = ((x / 8.).floor() as i32, (y / 8.).floor() as i32);
            let mut nearest: Option<f32> = None;
            for gx in cx - 1..=cx + 1 {
                for gy in cy - 1..=cy + 1 {
                    for point in cells.get(&(gx, gy)).into_iter().flatten() {
                        let distance = (point[0] - x).hypot(point[1] - y);
                        if distance <= NEAR && nearest.is_none_or(|n| distance < n) {
                            nearest = Some(distance);
                        }
                    }
                }
            }
            if let Some(distance) = nearest {
                hits += 1;
                apart += distance;
            }
        }
        (hits, apart)
    };
    let mut found: Vec<((usize, f32), f32, f32)> = Vec::new();
    for ((bin_a, bin_b), _) in top {
        let (a0, b0) = (bin_a as f32 * BIN, bin_b as f32 * BIN);
        let mut best: Option<((usize, f32), f32, f32)> = None;
        for da in -4..=4 {
            for db in -4..=4 {
                let (a, b) = (a0 + da as f32 * 0.5, b0 + db as f32 * 0.5);
                let result = score(a, b);
                if best.is_none_or(|(most, _, _)| result.0 > most.0 || (result.0 == most.0 && result.1 < most.1)) {
                    best = Some((result, a, b));
                }
            }
        }
        if let Some(best) = best {
            found.push(best);
        }
    }
    found.sort_by(|x, y| y.0.0.cmp(&x.0.0).then(x.0.1.partial_cmp(&y.0.1).unwrap_or(std::cmp::Ordering::Equal)));
    // Not two that are the same place.
    let mut out: Vec<(usize, f32, f32)> = Vec::new();
    for ((hits, _), a, b) in found {
        if out.iter().all(|(_, a2, b2)| (a - a2).hypot(b - b2) > 6.) {
            out.push((hits, a, b));
        }
        if out.len() == 12 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_places_give_the_shift_and_the_scale() {
        // One place: the shift, at the scale there is.
        let one = UserScene {
            map: 40,
            points: vec![Point { marker: [4967., 255.], position: [-1049.8, 205.9, -1728.2] }],
        };
        let found = one.fit(1.).unwrap();
        let at = found.to_map([-1049.8, 205.9, -1728.2]);
        assert!((at[0] - 4967.).abs() < 0.01 && (at[1] - 255.).abs() < 0.01);
        // The three places the user pinned in the Frost Moon, each on its waypoint
        // (log of 2026-11-08, 14:33 to 14:34): the scale is not one.
        let moon = UserScene {
            map: 40,
            points: vec![
                Point { marker: [5345.5, 558.2], position: [-1587.5, 191.1, -2389.6] },
                Point { marker: [4935.9, 168.1], position: [-901.4, 158.0, -1684.4] },
                Point { marker: [4973.0, -62.0], position: [-507.4, 194.2, -1734.3] },
            ],
        };
        let found = moon.fit(1.).unwrap();
        assert!((found.s - 0.574).abs() < 0.01, "{found:?}");
        assert!((found.a - 3973.).abs() < 6. && (found.b + 352.).abs() < 6., "{found:?}");
        // A place pinned on the wrong marker is not believed: the others do not agree
        // with it, so the last place alone is taken, at the scale there is.
        let wrong = UserScene {
            map: 40,
            points: vec![
                Point { marker: [5345.5, 558.2], position: [-1587.5, 191.1, -2389.6] },
                Point { marker: [4935.9, 168.1], position: [-901.4, 158.0, -1684.4] },
                Point { marker: [4000., -900.], position: [-507.4, 194.2, -1734.3] },
            ],
        };
        assert_eq!(wrong.fit(0.5742).unwrap().s, 0.5742);
    }

    #[test]
    fn the_frost_moon_is_known_at_its_scale() {
        let scenes = Scenes::in_memory();
        let moon = scenes.map_of(102).unwrap();
        assert!((moon.s - 0.574).abs() < 0.001);
        // The first pinned waypoint of the log is where the program puts the player.
        let at = moon.to_map([-1587.5, 191.1, -2389.6]);
        assert!((at[0] - 5345.5).abs() < 6. && (at[1] - 558.2).abs() < 6., "{at:?}");
    }

    #[test]
    fn the_world_and_the_chasm_are_known_from_the_start() {
        let scenes = Scenes::new();
        let world = scenes.map_of(3).unwrap();
        // The same place as the transform the chests have used all along.
        assert_eq!(world.to_map([2481., 207., -1242.]), [1242. - 750., -2481. + 2321.]);
        assert_eq!(scenes.map_of(6).unwrap().map, 9);
    }

    /// Things put where markers of a map are, in the coordinates of a scene that
    /// is shifted so: the scene is found to be that map, with that shift.
    #[test]
    fn a_scene_is_found_by_what_is_in_it() {
        let data = markers::data();
        let (a, b) = (-300., 1200.);
        // The things of an area: markers of the map near each other, as what is
        // loaded around a player is.
        let points: Vec<_> = data.points.iter().filter(|point| point.map == 7 && point.floor == 0).collect();
        assert!(points.len() > 100);
        // Where the player comes in is a waypoint, as after a teleport.
        let middle = *points
            .iter()
            .find(|point| (point.label == 2 || point.label == 3) && points.iter().filter(|other| (other.x - point.x).hypot(other.y - point.y) < 200.).count() >= 60)
            .expect("a waypoint with markers around it");
        let things: Vec<SceneGadget> = points
            .iter()
            .filter(|point| (point.x - middle.x).hypot(point.y - middle.y) < 200.)
            .take(80)
            .enumerate()
            .map(|(index, point)| SceneGadget {
                entity_id: index as u32 + 1,
                gadget_id: 70_000_000,
                group_id: 100_000_000,
                config_id: index as u32,
                // From the shift: the map's x is -z + a, its y is -x + b.
                position: Some([b - point.y + 0.7, 100., a - point.x - 0.5]),
            state: None,
            })
            .collect();
        assert!(things.len() >= FIRST_LOOK);

        let mut scenes = Scenes::in_memory();
        // Two looks: the first finds it, the second finds it again.
        scenes.note_gadgets(&things[..40]);
        let (first, _) = scenes.locate(77, [b - middle.y, 100., a - middle.x]);
        assert!(first.is_none(), "not taken at the first look");
        scenes.note_gadgets(&things[40..]);
        let (placed, said) = scenes.locate(77, [b - middle.y, 100., a - middle.x]);
        let found = scenes.map_of(77).expect("the scene is learned");
        assert_eq!(found.map, 7, "{said:?}");
        assert!((found.a - a).abs() <= 2. && (found.b - b).abs() <= 2., "{found:?}");
        let placed = placed.unwrap();
        assert_eq!(placed.map, 7);
        assert!((placed.at[0] - middle.x).abs() <= 3. && (placed.at[1] - middle.y).abs() <= 3.);
        // A scene with nothing in it that is on a map is not put on one.
        let mut other = Scenes::in_memory();
        let nothing: Vec<SceneGadget> = (0..60)
            .map(|index| SceneGadget {
                entity_id: index + 1,
                gadget_id: 70_000_000,
                group_id: 100_000_000,
                config_id: index,
                position: Some([index as f32 * 97.3 % 1900., 10., index as f32 * 53.9 % 1700.]),
            state: None,
            })
            .collect();
        other.note_gadgets(&nothing);
        let (placed, _) = other.locate(99, [0., 0., 0.]);
        assert!(placed.is_none());
    }

    /// What came into view in the Chasm underground, as the game said it (from the
    /// log of 2026-11-08): the scene is found to be the map of the Chasm, with the
    /// shift that was found by hand.
    const CHASM: [[f32; 3]; 65] = [
        [308.8, 4.2, 351.3],
        [326.8, 292.6, 339.0],
        [326.8, 354.5, 18.2],
        [340.3, 29.0, 22.4],
        [350.4, 353.8, 5.0],
        [354.0, 359.8, 4.4],
        [602.5, 377.0, 762.2],
        [621.2, 396.7, 671.8],
        [627.1, 395.0, 560.7],
        [627.5, 376.3, 747.5],
        [639.8, 378.6, 685.5],
        [650.5, 397.6, 760.4],
        [658.4, 380.6, 732.6],
        [665.5, 390.4, 581.4],
        [665.9, 420.3, 757.2],
        [670.2, 419.7, 752.2],
        [672.6, 407.9, 746.9],
        [673.7, 391.7, 729.2],
        [675.7, 420.3, 755.8],
        [679.3, 392.3, 733.9],
        [681.3, 393.1, 729.2],
        [687.0, 386.6, 738.3],
        [689.6, 390.8, 599.1],
        [691.5, 386.6, 736.8],
        [692.4, 384.8, 654.5],
        [693.7, 386.2, 631.5],
        [694.3, 395.3, 659.7],
        [695.0, 398.2, 649.3],
        [698.3, 397.2, 741.7],
        [699.9, 397.3, 737.2],
        [699.9, 399.0, 656.8],
        [700.9, 383.4, 635.5],
        [701.3, 383.6, 635.3],
        [702.6, 383.7, 635.9],
        [704.4, 377.0, 702.5],
        [704.4, 395.4, 659.2],
        [705.6, 399.0, 744.9],
        [705.9, 407.5, 656.7],
        [708.3, 384.4, 651.7],
        [710.8, 395.4, 664.0],
        [714.5, 388.7, 614.6],
        [716.3, 404.3, 733.1],
        [718.0, 376.8, 684.2],
        [718.6, 392.2, 707.0],
        [719.9, 389.5, 623.2],
        [722.0, 401.4, 595.5],
        [722.1, 401.8, 594.8],
        [724.0, 394.3, 572.1],
        [724.6, 394.5, 571.2],
        [729.5, 395.3, 658.8],
        [735.7, 406.8, 658.9],
        [748.6, 790.1, 667.7],
        [755.1, 407.0, 713.8],
        [756.6, 407.2, 712.6],
        [756.6, 707.6, 219.9],
        [759.7, 702.2, 712.3],
        [761.1, 790.9, 637.2],
        [771.2, 403.9, 712.3],
        [775.4, 791.1, 628.5],
        [776.9, 791.0, 649.2],
        [782.5, 418.4, 669.9],
        [782.8, 414.6, 682.9],
        [784.0, 790.5, 616.5],
        [790.4, 776.6, 713.6],
        [796.7, 417.7, 676.5],
    ];

    #[test]
    fn the_chasm_is_found_from_what_the_game_put_into_view() {
        let arrival = Some([710.8, 395.4, 664.0]);
        let (found, report) = learn(&CHASM, Some(9), arrival);
        let (found, hits) = found.unwrap_or_else(|| panic!("not found: {report}"));
        assert_eq!(found.map, 9, "{report}");
        assert!((found.a - 544.4).abs() <= 2. && (found.b - 605.6).abs() <= 2., "{found:?}");
        assert!(hits >= 8, "{hits}");
        // Among all the maps, so few things are not enough to say (more is asked).
        assert!(learn(&CHASM, None, arrival).0.is_none());
        // And it is not any other.
        let (wrong, said) = learn(&CHASM, Some(7), arrival);
        assert!(wrong.is_none(), "{said}");
    }

    /// For looking at what a scene gathered (`scene-N-things.json` of the log
    /// folder): `IRMINSUL_SCENE_DUMP=<file> cargo test looks_at_a_dump -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn looks_at_a_dump() {
        let Some(path) = std::env::var_os("IRMINSUL_SCENE_DUMP") else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let things: Vec<[f32; 3]> = serde_json::from_value(value["things"].clone()).unwrap();
        let arrival: Option<[f32; 3]> = std::env::var("IRMINSUL_SCENE_ARRIVAL").ok().and_then(|text| {
            let n: Vec<f32> = text.split(',').filter_map(|part| part.trim().parse().ok()).collect();
            (n.len() == 3).then(|| [n[0], n[1], n[2]])
        });
        println!("{} things, arrival {arrival:?}", things.len());
        for only in [None, Some(7), Some(34), Some(36), Some(37), Some(40), Some(9)] {
            let (found, report) = learn(&things, only, arrival);
            println!("{only:?}: {found:?} | {report}");
        }
    }

    /// Looks for where a cloud of things lies on a map by the way both are spread
    /// (where there are many of them), not by single things being on single
    /// markers: `IRMINSUL_SCENE_DUMP=<file> IRMINSUL_SCENE_MAP=<map> cargo test
    /// spreads_of_a_dump -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn spreads_of_a_dump() {
        let (Some(path), Some(map)) = (std::env::var_os("IRMINSUL_SCENE_DUMP"), std::env::var("IRMINSUL_SCENE_MAP").ok().and_then(|m| m.parse::<u32>().ok())) else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let things: Vec<[f32; 3]> = serde_json::from_value(value["things"].clone()).unwrap();
        let data = markers::data();
        let points: Vec<[f32; 2]> = data.points.iter().filter(|point| u32::from(point.map) == map).map(|point| [point.x, point.y]).collect();
        let (mut low, mut high) = ([f32::MAX; 2], [f32::MIN; 2]);
        for point in &points {
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        println!("{} things, {} markers, markers from {low:?} to {high:?}", things.len(), points.len());
        const CELLSIZE: f32 = 20.;
        const SIGMA_CELLS: i32 = 3;
        let width = ((high[0] - low[0]) / CELLSIZE) as i32 + 1 + 2 * SIGMA_CELLS;
        let height = ((high[1] - low[1]) / CELLSIZE) as i32 + 1 + 2 * SIGMA_CELLS;
        // The markers, spread out (a blur by a few cells).
        let mut density = vec![0f32; (width * height) as usize];
        for point in &points {
            let (cx, cy) = (((point[0] - low[0]) / CELLSIZE) as i32 + SIGMA_CELLS, ((point[1] - low[1]) / CELLSIZE) as i32 + SIGMA_CELLS);
            for dx in -SIGMA_CELLS..=SIGMA_CELLS {
                for dy in -SIGMA_CELLS..=SIGMA_CELLS {
                    let weight = (-((dx * dx + dy * dy) as f32) / (2. * 1.5 * 1.5)).exp();
                    density[((cy + dy) * width + cx + dx) as usize] += weight;
                }
            }
        }
        for way in 0..8 {
            let placed: Vec<[f32; 2]> = things.iter().map(|thing| laid(way, thing)).collect();
            let mut scores: Vec<(f32, f32, f32)> = Vec::new();
            // Shifts so that the cloud's middle is anywhere in the markers' box (and a
            // good way past it).
            let middle = [placed.iter().map(|p| p[0]).sum::<f32>() / placed.len() as f32, placed.iter().map(|p| p[1]).sum::<f32>() / placed.len() as f32];
            let mut a = low[0] - middle[0] - 600.;
            while a < high[0] - middle[0] + 600. {
                let mut b = low[1] - middle[1] - 600.;
                while b < high[1] - middle[1] + 600. {
                    let mut score = 0.;
                    for place in &placed {
                        let (cx, cy) = (((place[0] + a - low[0]) / CELLSIZE).floor() as i32 + SIGMA_CELLS, ((place[1] + b - low[1]) / CELLSIZE).floor() as i32 + SIGMA_CELLS);
                        if cx >= 0 && cy >= 0 && cx < width && cy < height {
                            score += density[(cy * width + cx) as usize];
                        }
                    }
                    scores.push((score, a, b));
                    b += CELLSIZE;
                }
                a += CELLSIZE;
            }
            let mean = scores.iter().map(|s| s.0).sum::<f32>() / scores.len() as f32;
            let sd = (scores.iter().map(|s| (s.0 - mean).powi(2)).sum::<f32>() / scores.len() as f32).sqrt();
            scores.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
            let top: Vec<String> = scores.iter().take(3).map(|s| format!("{:.1} sigma at ({:.0}, {:.0})", (s.0 - mean) / sd, s.1, s.2)).collect();
            println!("way {way}: {}", top.join("; "));
        }
    }

    /// Looks at shifts that put the game's origin on a multiple of 64 pixels of the
    /// canvas of the map (that is how the known scenes are placed):
    /// `IRMINSUL_SCENE_DUMP=<file> IRMINSUL_SCENE_MAP=<map> cargo test
    /// grid_of_a_dump -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn grid_of_a_dump() {
        let (Some(path), Some(map)) = (std::env::var_os("IRMINSUL_SCENE_DUMP"), std::env::var("IRMINSUL_SCENE_MAP").ok().and_then(|m| m.parse::<u32>().ok())) else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let things: Vec<[f32; 3]> = serde_json::from_value(value["things"].clone()).unwrap();
        let data = markers::data();
        let origin = data.atlas.map(map).unwrap().origin;
        let points: Vec<[f32; 2]> = data.points.iter().filter(|point| u32::from(point.map) == map).map(|point| [point.x, point.y]).collect();
        let mut cells: HashMap<(i32, i32), Vec<[f32; 2]>> = HashMap::new();
        for point in &points {
            cells.entry(((point[0] / 8.).floor() as i32, (point[1] / 8.).floor() as i32)).or_default().push(*point);
        }
        let hits = |a: f32, b: f32| -> usize {
            things
                .iter()
                .filter(|thing| {
                    let (x, y) = (-thing[2] + a, -thing[0] + b);
                    let (cx, cy) = ((x / 8.).floor() as i32, (y / 8.).floor() as i32);
                    (cx - 1..=cx + 1).any(|gx| (cy - 1..=cy + 1).any(|gy| cells.get(&(gx, gy)).is_some_and(|list| list.iter().any(|p| (p[0] - x).hypot(p[1] - y) <= 1.5))))
                })
                .count()
        };
        let mut found: Vec<(usize, f32, f32)> = Vec::new();
        let mut x0 = -1024.;
        while x0 <= 20000. {
            let mut y0 = -1024.;
            while y0 <= 16000. {
                let (a, b) = (x0 - origin[0], y0 - origin[1]);
                let mut best = (0, 0., 0.);
                for da in -3..=3 {
                    for db in -3..=3 {
                        let h = hits(a + da as f32, b + db as f32);
                        if h > best.0 {
                            best = (h, x0 + da as f32, y0 + db as f32);
                        }
                    }
                }
                found.push(best);
                y0 += 64.;
            }
            x0 += 64.;
        }
        found.sort_by_key(|f| std::cmp::Reverse(f.0));
        println!("{} things; best origins on the canvas (hits, x, y): {:?}", things.len(), &found[..6]);
    }

    /// The places teleports put the player at, in the log of 2026-11-08: the
    /// waypoints of a scene are found on the map of it, with the scale it has.
    #[test]
    fn waypoints_teleported_to_give_the_scale() {
        let moon = [
            [-901.4, 158.0, -1684.4],
            [-1676.8, 194.0, -2783.6],
            [-1587.5, 191.4, -2389.6],
            [-1051.7, 206.0, -1728.8],
            [-507.4, 195.3, -1734.3],
        ];
        let (found, matched) = from_waypoints(&moon).expect("the Frost Moon");
        assert_eq!(found.map, 40);
        assert!((found.s - 0.574).abs() < 0.02, "{found:?}");
        assert!(matched >= 4, "{matched}");
        let temple = [[-56.1, 95.0, 416.2], [-208.7, 99.8, 1078.6], [274.4, 81.7, -90.8], [-33.5, 95.7, 797.0], [-156.0, 90.6, 61.0]];
        let (found, _) = from_waypoints(&temple).expect("the Temple of Space");
        assert_eq!(found.map, 37);
        assert!((found.s - 0.947).abs() < 0.03, "{found:?}");
        // Too few places, or all of them at one place, say nothing.
        assert!(from_waypoints(&moon[..3]).is_none(), "three places are not enough");
        assert!(from_waypoints(&[[0., 0., 0.], [10., 0., 10.], [20., 0., 20.]]).is_none());
    }

    #[test]
    fn a_teleport_is_told_with_its_point_and_arrival() {
        let mut scenes = Scenes::in_memory();
        scenes.note_teleport(103, 18);
        assert!(scenes.note_arrival(102, [0., 0., 0.]).is_none(), "of another scene");
        scenes.note_teleport(103, 18);
        let said = scenes.note_arrival(103, [-156., 90.6, 61.]).unwrap();
        assert!(said.contains("point 18"), "{said}");
        assert_eq!(scenes.teleports[&103][&18], [-156., 90.6, 61.]);
    }
}
