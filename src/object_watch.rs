//! What state the things on the map that are done once are in (oculi, seelies,
//! puzzles), told by what the game puts in view.
//!
//! The game sends what is not done yet, and not what is done.  How far it sends
//! differs by the kind of thing (measured in the logs): about 100 to 130 metres
//! around the player as they go; up to about 580 metres when a scene is entered (a
//! teleport, logging in); and some things (the oculi that glow far off, for one)
//! from anywhere.  So:
//!
//! * a thing of the game at the marker: it is not done, whatever was marked;
//! * the court of a seelie (the thing next to it, in the same group) says whether the
//!   seelie is done: its state is 203 when it is (seen in the log of 2026-11-08);
//! * when a scene is entered, and what the game sent has all come, the markers
//!   within 420 metres of the player that were not sent are done (those of the kinds
//!   that are sent from anywhere, wherever they are);
//! * as the player goes, a marker that they are by, within the distance the kind is
//!   sent from, and not sent, is done (two reports in a row, three if far);
//! * if the game sends the thing, at any time, it is not done.  So what was
//!   guessed from far off is put right when the player comes near.
//!
//! Not all things are sent from as far: an oculus of one kind is sent only from about
//! 30 metres even when a scene is entered (one that was not collected, 36 metres from
//! the player, was not sent).  So each kind of thing of the game is told to be one that
//! is sent only near (`Local`), one that is sent in the crowd when a scene is entered
//! (`Bulk`), or one that is sent from anywhere (`Anywhere`); what is guessed from far
//! off is only for the last two.
//!
//! Which kind of thing of the game is which marker is learned: where a thing of the
//! game is, and the marker of a kind it is on, are noted, and the one it is on
//! most is its kind.  How far a kind is sent from is learned the same way.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use auto_artifactarium::SceneGadget;
use serde::{Deserialize, Serialize};

use crate::markers::{self, Data};
use crate::profiles;
use crate::scenes::SceneMap;

/// The kinds of marker followed: the oculi, the crimson agate, the seelies, and
/// some puzzles and finds that are done once.
pub const TRACKED: &[u32] = &[5, 6, 194, 403, 508, 626, 694, 833, 141, 834, 18, 205, 148, 70, 71, 72, 73, 64, 356, 357, 429];
/// The oculi among them.
const OCULI: &[u32] = &[5, 6, 194, 403, 508, 626, 694, 833];
/// What is known from the logs: a kind of thing of the game, and the marker it is.
const SEEDS: &[(u32, u32, Class)] = &[
    (70_710_010, 18, Class::Bulk),
    (70_520_017, 5, Class::Local),
    (70_800_004, 5, Class::Anywhere),
    (70_800_008, 5, Class::Anywhere),
    (70_800_009, 5, Class::Anywhere),
    (70_800_007, 5, Class::Anywhere),
];

/// From how far a kind of thing of the game is sent.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Class {
    Unknown,
    /// Only from near (about 100 metres or less).
    Local,
    /// Also in the crowd that comes when a scene is entered (up to about 580 metres).
    Bulk,
    /// From anywhere.
    Anywhere,
}

/// A thing of the game is a marker when it is this near it (pixels of the map): the
/// markers of the official map are off from where the things are by up to 12 pixels
/// (seen with the seelies).
const MATCH: f32 = 16.;
const LEARN_MATCH: f32 = 16.;
/// The seelie, and the parts of its court (the house it is taken to): the state of the
/// court is this when the seelie is done.
const SEELIE: u32 = 70_710_010;
const COURT: [u32; 2] = [70_710_006, 70_710_007];
const COURT_DONE: u32 = 203;
/// A court is of the seelie marker that is the only one this near it (pixels).
const COURT_NEAR: f32 = 100.;
/// A marker with a thing of the game of its kind this near (pixels) is not done, even
/// if the thing is not its own.
const CLAIMED: f32 = 32.;
const CELL: f32 = 16.;
const SETTLE: Duration = Duration::from_secs(10);
/// What the game sends on entering a scene comes in this long.
const ENTRY_WINDOW: Duration = Duration::from_secs(8);
/// Within this (metres) of where the scene was entered, what was not sent is done.
const BULK_METRES: f32 = 420.;
/// At least this many things must have been sent for that to be believed.
const BULK_LEAST: usize = 20;
/// Sent from this far (metres) when going about: from anywhere.
const ANYWHERE_METRES: f32 = 300.;
/// A thing seen sent from this far when a scene is entered is sent in the crowd.
const BULK_SEEN_METRES: f32 = 150.;
const OCULUS_METRES: f32 = 20.;
const OTHER_METRES: f32 = 30.;
const LEAST_METRES: f32 = 15.;

