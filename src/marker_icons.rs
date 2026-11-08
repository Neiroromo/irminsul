//! The pictures of the kinds of marker of the map, and the pictures of the
//! floors of its underground places.
//!
//! They are at the addresses the map gives for them; each is fetched once, when
//! it is first looked at (a marker's made small), and kept on disk.  Showing one
//! never waits: until it is there, `get` says so and the caller draws something
//! else.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::Result;
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use tokio::sync::mpsc as tokio_mpsc;

use crate::APP_ID;

/// The pictures are kept this big (they are shown smaller).
const SIZE: u32 = 48;
const PER_FRAME: usize = 16;

enum Loaded {
    Image(u32, ColorImage),
    Failed(u32),
}

fn icons_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IRMINSUL_MARKER_ICONS_DIR") {
        return Some(dir.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("map_icons"))
}

fn floors_dir() -> Option<PathBuf> {
    eframe::storage_dir(APP_ID).map(|dir| dir.join("map_floors"))
}

#[derive(Default)]
struct State {
    textures: HashMap<u32, TextureHandle>,
    /// The pictures as they are, for drawing by the program itself.
    images: HashMap<u32, std::sync::Arc<ColorImage>>,
    requested: HashSet<u32>,
    failed: HashSet<u32>,
}

pub struct MarkerIcons {
    ctx: Context,
    request_tx: tokio_mpsc::UnboundedSender<(u32, String)>,
    loaded_rx: Receiver<Loaded>,
    state: RefCell<State>,
    /// Kept as pictures in memory, not as textures.
    in_memory: bool,
}

impl MarkerIcons {
    pub fn new(ctx: &Context) -> Self {
        Self::start(ctx, icons_dir(), Some(SIZE))
    }

    /// For the pictures of floors: as they are, not made small.
    pub fn floors(ctx: &Context) -> Self {
        Self::start(ctx, floors_dir(), None)
    }

    fn start(ctx: &Context, dir: Option<PathBuf>, size: Option<u32>) -> Self {
        let (request_tx, request_rx) = tokio_mpsc::unbounded_channel();
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let worker_ctx = ctx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(worker(request_rx, loaded_tx, worker_ctx, dir, size)),
                Err(e) => tracing::error!("unable to start the loader of the marker pictures: {e}"),
            }
        });
        Self {
            ctx: ctx.clone(),
            request_tx,
            loaded_rx,
            state: RefCell::default(),
            in_memory: false,
        }
    }

    /// For the pictures of the kinds of marker, kept in memory (see `image`).
    pub fn icons_in_memory(ctx: &Context) -> Self {
        let mut icons = Self::start(ctx, icons_dir(), Some(SIZE));
        icons.in_memory = true;
        icons
    }

    /// For the pictures of floors, kept in memory (see `image`).
    pub fn floors_in_memory(ctx: &Context) -> Self {
        let mut icons = Self::floors(ctx);
        icons.in_memory = true;
        icons
    }

    /// The picture, in memory, if it is there.  If it is not, it is asked for.
    pub fn image(&self, label: u32, url: &str) -> Option<std::sync::Arc<ColorImage>> {
        if url.is_empty() {
            return None;
        }
        let mut state = self.state.borrow_mut();
        if let Some(image) = state.images.get(&label) {
            return Some(image.clone());
        }
        if !state.requested.contains(&label) {
            state.requested.insert(label);
            let _ = self.request_tx.send((label, url.to_owned()));
        }
        None
    }

    /// Call once a frame: turns what arrived into textures.
    pub fn poll(&self) {
        let mut state = self.state.borrow_mut();
        for message in self.loaded_rx.try_iter().take(PER_FRAME) {
            match message {
                Loaded::Image(id, image) if self.in_memory => {
                    state.images.insert(id, std::sync::Arc::new(image));
                }
                Loaded::Image(id, image) => {
                    let texture = self
                        .ctx
                        .load_texture(format!("marker:{id}"), image, TextureOptions::LINEAR);
                    state.textures.insert(id, texture);
                }
                Loaded::Failed(id) => {
                    state.failed.insert(id);
                }
            }
        }
    }

    /// The picture of a kind of marker, if it is there.  If it is not, it is
    /// asked for.
    pub fn get(&self, label: u32, url: &str) -> Option<TextureHandle> {
        if url.is_empty() {
            return None;
        }
        let mut state = self.state.borrow_mut();
        if let Some(texture) = state.textures.get(&label) {
            return Some(texture.clone());
        }
        if !state.requested.contains(&label) {
            state.requested.insert(label);
            let _ = self.request_tx.send((label, url.to_owned()));
        }
        None
    }
}

async fn worker(
    mut request_rx: tokio_mpsc::UnboundedReceiver<(u32, String)>,
    loaded_tx: Sender<Loaded>,
    ctx: Context,
    dir: Option<PathBuf>,
    size: Option<u32>,
) {
    let client = match reqwest::Client::builder()
        .user_agent("Irminsul")
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("unable to start the loader of the marker pictures: {e}");
            return;
        }
    };
    let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    while let Some((id, url)) = request_rx.recv().await {
        let (client, loaded_tx, ctx, dir, limit) =
            (client.clone(), loaded_tx.clone(), ctx.clone(), dir.clone(), limit.clone());
        tokio::spawn(async move {
            let Ok(_permit) = limit.acquire().await else {
                return;
            };
            let message = match load(&client, dir, id, &url, size).await {
                Ok(image) => Loaded::Image(id, image),
                Err(e) => {
                    tracing::debug!("no picture for the marker kind {id}: {e:#}");
                    Loaded::Failed(id)
                }
            };
            let _ = loaded_tx.send(message);
            ctx.request_repaint();
        });
    }
}

async fn load(client: &reqwest::Client, dir: Option<PathBuf>, id: u32, url: &str, size: Option<u32>) -> Result<ColorImage> {
    let path = dir.as_ref().map(|dir| dir.join(format!("{id}.webp")));
    if let Some(path) = &path
        && let Ok(bytes) = tokio::fs::read(path).await
        && let Ok(image) = decode(&bytes, size)
    {
        return Ok(image);
    }
    // A small picture, as the map itself asks for them.
    let sized = match size {
        Some(size) => format!("{url}?x-oss-process=image/resize,w_{size}/quality,Q_90/format,webp"),
        None => url.to_owned(),
    };
    let bytes = client.get(&sized).send().await?.error_for_status()?.bytes().await?.to_vec();
    let image = decode(&bytes, size)?;
    if let Some(path) = &path {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let temp = path.with_extension("webp.tmp");
        if tokio::fs::write(&temp, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&temp, path).await;
        }
    }
    Ok(image)
}

fn decode(bytes: &[u8], size: Option<u32>) -> Result<ColorImage> {
    let image = image::load_from_memory(bytes)?;
    let image = match size {
        Some(size) => image.resize(size, size, image::imageops::FilterType::Triangle),
        None => image,
    }
    .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}
