//! The markers of the official interactive map: kinds of marker (resources,
//! oculi, waypoints, enemies, and the rest) and where each marker is; and which
//! of them an account has collected, and when.
//!
//! The list is what the program was built with, and is fetched again by itself
//! now and then (the map's markers are added to with each version of the game).
//! What is collected is kept for each account, with the time, because what is
//! collected comes back after a time: plants, ores, enemies and the like
//! "refresh", the map says after how many hours.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::atlas::Atlas;
use crate::{APP_ID, profiles};

/// The labels of the chests of the map, which the chests part of the program
/// shows (and finds in the game) by itself.
pub const CHEST_LABELS: [u32; 5] = [17, 44, 45, 46, 269];

/// The kind of marker of the domains.
pub const DOMAIN_LABEL: u32 = 154;

/// A daily domain, from the file of the sources of materials.
#[derive(Clone, Debug, Deserialize)]
pub struct DomainRow {
    pub city: u32,
    pub kind: String,
    /// The days it is open, 0 for Monday.
    pub days: Vec<u8>,
    pub en: String,
    pub ru: String,
}

#[derive(Deserialize)]
struct SourceFile {
    /// Item -> kinds of marker of the enemies that drop it.
    mobs: HashMap<u32, Vec<u32>>,
    /// Item -> the daily domains it is got in.
    domains: HashMap<u32, Vec<DomainRow>>,
    /// "<city><M|F>" -> the marker of the domains of talents / weapons of that city.
    points: HashMap<String, u32>,
    /// Marker -> the artifact sets of the domain.
    #[serde(default)]
    artifacts: HashMap<u32, Vec<ArtifactSet>>,
    /// Marker -> the one-time or trounce domain it is.
    #[serde(default)]
    story: HashMap<u32, StoryDomain>,
}

/// An item that a story domain gives, named in both languages.
#[derive(Clone, Debug, Deserialize)]
pub struct Reward {
    pub en: String,
    pub ru: String,
    pub icon: String,
    #[serde(default)]
    pub rank: u32,
    /// The item of the game it is, 0 if it is not a material.
    #[serde(default)]
    pub id: u32,
    /// How many (rewards of the first completion only).
    #[serde(default)]
    pub qty: u32,
}

/// A one-time domain (a reward for the first completion) or a trounce domain (a weekly boss).
#[derive(Clone, Debug, Deserialize)]
pub struct StoryDomain {
    /// "once" or "trounce".
    pub kind: String,
    pub en: String,
    pub ru: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub rewards: Vec<Reward>,
    /// The materials only this boss drops.
    #[serde(default)]
    pub drops: Vec<Reward>,
    #[serde(default)]
    pub boss: String,
}

/// The one-time or trounce domain at a marker, if the program knows it.
pub fn story_domain(point: u32) -> Option<&'static StoryDomain> {
    source_file().story.get(&point)
}

/// An artifact set that a domain gives.
#[derive(Clone, Debug, Deserialize)]
pub struct ArtifactSet {
    pub id: u32,
    pub en: String,
    pub ru: String,
    /// The name of its picture.
    pub icon: String,
}

/// The artifact sets a domain of the map gives, if it is an artifact domain the program knows.
pub fn artifact_domain(point: u32) -> Option<&'static [ArtifactSet]> {
    source_file().artifacts.get(&point).map(Vec::as_slice).filter(|sets| !sets.is_empty())
}

/// Whether a click on this marker tells what the domain gives.
pub fn domain_described(point: u32) -> bool {
    artifact_domain(point).is_some() || domain_info(point).is_some() || story_domain(point).is_some()
}

fn source_file() -> &'static SourceFile {
    static FILE: std::sync::OnceLock<SourceFile> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        serde_json::from_str(include_str!("data/material_sources.json"))
            .unwrap_or(SourceFile { mobs: HashMap::new(), domains: HashMap::new(), points: HashMap::new(), artifacts: HashMap::new(), story: HashMap::new() })
    })
}

