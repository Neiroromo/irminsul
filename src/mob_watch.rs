//! The monsters around the player, as the game sends them, and which markers of enemies of the
//! map are defeated.
//!
//! The game sends the monsters that are alive when the player comes near (about a hundred
//! metres, see `SceneMonster`), with what kind each is and where it stands; one that was
//! killed is not sent.  When a monster is killed, what it drops comes as things on the ground
//! that name it as their owner.  From that:
//!
//! * the monsters that are alive around the player are known (for telling that one is near);
//! * a marker of enemies of the map is defeated when a monster of its kind is killed by it and
//!   none of that kind is left alive there; or when the player is by it for a while and no
//!   enemy at all was sent there (as with the chests that are opened);
//! * a marker that was defeated and has an enemy of its kind alive by it again is not any more.
//!
//! The markers of the map stand for a camp, not for each monster, and not exactly where the
//! monsters are: a monster belongs to the marker of its kind that is within `REACH`.
//!
//! Only the open world (the scene 3) has its markers followed; the monsters near are told in
//! any scene.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use auto_artifactarium::{SceneGadget, SceneMonster};
use serde::Deserialize;

use crate::atlas::WORLD;
use crate::markers::{self, Data};

/// A monster is of the marker of its kind that is this near (pixels of the map, metres).
const REACH: f32 = 70.;
/// A marker is looked at for being defeated when the player is this near it.
const NEAR: f32 = 55.;
/// And for this many of the game's reports of where the player is, in a row.
const TIMES: u8 = 3;
/// The player must be in a scene this long before what was not sent counts.
const SETTLE: Duration = Duration::from_secs(10);
/// A monster this far from the player is forgotten: it is sent again when the player is back.
const FORGET: f32 = 330.;
/// The group of the kinds of enemies in the markers of the map.
const ENEMIES: u32 = 50;
const CELL: f32 = 64.;

#[derive(Deserialize, Default)]
struct Table {
    /// The id of a monster without its last two digits, to the kind of marker.
    families: HashMap<String, u32>,
    /// The same, to the name in English and in Russian.
    names: HashMap<String, [String; 2]>,
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| serde_json::from_str(include_str!("data/monster_labels.json")).unwrap_or_default())
}

/// How a monster is looked up: the variants of an enemy differ in the last two digits of the
/// id, which are left out; the animals that differ in them are other animals.
fn key_of(monster: u32) -> String {
    if is_animal(monster) { monster.to_string() } else { (monster / 100).to_string() }
}

/// The kind of marker of the map a monster is, if it is one.
pub fn label_of(monster: u32) -> Option<u32> {
    table().families.get(&key_of(monster)).copied()
}

/// What a monster is called.
pub fn name_of(monster: u32, russian: bool) -> Option<&'static str> {
    table().names.get(&key_of(monster)).map(|names| names[usize::from(russian)].as_str())
}

/// Animals, birds, fish and the like: they are not enemies, and say nothing of a camp.
fn is_animal(monster: u32) -> bool {
    monster / 1_000_000 == 28
}

/// A monster that is alive, as far as the game said.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AliveMob {
    pub monster: u32,
    pub label: Option<u32>,
    pub scene: u32,
    pub position: [f32; 3],
}

/// The monsters alive around the player, for telling that one is near.
#[derive(Clone, Debug, Default)]
pub struct MobsSeen {
    pub version: u64,
    pub alive: Vec<AliveMob>,
}

struct Marker {
    id: u32,
    label: u32,
    x: f32,
    y: f32,
}

#[derive(Default)]
pub struct MobWatch {
    /// The monsters alive, by group and id in it.
    alive: HashMap<(u32, u32), AliveMob>,
    /// Which monster an entity is, while it is.
    entities: HashMap<u32, (u32, u32)>,
    /// The markers that are defeated (and since when), and those seen alive again.
    marks: HashMap<u32, Option<u64>>,
    /// How many reports in a row the player was by a marker with no enemy there.
    near: HashMap<u32, u8>,
    scene: Option<(u32, Instant)>,
    /// The scene monsters were sent in: there, what is not sent is not there.
    loaded: Option<u32>,
    /// The markers of enemies of the world, by cell; and the data they are of.
    cells: HashMap<(i32, i32), Vec<Marker>>,
    indexed: Option<u64>,
    marks_version: u64,
    alive_version: u64,
}

fn on_map(position: [f32; 3]) -> (f32, f32) {
    crate::official_chests::game_to_map(position)
}

