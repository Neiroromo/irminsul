//! The chests of the world, and which of them an account has opened.
//!
//! The game does not say which chests there are.  It says which are in view
//! (the ones that are still closed, when the area is loaded), and when one is
//! opened.  So what is known grows with what is seen.
//!
//! Where the chests are is the same for everyone: it is kept in one list,
//! whichever account saw them (`chest_locations.json`).  Whether a chest is
//! opened is of the account: each account has its own file of marks, with
//! when the chest was seen and when it was opened.  A chest is told by the
//! group it is in and its number in the group, which stay the same from one
//! session to the next.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use auto_artifactarium::{GadgetInteraction, SceneGadget};
use serde::{Deserialize, Serialize};

use crate::coverage::{self, Coverage};
use crate::{APP_ID, profiles};

/// What a kind of chest is, as the game's data tells.
#[derive(Clone, Debug, Deserialize)]
pub struct Kind {
    pub en: String,
    pub ru: String,
    /// 1 common, 2 exquisite, 3 precious, 4 luxurious, 5 remarkable; 0 for the
    /// others.
    pub tier: u8,
    pub locked: bool,
    pub water: bool,
}

/// The kinds of chest, by the id of the gadget.  Made by
/// `cargo run --release --example gen_chest_types`.
pub fn kinds() -> &'static HashMap<u32, Kind> {
    static KINDS: OnceLock<HashMap<u32, Kind>> = OnceLock::new();
    KINDS.get_or_init(|| {
        let raw: HashMap<String, Kind> =
            serde_json::from_str(include_str!("data/chest_types.json")).unwrap_or_default();
        raw.into_iter()
            .filter_map(|(id, kind)| Some((id.parse().ok()?, kind)))
            .collect()
    })
}

/// The name of a chest of this tier.
pub fn tier_name(tier: u8, russian: bool) -> &'static str {
    match (tier, russian) {
        (1, false) => "Common Chest",
        (1, true) => "Обычный сундук",
        (2, false) => "Exquisite Chest",
        (2, true) => "Богатый сундук",
        (3, false) => "Precious Chest",
        (3, true) => "Драгоценный сундук",
        (4, false) => "Luxurious Chest",
        (4, true) => "Роскошный сундук",
        (5, false) => "Remarkable Chest",
        (5, true) => "Удивительный сундук",
        (_, false) => "Chest",
        (_, true) => "Сундук",
    }
}

/// The name of the chest that has this gadget.
pub fn kind_name(gadget: u32, russian: bool) -> String {
    match kinds().get(&gadget) {
        Some(kind) if russian && !kind.ru.is_empty() => kind.ru.clone(),
        Some(kind) if !russian && !kind.en.is_empty() => kind.en.clone(),
        Some(kind) => tier_name(kind.tier, russian).to_owned(),
        None => tier_name(0, russian).to_owned(),
    }
}

/// A chest as shown: where it is (the same for everyone), and what the
/// account in use has of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Chest {
    pub group: u32,
    pub config: u32,
    pub gadget: u32,
    /// Where it is in the game's own coordinates: x, height, z.
    pub position: Option<[f32; 3]>,
    /// When the account saw it, or, if it has not, when anyone first did.
    pub seen_at: String,
    /// When the account opened it, if it did.
    pub opened_at: Option<String>,
    /// Whether the account has seen it (when it was closed).
    pub seen: bool,
}

impl Chest {
    pub fn tier(&self) -> u8 {
        kinds().get(&self.gadget).map_or(0, |kind| kind.tier)
    }
}

/// What the interface shows: the chests there are, with the marks of the
/// account being looked at.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub uid: Option<u32>,
    pub chests: Vec<Chest>,
    /// Where the account has been, as far as it was told of what is there.
    pub coverage: Arc<Coverage>,
    /// Counts the changes, so the interface can tell when to look again.
    pub version: u64,
}

impl Snapshot {
    /// How many are opened, and how many there are, of the usual four kinds.
    pub fn totals(&self) -> (u32, u32) {
        let usual = self.chests.iter().filter(|chest| chest.tier() > 0);
        let known = usual.clone().count() as u32;
        let opened = usual.filter(|chest| chest.opened_at.is_some()).count() as u32;
        (opened, known)
    }
}

/// A chest of the world, as everyone has it.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Location {
    group: u32,
    config: u32,
    gadget: u32,
    #[serde(default)]
    position: Option<[f32; 3]>,
    found_at: String,
}

#[derive(Default, Deserialize, Serialize)]
struct LocationBook {
    #[serde(default)]
    chests: Vec<Location>,
}