/// What a daily domain gives on which days.
#[derive(Clone, Debug)]
pub struct DomainDay {
    pub en: String,
    pub ru: String,
    /// The days it is open, 0 for Monday.
    pub days: Vec<u8>,
    /// What it gives (the items of the game).
    pub items: Vec<u32>,
}

/// The daily domains a marker of the map stands for (the one of the talents or of the weapons
/// of a nation), with their days and what they give; none for any other marker.  The first is
/// whether it is the domain of the talents.
pub fn domain_info(point: u32) -> Option<(bool, u32, Vec<DomainDay>)> {
    let file = source_file();
    let key = file.points.iter().find(|(_, id)| **id == point).map(|(key, _)| key.clone())?;
    let (city, kind) = key.split_at(key.len() - 1);
    let city: u32 = city.parse().ok()?;
    let talents = kind == "M";
    let mut days: Vec<DomainDay> = Vec::new();
    let mut items: Vec<(&u32, &Vec<DomainRow>)> = file.domains.iter().collect();
    items.sort_by_key(|(item, _)| **item);
    for (item, rows) in items {
        for row in rows.iter().filter(|row| row.city == city && row.kind == kind) {
            match days.iter_mut().find(|day| day.en == row.en) {
                Some(day) => day.items.push(*item),
                None => days.push(DomainDay { en: row.en.clone(), ru: row.ru.clone(), days: row.days.clone(), items: vec![*item] }),
            }
        }
    }
    days.sort_by_key(|day| day.days.first().copied().unwrap_or(7));
    (!days.is_empty()).then_some((talents, city, days))
}

/// Where an item is got.
#[derive(Clone, Debug, Default)]
pub struct Where {
    /// The kinds of marker.
    pub labels: Vec<u32>,
    /// The markers of the domains.
    pub points: Vec<u32>,
    pub domains: Vec<DomainRow>,
}

impl Where {
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.points.is_empty()
    }

    /// Whether some domain of these is open on a day (0 for Monday); true when there are none.
    pub fn open_on(&self, day: u8) -> bool {
        self.domains.is_empty() || self.domains.iter().any(|domain| domain.days.contains(&day))
    }
}

/// The day of the week of the game now, 0 for Monday: the day changes at 4 o'clock.
pub fn game_weekday() -> u8 {
    use chrono::Datelike;
    (chrono::Local::now() - chrono::Duration::hours(4)).weekday().num_days_from_monday() as u8
}

/// The kinds of marker that are shown at first.
pub const DEFAULT_SHOWN: [u32; 8] = [17, 44, 45, 46, 269, 2, 3, 154];

const API: &str = "https://sg-public-api-static.hoyolab.com/common/map_user/ys_obc";
/// The list is fetched again when it is this old.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// A kind of marker.
#[derive(Clone, Debug, Deserialize)]
pub struct Label {
    pub id: u32,
    /// The group it is in, 0 for a group.
    pub parent: u32,
    pub en: String,
    pub ru: String,
    /// Where its picture is.
    pub icon: String,
    /// After how many hours what is collected comes back, 0 if it does not.
    pub refresh: u32,
    /// The item of the game it is, if it is one (plants, ores, oculi).
    pub item: u32,
    /// A boss that is fought for resin: it is there again at once, as often as there is resin
    /// (set when the kinds are read; see `RESIN_BOSSES`).
    #[serde(skip)]
    pub resin: bool,
}

impl Label {
    pub fn name(&self, russian: bool) -> &str {
        if russian && !self.ru.is_empty() { &self.ru } else { &self.en }
    }

