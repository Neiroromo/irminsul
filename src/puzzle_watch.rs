//! Puzzles, challenges and gathering, told by what the game puts in view.
//!
//! **Puzzles with a reward chest** (the pillars, the seelies, the time trials, the
//! balloons, the rock piles, the torches ...): the objects of a puzzle and its chest are
//! of one group of the game.  The puzzle is done when
//!
//! * the chest is opened (the game says so; the chest is of a group), or
//! * the chest comes into view, not locked, while the player is by it and the rest of
//!   its group was already in view a while: the chest is what the puzzle gives when it is
//!   done (seen with the pillars, the seelies, the time trial and the balloons of the
//!   log of 2026-11-08).
//!
//! The marker is the one of a puzzle kind that is the nearest to an object of the group
//! that is not a chest, or to the middle of them (the markers are off by up to about 12
//! pixels, and the pillars of a puzzle are spread around its marker).
//!
//! The pillars that are lit and the court of a seelie that was led home say so by their
//! state (201 and 203; the pillars not lit are 901): seen in the log of 2026-11-08.
//!
//! A puzzle that is done is gone from what the game sends: when a scene is entered and what
//! the game sends has come, the markers of the kinds whose puzzles have objects (known from
//! having seen them) that have none by them within 420 metres are done.  If objects come
//! by a marker that was taken as done so, it is not.
//!
//! **Gathering**: an item picked up by the player, by a marker of a kind of the same
//! name (an iron chunk, by an iron chunk), is that marker picked: it is collected, and
//! comes back after the hours the kind has (what is picked is as it was when the time is
//! up).
//!
//! What is told is kept for each account and given to the maps as the chests are.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use auto_artifactarium::SceneGadget;

use crate::chest_watch::ChestMarks;
use crate::chests::kinds;
use crate::markers::{self, Data};
use crate::profiles;
use crate::scenes::SceneMap;

/// The group of kinds of marker that are puzzles and challenges (the group "Puzzle Chest").
const PUZZLE_GROUP: u32 = 186;
/// The groups of kinds of marker that are picked up: specialties, ores, materials, wood.
const GATHER_GROUPS: [u32; 4] = [10, 11, 60, 173];
/// A puzzle marker is of an object of the group when it is this near it (pixels).
const MARKER_NEAR: f32 = 32.;
/// The nearest marker must be this much nearer than the next one.
const UNIQUE: f32 = 1.5;
/// The kinds of puzzle that are known to have objects in the game (seen in logs).
const SEEDS: [u32; 8] = [70, 71, 72, 64, 68, 18, 205, 148];
/// Objects of a puzzle with the state that says it is done: the pillars (lit: 201, not lit:
/// 901), the court of a seelie (203), and the first of the balloons' objects (203), which is
/// all that stays of the balloons when they were shot.
const SOLVED: [(u32, u32); 4] = [(70_900_007, 201), (70_710_006, 203), (70_710_007, 203), (70_360_008, 203)];
/// What the game sends when a scene is entered comes in this long, within this far (metres),
/// and at least this many things must have come.
const ENTRY_WINDOW: Duration = Duration::from_secs(8);
const BULK_METRES: f32 = 420.;
const BULK_LEAST: usize = 20;
/// The chest must come after the rest of the group by this much.
const CHEST_AFTER: Duration = Duration::from_secs(4);
/// ...and the player must be this near it (metres) when it comes: farther, it came into
/// view as the player came, not as the puzzle was done.
const PLAYER_NEAR: f32 = 50.;
/// A picked up thing is the marker this near the player (metres).
const PICKED_NEAR: f32 = 8.;
const CELL: f32 = 16.;

/// How far (metres) from where a scene was entered the objects of the puzzles of a kind are
/// known to be sent (seen in logs): from 250 metres for the pillars, the piles of rocks, the
/// torches and the seelies; the time trial devices were first seen from 227 metres going
/// about, the balloons from 100 to 138, and at the entering of a scene at most from 110.
fn reach_of(label: u32) -> f32 {
    match label {
        64 => 200.,
        68 => 120.,
        _ => 250.,
    }
}

