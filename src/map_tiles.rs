//! The picture of the world, in tiles.
//!
//! The official interactive map of the game is made of tiles of 256 by 256
//! pixels, at five levels of detail: level 0 is the map at full size, each
//! level after it half as big.  They are fetched when they come into view, once
//! each, and kept on disk, so they are not downloaded again.  Showing one never
//! waits: until it is there, `get` says so and the caller draws something else.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, mpsc as tokio_mpsc};

use crate::APP_ID;

pub const TILE: f32 = 256.;
/// How many are fetched at once.
const PARALLEL: usize = 6;
/// How many are fetched at once when they are all being fetched, in the
/// background, which is not to be in the way of the ones that are wanted now.
const BACKGROUND_PARALLEL: usize = 6;
/// How long the game is looked for again while the fetching waits for it to end.
const GAME_CHECK: Duration = Duration::from_secs(3);
/// How many are turned into textures in a frame.
const PER_FRAME: usize = 12;
/// More textures than this and those not used lately are let go.
const KEEP: usize = 500;

const INFO_URL: &str = "https://sg-public-api-static.hoyolab.com/common/map_user/ys_obc/v1/map/info";
const TILE_URL: &str = "https://act-webstatic.mihoyo.com/ys-map-op/map";

/// What the map is: which version of it, how big it is, and where the middle of
/// the world is on it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct MapInfo {
    /// Which map of the official interactive map it is (2 is the world).
    #[serde(default = "world")]
    pub map: u32,
    pub version: String,
    /// Where the game's origin is, in pixels of the full size map.
    pub origin: [f32; 2],
    /// The size of the full size map, in pixels.
    pub size: [f32; 2],
    /// The coarsest level there is.
    pub max_level: u32,
}

fn world() -> u32 {
    crate::atlas::WORLD
}

impl Default for MapInfo {
    /// What the map was when this was written; the real one is asked for.
    fn default() -> Self {
        Self {
            map: world(),
            version: "bd70cde160dbd523926208d9fa8961a1".to_owned(),
            origin: [24207., 16411.],
            size: [36864., 32768.],
            max_level: 4,
        }
    }
}

/// A tile: its level of detail (0 is the finest), and its place in the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub level: u32,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    fn suffix(&self) -> String {
        if self.level == 0 {
            "P0".to_owned()
        } else {
            format!("N{}", self.level)
        }
    }
}

/// How the fetching of many tiles at once is going.
#[derive(Default)]
pub struct Progress {
    pub total: AtomicU32,
    /// Tiles that are done with: fetched, had already, or not to be had.
    pub done: AtomicU32,
    /// Of those, the ones that were there already.
    pub had: AtomicU32,
    /// Of those, the ones that could not be fetched.
    pub failed: AtomicU32,
    /// What was fetched, in bytes.
    pub bytes: AtomicU64,
    pub running: AtomicBool,
    pub cancel: AtomicBool,
    /// The fetching waits, because the game is running and it was not asked to
    /// go on then.
    pub paused: AtomicBool,
    /// Fetch also while the game is running.
    pub during_game: AtomicBool,
}

struct Job {
    /// The tiles, each with the version of the map it is of: tiles of more than
    /// one map can be in a job.
    keys: Vec<(String, TileKey)>,
    progress: Arc<Progress>,
}

/// How many tiles there are across and down at a level of detail.
pub fn grid(info: &MapInfo, level: u32) -> (u32, u32) {
    let span = TILE * 2f32.powi(level as i32);
    (
        (info.size[0] / span).ceil() as u32,
        (info.size[1] / span).ceil() as u32,
    )
}

/// Every tile of the levels.
pub fn all_keys(info: &MapInfo, levels: &[u32]) -> Vec<TileKey> {
    let mut keys = Vec::new();
    for &level in levels {
        let (columns, rows) = grid(info, level);
        for y in 0..rows {
            for x in 0..columns {
                keys.push(TileKey { level, x, y });
            }
        }
    }
    keys
}