    /// Whether a marker of this kind can be marked collected by hand: what is taken once and
    /// is never there again (oculi and the like, chests, puzzles, waypoints, finds).  What
    /// comes back (enemies, plants, ores, specialties, wood, animals, fish, what is picked up
    /// in the field) can not be marked yet: it would have to be taken off again by the clock.
    pub fn can_be_marked(&self) -> bool {
        if self.refresh > 0 {
            return false;
        }
        // The groups whose kinds are taken once: waypoints, special items (oculi ...),
        // chests, puzzles; and of the group "Experience" these kinds only (the chests of mora,
        // seals, secret places, books, recipes, viewpoints, gates, courts ...).
        const ONCE_GROUPS: [u32; 4] = [1, 4, 13, 186];
        const ONCE_EXPERIENCE: [u32; 31] = [
            129, 359, 366, 596, 329, 723, 819, 838, 361, 395, 424, 260, 321, 332, 330, 331, 639, 724, 792, 839, 793, 264, 87, 111, 112,
            85, 253, 322, 147, 189, 156,
        ];
        const EXPERIENCE: u32 = 51;
        let group = if self.parent == 0 { self.id } else { self.parent };
        ONCE_GROUPS.contains(&group) || (group == EXPERIENCE && (ONCE_EXPERIENCE.contains(&self.id) || [159, 268].contains(&self.id)))
    }
}

/// A marker: where it is on the map, from the origin of the map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub id: u32,
    pub label: u32,
    pub x: f32,
    pub y: f32,
    /// The map it is on.
    pub map: u16,
    /// The floor it is on, 0 for the surface of the map.
    pub floor: u16,
}

/// The bosses fought for resin (the normal bosses of the game), by kind of marker.  The map says
/// of every enemy that it comes back after 24 hours, which is not so for these: they are there
/// again at once.
const RESIN_BOSSES: [u32; 41] = [
    132, 133, 135, 137, 138, 157, 181, 183, 203, 204, 262, 263, 318, 352, 390, 391, 432, 433, 452, 457, 455, 497, 498, 540, 553, 564,
    608, 629, 660, 670, 687, 743, 744, 762, 764, 769, 788, 345, 222, 823, WAVEBINDER,
];
/// The kind of the Wavebinder, which the map has without a name (yet).
const WAVEBINDER: u32 = 878;

/// The group of the kinds of ore, and after how many hours it comes back when the map does not
/// say (an assumption: not measured yet).
const ORES: u32 = 11;
const ORE_HOURS: u32 = 72;

pub struct Data {
    pub labels: Vec<Label>,
    index: HashMap<u32, usize>,
    /// Sorted by label.
    pub points: Vec<Point>,
    ranges: HashMap<u32, (usize, usize)>,
    /// The maps, their regions and their underground places.
    pub atlas: Atlas,
    /// When it was made, in seconds; 0 for what the program was built with.
    pub stamp: u64,
}

impl Data {
    pub fn label(&self, id: u32) -> Option<&Label> {
        self.index.get(&id).map(|at| &self.labels[*at])
    }

    /// All the markers of a kind, on every map.
    pub fn points_of(&self, label: u32) -> &[Point] {
        self.ranges
            .get(&label)
            .map_or(&[], |(from, to)| &self.points[*from..*to])
    }