#[derive(Default, Serialize, Deserialize, Clone)]
struct Learned {
    /// How many times it was seen, and the markers of which kinds it was on.
    n: u32,
    votes: HashMap<u32, u32>,
    /// How far from the player it first came into view, going about (metres), and
    /// how far when a scene was entered.
    dist: Vec<f32>,
    #[serde(default)]
    bulk: Vec<f32>,
}

struct Mark {
    id: u32,
    x: f32,
    y: f32,
    label: u32,
    floor: u32,
}

struct Entry {
    scene: u32,
    at: [f32; 3],
    since: Instant,
    scanned: bool,
}

pub struct ObjectWatch {
    uid: Option<u32>,
    /// Marker: done since (seconds), or `None` if not.
    state: HashMap<u32, Option<u64>>,
    version: u64,
    dirty: bool,
    saved_at: Instant,
    data: Option<std::sync::Arc<Data>>,
    cells: HashMap<(u32, i32, i32), Vec<Mark>>,
    learned: HashMap<u32, Learned>,
    learned_dirty: bool,
    learned_saved: Instant,
    /// In this connection: the things counted (by scene and entity), the markers
    /// sent since the scene was entered, the thing of the game of each marker, how
    /// many times in a row a marker was passed with nothing sent.
    counted: HashSet<(u32, u32)>,
    fresh: HashSet<u32>,
    /// The things of the game of the kinds followed that were sent since the scene was
    /// entered: the map, the place on it, the kind.
    sent: Vec<(u32, [f32; 2], u32)>,
    marker_gadget: HashMap<u32, u32>,
    /// The marker of the seelie of each group of the game.
    group_marker: HashMap<u32, u32>,
    near: HashMap<u32, u8>,
    received: usize,
    scene: Option<(u32, Instant)>,
    last: Option<(u32, [f32; 3], Instant)>,
    entry: Option<Entry>,
}

fn marks_path(uid: u32) -> Option<PathBuf> {
    Some(profiles::profiles_dir().ok()?.join("object_marks").join(format!("{uid}.json")))
}

fn learned_path() -> Option<PathBuf> {
    Some(profiles::profiles_dir().ok()?.join("object_gadgets.json"))
}