/// How far (metres) round the player the objects of a kind are surely sent as they go (what
/// was first seen at, as far as it came from: about half).
fn walk_of(label: u32) -> f32 {
    match label {
        70 => 100.,
        71 => 80.,
        72 => 60.,
        64 => 180.,
        68 => 80.,
        _ => 90.,
    }
}

#[derive(Clone)]
struct Member {
    chest: bool,
    /// Where on the map it is.
    at: [f32; 2],
    first_seen: Instant,
}

struct Mark {
    id: u32,
    x: f32,
    y: f32,
    label: u32,
    floor: u32,
}

pub struct PuzzleWatch {
    uid: Option<u32>,
    /// Marker: since when it is done (seconds).
    done: HashMap<u32, u64>,
    /// The markers that are done because nothing of their puzzle is sent, and the kinds of
    /// puzzle that were seen to have objects.
    by_absence: HashSet<u32>,
    /// Markers taken as done by absence, whose objects came back.
    reappeared: HashSet<u32>,
    version: u64,
    dirty: bool,
    saved_at: Instant,
    data: Option<std::sync::Arc<Data>>,
    /// The markers of puzzles and of what is picked up, by map and place.
    puzzles: HashMap<(u32, i32, i32), Vec<Mark>>,
    picked: HashMap<(u32, i32, i32), Vec<Mark>>,
    /// In this connection: the members of each group, and the group of each entity.
    groups: HashMap<u32, Vec<Member>>,
    map_of_group: HashMap<u32, u32>,
    entities: HashMap<u32, u32>,
    /// Where the player was last told to be (game coordinates) and when.
    last: Option<(u32, [f32; 3], Instant)>,
    /// Since the scene was entered: the objects of puzzles sent (map, place, group), how many
    /// things came, and where and when the scene was entered.
    fresh: Vec<(u32, [f32; 2], u32)>,
    received: usize,
    entry: Option<(u32, [f32; 3], Instant, bool)>,
    /// What is kept was looked at for what is not of a kind that is known.
    pruned: bool,
    /// How many reports in a row a marker was by the player with none of its objects sent.
    near: HashMap<u32, u8>,
}

/// What is kept for an account.
#[derive(Default, serde::Deserialize, serde::Serialize)]
struct Book {
    #[serde(default)]
    done: HashMap<u32, u64>,
    #[serde(default)]
    by_absence: Vec<u32>,
}

fn path(uid: u32) -> Option<PathBuf> {
    Some(profiles::profiles_dir().ok()?.join("puzzle_marks").join(format!("{uid}.json")))
}

fn same_name(a: &str, b: &str) -> bool {
    let clean = |s: &str| s.replace('\u{a0}', " ").trim().to_lowercase();
    clean(a) == clean(b)
}

impl PuzzleWatch {
    pub fn new() -> Self {
        Self {
            uid: None,
            done: HashMap::new(),
            by_absence: HashSet::new(),
            reappeared: HashSet::new(),
            version: 1,
            dirty: false,
            saved_at: Instant::now(),
            data: None,
            puzzles: HashMap::new(),
            picked: HashMap::new(),
            groups: HashMap::new(),
            map_of_group: HashMap::new(),
            entities: HashMap::new(),
            last: None,
            fresh: Vec::new(),
            received: 0,
            entry: None,
            pruned: false,
            near: HashMap::new(),
        }
    }

    pub fn use_account(&mut self, uid: Option<u32>) {
        if self.uid == uid {
            return;
        }
        self.save(true);
        self.uid = uid;
        self.done.clear();
        self.by_absence.clear();
        self.pruned = false;
        if let Some(path) = uid.and_then(path)
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        {
            if value.get("done").is_some() {
                if let Ok(book) = serde_json::from_value::<Book>(value) {
                    self.done = book.done;
                    self.by_absence = book.by_absence.into_iter().collect();
                }
            } else if let Ok(old) = serde_json::from_value::<HashMap<u32, u64>>(value) {
                // The first form of the file: only the markers and their times.
                self.done = old;
            }
        }
        self.version += 1;
    }