    /// The markers of a kind on the floors of a map that are shown (0 is the
    /// surface).
    pub fn points_on<'a>(
        &'a self,
        label: u32,
        map: u32,
        floors: &'a HashSet<u32>,
    ) -> impl Iterator<Item = &'a Point> + 'a {
        self.points_of(label)
            .iter()
            .filter(move |point| u32::from(point.map) == map && floors.contains(&u32::from(point.floor)))
    }

    /// The markers of a kind on a floor of a map (0 is the surface).
    pub fn points_in(&self, label: u32, map: u32, floor: u32) -> impl Iterator<Item = &Point> {
        self.points_of(label)
            .iter()
            .filter(move |point| u32::from(point.map) == map && u32::from(point.floor) == floor)
    }

    /// The kinds of marker where an item of the game is got: the kinds that are that item
    /// (plants, ores, wood, oculi ...).  Mora and the books of character experience are got
    /// from too many places to name one: the ley line outcrops stand for them.  Empty when
    /// the map does not tell where the item is got.
    pub fn sources_of(&self, item: u32) -> Vec<u32> {
        const MORA: u32 = 202;
        const BOOKS: [u32; 3] = [104001, 104002, 104003];
        const LEY_LINE_OUTCROPS: u32 = 572;
        if item == MORA || BOOKS.contains(&item) {
            return if self.points_of(LEY_LINE_OUTCROPS).is_empty() { Vec::new() } else { vec![LEY_LINE_OUTCROPS] };
        }
        self.labels
            .iter()
            .filter(|label| label.item == item && !self.points_of(label.id).is_empty())
            .map(|label| label.id)
            .collect()
    }

    /// Where an item is got, for any of the items (a chain of materials): the kinds of marker
    /// (plants, ores, the enemies that drop it, the ley lines for mora and experience), and the
    /// daily domains with the days they are open and their markers.
    pub fn where_is(&self, items: &[u32]) -> Where {
        let file = source_file();
        let mut found = Where::default();
        for item in items {
            for label in self.sources_of(*item) {
                if !found.labels.contains(&label) {
                    found.labels.push(label);
                }
            }
            for label in file.mobs.get(item).into_iter().flatten() {
                if !found.labels.contains(label) && !self.points_of(*label).is_empty() {
                    found.labels.push(*label);
                }
            }
            // The boss domains: the materials of the weekly bosses.
            for (point, story) in &file.story {
                if story.drops.iter().any(|reward| reward.id == *item && *item != 0) && !found.points.contains(point) {
                    found.points.push(*point);
                }
            }
            for row in file.domains.get(item).into_iter().flatten() {
                let key = format!("{}{}", row.city, row.kind);
                if let Some(point) = file.points.get(&key)
                    && !found.points.contains(point)
                {
                    found.points.push(*point);
                }
                if !found.domains.iter().any(|known| known.en == row.en) {
                    found.domains.push(row.clone());
                }
            }
        }
        found
    }

    /// Whether a route can be made to get an item: the map has places of it in the open world
    /// (plants, ores, enemies, the ley lines, a domain marker).  What is crafted or cooked, bought,
    /// or has no place on the map (flour, the refinement materials ...) can not be.
    pub fn routable(&self, item: u32) -> bool {
        let found = self.where_is(&[item]);
        found.labels.iter().any(|label| !self.points_of(*label).is_empty())
            || found.points.iter().any(|point| self.points_of(DOMAIN_LABEL).iter().any(|known| known.id == *point))
    }

    /// The groups of kinds of marker.
    pub fn groups(&self) -> impl Iterator<Item = &Label> {
        self.labels.iter().filter(|label| label.parent == 0)
    }

    /// The kinds of marker in a group.
    pub fn children(&self, group: u32) -> impl Iterator<Item = &Label> {
        self.labels.iter().filter(move |label| label.parent == group)
    }

    /// The group a kind of marker is in (itself, for a group).
    pub fn group_of(&self, label: u32) -> u32 {
        match self.label(label) {
            Some(found) if found.parent != 0 => found.parent,
            _ => label,
        }
    }

    /// Reads the labels (as JSON), the markers (JSON, compressed) and the maps.  (See `ORES`.)
    pub fn parse(labels: &str, points_gz: &[u8], atlas: &str, stamp: u64) -> Option<Self> {
        let atlas = Atlas::parse(atlas)?;
        let mut labels: Vec<Label> = serde_json::from_str(labels).ok()?;
        // The map does not say when ore comes back; it does in the game: after three days.
        for label in &mut labels {
            if label.refresh == 0 && label.parent == ORES {
                label.refresh = ORE_HOURS;
            }
            if label.id == WAVEBINDER && label.en.is_empty() {
                label.en = "Wavebinder".to_owned();
                label.ru = "Удерживающий волны".to_owned();
            }
            if RESIN_BOSSES.contains(&label.id) {
                label.resin = true;
                label.refresh = 0;
            }
        }
        let mut text = String::new();
        flate2::read::GzDecoder::new(points_gz).read_to_string(&mut text).ok()?;
        let rows: Vec<(u32, u32, f32, f32, u16, u16)> = serde_json::from_str(&text).ok()?;
        let mut points: Vec<Point> = rows
            .into_iter()
            .map(|(id, label, x, y, map, floor)| Point { id, label, x, y, map, floor })
            .collect();
        points.sort_by_key(|point| (point.label, point.id));
        let mut ranges = HashMap::new();
        let mut from = 0;
        while from < points.len() {
            let label = points[from].label;
            let mut to = from;
            while to < points.len() && points[to].label == label {
                to += 1;
            }
            ranges.insert(label, (from, to));
            from = to;
        }
        let index = labels.iter().enumerate().map(|(at, label)| (label.id, at)).collect();
        Some(Self {
            labels,
            index,
            points,
            ranges,
            atlas,
            stamp,
        })
    }
}