/// The tiles of the levels that are around the places (given on the full size
/// map), as far as `margin` tiles.  The coarse levels first.
pub fn keys_around(info: &MapInfo, levels: &[u32], places: &[[f32; 2]], margin: i32) -> Vec<TileKey> {
    let mut keys = HashSet::new();
    for &level in levels {
        let span = TILE * 2f32.powi(level as i32);
        let (columns, rows) = grid(info, level);
        for place in places {
            let (cx, cy) = ((place[0] / span).floor() as i32, (place[1] / span).floor() as i32);
            for y in (cy - margin).max(0)..=(cy + margin).min(rows as i32 - 1) {
                for x in (cx - margin).max(0)..=(cx + margin).min(columns as i32 - 1) {
                    keys.insert(TileKey {
                        level,
                        x: x as u32,
                        y: y as u32,
                    });
                }
            }
        }
    }
    let mut keys: Vec<TileKey> = keys.into_iter().collect();
    keys.sort_by_key(|key| (std::cmp::Reverse(key.level), key.y, key.x));
    keys
}

/// The tiles of the levels that a place of the map covers (left, top, right,
/// bottom, from the origin of the map).
pub fn keys_in(info: &MapInfo, levels: &[u32], rect: [f32; 4]) -> Vec<TileKey> {
    let mut keys = Vec::new();
    for &level in levels {
        let span = TILE * 2f32.powi(level as i32);
        let (columns, rows) = grid(info, level);
        let first_x = (((info.origin[0] + rect[0]) / span).floor().max(0.) as u32).min(columns.saturating_sub(1));
        let last_x = (((info.origin[0] + rect[2]) / span).floor().max(0.) as u32).min(columns.saturating_sub(1));
        let first_y = (((info.origin[1] + rect[1]) / span).floor().max(0.) as u32).min(rows.saturating_sub(1));
        let last_y = (((info.origin[1] + rect[3]) / span).floor().max(0.) as u32).min(rows.saturating_sub(1));
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                keys.push(TileKey { level, x, y });
            }
        }
    }
    keys
}

/// What a tile of each level weighs, in kilobytes, on average (measured over
/// tiles of the whole map; most of it is sea, which weighs next to nothing).
pub const AVERAGE_KB: [f32; 5] = [3.5, 4.4, 3.2, 6.5, 3.8];

/// About how many megabytes `count` tiles of a level are.
pub fn estimate_level_mb(level: u32, count: usize) -> f32 {
    AVERAGE_KB.get(level as usize).copied().unwrap_or(4.) * count as f32 / 1024.
}

/// About how many megabytes the tiles are.
pub fn estimate_mb(keys: &[TileKey]) -> f32 {
    keys.iter()
        .map(|key| AVERAGE_KB.get(key.level as usize).copied().unwrap_or(4.))
        .sum::<f32>()
        / 1024.
}

/// The tile a file is of, by its name (`12_8_N3.webp`).
fn parse_tile_name(name: &str) -> Option<TileKey> {
    let stem = name.strip_suffix(".webp")?;
    let mut parts = stem.split('_');
    let (x, y, suffix) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let level = match suffix {
        "P0" => 0,
        other => other.strip_prefix('N')?.parse().ok()?,
    };
    Some(TileKey {
        level,
        x: x.parse().ok()?,
        y: y.parse().ok()?,
    })
}

/// Which map a version of tiles is of.  A version is the hash of the picture of
/// one map, so it tells the map; it is told here when a map is chosen.
fn maps_of_versions() -> &'static std::sync::Mutex<HashMap<String, u32>> {
    static MAPS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, u32>>> = std::sync::OnceLock::new();
    MAPS.get_or_init(Default::default)
}

fn remember_map(info: &MapInfo) {
    if let Ok(mut maps) = maps_of_versions().lock() {
        maps.insert(info.version.clone(), info.map);
    }
}

fn map_of_version(version: &str) -> u32 {
    maps_of_versions()
        .lock()
        .ok()
        .and_then(|maps| maps.get(version).copied())
        .unwrap_or_else(world)
}

fn valid_version(version: &str) -> bool {
    // The version becomes a folder name.
    !version.is_empty() && version.chars().all(|c| c.is_ascii_alphanumeric())
}

enum Loaded {
    Tile(String, TileKey, bool, ColorImage),
    Missing(String, TileKey),
    Info(MapInfo),
}