impl ObjectWatch {
    pub fn new() -> Self {
        let learned = learned_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            uid: None,
            state: HashMap::new(),
            version: 1,
            dirty: false,
            saved_at: Instant::now(),
            data: None,
            cells: HashMap::new(),
            learned,
            learned_dirty: false,
            learned_saved: Instant::now(),
            counted: HashSet::new(),
            fresh: HashSet::new(),
            sent: Vec::new(),
            marker_gadget: HashMap::new(),
            group_marker: HashMap::new(),
            near: HashMap::new(),
            received: 0,
            scene: None,
            last: None,
            entry: None,
        }
    }

    pub fn use_account(&mut self, uid: Option<u32>) {
        if self.uid == uid {
            return;
        }
        self.save(true);
        self.uid = uid;
        self.state.clear();
        if let Some(path) = uid.and_then(marks_path)
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(book) = serde_json::from_str::<HashMap<u32, u64>>(&text)
        {
            self.state = book.into_iter().map(|(id, at)| (id, (at > 0).then_some(at))).collect();
        }
        self.version += 1;
    }

    pub fn start_session(&mut self) {
        self.counted.clear();
        self.fresh.clear();
        self.sent.clear();
        self.marker_gadget.clear();
        self.group_marker.clear();
        self.near.clear();
        self.received = 0;
        self.scene = None;
        self.last = None;
        self.entry = None;
    }

    /// The scene was entered (a teleport or the like): what the game sends now is a
    /// new look at what is around, and is waited for.
    pub fn entered(&mut self, scene: u32, position: [f32; 3]) {
        self.fresh.clear();
        self.sent.clear();
        self.near.clear();
        self.received = 0;
        self.scene = Some((scene, Instant::now()));
        self.entry = Some(Entry { scene, at: position, since: Instant::now(), scanned: false });
    }

    fn save(&mut self, force: bool) {
        if self.dirty && (force || self.saved_at.elapsed() >= Duration::from_secs(5)) {
            self.dirty = false;
            self.saved_at = Instant::now();
            if let Some(path) = self.uid.and_then(marks_path) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let book: HashMap<u32, u64> = self.state.iter().map(|(id, at)| (*id, at.unwrap_or(0))).collect();
                if let Ok(bytes) = serde_json::to_vec(&book) {
                    let _ = std::fs::write(path, bytes);
                }
            }
        }
        if self.learned_dirty && (force || self.learned_saved.elapsed() >= Duration::from_secs(30)) {
            self.learned_dirty = false;
            self.learned_saved = Instant::now();
            if let Some(path) = learned_path() {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(bytes) = serde_json::to_vec(&self.learned) {
                    let _ = std::fs::write(path, bytes);
                }
            }
        }
    }

    fn index(&mut self) -> std::sync::Arc<Data> {
        let data = markers::data();
        if self.data.as_ref().is_some_and(|made| std::sync::Arc::ptr_eq(made, &data)) {
            return data;
        }
        self.cells.clear();
        for point in data.points.iter().filter(|point| TRACKED.contains(&point.label)) {
            self.cells
                .entry((u32::from(point.map), (point.x / CELL).floor() as i32, (point.y / CELL).floor() as i32))
                .or_default()
                .push(Mark { id: point.id, x: point.x, y: point.y, label: point.label, floor: u32::from(point.floor) });
        }
        self.data = Some(data.clone());
        data
    }

    fn set(&mut self, id: u32, done: Option<u64>) {
        if self.state.get(&id) != Some(&done) {
            self.state.insert(id, done);
            self.version += 1;
            self.dirty = true;
        }
    }

    /// The kind of marker a thing of the game is: known from the logs, or learned (seen
    /// at least four times, and on a marker of the kind in at least three of them and
    /// four in five).
    fn label_of(&self, gadget: u32) -> Option<u32> {
        if let Some((_, label, _)) = SEEDS.iter().find(|(id, _, _)| *id == gadget) {
            return Some(*label);
        }
        let learned = self.learned.get(&gadget)?;
        let (label, votes) = learned.votes.iter().max_by_key(|(_, votes)| **votes)?;
        (learned.n >= 4 && *votes >= 3 && *votes * 5 >= learned.n * 4).then_some(*label)
    }

    /// The nearest marker (of the kind, if given) to a place of a map.
    fn marker_near(&self, map: u32, at: [f32; 2], label: Option<u32>, within: f32) -> Option<&Mark> {
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let mut best: Option<(f32, &Mark)> = None;
        for gy in cy - 1..=cy + 1 {
            for gx in cx - 1..=cx + 1 {
                for mark in self.cells.get(&(map, gx, gy)).into_iter().flatten() {
                    let distance = (mark.x - at[0]).hypot(mark.y - at[1]);
                    if distance <= within && label.is_none_or(|label| mark.label == label) && best.is_none_or(|(nearest, _)| distance < nearest) {
                        best = Some((distance, mark));
                    }
                }
            }
        }
        best.map(|(_, mark)| mark)
    }

    /// The seelie marker that is the only one near a place (a court is by its seelie).
    fn only_seelie_near(&self, map: u32, at: [f32; 2]) -> Option<u32> {
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let reach = (COURT_NEAR / CELL).ceil() as i32 + 1;
        let mut found: Vec<(f32, u32)> = Vec::new();
        for gy in cy - reach..=cy + reach {
            for gx in cx - reach..=cx + reach {
                for mark in self.cells.get(&(map, gx, gy)).into_iter().flatten().filter(|mark| mark.label == 18) {
                    let distance = (mark.x - at[0]).hypot(mark.y - at[1]);
                    if distance <= COURT_NEAR {
                        found.push((distance, mark.id));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        match found.as_slice() {
            [(_, id)] => Some(*id),
            [(first, id), (second, _), ..] if *second >= *first * 1.5 => Some(*id),
            _ => None,
        }
    }

    /// Whether a thing of the game of the kind of a marker was sent near it.
    fn claimed(&self, map: u32, mark: &Mark) -> bool {
        self.sent
            .iter()
            .any(|(was, at, label)| *was == map && *label == mark.label && (at[0] - mark.x).hypot(at[1] - mark.y) <= CLAIMED)
    }

    /// The kinds that have a thing of the game known.
    fn known_labels(&self) -> HashSet<u32> {
        let mut known: HashSet<u32> = SEEDS.iter().map(|(_, label, _)| *label).collect();
        for gadget in self.learned.keys() {
            if let Some(label) = self.label_of(*gadget) {
                known.insert(label);
            }
        }
        known
    }

    /// How far (metres) a kind of marker is looked at: what its things were seen to
    /// be sent from (four fifths of the farthest that is usual), no more than the
    /// radius of the radar chosen; before it is known, a short way.
    pub fn radius_of(&self, label: u32) -> f32 {
        let mut samples: Vec<f32> = Vec::new();
        for (gadget, learned) in &self.learned {
            if self.label_of(*gadget) == Some(label) {
                samples.extend(learned.dist.iter().filter(|distance| **distance < ANYWHERE_METRES));
            }
        }
        let radar = crate::chest_watch::radar();
        if samples.len() >= 4 {
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let usual = samples[(samples.len() as f32 * 0.8) as usize % samples.len()];
            (usual * 0.8).clamp(LEAST_METRES, radar)
        } else if OCULI.contains(&label) {
            OCULUS_METRES.min(radar)
        } else {
            OTHER_METRES.min(radar)
        }
    }

    /// From how far a kind of thing of the game is sent.
    fn class_of(&self, gadget: u32) -> Class {
        if let Some((_, _, class)) = SEEDS.iter().find(|(id, _, _)| *id == gadget) {
            return *class;
        }
        let Some(learned) = self.learned.get(&gadget) else {
            return Class::Unknown;
        };
        if learned.dist.iter().any(|distance| *distance >= ANYWHERE_METRES) {
            Class::Anywhere
        } else if learned.bulk.iter().any(|distance| *distance >= BULK_SEEN_METRES) {
            Class::Bulk
        } else if learned.dist.len() + learned.bulk.len() >= 3 {
            Class::Local
        } else {
            Class::Unknown
        }
    }

    /// Whether what is left out of the crowd sent on entering a scene says a marker is
    /// done: its thing is known to be sent in the crowd, or from anywhere; or it was not
    /// seen, and every kind of thing of its marker is one of those.
    fn scannable(&self, mark: &Mark) -> bool {
        let sent_far = |class: Class| matches!(class, Class::Bulk | Class::Anywhere);
        if let Some(gadget) = self.marker_gadget.get(&mark.id) {
            return sent_far(self.class_of(*gadget));
        }
        let mut any = false;
        let gadgets = self.learned.keys().copied().chain(SEEDS.iter().map(|(id, _, _)| *id));
        for gadget in gadgets {
            if self.label_of(gadget) == Some(mark.label) {
                if !sent_far(self.class_of(gadget)) {
                    return false;
                }
                any = true;
            }
        }
        any
    }

    /// Whether the thing of a marker is sent from anywhere.
    fn from_anywhere(&self, marker: u32) -> bool {
        self.marker_gadget.get(&marker).is_some_and(|gadget| self.class_of(*gadget) == Class::Anywhere)
    }

    /// Things came into view in a scene.
    pub fn gadgets(&mut self, scene: u32, scene_map: SceneMap, gadgets: &[SceneGadget]) {
        self.index();
        self.received += gadgets.len();
        // Going about (not what comes on entering a scene), how far from the player.
        let going_about = self.entry.as_ref().is_none_or(|entry| entry.since.elapsed() > ENTRY_WINDOW);
        let player = self.last.filter(|(was, _, when)| *was == scene && when.elapsed() < Duration::from_secs(10));
        let arrival = self.entry.as_ref().filter(|entry| entry.scene == scene).map(|entry| entry.at);
        let mut learned_any = false;
        for gadget in gadgets {
            let Some(position) = gadget.position else {
                continue;
            };
            let at = scene_map.to_map(position);
            if self.counted.insert((scene, gadget.entity_id)) {
                let on = self.marker_near(scene_map.map, at, None, LEARN_MATCH).map(|mark| mark.label);
                let entry = self.learned.entry(gadget.gadget_id).or_default();
                entry.n += 1;
                if let Some(label) = on {
                    *entry.votes.entry(label).or_insert(0) += 1;
                }
                if going_about {
                    if let Some((_, at_player, _)) = player
                        && entry.dist.len() < 60
                    {
                        entry.dist.push((position[0] - at_player[0]).hypot(position[2] - at_player[2]));
                    }
                } else if let Some(arrival) = arrival
                    && entry.bulk.len() < 60
                {
                    entry.bulk.push((position[0] - arrival[0]).hypot(position[2] - arrival[2]));
                }
                learned_any = true;
            }
            // The court of a seelie says if it is done.
            if COURT.contains(&gadget.gadget_id) {
                let done = gadget.state == Some(COURT_DONE);
                let known = self.group_marker.get(&gadget.group_id).copied();
                let id = known.or_else(|| if done { self.only_seelie_near(scene_map.map, at) } else { None });
                if let Some(id) = id {
                    if done {
                        if !matches!(self.state.get(&id), Some(Some(_))) {
                            self.set(id, Some(markers::unix_now()));
                        }
                        self.fresh.remove(&id);
                    } else {
                        self.fresh.insert(id);
                        self.set(id, None);
                    }
                }
                continue;
            }
            let Some(label) = self.label_of(gadget.gadget_id) else {
                continue;
            };
            if self.sent.len() < 4000 {
                self.sent.push((scene_map.map, at, label));
            }
            let Some(id) = self.marker_near(scene_map.map, at, Some(label), MATCH).map(|mark| mark.id) else {
                continue;
            };
            if gadget.gadget_id == SEELIE {
                self.group_marker.insert(gadget.group_id, id);
            }
            self.fresh.insert(id);
            self.marker_gadget.insert(id, gadget.gadget_id);
            self.near.remove(&id);
            // The game sends it: it is not done.
            self.set(id, None);
        }
        if learned_any {
            self.learned_dirty = true;
        }
        self.save(false);
    }

    /// The player is here (in a scene known to be on a map, on this floor of it).
    pub fn player_at(&mut self, scene: u32, scene_map: SceneMap, position: [f32; 3], floor: u32) {
        self.last = Some((scene, position, Instant::now()));
        if self.scene.is_none_or(|(was, _)| was != scene) {
            self.scene = Some((scene, Instant::now()));
            self.near.clear();
            self.entry = Some(Entry { scene, at: position, since: Instant::now(), scanned: false });
        }
        let settled = self.scene.is_some_and(|(_, since)| since.elapsed() >= SETTLE);
        if !settled || self.received < BULK_LEAST {
            return;
        }
        self.index();
        let known = self.known_labels();
        let now = markers::unix_now();
        let mut changed = false;

        // What came on entering the scene is all in: the markers it left out are done.
        if let Some(entry) = self.entry.as_mut()
            && entry.scene == scene
            && !entry.scanned
            && entry.since.elapsed() >= ENTRY_WINDOW
        {
            entry.scanned = true;
            let from = scene_map.to_map(entry.at);
            let radius = BULK_METRES * scene_map.s;
            let (cx, cy) = ((from[0] / CELL).floor() as i32, (from[1] / CELL).floor() as i32);
            let reach = (radius / CELL).ceil() as i32 + 1;
            let mut gone: Vec<u32> = Vec::new();
            for gy in cy - reach..=cy + reach {
                for gx in cx - reach..=cx + reach {
                    for mark in self.cells.get(&(scene_map.map, gx, gy)).into_iter().flatten() {
                        if mark.floor == floor
                            && known.contains(&mark.label)
                            && !self.fresh.contains(&mark.id)
                            && !self.claimed(scene_map.map, mark)
                            && self.scannable(mark)
                        {
                            let far = (mark.x - from[0]).hypot(mark.y - from[1]);
                            if far <= radius || self.from_anywhere(mark.id) {
                                gone.push(mark.id);
                            }
                        }
                    }
                }
            }
            for id in gone {
                if !matches!(self.state.get(&id), Some(Some(_))) {
                    changed = true;
                    self.set(id, Some(now));
                }
            }
        }

        // Going about: a marker passed, within where its kind is sent from, with
        // nothing sent.
        let at = scene_map.to_map(position);
        let widest = crate::chest_watch::radar() * scene_map.s;
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let reach = (widest / CELL).ceil() as i32 + 1;
        let mut here: Vec<(u32, u8)> = Vec::new();
        for gy in cy - reach..=cy + reach {
            for gx in cx - reach..=cx + reach {
                for mark in self.cells.get(&(scene_map.map, gx, gy)).into_iter().flatten() {
                    if mark.floor != floor
                        || !known.contains(&mark.label)
                        || self.fresh.contains(&mark.id)
                        || self.claimed(scene_map.map, mark)
                    {
                        continue;
                    }
                    let radius = self.radius_of(mark.label) * scene_map.s;
                    let distance = (mark.x - at[0]).hypot(mark.y - at[1]);
                    if distance <= radius {
                        // Three reports in a row if it is far, two if near.
                        here.push((mark.id, if distance <= 0.6 * radius { 2 } else { 3 }));
                    }
                }
            }
        }
        for (id, needed) in &here {
            let times = self.near.entry(*id).or_insert(0);
            *times = times.saturating_add(1);
            if *times >= *needed && !matches!(self.state.get(id), Some(Some(_))) {
                changed = true;
                self.set(*id, Some(now));
            }
        }
        self.near.retain(|id, _| here.iter().any(|(near, _)| near == id));
        if changed {
            self.save(false);
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// What the maps are given.
    pub fn snapshot(&self) -> crate::chest_watch::ChestMarks {
        let mut marks = crate::chest_watch::ChestMarks { version: self.version, uid: self.uid, ..Default::default() };
        for (id, state) in &self.state {
            match state {
                Some(at) => marks.opened.push((*id, *at)),
                None => marks.closed.push(*id),
            }
        }
        marks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gadget(entity: u32, id: u32, position: [f32; 3]) -> SceneGadget {
        SceneGadget { entity_id: entity, gadget_id: id, group_id: 1, config_id: entity, position: Some(position), state: None }
    }

    /// The game's coordinates of a place of the world map.
    fn game_of(x: f32, y: f32) -> [f32; 3] {
        [2321. - y, 100., -750. - x]
    }

    fn world() -> SceneMap {
        SceneMap { map: crate::atlas::WORLD, a: -750., b: 2321., s: 1. }
    }

    fn a_seelie_marker() -> (u32, f32, f32) {
        let data = markers::data();
        let point = data
            .points
            .iter()
            .find(|point| u32::from(point.map) == crate::atlas::WORLD && point.label == 18 && point.floor == 0)
            .expect("a seelie of the world");
        (point.id, point.x, point.y)
    }

    #[test]
    fn a_seelie_that_is_sent_is_not_done_and_one_not_sent_by_which_the_player_walks_is() {
        let (id, x, y) = a_seelie_marker();
        let mut watch = ObjectWatch::new();
        watch.learned.clear();
        // Things came in a crowd, and the seelie among them.
        let mut crowd: Vec<SceneGadget> = (0..30).map(|n| gadget(n + 100, 70_520_001, game_of(x + 300., y + 300. + n as f32 * 3.))).collect();
        crowd.push(gadget(1, 70_710_010, game_of(x, y)));
        watch.player_at(3, world(), game_of(x + 200., y), 0);
        watch.gadgets(3, world(), &crowd);
        assert_eq!(watch.state.get(&id), Some(&None), "sent: not done");
        // Later it is passed by, and it is not sent (a new look at the scene).
        watch.entered(3, game_of(x + 200., y));
        watch.gadgets(3, world(), &crowd[..30]);
        watch.scene = Some((3, Instant::now() - Duration::from_secs(60)));
        watch.entry.as_mut().unwrap().since = Instant::now() - Duration::from_secs(60);
        watch.player_at(3, world(), game_of(x + 8., y), 0);
        watch.player_at(3, world(), game_of(x + 8., y), 0);
        assert!(matches!(watch.state.get(&id), Some(Some(_))), "passed by, not sent: done");
        // The game sends it again: not done.
        watch.gadgets(3, world(), &[gadget(2, 70_710_010, game_of(x, y))]);
        assert_eq!(watch.state.get(&id), Some(&None));
    }

    #[test]
    fn what_is_not_sent_when_a_scene_is_entered_is_done_if_within_reach() {
        let (id, x, y) = a_seelie_marker();
        let mut watch = ObjectWatch::new();
        watch.learned.clear();
        let crowd: Vec<SceneGadget> = (0..30).map(|n| gadget(n + 100, 70_520_001, game_of(x + 100., y + 200. + n as f32 * 3.))).collect();
        // A look at the scene 200 metres from the seelie.
        watch.player_at(3, world(), game_of(x + 200., y), 0);
        watch.entered(3, game_of(x + 200., y));
        watch.gadgets(3, world(), &crowd);
        watch.scene = Some((3, Instant::now() - Duration::from_secs(60)));
        watch.entry.as_mut().unwrap().since = Instant::now() - Duration::from_secs(60);
        watch.player_at(3, world(), game_of(x + 200., y), 0);
        assert!(matches!(watch.state.get(&id), Some(Some(_))), "not sent from 200 m: done");
    }

    #[test]
    fn what_a_thing_is_is_learned_from_the_marker_it_is_on() {
        let (_, x, y) = a_seelie_marker();
        let mut watch = ObjectWatch::new();
        watch.learned.clear();
        watch.index();
        for n in 0..5 {
            watch.gadgets(3, world(), &[gadget(n, 99_999_999, game_of(x, y))]);
        }
        assert_eq!(watch.label_of(99_999_999), Some(18));
    }

    #[test]
    fn an_oculus_that_is_sent_only_near_is_not_done_by_what_the_crowd_leaves_out() {
        let data = markers::data();
        let point = data
            .points
            .iter()
            .find(|point| u32::from(point.map) == crate::atlas::WORLD && point.label == 5 && point.floor == 0)
            .expect("an anemoculus of the world");
        let (id, x, y) = (point.id, point.x, point.y);
        let mut watch = ObjectWatch::new();
        watch.learned.clear();
        let crowd: Vec<SceneGadget> = (0..30).map(|n| gadget(n + 100, 70_520_001, game_of(x + 100., y + 200. + n as f32 * 3.))).collect();
        watch.player_at(3, world(), game_of(x + 36., y), 0);
        watch.entered(3, game_of(x + 36., y));
        watch.gadgets(3, world(), &crowd);
        watch.scene = Some((3, Instant::now() - Duration::from_secs(60)));
        watch.entry.as_mut().unwrap().since = Instant::now() - Duration::from_secs(60);
        watch.player_at(3, world(), game_of(x + 36., y), 0);
        assert!(!matches!(watch.state.get(&id), Some(Some(_))), "36 m off, not sent: not done");
    }

    #[test]
    fn the_court_of_a_seelie_says_it_is_done() {
        let (id, x, y) = a_seelie_marker();
        let mut watch = ObjectWatch::new();
        watch.learned.clear();
        watch.index();
        // The seelie and its court (not done yet): not done.
        let mut seelie = gadget(1, SEELIE, game_of(x, y));
        seelie.group_id = 77;
        let mut court = gadget(2, COURT[0], game_of(x + 40., y));
        court.group_id = 77;
        watch.gadgets(3, world(), &[seelie, court.clone()]);
        assert_eq!(watch.state.get(&id), Some(&None));
        // The court says 203: done.
        court.state = Some(COURT_DONE);
        court.entity_id = 3;
        watch.gadgets(3, world(), &[court]);
        assert!(matches!(watch.state.get(&id), Some(Some(_))));
    }
}