/// Where the list that was fetched is kept.  `IRMINSUL_MARKERS_DIR` replaces it.
fn dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IRMINSUL_MARKERS_DIR") {
        return Some(dir.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("markers"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

fn built_in() -> Data {
    Data::parse(
        include_str!("data/map_labels.json"),
        include_bytes!("data/map_points.json.gz"),
        include_str!("data/map_atlas.json"),
        0,
    )
    .unwrap_or_else(|| Data {
        labels: Vec::new(),
        index: HashMap::new(),
        points: Vec::new(),
        ranges: HashMap::new(),
        atlas: Atlas::default(),
        stamp: 0,
    })
}

/// What was fetched last, if there is such a thing and it can be read.
fn fetched() -> Option<Data> {
    let dir = dir()?;
    let stamp: u64 = std::fs::read_to_string(dir.join("stamp")).ok()?.trim().parse().ok()?;
    let labels = std::fs::read_to_string(dir.join("labels.json")).ok()?;
    let points = std::fs::read(dir.join("points.json.gz")).ok()?;
    let atlas = std::fs::read_to_string(dir.join("atlas.json")).ok()?;
    Data::parse(&labels, &points, &atlas, stamp)
}

static CURRENT: RwLock<Option<Arc<Data>>> = RwLock::new(None);

/// The markers: the ones fetched last, or the ones the program has.
pub fn data() -> Arc<Data> {
    if let Ok(current) = CURRENT.read()
        && let Some(data) = current.as_ref()
    {
        return data.clone();
    }
    let data = Arc::new(fetched().unwrap_or_else(built_in));
    if let Ok(mut current) = CURRENT.write() {
        *current = Some(data.clone());
    }
    data
}

/// How the fetching of the markers is going.
#[derive(Clone, Debug, Default)]
pub struct RefreshStatus {
    pub running: bool,
    pub error: Option<String>,
    /// How many times it was fetched in this run, to tell it was.
    pub done: u32,
}

fn status() -> &'static Mutex<RefreshStatus> {
    static STATUS: std::sync::OnceLock<Mutex<RefreshStatus>> = std::sync::OnceLock::new();
    STATUS.get_or_init(Default::default)
}

pub fn refresh_status() -> RefreshStatus {
    status().lock().map(|status| status.clone()).unwrap_or_default()
}

/// Fetches the markers again if the ones there are old (or `force`), in the
/// background.  What is fetched is kept, and used from then on.
pub fn refresh_in_background(ctx: &egui::Context, force: bool) {
    let current = data();
    let old = force || now().saturating_sub(current.stamp) > MAX_AGE.as_secs();
    if !old {
        return;
    }
    {
        let Ok(mut state) = status().lock() else {
            return;
        };
        if state.running {
            return;
        }
        state.running = true;
        state.error = None;
    }
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| runtime.block_on(fetch_and_keep()));
        if let Ok(mut state) = status().lock() {
            state.running = false;
            match result {
                Ok(()) => state.done += 1,
                Err(e) => {
                    tracing::warn!("the markers of the map could not be fetched: {e:#}");
                    state.error = Some(format!("{e:#}"));
                }
            }
        }
        ctx.request_repaint();
    });
}