/// Where the tiles are kept.  `IRMINSUL_MAP_DIR` replaces it.
fn map_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IRMINSUL_MAP_DIR") {
        return Some(dir.into());
    }
    let mut dir = eframe::storage_dir(APP_ID)?;
    dir.push("map_tiles");
    Some(dir)
}

/// Where the tiles of a version of a map are kept.
pub fn version_dir(version: &str) -> Option<PathBuf> {
    map_dir().map(|dir| dir.join(version))
}

#[derive(Default)]
struct State {
    /// The tiles as pictures in memory, for drawing the map by the program
    /// itself (see `image`), not as textures.
    images: HashMap<TileKey, Arc<ColorImage>>,
    textures: HashMap<TileKey, (TextureHandle, u64)>,
    requested: HashSet<TileKey>,
    missing: HashSet<TileKey>,
}

pub struct MapTiles {
    ctx: Context,
    request_tx: tokio_mpsc::UnboundedSender<(String, TileKey, bool)>,
    job_tx: tokio_mpsc::UnboundedSender<Job>,
    /// Asks for what a map is now.
    info_tx: tokio_mpsc::UnboundedSender<u32>,
    progress: Arc<Progress>,
    loaded_rx: Receiver<Loaded>,
    state: RefCell<State>,
    info: RefCell<MapInfo>,
    frame: RefCell<u64>,
    /// The tiles are made black and white.
    gray: std::cell::Cell<bool>,
    /// The tiles are kept as pictures in memory, not as textures.
    in_memory: std::cell::Cell<bool>,
}