/// What an account has of a chest.  Older files also had where the chest is;
/// that is read (and moved to the list everyone has) but not written again.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Mark {
    group: u32,
    config: u32,
    seen_at: String,
    #[serde(default)]
    opened_at: Option<String>,
    #[serde(default, skip_serializing)]
    gadget: Option<u32>,
    #[serde(default, skip_serializing)]
    position: Option<[f32; 3]>,
}

#[derive(Default, Deserialize, Serialize)]
struct MarkBook {
    #[serde(default)]
    chests: Vec<Mark>,
}

/// What coming into view came to: how many chests of each tier were not told
/// about yet.
#[derive(Debug, Default, PartialEq)]
pub struct ViewReport {
    pub new_by_tier: std::collections::BTreeMap<u8, u32>,
    /// What each chest that was told of is (gadget, state, place), for the log.
    pub details: Vec<String>,
}

/// A chest that was opened.
#[derive(Debug, PartialEq)]
pub struct Opened {
    pub gadget: u32,
    pub tier: u8,
    /// How many are opened, and how many are known.
    pub totals: (u32, u32),
}

/// A chest opened that was not seen coming into view is the one of the list this near
/// the player (metres across, and up or down).
const BY_PLAYER_ACROSS: f32 = 14.;
const BY_PLAYER_HEIGHT: f32 = 25.;

/// How long after a chest begins to open the rewards are taken for the proof
/// that it did.
const REWARD_WINDOW: Duration = Duration::from_secs(3);

type Key = (u32, u32);

pub struct Tracker {
    locations_path: Option<PathBuf>,
    accounts_dir: Option<PathBuf>,
    uid: Option<u32>,
    locations: Vec<Location>,
    location_index: HashMap<Key, usize>,
    marks: HashMap<Key, Mark>,
    locations_dirty: bool,
    marks_dirty: bool,
    /// Where the account has been.
    coverage: Arc<Coverage>,
    coverage_dirty: bool,
    coverage_saved: Option<Instant>,
    /// Where the player was last told to be, and when.
    last_place: Option<(Instant, f32, f32)>,
    /// The same in the game's coordinates (x, height, z), for a chest that is opened
    /// and was not seen coming into view (the program was started after).
    last_position: Option<(Instant, [f32; 3])>,
    version: u64,
    /// In this session: the chest an entity is.  Entities are numbered anew each
    /// time the game is entered.
    entities: HashMap<u32, Key>,
    /// In this session: the chests the user was told about.
    told: HashSet<Key>,
    /// A chest that began to open: its entity, and when.
    pending: Option<(u32, u32, Instant)>,
    /// Chests opened that were not found in the list, told once.
    told_opened: HashSet<u32>,
}

fn marks_path(dir: &Path, uid: u32) -> PathBuf {
    dir.join(format!("{uid}.json"))
}

fn coverage_path(dir: &Path, uid: u32) -> PathBuf {
    dir.join(format!("{uid}.coverage"))
}

/// How seldom the places walked are written: they are a lot of bytes, and
/// change all the time while walking.
const COVERAGE_SAVE_EVERY: Duration = Duration::from_secs(20);

/// Writes a file next to it and renames, so an interrupted write leaves
/// nothing half done.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_file_as(path, "json.tmp", bytes)
}

fn write_file_as(path: &Path, temporary: &str, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(temporary);
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)
}

impl Tracker {
    /// `IRMINSUL_CHEST_LOCATIONS` replaces the file of the places, for trying
    /// things out without touching the real one.
    pub fn new() -> Self {
        let locations = std::env::var_os("IRMINSUL_CHEST_LOCATIONS")
            .map(PathBuf::from)
            .or_else(|| eframe::storage_dir(APP_ID).map(|dir| dir.join("chest_locations.json")));
        let accounts = profiles::profiles_dir().ok().map(|dir| dir.join("chests"));
        Self::with_paths(locations, accounts)
    }

    /// For tests: everything in one folder.
    pub fn with_dir(dir: Option<PathBuf>) -> Self {
        Self::with_paths(
            dir.as_ref().map(|dir| dir.join("chest_locations.json")),
            dir.map(|dir| dir.join("chests")),
        )
    }