async fn fetch_and_keep() -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("Irminsul")
        .timeout(Duration::from_secs(180))
        .build()?;
    let fetched = crate::atlas_fetch::fetch_all(&client).await?;
    // It has to be readable before it is kept.
    let stamp = now();
    let data = Data::parse(&fetched.labels, &fetched.points_gz, &fetched.atlas, stamp)
        .context("what was fetched can not be read")?;
    if let Some(dir) = dir() {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("labels.json"), &fetched.labels)?;
        std::fs::write(dir.join("atlas.json"), &fetched.atlas)?;
        std::fs::write(dir.join("points.json.gz"), &fetched.points_gz)?;
        // The stamp last: a list is only taken as there when it is all there.
        std::fs::write(dir.join("stamp"), stamp.to_string())?;
    }
    tracing::info!(
        "the markers of the map were fetched: {} kinds, {} markers, {} maps",
        data.labels.len(),
        data.points.len(),
        data.atlas.maps.len()
    );
    if let Ok(mut current) = CURRENT.write() {
        *current = Some(Arc::new(data));
    }
    Ok(())
}

/// Where a marker is in what its account has done with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not collected, or collected and come back.
    Available,
    /// Collected at this time, and comes back at this time (never if `None`).
    Done { at: u64, back: Option<u64> },
}

#[derive(Default, Deserialize, Serialize)]
struct Book {
    /// Marker, and when it was collected.
    #[serde(default)]
    collected: BTreeMap<u32, u64>,
}

/// What an account has collected.
#[derive(Default)]
pub struct Store {
    uid: Option<u32>,
    collected: BTreeMap<u32, u64>,
    dirty: bool,
    /// Counts the changes.
    pub version: u64,
}

fn store_path(uid: u32) -> Option<PathBuf> {
    let dir = profiles::profiles_dir().ok()?.join("markers");
    Some(dir.join(format!("{uid}.json")))
}

impl Store {
    /// Takes the markers of this account (none, if it is not known).
    pub fn use_account(&mut self, uid: Option<u32>) {
        if self.uid == uid && self.version > 0 {
            return;
        }
        self.save();
        self.uid = uid;
        self.collected.clear();
        if let Some(path) = uid.and_then(store_path) {
            match std::fs::read_to_string(&path) {
                Ok(text) => match serde_json::from_str::<Book>(&text) {
                    Ok(book) => self.collected = book.collected,
                    Err(e) => tracing::warn!("the collected markers could not be read: {e}"),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => tracing::warn!("the collected markers could not be read: {e}"),
            }
        }
        self.version += 1;
    }

    pub fn state(&self, point: u32, refresh_hours: u32, now: u64) -> State {
        match self.collected.get(&point) {
            None => State::Available,
            Some(&at) => {
                let back = (refresh_hours > 0).then(|| at + u64::from(refresh_hours) * 3600);
                if back.is_some_and(|back| now >= back) {
                    State::Available
                } else {
                    State::Done { at, back }
                }
            }
        }
    }

    /// Collected, or not any more.
    pub fn toggle(&mut self, point: u32, now: u64) {
        if self.collected.remove(&point).is_none() {
            self.collected.insert(point, now);
        }
        self.dirty = true;
        self.version += 1;
    }

    /// The account whose markers these are.
    pub fn account(&self) -> Option<u32> {
        self.uid
    }

    pub fn is_collected(&self, point: u32) -> bool {
        self.collected.contains_key(&point)
    }

    /// Since when a marker is collected (seconds), if it is.
    pub fn collected_at(&self, point: u32) -> Option<u64> {
        self.collected.get(&point).copied()
    }

    /// Not collected any more (the game says it is there).
    pub fn uncollect(&mut self, point: u32) {
        if self.collected.remove(&point).is_some() {
            self.dirty = true;
            self.version += 1;
        }
    }

    /// Marks a marker as collected now.
    pub fn collect(&mut self, point: u32, now: u64) {
        self.collected.insert(point, now);
        self.dirty = true;
        self.version += 1;
    }

    /// How many of each kind are collected (and not come back).
    pub fn done_counts(&self, data: &Data, now: u64) -> HashMap<u32, usize> {
        let by_point: HashMap<u32, u32> = if self.collected.is_empty() {
            HashMap::new()
        } else {
            data.points.iter().map(|point| (point.id, point.label)).collect()
        };
        let mut counts = HashMap::new();
        for (&point, _) in &self.collected {
            let Some(&label) = by_point.get(&point) else {
                continue;
            };
            let refresh = data.label(label).map_or(0, |label| label.refresh);
            if matches!(self.state(point, refresh, now), State::Done { .. }) {
                *counts.entry(label).or_insert(0) += 1;
            }
        }
        counts
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(path) = self.uid.and_then(store_path) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let book = Book {
            collected: self.collected.clone(),
        };
        let temp = path.with_extension("json.tmp");
        let result = serde_json::to_vec(&book)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(std::fs::write(&temp, bytes)?))
            .and_then(|()| Ok(std::fs::rename(&temp, &path)?));
        if let Err(e) = result {
            tracing::error!("unable to store the collected markers: {e:#}");
        }
    }
}