impl MapTiles {
    pub fn new(ctx: &Context) -> Self {
        let (request_tx, request_rx) = tokio_mpsc::unbounded_channel();
        let (job_tx, job_rx) = tokio_mpsc::unbounded_channel();
        let (info_tx, info_rx) = tokio_mpsc::unbounded_channel();
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let worker_ctx = ctx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(worker(request_rx, job_rx, info_rx, loaded_tx, worker_ctx)),
                Err(e) => tracing::error!("unable to start the map loader: {e}"),
            }
        });
        Self {
            ctx: ctx.clone(),
            request_tx,
            job_tx,
            info_tx,
            progress: Arc::default(),
            loaded_rx,
            state: RefCell::default(),
            info: RefCell::new(MapInfo::default()),
            frame: RefCell::new(0),
            gray: std::cell::Cell::new(false),
            in_memory: std::cell::Cell::new(false),
        }
    }

    /// Keeps the tiles as pictures in memory (asked for with `image`), for
    /// drawing the map by the program itself.
    pub fn keep_in_memory(&self) {
        self.in_memory.set(true);
    }

    /// The tile as a picture in memory, if it is there.  If it is not, it is
    /// asked for.
    pub fn image(&self, key: TileKey) -> Option<Arc<ColorImage>> {
        let mut state = self.state.borrow_mut();
        if let Some(image) = state.images.get(&key) {
            return Some(image.clone());
        }
        if !state.requested.contains(&key) && !state.missing.contains(&key) {
            state.requested.insert(key);
            let _ = self
                .request_tx
                .send((self.info.borrow().version.clone(), key, self.gray.get()));
        }
        None
    }

    pub fn info(&self) -> MapInfo {
        self.info.borrow().clone()
    }

    /// Shows another map: its tiles are asked for from now on.  What it is now
    /// is asked for too, and replaces what is given here when it has come.
    pub fn set_map(&self, info: MapInfo) {
        if self.info.borrow().map == info.map {
            return;
        }
        remember_map(&info);
        let map = info.map;
        *self.state.borrow_mut() = State::default();
        *self.info.borrow_mut() = info;
        let _ = self.info_tx.send(map);
    }

    /// The tiles that are on the disk now (of the version of the map there is).
    pub fn on_disk(&self) -> HashSet<TileKey> {
        let version = self.info.borrow().version.clone();
        Self::on_disk_of(&version)
    }

    /// The tiles of a version of a map that are on the disk.
    pub fn on_disk_of(version: &str) -> HashSet<TileKey> {
        let mut found = HashSet::new();
        let Some(dir) = map_dir().map(|dir| dir.join(version)) else {
            return found;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(key) = name.to_str().and_then(parse_tile_name) {
                found.insert(key);
            }
        }
        found
    }

    pub fn progress(&self) -> &Arc<Progress> {
        &self.progress
    }

    /// Starts fetching these tiles in the background, to be had when they are
    /// wanted.  Those that are on disk already are passed over.  Nothing
    /// happens while a fetching is going.
    pub fn start_download(&self, jobs: Vec<(MapInfo, Vec<TileKey>)>, during_game: bool) {
        let mut keys: Vec<(String, TileKey)> = Vec::new();
        for (info, tiles) in jobs {
            remember_map(&info);
            keys.extend(tiles.into_iter().map(|key| (info.version.clone(), key)));
        }
        if self.progress.running.load(Ordering::Relaxed) || keys.is_empty() {
            return;
        }
        tracing::info!("fetching {} map tiles in the background (during the game: {during_game})", keys.len());
        self.progress.during_game.store(during_game, Ordering::Relaxed);
        self.progress.paused.store(false, Ordering::Relaxed);
        let progress = &self.progress;
        for counter in [&progress.done, &progress.had, &progress.failed] {
            counter.store(0, Ordering::Relaxed);
        }
        progress.bytes.store(0, Ordering::Relaxed);
        progress.total.store(keys.len() as u32, Ordering::Relaxed);
        progress.cancel.store(false, Ordering::Relaxed);
        progress.running.store(true, Ordering::Relaxed);
        let _ = self.job_tx.send(Job {
            keys,
            progress: progress.clone(),
        });
    }

    /// Stops the fetching that is going; what is fetched stays.
    pub fn stop_download(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }

    /// How many tiles there are, and how many are not to be had.
    pub fn counts(&self) -> (usize, usize) {
        let state = self.state.borrow();
        (state.textures.len(), state.missing.len())
    }

    /// Call once a frame: turns what arrived into textures.
    pub fn poll(&self) {
        let mut frame = self.frame.borrow_mut();
        *frame += 1;
        let frame = *frame;
        let mut state = self.state.borrow_mut();
        let mut arrived = 0;
        for message in self.loaded_rx.try_iter().take(PER_FRAME) {
            arrived += 1;
            match message {
                Loaded::Tile(version, key, gray, image) => {
                    if version != self.info.borrow().version || gray != self.gray.get() {
                        continue;
                    }
                    if self.in_memory.get() {
                        state.images.insert(key, Arc::new(image));
                        continue;
                    }
                    let texture = self.ctx.load_texture(
                        format!("map:{}:{}:{}", key.suffix(), key.x, key.y),
                        image,
                        TextureOptions::LINEAR,
                    );
                    state.textures.insert(key, (texture, frame));
                }
                Loaded::Missing(version, key) => {
                    if version == self.info.borrow().version {
                        state.missing.insert(key);
                    }
                }
                Loaded::Info(info) => {
                    // Of a map that is not shown any more.
                    if info.map != self.info.borrow().map {
                        continue;
                    }
                    // A newer version of the map: its tiles are other ones.
                    if info != *self.info.borrow() {
                        remember_map(&info);
                        *state = State::default();
                        *self.info.borrow_mut() = info;
                    }
                }
            }
        }
        if arrived == PER_FRAME {
            self.ctx.request_repaint();
        }
        if state.images.len() > 300 {
            // The finer ones come again from the disk as they are looked at.
            state.images.retain(|key, _| key.level >= 3);
            let State {
                images, requested, missing, ..
            } = &mut *state;
            requested.retain(|key| images.contains_key(key) || missing.contains(key));
        }
        if state.textures.len() > KEEP {
            state
                .textures
                .retain(|key, (_, used)| frame.saturating_sub(*used) < 120 || key.level >= 3);
            // What was let go may be asked for again.
            let State {
                textures,
                requested,
                missing,
                ..
            } = &mut *state;
            requested.retain(|key| textures.contains_key(key) || missing.contains(key));
        }
    }

    /// The tile, if it is there.  If it is not, it is asked for.
    pub fn get(&self, key: TileKey) -> Option<TextureHandle> {
        let frame = *self.frame.borrow();
        let mut state = self.state.borrow_mut();
        if let Some((texture, used)) = state.textures.get_mut(&key) {
            *used = frame;
            return Some(texture.clone());
        }
        if !state.requested.contains(&key) && !state.missing.contains(&key) {
            state.requested.insert(key);
            let _ = self
                .request_tx
                .send((self.info.borrow().version.clone(), key, self.gray.get()));
        }
        None
    }
}