    pub fn start_session(&mut self) {
        self.groups.clear();
        self.map_of_group.clear();
        self.entities.clear();
        self.last = None;
        self.fresh.clear();
        self.received = 0;
        self.entry = None;
    }

    /// The scene was entered (a teleport or the like): what comes now is a new look.
    pub fn entered(&mut self, scene: u32, position: [f32; 3]) {
        self.fresh.clear();
        self.received = 0;
        self.entry = Some((scene, position, Instant::now(), false));
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
        let book = Book { done: self.done.clone(), by_absence: self.by_absence.iter().copied().collect() };
        if let Ok(bytes) = serde_json::to_vec(&book) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn index(&mut self) -> std::sync::Arc<Data> {
        let data = markers::data();
        if self.data.as_ref().is_some_and(|made| std::sync::Arc::ptr_eq(made, &data)) {
            return data;
        }
        self.puzzles.clear();
        self.picked.clear();
        let puzzle_kinds: HashSet<u32> = data.children(PUZZLE_GROUP).map(|label| label.id).collect();
        let gather_kinds: HashSet<u32> =
            GATHER_GROUPS.iter().flat_map(|group| data.children(*group)).map(|label| label.id).collect();
        for point in &data.points {
            let target = if puzzle_kinds.contains(&point.label) {
                &mut self.puzzles
            } else if gather_kinds.contains(&point.label) {
                &mut self.picked
            } else {
                continue;
            };
            target
                .entry((u32::from(point.map), (point.x / CELL).floor() as i32, (point.y / CELL).floor() as i32))
                .or_default()
                .push(Mark { id: point.id, x: point.x, y: point.y, label: point.label, floor: u32::from(point.floor) });
        }
        self.data = Some(data.clone());
        // What was taken as done by absence, of a kind that is not known to have objects, was
        // a guess of an earlier version: it is not kept.
        if !self.pruned {
            self.pruned = true;
            let wrong: Vec<u32> = self
                .by_absence
                .iter()
                .copied()
                .filter(|marker| {
                    !data.points.iter().find(|point| point.id == *marker).is_some_and(|point| SEEDS.contains(&point.label))
                })
                .collect();
            for marker in wrong {
                self.by_absence.remove(&marker);
                self.done.remove(&marker);
                self.reappeared.insert(marker);
                self.version += 1;
                self.dirty = true;
            }
        }
        data
    }

    fn mark_done(&mut self, marker: u32) {
        if !self.done.contains_key(&marker) {
            self.done.insert(marker, markers::unix_now());
            self.version += 1;
            self.dirty = true;
        }
    }

    /// Where the player was last told to be in a scene, if it is lately.
    pub fn position(&self, scene: u32) -> Option<[f32; 3]> {
        self.last.filter(|(was, _, when)| *was == scene && when.elapsed() < Duration::from_secs(10)).map(|(_, at, _)| at)
    }

    /// Where the player is now (from the client's movement too).
    pub fn player_at(&mut self, scene: u32, position: [f32; 3]) {
        self.last = Some((scene, position, Instant::now()));
    }

    /// Things came into view.  The ones of a group are kept, and a chest that comes after
    /// the rest of its group, by the player, finishes the puzzle.
    pub fn gadgets(&mut self, scene: u32, scene_map: SceneMap, gadgets: &[SceneGadget]) {
        self.index();
        let now = Instant::now();
        let player = self.last.filter(|(was, _, when)| *was == scene && when.elapsed() < Duration::from_secs(20)).map(|(_, at, _)| at);
        let mut finished: Vec<u32> = Vec::new();
        let mut solved: Vec<u32> = Vec::new();
        for gadget in gadgets {
            let Some(position) = gadget.position else {
                continue;
            };
            // The groups of the world (133...), not those the game makes for the moment.
            if gadget.group_id < 100_000_000 {
                continue;
            }
            let at = scene_map.to_map(position);
            let chest = kinds().get(&gadget.gadget_id).is_some_and(|kind| kind.tier > 0);
            if !chest && self.fresh.len() < 6000 {
                self.fresh.push((scene_map.map, at, gadget.group_id));
            }
            self.entities.insert(gadget.entity_id, gadget.group_id);
            self.map_of_group.insert(gadget.group_id, scene_map.map);
            if !chest && gadget.state.is_some_and(|state| SOLVED.contains(&(gadget.gadget_id, state))) {
                solved.push(gadget.group_id);
            }
            let members = self.groups.entry(gadget.group_id).or_default();
            if chest
                && gadget.state.is_none()
                && !members.iter().any(|member| member.chest)
                && members.iter().any(|member| !member.chest && now.duration_since(member.first_seen) >= CHEST_AFTER)
                && player.is_some_and(|player| (player[0] - position[0]).hypot(player[2] - position[2]) <= PLAYER_NEAR)
            {
                finished.push(gadget.group_id);
            }
            if !members.iter().any(|member| member.chest == chest && (member.at[0] - at[0]).abs() < 0.5 && (member.at[1] - at[1]).abs() < 0.5) {
                members.push(Member { chest, at, first_seen: now });
            }
        }
        self.received += gadgets.len();
        for group in finished {
            self.complete(group);
        }
        // Complete the groups whose objects say it, once all of them were put down (they come
        // together).
        solved.sort_unstable();
        solved.dedup();
        for group in solved {
            self.complete(group);
        }
        self.undo_absent(scene_map.map, gadgets, scene_map);
        self.save(false);
    }

    /// Objects of a puzzle came by a marker that was taken as done because none were sent:
    /// it was not.
    fn undo_absent(&mut self, map: u32, gadgets: &[SceneGadget], scene_map: SceneMap) {
        if self.by_absence.is_empty() {
            return;
        }
        for gadget in gadgets {
            let Some(position) = gadget.position else {
                continue;
            };
            if gadget.group_id < 100_000_000 || kinds().get(&gadget.gadget_id).is_some_and(|kind| kind.tier > 0) {
                continue;
            }
            let at = scene_map.to_map(position);
            let back: Vec<u32> = self
                .by_absence
                .iter()
                .copied()
                .filter(|marker| {
                    self.mark_of(map, *marker).is_some_and(|mark| (mark.0 - at[0]).hypot(mark.1 - at[1]) <= MARKER_NEAR)
                })
                .collect();
            for marker in back {
                self.by_absence.remove(&marker);
                self.done.remove(&marker);
                self.reappeared.insert(marker);
                self.version += 1;
                self.dirty = true;
            }
        }
    }

    /// Where a puzzle marker is (x, y) on a map.
    fn mark_of(&self, map: u32, marker: u32) -> Option<(f32, f32)> {
        self.puzzles.iter().filter(|((on, _, _), _)| *on == map).flat_map(|(_, marks)| marks).find(|mark| mark.id == marker).map(|mark| (mark.x, mark.y))
    }

    /// A chest was opened by the player: if it is of a group with a puzzle, that is done.
    pub fn chest_opened(&mut self, entity: u32) {
        self.index();
        if let Some(group) = self.entities.get(&entity).copied() {
            self.complete(group);
            self.save(false);
        }
    }

    /// The puzzle of a group is done: its marker is.
    fn complete(&mut self, group: u32) {
        let Some(map) = self.map_of_group.get(&group).copied() else {
            return;
        };
        let Some(members) = self.groups.get(&group).cloned() else {
            return;
        };
        let objects: Vec<[f32; 2]> = members.iter().filter(|member| !member.chest).map(|member| member.at).collect();
        if objects.is_empty() {
            return;
        }
        // The objects, and the middle of them (the pillars are spread round the marker).
        let mut anchors = objects.clone();
        let count = objects.len() as f32;
        anchors.push([objects.iter().map(|at| at[0]).sum::<f32>() / count, objects.iter().map(|at| at[1]).sum::<f32>() / count]);
        let Some(marker) = self.marker_by(map, &anchors) else {
            return;
        };
        self.mark_done(marker);
    }

    /// The marker of a puzzle that is the nearest to the places, if it is near enough and
    /// clearly the nearest.
    fn marker_by(&self, map: u32, anchors: &[[f32; 2]]) -> Option<u32> {
        let mut found: HashMap<u32, f32> = HashMap::new();
        for anchor in anchors {
            let (cx, cy) = ((anchor[0] / CELL).floor() as i32, (anchor[1] / CELL).floor() as i32);
            let reach = (MARKER_NEAR / CELL).ceil() as i32 + 1;
            for gy in cy - reach..=cy + reach {
                for gx in cx - reach..=cx + reach {
                    for mark in self.puzzles.get(&(map, gx, gy)).into_iter().flatten() {
                        let distance = (mark.x - anchor[0]).hypot(mark.y - anchor[1]);
                        if distance <= MARKER_NEAR {
                            let entry = found.entry(mark.id).or_insert(distance);
                            *entry = entry.min(distance);
                        }
                    }
                }
            }
        }
        let mut sorted: Vec<(f32, u32)> = found.into_iter().map(|(id, distance)| (distance, id)).collect();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        match sorted.as_slice() {
            [] => None,
            [(_, id)] => Some(*id),
            [(first, id), (second, _), ..] => (*second >= *first * UNIQUE).then_some(*id),
        }
    }

    fn label_of_marker(&self, map: u32, marker: u32) -> Option<u32> {
        self.puzzles.iter().filter(|((on, _, _), _)| *on == map).flat_map(|(_, marks)| marks).find(|mark| mark.id == marker).map(|mark| mark.label)
    }

    /// The player is here in a scene on a map: when the scene was entered and what the game
    /// sends has come, the puzzles whose objects are not sent are done.
    pub fn report(&mut self, scene: u32, scene_map: SceneMap, position: [f32; 3], floor: u32) {
        self.last = Some((scene, position, Instant::now()));
        if self.entry.is_none_or(|(was, _, _, _)| was != scene) {
            self.entry = Some((scene, position, Instant::now(), false));
            self.near.clear();
        }
        let Some((_, at, since, scanned)) = self.entry else {
            return;
        };
        if since.elapsed() < ENTRY_WINDOW || self.received < BULK_LEAST {
            return;
        }
        self.index();
        let mut gone: Vec<u32> = Vec::new();
        // When the scene was entered and what the game sends came: the markers within reach of
        // where it was entered, with none of the objects of the game by them.
        if !scanned {
            self.entry = Some((scene, at, since, true));
            gone.extend(self.unclaimed(scene_map, scene_map.to_map(at), BULK_METRES, floor, reach_of));
        }
        // Going about: the same by the player, within what is sent round them, twice in a row.
        let here = self.unclaimed(scene_map, scene_map.to_map(position), BULK_METRES, floor, walk_of);
        for marker in &here {
            let times = self.near.entry(*marker).or_insert(0);
            *times = times.saturating_add(1);
            if *times >= 2 {
                gone.push(*marker);
            }
        }
        self.near.retain(|marker, _| here.contains(marker));
        gone.sort_unstable();
        gone.dedup();
        for marker in gone {
            if self.done.contains_key(&marker) {
                continue;
            }
            self.done.insert(marker, markers::unix_now());
            self.by_absence.insert(marker);
            self.reappeared.remove(&marker);
            self.version += 1;
            self.dirty = true;
        }
        self.save(false);
    }

    /// The markers of the kinds of puzzle known to have objects, not done, within `limit_of`
    /// (metres) of a place and not further than `most` from it, that have none of the objects
    /// the game sent since the scene was entered by them (the objects, or the middle of their
    /// group).
    fn unclaimed(&self, scene_map: SceneMap, from: [f32; 2], most: f32, floor: u32, limit_of: fn(u32) -> f32) -> Vec<u32> {
        let radius = most * scene_map.s;
        let (cx, cy) = ((from[0] / CELL).floor() as i32, (from[1] / CELL).floor() as i32);
        let reach = (radius / CELL).ceil() as i32 + 1;
        let mut centres: HashMap<u32, ([f32; 2], f32)> = HashMap::new();
        for (map, place, group) in &self.fresh {
            if *map == scene_map.map {
                let entry = centres.entry(*group).or_insert(([0., 0.], 0.));
                entry.0[0] += place[0];
                entry.0[1] += place[1];
                entry.1 += 1.;
            }
        }
        let middles: Vec<[f32; 2]> = centres.values().map(|(sum, count)| [sum[0] / count, sum[1] / count]).collect();
        let mut found: Vec<u32> = Vec::new();
        for gy in cy - reach..=cy + reach {
            for gx in cx - reach..=cx + reach {
                for mark in self.puzzles.get(&(scene_map.map, gx, gy)).into_iter().flatten() {
                    if mark.floor != floor
                        || !SEEDS.contains(&mark.label)
                        || self.done.contains_key(&mark.id)
                        || (mark.x - from[0]).hypot(mark.y - from[1]) > radius.min(limit_of(mark.label) * scene_map.s)
                    {
                        continue;
                    }
                    let near = |place: &[f32; 2]| (mark.x - place[0]).hypot(mark.y - place[1]) <= MARKER_NEAR;
                    let claimed = self.fresh.iter().any(|(map, place, _)| *map == scene_map.map && near(place)) || middles.iter().any(near);
                    if !claimed {
                        found.push(mark.id);
                    }
                }
            }
        }
        found
    }

    /// The player got an item (more of it than before): the marker of the same name that
    /// they are by is picked.
    pub fn picked_up(&mut self, name_en: &str, scene_map: SceneMap, position: [f32; 3], floor: u32) {
        let data = self.index();
        let at = scene_map.to_map(position);
        let radius = PICKED_NEAR * scene_map.s;
        let (cx, cy) = ((at[0] / CELL).floor() as i32, (at[1] / CELL).floor() as i32);
        let reach = (radius / CELL).ceil() as i32 + 1;
        let mut best: Option<(f32, u32)> = None;
        for gy in cy - reach..=cy + reach {
            for gx in cx - reach..=cx + reach {
                for mark in self.picked.get(&(scene_map.map, gx, gy)).into_iter().flatten() {
                    if mark.floor != floor {
                        continue;
                    }
                    let distance = (mark.x - at[0]).hypot(mark.y - at[1]);
                    if distance <= radius
                        && data.label(mark.label).is_some_and(|label| same_name(&label.en, name_en))
                        && best.is_none_or(|(nearest, _)| distance < nearest)
                    {
                        best = Some((distance, mark.id));
                    }
                }
            }
        }
        if let Some((_, marker)) = best {
            // What is picked is collected again each time it comes back.
            self.done.insert(marker, markers::unix_now());
            self.version += 1;
            self.dirty = true;
            self.save(false);
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// What the maps are given: the markers that are done, and since when.  (What comes back
    /// after some hours is the map's own way: the marker is collected, and the kind says when it
    /// is back.)
    pub fn snapshot(&self) -> ChestMarks {
        ChestMarks {
            version: self.version,
            uid: self.uid,
            opened: self.done.iter().map(|(marker, at)| (*marker, *at)).collect(),
            closed: self.reappeared.iter().copied().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD_MAP: SceneMap = SceneMap { map: crate::atlas::WORLD, a: -750., b: 2321., s: 1. };

    fn game_of(x: f32, y: f32) -> [f32; 3] {
        [2321. - y, 100., -750. - x]
    }

    fn gadget(entity: u32, id: u32, group: u32, at: [f32; 3], state: Option<u32>) -> SceneGadget {
        SceneGadget { entity_id: entity, gadget_id: id, group_id: group, config_id: entity, position: Some(at), state }
    }

    /// A marker of a puzzle of the world (the first one found).
    fn a_puzzle_marker() -> (u32, f32, f32) {
        let data = markers::data();
        let kinds: HashSet<u32> = data.children(PUZZLE_GROUP).map(|label| label.id).collect();
        let point = data
            .points
            .iter()
            .find(|point| u32::from(point.map) == crate::atlas::WORLD && point.floor == 0 && kinds.contains(&point.label))
            .expect("a puzzle of the world");
        (point.id, point.x, point.y)
    }

    #[test]
    fn a_chest_that_comes_by_the_player_after_the_puzzle_finishes_it() {
        let (id, x, y) = a_puzzle_marker();
        let mut watch = PuzzleWatch::new();
        watch.index();
        watch.player_at(3, game_of(x + 10., y));
        // The objects of the puzzle, in view for a while.
        watch.gadgets(3, WORLD_MAP, &[gadget(1, 70_900_007, 133_001_146, game_of(x + 2., y), None)]);
        for members in watch.groups.values_mut() {
            for member in members {
                member.first_seen = Instant::now() - Duration::from_secs(30);
            }
        }
        assert!(!watch.done.contains_key(&id));
        // The chest comes (not locked), the player is by.
        watch.gadgets(3, WORLD_MAP, &[gadget(2, 70_211_121, 133_001_146, game_of(x + 8., y), None)]);
        assert!(watch.done.contains_key(&id), "the chest came by the player: done");
    }

    #[test]
    fn a_chest_that_comes_far_off_or_locked_does_not_finish_it() {
        let (id, x, y) = a_puzzle_marker();
        let mut watch = PuzzleWatch::new();
        watch.index();
        watch.player_at(3, game_of(x + 150., y));
        watch.gadgets(3, WORLD_MAP, &[gadget(1, 70_900_007, 133_001_146, game_of(x + 2., y), None)]);
        for members in watch.groups.values_mut() {
            for member in members {
                member.first_seen = Instant::now() - Duration::from_secs(30);
            }
        }
        // Far off: it came into view as the player came.
        watch.gadgets(3, WORLD_MAP, &[gadget(2, 70_211_121, 133_001_146, game_of(x + 8., y), None)]);
        assert!(!watch.done.contains_key(&id));
        // Locked chest (state 101) by the player: it is told by its opening only.
        let mut other = PuzzleWatch::new();
        other.index();
        other.player_at(3, game_of(x + 10., y));
        other.gadgets(3, WORLD_MAP, &[gadget(1, 70_900_007, 133_001_278, game_of(x + 2., y), None)]);
        for members in other.groups.values_mut() {
            for member in members {
                member.first_seen = Instant::now() - Duration::from_secs(30);
            }
        }
        other.gadgets(3, WORLD_MAP, &[gadget(2, 70_211_012, 133_001_278, game_of(x + 8., y), Some(101))]);
        assert!(!other.done.contains_key(&id));
        // Then it is opened.
        other.chest_opened(2);
        assert!(other.done.contains_key(&id));
    }

    #[test]
    fn what_is_picked_by_a_marker_of_its_name_is_collected() {
        let data = markers::data();
        let point = data
            .points
            .iter()
            .find(|point| {
                u32::from(point.map) == crate::atlas::WORLD
                    && point.floor == 0
                    && data.label(point.label).is_some_and(|label| label.en == "Iron Chunk")
            })
            .expect("an iron chunk");
        let (id, x, y) = (point.id, point.x, point.y);
        let mut watch = PuzzleWatch::new();
        // Another name: nothing.
        watch.picked_up("Mint", WORLD_MAP, game_of(x + 2., y), 0);
        assert!(!watch.done.contains_key(&id));
        watch.picked_up("Iron Chunk", WORLD_MAP, game_of(x + 2., y), 0);
        assert!(watch.done.contains_key(&id));
    }

    /// A world marker of a kind (the first one found), for the tests.
    fn a_marker_of(label: u32) -> (u32, f32, f32) {
        let data = markers::data();
        let point = data
            .points
            .iter()
            .find(|point| u32::from(point.map) == crate::atlas::WORLD && point.floor == 0 && point.label == label)
            .expect("a marker");
        (point.id, point.x, point.y)
    }

    #[test]
    fn pillars_spread_round_the_marker_are_its_puzzle() {
        let (id, x, y) = a_marker_of(70);
        let mut watch = PuzzleWatch::new();
        watch.index();
        watch.player_at(3, game_of(x, y));
        // Four pillars 22 to 28 pixels off the marker on four sides: the middle is by it.
        let pillars = [(22., 0.), (-22., 0.), (0., 24.), (0., -24.)];
        let gadgets: Vec<SceneGadget> = pillars
            .iter()
            .enumerate()
            .map(|(n, (dx, dy))| gadget(n as u32 + 1, 70_900_007, 133_002_344, game_of(x + dx, y + dy), Some(901)))
            .collect();
        watch.gadgets(3, WORLD_MAP, &gadgets);
        for members in watch.groups.values_mut() {
            for member in members {
                member.first_seen = Instant::now() - Duration::from_secs(30);
            }
        }
        watch.gadgets(3, WORLD_MAP, &[gadget(9, 70_211_111, 133_002_344, game_of(x, y), None)]);
        assert!(watch.done.contains_key(&id));
    }

    #[test]
    fn a_puzzle_whose_objects_are_not_sent_when_a_scene_is_entered_is_done_and_not_if_they_come() {
        let (id, x, y) = a_marker_of(68);
        let mut watch = PuzzleWatch::new();
        watch.index();
        // The scene is entered 100 metres off, and things of other groups came.
        let crowd: Vec<SceneGadget> =
            (0..30).map(|n| gadget(n + 100, 70_520_001, 133_004_000 + n, game_of(x + 300. + n as f32, y + 300.), None)).collect();
        watch.report(3, WORLD_MAP, game_of(x + 60., y), 0);
        watch.entered(3, game_of(x + 60., y));
        watch.gadgets(3, WORLD_MAP, &crowd);
        watch.entry = watch.entry.map(|(scene, at, _, scanned)| (scene, at, Instant::now() - Duration::from_secs(60), scanned));
        watch.report(3, WORLD_MAP, game_of(x + 60., y), 0);
        assert!(watch.done.contains_key(&id), "nothing of the puzzle by the marker: done");
        assert!(watch.by_absence.contains(&id));
        // Its objects come: it was not.
        watch.gadgets(3, WORLD_MAP, &[gadget(500, 70_360_008, 133_002_338, game_of(x + 3., y), None)]);
        assert!(!watch.done.contains_key(&id));
        assert!(watch.snapshot().closed.contains(&id));
    }

    #[test]
    fn pillars_that_are_lit_say_the_puzzle_is_done_and_not_lit_ones_do_not() {
        let (id, x, y) = a_marker_of(70);
        let make = |state: u32| -> Vec<SceneGadget> {
            [(22., 0.), (-22., 0.), (0., 24.), (0., -24.)]
                .iter()
                .enumerate()
                .map(|(n, (dx, dy))| gadget(n as u32 + 1, 70_900_007, 133_002_344, game_of(x + dx, y + dy), Some(state)))
                .collect()
        };
        let mut watch = PuzzleWatch::new();
        watch.index();
        watch.gadgets(3, WORLD_MAP, &make(901));
        assert!(!watch.done.contains_key(&id));
        let mut again = PuzzleWatch::new();
        again.index();
        again.gadgets(3, WORLD_MAP, &make(201));
        assert!(again.done.contains_key(&id));
    }

    #[test]
    fn a_puzzle_not_sent_by_the_player_going_by_it_is_done_after_two_reports() {
        let (id, x, y) = a_marker_of(68);
        let mut watch = PuzzleWatch::new();
        watch.index();
        let crowd: Vec<SceneGadget> =
            (0..30).map(|n| gadget(n + 100, 70_520_001, 133_004_000 + n, game_of(x + 300. + n as f32, y + 300.), None)).collect();
        // The scene is entered far off (the marker is not looked at then).
        watch.report(3, WORLD_MAP, game_of(x + 300., y), 0);
        watch.entered(3, game_of(x + 300., y));
        watch.gadgets(3, WORLD_MAP, &crowd);
        watch.entry = watch.entry.map(|(scene, at, _, scanned)| (scene, at, Instant::now() - Duration::from_secs(60), scanned));
        watch.report(3, WORLD_MAP, game_of(x + 300., y), 0);
        assert!(!watch.done.contains_key(&id));
        // Then the player walks by it.
        watch.report(3, WORLD_MAP, game_of(x + 30., y), 0);
        assert!(!watch.done.contains_key(&id), "once is not enough");
        watch.report(3, WORLD_MAP, game_of(x + 25., y), 0);
        assert!(watch.done.contains_key(&id));
    }
}