pub fn unix_now() -> u64 {
    now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wavebinder_is_a_resin_boss_with_a_name() {
        let data = data();
        let label = data.label(WAVEBINDER).expect("the kind");
        assert_eq!(label.en, "Wavebinder");
        assert!(label.resin && label.refresh == 0);
        assert!(data.where_is(&[113093]).labels.contains(&WAVEBINDER), "the anchor is dropped by it");
        assert!(data.routable(113093));
        // The ordinary enemies keep the time of the map.
        assert!(!data.label(53).expect("lawachurl").resin);
    }

    #[test]
    fn only_what_is_got_in_the_open_world_can_be_routed() {
        let data = data();
        assert!(data.routable(112002), "slime condensate: enemies");
        assert!(data.routable(104304), "a talent book: its domain");
        assert!(data.routable(113060), "a weekly boss material: its domain");
        assert!(!data.routable(200103), "a refinement material");
        assert!(!data.routable(104011), "an enhancement ore");
        assert!(!data.routable(113063), "a material of the Traveler");
    }

    #[test]
    fn the_markers_the_program_has_are_there() {
        let data = built_in();
        assert!(data.labels.len() > 500, "{} kinds", data.labels.len());
        assert!(data.points.len() > 80_000, "{} markers", data.points.len());
        // The surface of the world, and of another map.
        assert!(data.points_in(17, 2, 0).count() > 1000);
        assert!(data.points_in(17, 2, 0).count() <= data.points_of(17).len());
        assert!(data.points.iter().any(|point| point.map == 7 && point.floor == 0));
        assert!(data.points.iter().any(|point| point.floor != 0));
        assert!(data.atlas.map(2).is_some());
        // A chest label has its markers and a name in two languages.
        let chest = data.label(17).unwrap();
        assert_eq!(chest.en, "Common Chest");
        assert!(!chest.ru.is_empty());
        assert!(data.points_of(17).len() > 1000);
        assert!(data.points_of(17).iter().all(|point| point.label == 17));
        assert!(data.points_of(999_999).is_empty());
        // Groups, and the kinds in them.
        assert!(data.groups().count() >= 10);
        assert!(data.children(13).count() == 5);
    }

    #[test]
    fn what_is_collected_comes_back_after_its_hours() {
        let mut store = Store::default();
        store.collect(10, 1_000);
        // Never comes back, as a chest.
        assert_eq!(store.state(10, 0, 9_999_999), State::Done { at: 1_000, back: None });
        // A plant, back after 48 hours.
        assert_eq!(store.state(10, 48, 1_000 + 3600), State::Done { at: 1_000, back: Some(1_000 + 48 * 3600) });
        assert_eq!(store.state(10, 48, 1_000 + 48 * 3600), State::Available);
        assert_eq!(store.state(11, 48, 5), State::Available);
        // Marked again, it is done again.
        store.toggle(10, 500_000);
        assert_eq!(store.state(10, 48, 500_001), State::Available, "toggled off");
        store.toggle(10, 600_000);
        assert!(matches!(store.state(10, 48, 600_001), State::Done { at: 600_000, .. }));
    }

    #[test]
    fn counts_are_of_what_is_done_now() {
        let data = built_in();
        let mut store = Store::default();
        let label = 17;
        let points = data.points_of(label);
        store.collect(points[0].id, 100);
        store.collect(points[1].id, 100);
        let counts = store.done_counts(&data, 200);
        assert_eq!(counts.get(&label), Some(&2));
    }
}