async fn worker(
    mut request_rx: tokio_mpsc::UnboundedReceiver<(String, TileKey, bool)>,
    mut job_rx: tokio_mpsc::UnboundedReceiver<Job>,
    mut info_rx: tokio_mpsc::UnboundedReceiver<u32>,
    loaded_tx: Sender<Loaded>,
    ctx: Context,
) {
    let client = match reqwest::Client::builder()
        .user_agent("Irminsul")
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("unable to start the map loader: {e}");
            return;
        }
    };
    let dir = map_dir();

    // What the map is now: asked for first, in the background.
    {
        let client = client.clone();
        let dir = dir.clone();
        let loaded_tx = loaded_tx.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Some(info) = fetch_info(&client, dir.as_deref(), world()).await {
                let _ = loaded_tx.send(Loaded::Info(info));
                ctx.request_repaint();
            }
        });
    }

    let limit = Arc::new(Semaphore::new(PARALLEL));
    loop {
        tokio::select! {
            request = request_rx.recv() => {
                let Some((version, key, gray)) = request else { break };
                let client = client.clone();
                let limit = limit.clone();
                let loaded_tx = loaded_tx.clone();
                let ctx = ctx.clone();
                let dir = dir.clone();
                tokio::spawn(async move {
                    let Ok(_permit) = limit.acquire().await else {
                        return;
                    };
                    let message = match load(&client, dir.as_deref(), &version, key, gray).await {
                        Ok(image) => Loaded::Tile(version, key, gray, image),
                        Err(e) => {
                            tracing::debug!("no map tile {key:?}: {e:#}");
                            Loaded::Missing(version, key)
                        }
                    };
                    let _ = loaded_tx.send(message);
                    ctx.request_repaint();
                });
            }
            job = job_rx.recv() => {
                let Some(job) = job else { break };
                tokio::spawn(download_all(client.clone(), dir.clone(), job, ctx.clone()));
            }
            map = info_rx.recv() => {
                let Some(map) = map else { break };
                let (client, dir, loaded_tx, ctx) = (client.clone(), dir.clone(), loaded_tx.clone(), ctx.clone());
                tokio::spawn(async move {
                    if let Some(info) = fetch_info(&client, dir.as_deref(), map).await {
                        let _ = loaded_tx.send(Loaded::Info(info));
                        ctx.request_repaint();
                    }
                });
            }
        }
    }
}