impl MobWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new connection to the game: what was sent in the one before says nothing.
    pub fn start_session(&mut self) {
        if !self.alive.is_empty() {
            self.alive_version += 1;
        }
        self.alive.clear();
        self.entities.clear();
        self.near.clear();
        self.scene = None;
        self.loaded = None;
    }

    fn index(&mut self, data: &Data) {
        if self.indexed == Some(data.stamp) && !self.cells.is_empty() {
            return;
        }
        self.indexed = Some(data.stamp);
        self.cells.clear();
        for label in data.children(ENEMIES).filter(|label| !label.resin) {
            for point in data.points_of(label.id).iter().filter(|point| u32::from(point.map) == WORLD) {
                self.cells
                    .entry(((point.x / CELL).floor() as i32, (point.y / CELL).floor() as i32))
                    .or_default()
                    .push(Marker { id: point.id, label: label.id, x: point.x, y: point.y });
            }
        }
    }

    /// The markers within a way of a place of the map.
    fn around(&self, at: (f32, f32), reach: f32) -> impl Iterator<Item = (&Marker, f32)> {
        let (cx, cy) = ((at.0 / CELL).floor() as i32, (at.1 / CELL).floor() as i32);
        let span = (reach / CELL).ceil() as i32;
        (cy - span..=cy + span)
            .flat_map(move |gy| (cx - span..=cx + span).map(move |gx| (gx, gy)))
            .filter_map(|cell| self.cells.get(&cell))
            .flatten()
            .map(move |marker| (marker, (marker.x - at.0).hypot(marker.y - at.1)))
            .filter(move |(_, distance)| *distance <= reach)
    }

    /// The marker of a kind a monster at a place belongs to.
    fn marker_of(&self, label: u32, at: (f32, f32)) -> Option<u32> {
        self.around(at, REACH)
            .filter(|(marker, _)| marker.label == label)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(marker, _)| marker.id)
    }

    fn marker_place(&self, id: u32) -> Option<(f32, f32, u32)> {
        self.cells.values().flatten().find(|marker| marker.id == id).map(|marker| (marker.x, marker.y, marker.label))
    }

    /// Whether an enemy is alive by a marker: of its kind, or (`any`) of any kind.
    fn alive_by(&self, marker: (f32, f32, u32), any: bool) -> bool {
        self.alive.values().any(|mob| {
            if mob.scene != 3 || is_animal(mob.monster) || !(any || mob.label == Some(marker.2)) {
                return false;
            }
            let at = on_map(mob.position);
            (at.0 - marker.0).hypot(at.1 - marker.1) <= REACH
        })
    }

    fn set(&mut self, marker: u32, state: Option<u64>) {
        if self.marks.get(&marker) != Some(&state) {
            self.marks.insert(marker, state);
            self.marks_version += 1;
        }
    }

    /// Monsters came into view.
    pub fn saw(&mut self, scene: u32, monsters: &[SceneMonster], data: &Data) {
        self.index(data);
        self.loaded = Some(scene);
        for monster in monsters {
            let Some(position) = monster.position else {
                continue;
            };
            let key = (monster.group_id, monster.config_id);
            let mob = AliveMob { monster: monster.monster_id, label: label_of(monster.monster_id), scene, position };
            self.entities.insert(monster.entity_id, key);
            if self.alive.insert(key, mob) != Some(mob) {
                self.alive_version += 1;
            }
            // Alive by a marker that was defeated: it is back.
            if scene == 3
                && let Some(label) = mob.label
                && let Some(marker) = self.marker_of(label, on_map(position))
                && matches!(self.marks.get(&marker), Some(Some(_)))
            {
                self.set(marker, None);
            }
        }
    }

    /// Things came into view: what a monster drops, with the monster as its owner, tells that
    /// it was killed.  Returns the kinds of the monsters killed.
    pub fn drops(&mut self, gadgets: &[SceneGadget], data: &Data) -> Vec<u32> {
        let mut killed = Vec::new();
        for gadget in gadgets {
            // The owner is where the group of a thing of the world is.
            if gadget.group_id >> 27 != 2 {
                continue;
            }
            let Some(key) = self.entities.remove(&gadget.group_id) else {
                continue;
            };
            let Some(mob) = self.alive.remove(&key) else {
                continue;
            };
            self.alive_version += 1;
            killed.push(mob.monster);
            self.index(data);
            if mob.scene != 3 {
                continue;
            }
            let Some(label) = mob.label else {
                continue;
            };
            if let Some(marker) = self.marker_of(label, on_map(mob.position))
                && let Some(place) = self.marker_place(marker)
                && !self.alive_by(place, false)
            {
                self.set(marker, Some(markers::unix_now()));
            }
        }
        killed
    }

    /// The player is here, as the game tells every few seconds.
    pub fn player_at(&mut self, scene: u32, position: [f32; 3], data: &Data) {
        if self.scene.is_none_or(|(was, _)| was != scene) {
            self.scene = Some((scene, Instant::now()));
            self.near.clear();
        }
        // What is far is sent again when the player is back; what is of another scene is gone.
        let before = self.alive.len();
        self.alive.retain(|_, mob| {
            mob.scene == scene
                && (mob.position[0] - position[0]).hypot(mob.position[2] - position[2]) <= FORGET
        });
        if self.alive.len() != before {
            self.alive_version += 1;
            let alive = &self.alive;
            self.entities.retain(|_, key| alive.contains_key(key));
        }
        let settled = self.scene.is_some_and(|(_, since)| since.elapsed() >= SETTLE);
        if scene != 3 || !settled || self.loaded != Some(scene) {
            return;
        }
        self.index(data);
        let at = on_map(position);
        let here: Vec<(u32, (f32, f32, u32))> = self.around(at, NEAR).map(|(marker, _)| (marker.id, (marker.x, marker.y, marker.label))).collect();
        for (id, place) in &here {
            // An enemy of any kind there: it is not known to be defeated (the kinds of the
            // markers and of the monsters are not always the same).
            if self.alive_by(*place, true) {
                self.near.remove(id);
                continue;
            }
            let times = self.near.entry(*id).or_insert(0);
            *times = times.saturating_add(1);
            if *times >= TIMES && !matches!(self.marks.get(id), Some(Some(_))) {
                self.set(*id, Some(markers::unix_now()));
            }
        }
        self.near.retain(|id, _| here.iter().any(|(near, _)| near == id));
    }

    /// Changes when the markers that are defeated do.
    pub fn version(&self) -> u64 {
        self.marks_version
    }

    /// The markers that are defeated (and since when), and those that are not any more.
    pub fn snapshot(&self) -> (Vec<(u32, u64)>, Vec<u32>) {
        let mut opened = Vec::new();
        let mut closed = Vec::new();
        for (id, state) in &self.marks {
            match state {
                Some(at) => opened.push((*id, *at)),
                None => closed.push(*id),
            }
        }
        (opened, closed)
    }

    pub fn alive_version(&self) -> u64 {
        self.alive_version
    }

    /// The monsters alive around the player.
    pub fn seen(&self) -> MobsSeen {
        MobsSeen { version: self.alive_version, alive: self.alive.values().copied().collect() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monster(entity: u32, kind: u32, group: u32, config: u32, position: [f32; 3]) -> SceneMonster {
        SceneMonster { entity_id: entity, monster_id: kind, group_id: group, config_id: config, position: Some(position) }
    }

    fn drop_of(owner: u32, position: [f32; 3]) -> SceneGadget {
        SceneGadget { entity_id: 0x2000_0001, gadget_id: 70590011, group_id: owner, config_id: 4, position: Some(position), state: None }
    }

    /// A marker of chimeras of the world with no other one near, to try things by.
    fn lonely(data: &Data) -> u32 {
        let points: Vec<&markers::Point> = data.points_of(845).iter().filter(|point| u32::from(point.map) == WORLD).collect();
        points
            .iter()
            .find(|point| points.iter().all(|other| other.id == point.id || (other.x - point.x).hypot(other.y - point.y) > 300.))
            .expect("a marker apart")
            .id
    }

    /// A place in the game by a marker of the map (on the map `dx`, `dy` from it).
    fn by_marker(data: &Data, marker: u32, dx: f32, dy: f32) -> [f32; 3] {
        let point = data.points_of(845).iter().find(|point| point.id == marker).expect("the marker");
        // map x = -z - 750, map y = -x + 2321.
        [-(point.y + dy) + 2321., 450., -(point.x + dx) - 750.]
    }

    /// A raw log of the probe (`IRMINSUL_RAW_LOG`) put through the watch: what it sees.
    #[test]
    #[ignore]
    fn a_raw_log_is_watched() {
        let path = std::env::var("IRMINSUL_RAW_LOG").expect("the log");
        let from = std::env::var("IRMINSUL_RAW_FROM").unwrap_or_default();
        let data = markers::data();
        let mut watch = MobWatch::new();
        let (mut sent, mut kinds) = (0, std::collections::BTreeMap::<String, usize>::new());
        for line in std::fs::read_to_string(path).expect("read").lines() {
            let mut parts = line.split(' ');
            let time = parts.next().unwrap_or_default();
            let Some(hex) = line.rsplit(' ').next() else {
                continue;
            };
            if time < from.as_str() || !line.contains("cmd=4469 ") {
                continue;
            }
            let body: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()).collect();
            let command = auto_artifactarium::GameCommand { command_id: 4469, header_len: 0, data_len: body.len() as u32, proto_data: body, from_client: false };
            if let Some(monsters) = auto_artifactarium::matches_monsters_in_view(&command) {
                sent += monsters.len();
                for monster in &monsters {
                    let name = format!("{} -> {:?}", name_of(monster.monster_id, true).unwrap_or("?"), label_of(monster.monster_id).and_then(|label| data.label(label)).map(|label| label.ru.clone()));
                    *kinds.entry(name).or_default() += 1;
                }
                watch.saw(3, &monsters, &data);
            }
            if let Some(gadgets) = auto_artifactarium::matches_gadgets_in_view(&command) {
                let killed = watch.drops(&gadgets, &data);
                if !killed.is_empty() {
                    println!("{time} killed {:?}; markers defeated now: {:?}", killed.iter().map(|monster| name_of(*monster, true)).collect::<Vec<_>>(), watch.snapshot().0);
                }
            }
        }
        println!("{sent} monsters sent, {} alive at the end", watch.seen().alive.len());
        for (name, count) in kinds {
            println!("  {count} x {name}");
        }
    }

    #[test]
    fn the_kinds_of_the_monsters_are_known() {
        // The chimeras of the user's log, and a variant of a monster (the last two digits).
        assert_eq!(label_of(26310201), Some(845));
        assert_eq!(label_of(26310101), Some(845));
        assert_eq!(label_of(25131602), Some(843));
        assert_eq!(label_of(21010101), Some(55));
        assert_eq!(name_of(26310201, false), Some("Chimeric Burrowbeast"));
        assert!(is_animal(28050110) && !is_animal(26310201));
    }

    #[test]
    fn a_camp_is_defeated_when_its_last_monster_is_killed() {
        let data = markers::data();
        let mut watch = MobWatch::new();
        // Two chimeras by a marker of theirs (as in the user's log).
        let marker = lonely(&data);
        let (a, b) = (by_marker(&data, marker, 5., 4.), by_marker(&data, marker, -8., 10.));
        watch.saw(3, &[monster(0x1000_0001, 26310201, 133709100, 1, a), monster(0x1000_0002, 26310101, 133709100, 2, b)], &data);
        assert_eq!(watch.seen().alive.len(), 2);
        // One killed: the other is still there.
        assert_eq!(watch.drops(&[drop_of(0x1000_0001, a)], &data), vec![26310201]);
        assert!(watch.snapshot().0.is_empty());
        // The other killed: the marker is defeated.
        watch.drops(&[drop_of(0x1000_0002, b), drop_of(0x1000_0002, b)], &data);
        assert_eq!(watch.snapshot().0.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![marker]);
        assert!(watch.seen().alive.is_empty());
        // Sent again later (it came back): the marker is not defeated any more.
        watch.saw(3, &[monster(0x1000_0009, 26310201, 133709100, 1, a)], &data);
        assert_eq!(watch.snapshot(), (Vec::new(), vec![marker]));
    }

    #[test]
    fn a_marker_with_no_enemy_sent_by_it_is_defeated_after_a_while() {
        let data = markers::data();
        let mut watch = MobWatch::new();
        let marker = lonely(&data);
        let here = by_marker(&data, marker, 20., 0.);
        // Something else was sent far away, so the scene is known to send monsters.
        watch.saw(3, &[monster(0x1000_0001, 21010101, 133001001, 1, [here[0] + 250., 450., here[2]])], &data);
        watch.player_at(3, here, &data);
        // Not before the scene is settled.
        assert!(watch.snapshot().0.is_empty());
        watch.scene = Some((3, Instant::now() - Duration::from_secs(60)));
        for _ in 0..TIMES {
            watch.player_at(3, here, &data);
        }
        assert!(watch.snapshot().0.iter().any(|(id, _)| *id == marker));
    }

    #[test]
    fn a_marker_with_an_enemy_by_it_is_not_defeated() {
        let data = markers::data();
        let mut watch = MobWatch::new();
        let marker = lonely(&data);
        let here = by_marker(&data, marker, 20., 0.);
        watch.saw(3, &[monster(0x1000_0001, 26310201, 133709100, 1, by_marker(&data, marker, 5., 4.))], &data);
        watch.scene = Some((3, Instant::now() - Duration::from_secs(60)));
        for _ in 0..TIMES + 2 {
            watch.player_at(3, here, &data);
        }
        assert!(!watch.snapshot().0.iter().any(|(id, _)| *id == marker));
        // Far away, it is forgotten.
        watch.player_at(3, [here[0] + 2000., 450., here[2]], &data);
        assert!(watch.seen().alive.is_empty());
    }
}
