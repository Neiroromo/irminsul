//! The pictures of characters, weapons and items.
//!
//! They are neither in the packets nor in the game's data files, which only
//! name them.  They are taken from the open collections of the game's icons,
//! Enka.Network first and gi.yatta.moe if that fails, once each, and kept on
//! disk so they are not downloaded again.  Showing one never waits: until it
//! is there, `get` says so and the caller draws something else.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use tokio::sync::{Semaphore, mpsc as tokio_mpsc};

use crate::APP_ID;

/// The pictures are kept this big, whatever they were.
const SIZE: u32 = 128;
/// How many are fetched at once.
const PARALLEL: usize = 4;
/// How many are turned into textures in a frame, so a screenful that arrives
/// at once doesn't make one long frame.
const PER_FRAME: usize = 24;

enum Loaded {
    Image(String, ColorImage),
    Failed(String),
}

/// Where the pictures are kept.  `IRMINSUL_ICONS_DIR` replaces it.
pub fn icons_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IRMINSUL_ICONS_DIR") {
        return Some(dir.into());
    }
    let mut dir = eframe::storage_dir(APP_ID)?;
    dir.push("icons");
    Some(dir)
}

#[derive(Default)]
struct State {
    textures: HashMap<String, TextureHandle>,
    requested: HashSet<String>,
    failed: HashSet<String>,
}

pub struct Icons {
    ctx: Context,
    request_tx: tokio_mpsc::UnboundedSender<String>,
    loaded_rx: Receiver<Loaded>,
    state: RefCell<State>,
}

impl Icons {
    pub fn new(ctx: &Context) -> Self {
        let (request_tx, request_rx) = tokio_mpsc::unbounded_channel();
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let worker_ctx = ctx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(worker(request_rx, loaded_tx, worker_ctx)),
                Err(e) => tracing::error!("unable to start the picture loader: {e}"),
            }
        });
        Self {
            ctx: ctx.clone(),
            request_tx,
            loaded_rx,
            state: RefCell::default(),
        }
    }

    /// Call once a frame: turns what arrived into textures.
    pub fn poll(&self) {
        let mut state = self.state.borrow_mut();
        let mut arrived = 0;
        for message in self.loaded_rx.try_iter().take(PER_FRAME) {
            arrived += 1;
            match message {
                Loaded::Image(name, image) => {
                    let texture = self.ctx.load_texture(
                        format!("icon:{name}"),
                        image,
                        TextureOptions::LINEAR,
                    );
                    state.textures.insert(name, texture);
                }
                Loaded::Failed(name) => {
                    state.failed.insert(name);
                }
            }
        }
        if arrived == PER_FRAME {
            // More may be waiting.
            self.ctx.request_repaint();
        }
    }

    /// The picture, if it is there.  If it is not, it is asked for.
    pub fn get(&self, name: &str) -> Option<TextureHandle> {
        if name.is_empty() {
            return None;
        }
        let mut state = self.state.borrow_mut();
        if let Some(texture) = state.textures.get(name) {
            return Some(texture.clone());
        }
        if !state.requested.contains(name) {
            state.requested.insert(name.to_owned());
            let _ = self.request_tx.send(name.to_owned());
        }
        None
    }
}

async fn worker(
    mut request_rx: tokio_mpsc::UnboundedReceiver<String>,
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
            tracing::error!("unable to start the picture loader: {e}");
            return;
        }
    };
    let limit = Arc::new(Semaphore::new(PARALLEL));
    let dir = icons_dir();

    while let Some(name) = request_rx.recv().await {
        let client = client.clone();
        let limit = limit.clone();
        let loaded_tx = loaded_tx.clone();
        let ctx = ctx.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            let Ok(_permit) = limit.acquire().await else {
                return;
            };
            let message = match load(&client, dir.as_deref(), &name).await {
                Ok(image) => Loaded::Image(name, image),
                Err(e) => {
                    tracing::warn!("no picture for {name}: {e:#}");
                    Loaded::Failed(name)
                }
            };
            let _ = loaded_tx.send(message);
            ctx.request_repaint();
        });
    }
}

/// The picture from disk, or from the net, in which case it is stored.
async fn load(client: &reqwest::Client, dir: Option<&Path>, name: &str) -> Result<ColorImage> {
    // Names become file names, so they are what the game uses and nothing else.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(anyhow!("not a picture name"));
    }

    for extension in ["png", "webp"] {
        if let Some(dir) = dir
            && let Ok(bytes) = tokio::fs::read(dir.join(format!("{name}.{extension}"))).await
            && let Ok(image) = decode(&bytes)
        {
            return Ok(image);
        }
    }

    let bytes = download(client, name).await?;
    let image = decode(&bytes)?;
    let extension = if bytes.starts_with(b"RIFF") { "webp" } else { "png" };
    let path = dir.map(|dir| dir.join(format!("{name}.{extension}")));
    if let Some(path) = &path {
        if let Some(dir) = path.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        // Written beside and renamed, so a half written file is never read.
        let temp = path.with_extension(format!("{extension}.tmp"));
        if tokio::fs::write(&temp, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&temp, path).await;
        }
    }
    Ok(image)
}

async fn download(client: &reqwest::Client, name: &str) -> Result<Vec<u8>> {
    let from_yatta = if name.starts_with("UI_RelicIcon_") {
        format!("https://gi.yatta.moe/assets/UI/reliquary/{name}.png")
    } else {
        format!("https://gi.yatta.moe/assets/UI/{name}.png")
    };
    // The database of the versions to come has what the other two do not have
    // yet.  Its pictures are WebP.
    let folder = if name.starts_with("UI_AvatarIcon_Side_") {
        "avatarside"
    } else if name.starts_with("UI_AvatarIcon_") {
        "avataricon"
    } else if name.starts_with("UI_EquipIcon_") {
        "weaponicon"
    } else if name.starts_with("UI_RelicIcon_") {
        "artifacts"
    } else if name.starts_with("Skill_") || name.starts_with("UI_Talent_") {
        "skills"
    } else {
        "items"
    };
    let urls = [
        format!("https://enka.network/ui/{name}.png"),
        from_yatta,
        format!("https://api.lunaris.moe/data/assets/{folder}/{name}.webp"),
    ];

    let mut last_error = anyhow!("no source");
    for url in urls {
        let attempt = async {
            let response = client.get(&url).send().await?.error_for_status()?;
            anyhow::Ok(response.bytes().await?.to_vec())
        };
        match attempt.await.with_context(|| format!("{url} failed")) {
            Ok(bytes) => return Ok(bytes),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

fn decode(bytes: &[u8]) -> Result<ColorImage> {
    let image = image::load_from_memory(bytes)?
        .resize(SIZE, SIZE, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}