/// Fetches the tiles of a job to the disk, a few at a time, not decoding them.
async fn download_all(client: reqwest::Client, dir: Option<PathBuf>, job: Job, ctx: Context) {
    let progress = job.progress.clone();
    let limit = Arc::new(Semaphore::new(BACKGROUND_PARALLEL));
    let mut handles = Vec::new();
    if let Some(dir) = dir {
        // Whether the game is running, looked at now and then: the tiles do not
        // compete with it for the connection.
        let mut game = false;
        let mut checked: Option<std::time::Instant> = None;
        for (version, key) in job.keys {
            if !valid_version(&version) {
                progress.failed.fetch_add(1, Ordering::Relaxed);
                progress.done.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let folder = dir.join(&version);
            loop {
                if progress.cancel.load(Ordering::Relaxed) {
                    break;
                }
                if progress.during_game.load(Ordering::Relaxed) {
                    progress.paused.store(false, Ordering::Relaxed);
                    break;
                }
                if checked.is_none_or(|at| at.elapsed() >= GAME_CHECK) {
                    game = tokio::task::spawn_blocking(crate::game_launch::game_running)
                        .await
                        .unwrap_or(false);
                    checked = Some(std::time::Instant::now());
                }
                progress.paused.store(game, Ordering::Relaxed);
                if !game {
                    break;
                }
                ctx.request_repaint();
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
            if progress.cancel.load(Ordering::Relaxed) {
                break;
            }
            let Ok(permit) = limit.clone().acquire_owned().await else {
                break;
            };
            let (client, folder, version, progress, ctx) = (
                client.clone(),
                folder.clone(),
                version,
                progress.clone(),
                ctx.clone(),
            );
            handles.push(tokio::spawn(async move {
                let name = format!("{}_{}_{}.webp", key.x, key.y, key.suffix());
                let path = folder.join(&name);
                let there = tokio::fs::metadata(&path).await.is_ok_and(|meta| meta.len() > 0);
                if there {
                    progress.had.fetch_add(1, Ordering::Relaxed);
                } else {
                    match fetch_tile(&client, &version, &name).await {
                        Ok(bytes) => {
                            let _ = tokio::fs::create_dir_all(&folder).await;
                            let temp = path.with_extension("webp.tmp");
                            if tokio::fs::write(&temp, &bytes).await.is_ok()
                                && tokio::fs::rename(&temp, &path).await.is_ok()
                            {
                                progress.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                            } else {
                                progress.failed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            tracing::debug!("no map tile {name}: {e:#}");
                            progress.failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                let done = progress.done.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 25 == 0 {
                    ctx.request_repaint();
                }
                drop(permit);
            }));
        }
    }
    for handle in handles {
        let _ = handle.await;
    }
    tracing::info!(
        "map tiles: {} of {} done, {} fetched ({} bytes), {} not to be had{}",
        progress.done.load(Ordering::Relaxed),
        progress.total.load(Ordering::Relaxed),
        progress.done.load(Ordering::Relaxed)
            - progress.had.load(Ordering::Relaxed)
            - progress.failed.load(Ordering::Relaxed),
        progress.bytes.load(Ordering::Relaxed),
        progress.failed.load(Ordering::Relaxed),
        if progress.cancel.load(Ordering::Relaxed) { " (stopped)" } else { "" }
    );
    progress.paused.store(false, Ordering::Relaxed);
    progress.running.store(false, Ordering::Relaxed);
    ctx.request_repaint();
}

async fn fetch_tile(client: &reqwest::Client, version: &str, name: &str) -> Result<Vec<u8>> {
    let url = format!("{TILE_URL}/{}/{version}/{name}", map_of_version(version));
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("{url} failed"))?
        .error_for_status()
        .with_context(|| format!("{url} failed"))?;
    Ok(response.bytes().await?.to_vec())
}

/// What the official map says it is now, or, if that can't be had, what it was
/// when it was last had.
async fn fetch_info(client: &reqwest::Client, dir: Option<&Path>, map: u32) -> Option<MapInfo> {
    // The world's file is the one there always was.
    let cached = dir.map(|dir| {
        if map == world() { dir.join("info.json") } else { dir.join(format!("info_{map}.json")) }
    });
    let fetched = async {
        let url = format!("{INFO_URL}?map_id={map}&app_sn=ys_obc&lang=en-us");
        let body: serde_json::Value = client.get(url).send().await?.error_for_status()?.json().await?;
        let detail = &body["data"]["info"]["detail_v2"];
        let pair = |key: &str| -> Result<[f32; 2]> {
            let list = detail[key].as_array().context("no such pair")?;
            Ok([
                list.first().and_then(|v| v.as_f64()).context("no number")? as f32,
                list.get(1).and_then(|v| v.as_f64()).context("no number")? as f32,
            ])
        };
        anyhow::Ok(MapInfo {
            map,
            version: detail["map_version"]
                .as_str()
                .context("no version")?
                .to_owned(),
            origin: pair("origin")?,
            size: pair("total_size")?,
            max_level: detail["min_zoom"].as_i64().map_or(4, |zoom| zoom.unsigned_abs() as u32),
        })
    }
    .await;
    match fetched {
        Ok(info) => {
            if let Some(path) = &cached {
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                if let Ok(text) = serde_json::to_vec(&info) {
                    let _ = tokio::fs::write(path, text).await;
                }
            }
            Some(info)
        }
        Err(e) => {
            tracing::warn!("the map's description could not be fetched: {e:#}");
            let bytes = tokio::fs::read(cached?).await.ok()?;
            let mut info: MapInfo = serde_json::from_slice(&bytes).ok()?;
            info.map = map;
            Some(info)
        }
    }
}

/// The tile from disk, or from the net, in which case it is stored.
async fn load(
    client: &reqwest::Client,
    dir: Option<&Path>,
    version: &str,
    key: TileKey,
    gray: bool,
) -> Result<ColorImage> {
    if !valid_version(version) {
        return Err(anyhow!("not a version"));
    }
    let name = format!("{}_{}_{}.webp", key.x, key.y, key.suffix());
    let path = dir.map(|dir| dir.join(version).join(&name));
    if let Some(path) = &path
        && let Ok(bytes) = tokio::fs::read(path).await
        && let Ok(image) = decode(&bytes, gray)
    {
        return Ok(image);
    }

    let bytes = fetch_tile(client, version, &name).await?;
    let image = decode(&bytes, gray)?;
    if let Some(path) = &path {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        // Written beside and renamed, so a half written file is never read.
        let temp = path.with_extension("webp.tmp");
        if tokio::fs::write(&temp, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&temp, path).await;
        }
    }
    Ok(image)
}

fn decode(bytes: &[u8], gray: bool) -> Result<ColorImage> {
    let mut image = image::load_from_memory(bytes)?.to_rgba8();
    if gray {
        for pixel in image.pixels_mut() {
            let [r, g, b, a] = pixel.0;
            let luma = ((299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000) as u8;
            pixel.0 = [luma, luma, luma, a];
        }
    }
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_is_known_by_the_name_of_its_file() {
        assert_eq!(parse_tile_name("12_8_N3.webp"), Some(TileKey { level: 3, x: 12, y: 8 }));
        assert_eq!(parse_tile_name("0_143_P0.webp"), Some(TileKey { level: 0, x: 0, y: 143 }));
        assert_eq!(parse_tile_name("12_8_N3.webp.tmp"), None);
        assert_eq!(parse_tile_name("info.json"), None);
        assert_eq!(parse_tile_name("1_2_3_N1.webp"), None);
    }

    #[test]
    fn tiles_are_named_by_their_level() {
        assert_eq!(TileKey { level: 0, x: 1, y: 2 }.suffix(), "P0");
        assert_eq!(TileKey { level: 3, x: 1, y: 2 }.suffix(), "N3");
    }

    #[test]
    fn the_grid_is_as_big_as_the_map_needs() {
        let info = MapInfo::default();
        assert_eq!(grid(&info, 0), (144, 128));
        assert_eq!(grid(&info, 4), (9, 8));
        assert_eq!(all_keys(&info, &[4, 3]).len(), 72 + 288);
        assert!((estimate_mb(&all_keys(&info, &[0, 1, 2, 3, 4])) - 93.).abs() < 6.);
    }

    #[test]
    fn the_tiles_around_places_are_few() {
        let info = MapInfo::default();
        // One place, a tile on each side: nine tiles of a level.
        let around = keys_around(&info, &[2], &[[24690., 16264.]], 1);
        assert_eq!(around.len(), 9);
        // Near the edge there are fewer, never any that are not on the map.
        let corner = keys_around(&info, &[4], &[[10., 10.]], 1);
        assert_eq!(corner.len(), 4);
        // Two places close together share tiles; the coarse levels come first.
        let both = keys_around(&info, &[0, 4], &[[24690., 16264.], [24700., 16270.]], 1);
        assert!(both.len() >= 18 && both.len() <= 20);
        assert_eq!(both[0].level, 4);
    }

    #[test]
    fn a_tile_is_decoded() {
        let mut png = Vec::new();
        image::RgbaImage::new(4, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let image = decode(&png, false).unwrap();
        assert_eq!(image.size, [4, 2]);
        assert!(decode(b"not a picture", false).is_err());
        // Black and white has the same red, green and blue.
        let mut colour = image::RgbaImage::new(1, 1);
        colour.put_pixel(0, 0, image::Rgba([200, 40, 10, 255]));
        let mut png = Vec::new();
        colour
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let grey = decode(&png, true).unwrap().pixels[0];
        assert!(grey.r() == grey.g() && grey.g() == grey.b() && grey.a() == 255);
    }
}