#[cfg(test)]
mod sources_tests {
    use super::*;

    #[test]
    fn the_domains_of_a_nation_are_told() {
        // The domain of the talents of Liyue (Taishan Mansion): three domains, each on two days and Sunday.
        let (talents, city, days) = domain_info(7116).expect("known");
        assert!(talents && city == 2 && days.len() == 3);
        assert!(days.iter().all(|day| day.days.contains(&6) && !day.items.is_empty()));
        assert!(domain_info(1).is_none());
        // A one-time domain tells the reward of its first completion, a trounce one its drops.
        let once = story_domain(18667).expect("Moshiri Kara");
        assert!(once.kind == "once" && once.rewards.iter().any(|reward| reward.en == "Mora" && reward.qty == 30000));
        let boss = story_domain(81383).expect("Stone Stele Records");
        assert!(boss.kind == "trounce" && boss.drops.len() == 3 && domain_described(18667));
        // The domain of the artifacts "Valley of Remembrance" gives these sets.
        let sets = artifact_domain(7137).expect("known");
        assert!(sets.iter().any(|set| set.en == "Viridescent Venerer"));
        assert!(domain_described(7137) && domain_described(7116) && !domain_described(1));
    }

    #[test]
    fn where_materials_are_got() {
        let data = data();
        assert_eq!(data.sources_of(101003), vec![16], "crystal chunk");
        assert_eq!(data.sources_of(202), vec![572], "mora: the ley lines");
        assert_eq!(data.sources_of(104002), vec![572], "books of experience");
        assert!(!data.sources_of(100022).is_empty(), "a specialty");
        assert!(data.sources_of(999_999_999).is_empty(), "not known");
        // Talent books come from a domain (with its days), a core of a chimera from enemies.
        let book = data.where_is(&[104301]);
        assert!(!book.points.is_empty() && !book.domains.is_empty(), "{book:?}");
        assert!(book.domains.iter().all(|domain| !domain.days.is_empty()));
        assert!(!data.where_is(&[112005]).labels.is_empty(), "damaged mask: hilichurls");
        // The chimeric cores come from the newest enemies, which only the text of the item names.
        for item in [112149, 112150, 112151] {
            assert!(data.where_is(&[item]).labels.contains(&845), "chimeric core {item}");
        }
        // Mora and experience never show enemies.
        assert_eq!(data.where_is(&[202]).labels, vec![572]);
    }
}

#[cfg(test)]
mod can_be_marked_tests {
    use super::*;

    #[test]
    fn only_what_is_taken_once_can_be_marked() {
        let data = data();
        let can = |id: u32| data.label(id).is_some_and(|label| label.can_be_marked());
        // Oculi, chests, puzzles, mora chests, seals: yes.
        for id in [5, 6, 194, 17, 269, 18, 70, 129, 260] {
            assert!(can(id), "label {id} is taken once");
        }
        // Enemies, ores, plants, specialties, wood, animals, fishing spots: no.
        for id in [55, 172, 161, 34, 174, 284, 261, 114] {
            assert!(!can(id), "label {id} comes back");
        }
    }
}