    fn with_paths(locations_path: Option<PathBuf>, accounts_dir: Option<PathBuf>) -> Self {
        let mut tracker = Self {
            locations_path,
            accounts_dir,
            uid: None,
            locations: Vec::new(),
            location_index: HashMap::new(),
            marks: HashMap::new(),
            locations_dirty: false,
            marks_dirty: false,
            coverage: Arc::new(Coverage::new()),
            coverage_dirty: false,
            coverage_saved: None,
            last_place: None,
            last_position: None,
            version: 0,
            entities: HashMap::new(),
            told: HashSet::new(),
            pending: None,
            told_opened: HashSet::new(),
        };
        tracker.load_locations();
        tracker
    }

    fn load_locations(&mut self) {
        let Some(path) = &self.locations_path else {
            return;
        };
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<LocationBook>(&text) {
                Ok(book) => self.locations = book.chests,
                Err(e) => tracing::warn!("unable to read the chest locations: {e}"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => tracing::warn!("unable to read the chest locations: {e}"),
        }
        self.location_index = self
            .locations
            .iter()
            .enumerate()
            .map(|(at, location)| ((location.group, location.config), at))
            .collect();
    }

    /// A new visit to the game: nothing of the last one carries over but what
    /// is stored.
    pub fn start_session(&mut self) {
        self.entities.clear();
        self.told.clear();
        self.pending = None;
        self.told_opened.clear();
    }

    pub fn account(&self) -> Option<u32> {
        self.uid
    }

    /// Adds a chest to the list everyone has, if it is not there.
    fn add_location(&mut self, key: Key, gadget: u32, position: Option<[f32; 3]>, found_at: &str) {
        match self.location_index.get(&key).copied() {
            Some(at) => {
                let location = &mut self.locations[at];
                if location.position.is_none() && position.is_some() {
                    location.position = position;
                    self.locations_dirty = true;
                    self.version += 1;
                }
            }
            None => {
                self.location_index.insert(key, self.locations.len());
                self.locations.push(Location {
                    group: key.0,
                    config: key.1,
                    gadget,
                    position,
                    found_at: found_at.to_owned(),
                });
                self.locations_dirty = true;
                self.version += 1;
            }
        }
    }

    /// Makes the marks held those of this account.
    pub fn use_account(&mut self, uid: Option<u32>) {
        if self.uid == uid {
            return;
        }
        self.save(true);
        self.uid = uid;
        self.marks.clear();
        self.coverage = Arc::new(Coverage::new());
        self.coverage_dirty = false;
        self.last_place = None;
        if let (Some(uid), Some(dir)) = (uid, &self.accounts_dir) {
            match fs::read(coverage_path(dir, uid)) {
                Ok(bytes) => match Coverage::from_bytes(&bytes) {
                    Some(places) => self.coverage = Arc::new(places),
                    None => tracing::warn!("the places walked by {uid} could not be read"),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => tracing::warn!("unable to read the places walked by {uid}: {e}"),
            }
        }
        if let (Some(uid), Some(dir)) = (uid, self.accounts_dir.clone()) {
            match fs::read_to_string(marks_path(&dir, uid)) {
                Ok(text) => match serde_json::from_str::<MarkBook>(&text) {
                    Ok(book) => {
                        for mark in book.chests {
                            // Where a chest is was kept here once; it is
                            // everyone's now.
                            if let Some(gadget) = mark.gadget {
                                self.add_location(
                                    (mark.group, mark.config),
                                    gadget,
                                    mark.position,
                                    &mark.seen_at,
                                );
                                self.marks_dirty = true;
                            }
                            self.marks.insert((mark.group, mark.config), mark);
                        }
                    }
                    Err(e) => tracing::warn!("unable to read the chests of {uid}: {e}"),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => tracing::warn!("unable to read the chests of {uid}: {e}"),
            }
        }
        // What the entities were is of the account they were seen for.
        self.entities.clear();
        self.told.clear();
        self.version += 1;
    }

    /// The account of the chests seen before it was known is told: what was
    /// marked is added to the marks it has.
    pub fn adopt_account(&mut self, uid: u32) {
        if self.uid.is_some() {
            return self.use_account(Some(uid));
        }
        let seen = std::mem::take(&mut self.marks);
        let entities = std::mem::take(&mut self.entities);
        let told = std::mem::take(&mut self.told);
        let walked = std::mem::replace(&mut self.coverage, Arc::new(Coverage::new()));
        let last_place = self.last_place;
        self.use_account(Some(uid));
        if Arc::make_mut(&mut self.coverage).merge(&walked) {
            self.coverage_dirty = true;
        }
        self.last_place = last_place;
        for (key, mark) in seen {
            match self.marks.get_mut(&key) {
                Some(have) => {
                    if mark.opened_at.is_some() && have.opened_at.is_none() {
                        have.opened_at = mark.opened_at;
                    }
                }
                None => {
                    self.marks.insert(key, mark);
                }
            }
        }
        // The entities of this session are still those, for this account.
        self.entities = entities;
        self.told = told;
        self.marks_dirty = true;
        self.version += 1;
    }

    /// How many are opened, and how many are known.
    fn totals(&self) -> (u32, u32) {
        let usual = self
            .locations
            .iter()
            .filter(|location| kinds().get(&location.gadget).is_some_and(|kind| kind.tier > 0));
        let known = usual.clone().count() as u32;
        let opened = usual
            .filter(|location| {
                self.marks
                    .get(&(location.group, location.config))
                    .is_some_and(|mark| mark.opened_at.is_some())
            })
            .count() as u32;
        (opened, known)
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        let chests = self
            .locations
            .iter()
            .map(|location| {
                let mark = self.marks.get(&(location.group, location.config));
                Chest {
                    group: location.group,
                    config: location.config,
                    gadget: location.gadget,
                    position: location.position,
                    seen_at: mark.map_or_else(|| location.found_at.clone(), |mark| mark.seen_at.clone()),
                    opened_at: mark.and_then(|mark| mark.opened_at.clone()),
                    seen: mark.is_some(),
                }
            })
            .collect();
        Arc::new(Snapshot {
            uid: self.uid,
            chests,
            coverage: self.coverage.clone(),
            version: self.version,
        })
    }

    pub fn save_if_dirty(&mut self) {
        self.save(false);
    }

    /// Stores what changed.  The places walked are stored now and then, unless
    /// `force` says it is the last chance.
    fn save(&mut self, force: bool) {
        if self.coverage_dirty
            && let (Some(uid), Some(dir)) = (self.uid, &self.accounts_dir)
            && (force || self.coverage_saved.is_none_or(|at| at.elapsed() >= COVERAGE_SAVE_EVERY))
        {
            self.coverage_dirty = false;
            self.coverage_saved = Some(Instant::now());
            let path = coverage_path(dir, uid);
            if let Err(e) = write_file_as(&path, "coverage.tmp", &self.coverage.to_bytes()) {
                tracing::error!("unable to store the places walked by {uid}: {e}");
            }
        }
        if self.locations_dirty
            && let Some(path) = &self.locations_path
        {
            self.locations_dirty = false;
            let book = LocationBook {
                chests: self.locations.clone(),
            };
            if let Err(e) = write_file(path, &serde_json::to_vec(&book).unwrap_or_default()) {
                tracing::error!("unable to store the chest locations: {e}");
            }
        }
        if self.marks_dirty
            && let (Some(uid), Some(dir)) = (self.uid, &self.accounts_dir)
        {
            self.marks_dirty = false;
            let mut chests: Vec<Mark> = self.marks.values().cloned().collect();
            chests.sort_by_key(|mark| (mark.group, mark.config));
            let book = MarkBook { chests };
            if let Err(e) = write_file(
                &marks_path(dir, uid),
                &serde_json::to_vec(&book).unwrap_or_default(),
            ) {
                tracing::error!("unable to store the chests of {uid}: {e}");
            }
        }
    }

    /// The player is at this place of the scene, as the game tells every few
    /// seconds.  What is around it has been sent: the place is marked, and so is
    /// the way from where the player was a moment ago, unless that is too far to
    /// have been walked.  Returns whether there is anything new.
    pub fn player_at(&mut self, scene: u32, position: [f32; 3]) -> bool {
        // Only the open world is on the map (the scene of Teyvat is 3).
        if scene != 3 {
            self.last_place = None;
            return false;
        }
        let (x, y) = crate::official_chests::game_to_map(position);
        let now = Instant::now();
        self.last_position = Some((now, position));
        let coverage = Arc::make_mut(&mut self.coverage);
        let changed = match self.last_place {
            Some((at, last_x, last_y))
                if now.duration_since(at) <= Duration::from_secs(20)
                    && (x - last_x).hypot(y - last_y) <= coverage::MAX_STEP =>
            {
                coverage.mark_walk((last_x, last_y), (x, y))
            }
            _ => coverage.mark_disc(x, y),
        };
        self.last_place = Some((now, x, y));
        if changed {
            self.coverage_dirty = true;
            self.version += 1;
        }
        changed
    }

    /// Takes in what came into view.  Chests are added to the list, if they
    /// were not in it, and marked seen, closed, for the account.
    pub fn gadgets_in_view(&mut self, gadgets: &[SceneGadget]) -> ViewReport {
        self.gadgets_in_scene(gadgets, true)
    }

    /// Chests came into view; `world` is whether the scene is the world, where
    /// the places of the game are those of the map: the places of the chests of
    /// other scenes are not kept (they would be put where they are not).
    pub fn gadgets_in_scene(&mut self, gadgets: &[SceneGadget], world: bool) -> ViewReport {
        let mut report = ViewReport::default();
        let now = profiles::now();
        for gadget in gadgets {
            let Some(kind) = kinds().get(&gadget.gadget_id) else {
                continue;
            };
            if kind.tier == 0 {
                continue;
            }
            let key = (gadget.group_id, gadget.config_id);
            self.entities.insert(gadget.entity_id, key);
            // A chest the game says is opened (it does not send those in the world,
            // but may in other scenes) is opened, and is not told of as found.
            if gadget.state == Some(102) {
                match self.marks.get_mut(&key) {
                    Some(mark) if mark.opened_at.is_none() => {
                        mark.opened_at = Some(now.clone());
                        self.marks_dirty = true;
                        self.version += 1;
                    }
                    Some(_) => (),
                    None => {
                        self.marks.insert(
                            key,
                            Mark {
                                group: key.0,
                                config: key.1,
                                seen_at: now.clone(),
                                opened_at: Some(now.clone()),
                                gadget: None,
                                position: None,
                            },
                        );
                        self.marks_dirty = true;
                        self.version += 1;
                    }
                }
                self.told.insert(key);
                continue;
            }
            if world {
                self.add_location(key, gadget.gadget_id, gadget.position, &now);
            }
            match self.marks.get_mut(&key) {
                Some(mark) => {
                    // In view as closed: it is not opened, whatever was thought.
                    if mark.opened_at.take().is_some() {
                        self.marks_dirty = true;
                        self.version += 1;
                    }
                }
                None => {
                    self.marks.insert(
                        key,
                        Mark {
                            group: key.0,
                            config: key.1,
                            seen_at: now.clone(),
                            opened_at: None,
                            gadget: None,
                            position: None,
                        },
                    );
                    self.marks_dirty = true;
                    self.version += 1;
                }
            }
            if self.told.insert(key) {
                *report.new_by_tier.entry(kind.tier).or_insert(0) += 1;
                report.details.push(format!(
                    "gadget {} state {:?} at {:?}{}",
                    gadget.gadget_id,
                    gadget.state,
                    gadget.position.map(|p| [p[0].round(), p[1].round(), p[2].round()]),
                    if world { "" } else { " (not in the world)" }
                ));
            }
        }
        report
    }

    /// Where the player is now, as the client says as it moves (not told to the coverage:
    /// that is of what the game says).
    pub fn note_position(&mut self, scene: u32, position: [f32; 3]) {
        if scene == 3 {
            self.last_position = Some((Instant::now(), position));
        }
    }

    /// What the game says was done to a gadget.  A chest that is done opening
    /// is told by this, once.
    pub fn interaction(&mut self, event: &GadgetInteraction) -> Option<Opened> {
        // 3 is opening a chest.
        if event.kind != 3 || !kinds().contains_key(&event.gadget_id) {
            return None;
        }
        if event.begins {
            self.pending = Some((event.entity_id, event.gadget_id, Instant::now()));
            return None;
        }
        self.pending = None;
        self.opened(event.entity_id, event.gadget_id)
    }

    /// Rewards came (items, mora): a chest that began to open a moment ago is
    /// open.
    pub fn rewards(&mut self) -> Option<Opened> {
        let (entity, gadget, at) = self.pending?;
        if at.elapsed() > REWARD_WINDOW {
            self.pending = None;
            return None;
        }
        self.pending = None;
        self.opened(entity, gadget)
    }

    /// The chest of the list, of this tier, not opened yet, that is by the player: for
    /// a chest that was opened and was not seen coming into view (the program was
    /// started while the game ran).  The one only that is near enough, and alone.
    fn chest_by_player(&self, tier: u8) -> Option<Key> {
        let (when, at) = self.last_position?;
        if when.elapsed() > Duration::from_secs(20) {
            return None;
        }
        let mut found: Vec<(f32, Key)> = Vec::new();
        for location in &self.locations {
            let Some(position) = location.position else {
                continue;
            };
            if kinds().get(&location.gadget).map_or(0, |kind| kind.tier) != tier {
                continue;
            }
            let key = (location.group, location.config);
            if self.marks.get(&key).is_some_and(|mark| mark.opened_at.is_some()) {
                continue;
            }
            let across = (position[0] - at[0]).hypot(position[2] - at[2]);
            if across <= BY_PLAYER_ACROSS && (position[1] - at[1]).abs() <= BY_PLAYER_HEIGHT {
                found.push((across, key));
            }
        }
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        match found.as_slice() {
            [(_, key)] => Some(*key),
            [(first, key), (second, _), ..] if *second >= *first * 2. => Some(*key),
            _ => None,
        }
    }

    fn opened(&mut self, entity: u32, gadget: u32) -> Option<Opened> {
        let tier = kinds().get(&gadget).map_or(0, |kind| kind.tier);
        let known = self.entities.get(&entity).copied().or_else(|| {
            let key = self.chest_by_player(tier)?;
            self.entities.insert(entity, key);
            Some(key)
        });
        match known {
            Some(key) => {
                let now = profiles::now();
                let mark = self.marks.entry(key).or_insert_with(|| Mark {
                    group: key.0,
                    config: key.1,
                    seen_at: now.clone(),
                    opened_at: None,
                    gadget: None,
                    position: None,
                });
                if mark.opened_at.is_some() {
                    return None;
                }
                mark.opened_at = Some(now);
                self.marks_dirty = true;
                self.version += 1;
            }
            // Not seen coming into view: told, but there is no chest of the
            // list to mark.
            None => {
                if !self.told_opened.insert(entity) {
                    return None;
                }
            }
        }
        Some(Opened {
            gadget,
            tier,
            totals: self.totals(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const COMMON: u32 = 70_211_002;
    const EXQUISITE: u32 = 70_211_012;

    fn gadget(entity: u32, gadget: u32, config: u32) -> SceneGadget {
        SceneGadget {
            entity_id: entity,
            gadget_id: gadget,
            group_id: 133_003_378,
            config_id: config,
            position: Some([2468., 206.3, -1234.3]),
            state: None,
        }
    }

    fn interaction(entity: u32, begins: bool) -> GadgetInteraction {
        GadgetInteraction {
            gadget_id: COMMON,
            entity_id: entity,
            kind: 3,
            begins,
        }
    }

    fn tracker() -> (Tracker, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = Tracker::with_dir(Some(dir.path().to_path_buf()));
        tracker.use_account(Some(700_000_001));
        (tracker, dir)
    }

    #[test]
    fn the_kinds_of_chest_are_known() {
        assert_eq!(kinds().get(&COMMON).map(|kind| kind.tier), Some(1));
        assert_eq!(kinds().get(&EXQUISITE).map(|kind| kind.tier), Some(2));
        assert_eq!(kind_name(COMMON, true), "Обычный сундук");
        assert_eq!(kind_name(EXQUISITE, false), "Exquisite Chest");
    }

    #[test]
    fn chests_in_view_are_added_and_told_once() {
        let (mut tracker, _dir) = tracker();
        let view = [gadget(1, COMMON, 4156), gadget(2, EXQUISITE, 4157)];
        let report = tracker.gadgets_in_view(&view);
        assert_eq!(report.new_by_tier, BTreeMap::from([(1, 1), (2, 1)]));
        // The same again says nothing new.
        assert!(tracker.gadgets_in_view(&view).new_by_tier.is_empty());
        assert_eq!(tracker.snapshot().chests.len(), 2);
        // What is not a chest is not taken.
        assert!(tracker.gadgets_in_view(&[gadget(3, 70_520_005, 9)]).new_by_tier.is_empty());
        assert_eq!(tracker.snapshot().chests.len(), 2);
        // Where it is, is kept.
        assert_eq!(tracker.snapshot().chests[0].position.map(|p| p[0]), Some(2468.));
    }

    #[test]
    fn a_chest_is_opened_when_the_game_says_it_is_done() {
        let (mut tracker, _dir) = tracker();
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4156), gadget(2, COMMON, 4157)]);
        // It begins to open: not yet.
        assert_eq!(tracker.interaction(&interaction(1, true)), None);
        let opened = tracker.interaction(&interaction(1, false)).unwrap();
        assert_eq!((opened.gadget, opened.tier, opened.totals), (COMMON, 1, (1, 2)));
        // Told once.
        assert_eq!(tracker.interaction(&interaction(1, false)), None);
        let chests = tracker.snapshot().chests.clone();
        assert!(chests[0].opened_at.is_some());
        assert!(chests[1].opened_at.is_none());
    }

    #[test]
    fn rewards_after_a_beginning_are_the_proof() {
        let (mut tracker, _dir) = tracker();
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4156)]);
        assert_eq!(tracker.rewards(), None, "nothing began");
        tracker.interaction(&interaction(1, true));
        assert_eq!(tracker.rewards().unwrap().totals, (1, 1));
        assert_eq!(tracker.rewards(), None);
    }

    #[test]
    fn other_things_done_to_gadgets_are_not_chests_opened() {
        let (mut tracker, _dir) = tracker();
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4156)]);
        let pickup = GadgetInteraction {
            gadget_id: 70_600_041,
            entity_id: 1,
            kind: 1,
            begins: false,
        };
        assert_eq!(tracker.interaction(&pickup), None);
        assert!(tracker.snapshot().chests[0].opened_at.is_none());
    }

    #[test]
    fn what_was_seen_and_opened_outlives_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = Tracker::with_dir(Some(dir.path().to_path_buf()));
        first.use_account(Some(700_000_001));
        first.gadgets_in_view(&[gadget(1, COMMON, 4156), gadget(2, COMMON, 4157)]);
        first.interaction(&interaction(1, false));
        first.save_if_dirty();

        let mut second = Tracker::with_dir(Some(dir.path().to_path_buf()));
        second.use_account(Some(700_000_001));
        assert_eq!(second.snapshot().chests.len(), 2);
        assert_eq!(second.snapshot().totals(), (1, 2));
        // Next time in the game, the entities are numbered anew; the chest is
        // the same by its group and config.
        second.start_session();
        let report = second.gadgets_in_view(&[gadget(900, COMMON, 4157)]);
        assert_eq!(report.new_by_tier, BTreeMap::from([(1, 1)]));
        assert_eq!(second.snapshot().chests.len(), 2);
    }

    #[test]
    fn where_chests_are_is_for_everyone_and_what_is_opened_for_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = Tracker::with_dir(Some(dir.path().to_path_buf()));
        tracker.use_account(Some(700_000_001));
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4156), gadget(2, COMMON, 4157)]);
        tracker.interaction(&interaction(1, false));
        assert_eq!(tracker.snapshot().totals(), (1, 2));

        // Another account: the same chests are there, none of them opened.
        tracker.use_account(Some(1_826_078_112));
        let other = tracker.snapshot();
        assert_eq!(other.chests.len(), 2);
        assert_eq!(other.totals(), (0, 2));
        assert!(other.chests.iter().all(|chest| chest.position.is_some()));

        // It opens one of them, and sees another one.
        tracker.start_session();
        tracker.gadgets_in_view(&[gadget(7, COMMON, 4157), gadget(8, EXQUISITE, 4200)]);
        tracker.interaction(&interaction(7, false));
        assert_eq!(tracker.snapshot().totals(), (1, 3));
        tracker.save_if_dirty();

        // Each has its own marks, and everyone the places.
        tracker.use_account(Some(700_000_001));
        let first = tracker.snapshot();
        assert_eq!(first.chests.len(), 3);
        assert_eq!(first.totals(), (1, 3));
        let opened_by_first: Vec<u32> = first
            .chests
            .iter()
            .filter(|chest| chest.opened_at.is_some())
            .map(|chest| chest.config)
            .collect();
        assert_eq!(opened_by_first, vec![4156]);
    }

    #[test]
    fn chests_seen_before_the_account_was_known_go_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = Tracker::with_dir(Some(dir.path().to_path_buf()));
        tracker.use_account(Some(700_000_001));
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4100)]);
        tracker.save_if_dirty();

        // A session whose account is not known yet.
        tracker.use_account(None);
        tracker.start_session();
        tracker.gadgets_in_view(&[gadget(5, COMMON, 4156), gadget(6, COMMON, 4157)]);
        tracker.interaction(&interaction(5, false));
        assert_eq!(tracker.snapshot().totals(), (1, 3));

        // Now it is: its own marks, with what was seen.
        tracker.adopt_account(1_826_078_112);
        assert_eq!(tracker.account(), Some(1_826_078_112));
        assert_eq!(tracker.snapshot().totals(), (1, 3));
        tracker.save_if_dirty();
        // Chests that are opened after are those of the entities of this session.
        tracker.interaction(&interaction(6, false));
        assert_eq!(tracker.snapshot().totals(), (2, 3));

        // The other account has not opened them, though it knows where they are.
        tracker.use_account(Some(700_000_001));
        assert_eq!(tracker.snapshot().chests.len(), 3);
        assert_eq!(tracker.snapshot().totals(), (0, 3));
    }

    #[test]
    fn where_the_player_walks_is_marked_and_kept_for_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = Tracker::with_dir(Some(dir.path().to_path_buf()));
        tracker.use_account(Some(700_000_001));
        let here = [2477.0, 206.0, -1239.0];
        assert!(tracker.player_at(3, here));
        assert!(!tracker.player_at(3, here), "nothing new");
        let (x, y) = crate::official_chests::game_to_map(here);
        assert!(tracker.snapshot().coverage.is_covered(x, y));
        // Another scene is not on the map.
        assert!(!tracker.player_at(7, [0., 0., 0.]));
        // A jump is not a walk: nothing between is marked.
        tracker.player_at(3, [2477.0, 206.0, -1239.0 - 2000.0]);
        let (mx, my) = crate::official_chests::game_to_map([2477.0, 206.0, -1239.0 - 1000.0]);
        assert!(!tracker.snapshot().coverage.is_covered(mx, my));

        // Kept for the account, not for another.
        tracker.use_account(Some(1_826_078_112));
        assert!(!tracker.snapshot().coverage.is_covered(x, y));
        tracker.use_account(Some(700_000_001));
        assert!(tracker.snapshot().coverage.is_covered(x, y));
    }

    #[test]
    fn what_was_walked_before_the_account_was_known_goes_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = Tracker::with_dir(Some(dir.path().to_path_buf()));
        tracker.use_account(None);
        tracker.player_at(3, [2477.0, 206.0, -1239.0]);
        tracker.adopt_account(1_826_078_112);
        let (x, y) = crate::official_chests::game_to_map([2477.0, 206.0, -1239.0]);
        assert!(tracker.snapshot().coverage.is_covered(x, y));
    }

    #[test]
    fn a_chest_seen_closed_again_is_not_opened() {
        let (mut tracker, _dir) = tracker();
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4156)]);
        tracker.interaction(&interaction(1, false));
        assert_eq!(tracker.snapshot().totals(), (1, 1));
        tracker.start_session();
        tracker.gadgets_in_view(&[gadget(7, COMMON, 4156)]);
        assert_eq!(tracker.snapshot().totals(), (0, 1));
    }

    #[test]
    fn old_files_have_their_places_moved_to_the_shared_list() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("chests");
        fs::create_dir_all(&accounts).unwrap();
        // As a file of an account was when it had the places too.
        fs::write(
            accounts.join("700000001.json"),
            r#"{"chests":[{"group":133003378,"config":4156,"gadget":70211002,
                "position":[2468.0,206.3,-1234.3],"seen_at":"2026-11-07T23:00:00+03:00",
                "opened_at":"2026-11-07T23:39:03+03:00"}]}"#,
        )
        .unwrap();
        let mut tracker = Tracker::with_dir(Some(dir.path().to_path_buf()));
        tracker.use_account(Some(700_000_001));
        assert_eq!(tracker.snapshot().chests.len(), 1);
        assert_eq!(tracker.snapshot().totals(), (1, 1));
        tracker.save_if_dirty();

        // Another account, in another run, knows where it is.
        let mut other = Tracker::with_dir(Some(dir.path().to_path_buf()));
        other.use_account(Some(1_826_078_112));
        assert_eq!(other.snapshot().chests.len(), 1);
        assert_eq!(other.snapshot().totals(), (0, 1));
        let text = fs::read_to_string(accounts.join("700000001.json")).unwrap();
        assert!(!text.contains("gadget"), "the places are not in the file of the account");
    }

    #[test]
    fn a_chest_opened_that_was_not_seen_coming_into_view_is_the_one_by_the_player() {
        let (mut tracker, _dir) = tracker();
        // Seen in an earlier run (kept in the list); this run did not see it come.
        tracker.gadgets_in_view(&[gadget(1, COMMON, 4156)]);
        tracker.entities.clear();
        tracker.last_position = Some((Instant::now(), [2460., 215., -1234.]));
        let opened = tracker.interaction(&interaction(99, false)).unwrap();
        assert_eq!((opened.gadget, opened.totals), (COMMON, (1, 1)));
        assert!(tracker.snapshot().chests[0].opened_at.is_some());
        // Not near: nothing of the list is marked.
        let (mut far, _dir2) = self::tracker();
        far.gadgets_in_view(&[gadget(1, COMMON, 4156)]);
        far.entities.clear();
        far.last_position = Some((Instant::now(), [2300., 215., -1234.]));
        far.interaction(&interaction(98, false));
        assert!(far.snapshot().chests[0].opened_at.is_none());
    }
}
