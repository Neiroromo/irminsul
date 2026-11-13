#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use std::fmt::Display;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing_appender::rolling::Rotation;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, reload};

use crate::player_data::ExportSettings;

mod achievement;
mod achievement_data;
mod achievement_tab;
mod admin;
mod app;
mod atlas;
mod atlas_fetch;
mod autostart;
mod capture;
mod capture_popup;
mod changes;
mod changes_popup;
mod chest_watch;
mod object_watch;
mod quest_watch;
mod puzzle_watch;
mod chests;
mod chests_tab;
mod coverage;
mod events;
mod events_tab;
mod debug_timeline;
mod good;
mod game_launch;
mod hotkey;
mod levels;
mod scenes;
mod icons;
mod live_data;
mod map_overlay;
mod minimap;
mod marker_icons;
mod markers;
mod map_render;
mod game_map;
mod game_map_panel;
mod map_sync;
mod map_tab;
mod route;
mod session_log;
mod route_ui;
mod map_tiles;
mod official_chests;
mod overlay;
mod overlay_window;
mod planner_tab;
mod probe;
mod mob_watch;
mod monitor;
mod player_data;
mod profiles;
mod roster_tab;
mod tray;
mod window_visibility;
mod update;
mod upgrade_data;
mod upgrade_store;
mod wish;

const APP_ID: &str = "Irminsul";

#[derive(Clone, Copy, Debug)]
pub enum ConfirmationType {
    Initial,
    Update,
}

#[derive(Clone, Debug)]
pub enum State {
    Starting,
    CheckingForUpdate,
    WaitingForUpdateConfirmation(String),
    Updating,
    /// `needs_caps` is set when the update dropped the packet capture
    /// permissions the user had granted, and they need to be re-granted.
    Updated {
        needs_caps: bool,
    },
    CheckingForData,
    WaitingForDownloadConfirmation(ConfirmationType),
    Downloading,
    Main,
}

#[derive(Debug)]
pub enum Message {
    UpdateAcknowledged,
    UpdateCanceled,
    DownloadAcknowledged,
    StartCapture,
    StopCapture,
    ExportGenshinOptimizer(ExportSettings, oneshot::Sender<Result<String>>),
    ExportAchievements(oneshot::Sender<Result<String>>),
    FinishedAchievements(oneshot::Sender<Vec<u32>>),
    /// A marker for the debug timeline, e.g. "Door clicked".
    DebugMark(String),
    /// Use the stored data of an account (or none) for the exports.
    SelectProfile(Option<u32>),
    DeleteProfile(u32),
}

#[derive(Clone, Debug)]
pub struct DataUpdated {
    achievements_updated: Option<Instant>,
    characters_updated: Option<Instant>,
    items_updated: Option<Instant>,
}

impl DataUpdated {
    pub fn new() -> Self {
        Self {
            achievements_updated: None,
            characters_updated: None,
            items_updated: None,
        }
    }
}

impl Default for DataUpdated {
    fn default() -> Self {
        Self::new()
    }
}

/// The game account whose data the exports use.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveProfile {
    pub uid: u32,
    pub nickname: Option<String>,
}

/// Where the player is in the world, as the game last said (every few seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerPlace {
    pub scene: u32,
    /// The game's coordinates: x, height, z.
    pub position: [f32; 3],
    /// The floor of the map the player is on, as far as it can be told (0 is
    /// the surface; see `levels`).
    pub floor: u32,
    /// The map of the official map the player is on (0 if the scene is not
    /// known to be one), and where on it, from the origin of the map.
    pub map: u32,
    pub at: [f32; 2],
    /// Pixels of the map to a metre of the scene (1 for most maps).
    pub scale: f32,
    /// The way the player faces, in degrees of the game (0 along z, 90 along x).
    pub yaw: Option<f32>,
    /// How far around them (metres) what is followed is looked at.
    pub range: f32,
    /// Counts what the game said, so a new one can be told from the one before.
    pub count: u64,
}

/// How far capturing the data of the current connection to the game has come.
#[derive(Clone, Debug, Default)]
pub struct CaptureProgress {
    /// Counts the connections to the game server, to tell a new one apart.
    pub session: u64,
    pub key_found: bool,
    pub items: bool,
    pub characters: bool,
    pub achievements: bool,
    /// The data of this connection can't be read.
    pub failed: bool,
}

#[derive(Clone, Debug)]
pub struct AppState {
    state: State,
    capturing: bool,
    /// Why packet capture or decoding is not working, if it is not.
    capture_error: Option<String>,
    /// The latest debug timeline lines.
    debug_events: Vec<String>,
    /// What changed while playing, the latest few batches.
    changes: Vec<changes::ChangeBatch>,
    capture_progress: CaptureProgress,
    /// The account the data belongs to, if it is known.
    profile: Option<ActiveProfile>,
    /// Counts the changes to the stored profiles.
    profiles_version: u64,
    /// What the account in use has, to show.
    roster: std::sync::Arc<player_data::Roster>,
    /// The chests of that account that have been seen, and which are opened.
    chests: std::sync::Arc<chests::Snapshot>,
    /// What state the chests of the markers of the other maps are in, in the game.
    chest_marks: std::sync::Arc<chest_watch::ChestMarks>,
    /// The monsters alive around the player, as the game sent them.
    mobs: std::sync::Arc<mob_watch::MobsSeen>,
    /// Where the player is, if the game has said.
    player: Option<PlayerPlace>,
    updated: DataUpdated,
}

impl AppState {
    fn new() -> Self {
        AppState {
            state: State::Starting,
            capturing: false,
            capture_error: None,
            debug_events: Vec::new(),
            changes: Vec::new(),
            capture_progress: CaptureProgress::default(),
            profile: None,
            profiles_version: 0,
            roster: Default::default(),
            chests: Default::default(),
            chest_marks: Default::default(),
            mobs: Default::default(),
            player: None,
            updated: DataUpdated::new(),
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = false)]
    no_admin: bool,

    /// Start with the window hidden in the tray, capturing right away.
    #[arg(long, default_value_t = false)]
    minimized: bool,

    /// Play the capture popup once after starting.
    #[arg(long, hide = true, default_value_t = false)]
    preview_popup: bool,

    #[arg(
        long = "capture-backend",
        value_enum,
        default_value_t = capture::DEFAULT_CAPTURE_BACKEND_TYPE
    )]
    capture_backend: capture::BackendType,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Default)]
pub enum TracingLevel {
    #[default]
    Default,
    VerboseInfo,
    VerboseDebug,
    VerboseTrace,
}

impl Display for TracingLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TracingLevel::Default => write!(f, "Default"),
            TracingLevel::VerboseInfo => write!(f, "Verbose Info"),
            TracingLevel::VerboseDebug => write!(f, "Verbose Debug"),
            TracingLevel::VerboseTrace => write!(f, "Verbose Trace"),
        }
    }
}

impl TracingLevel {
    fn get_filter(&self) -> &'static str {
        match self {
            TracingLevel::Default => {
                if cfg!(debug_assertions) {
                    "info"
                } else {
                    "warn,irminsul=info"
                }
            }
            TracingLevel::VerboseInfo => "info",
            TracingLevel::VerboseDebug => "debug",
            TracingLevel::VerboseTrace => "trace",
        }
    }
}

struct ReloadHandle(reload::Handle<EnvFilter, tracing_subscriber::Registry>);

/// The lines of the debug timeline go to the background log only.
fn usual_filter(filter: &str) -> String {
    format!("{filter},{}=off", session_log::TIMELINE)
}

impl ReloadHandle {
    pub fn set_filter(&mut self, filter: &str) {
        if let Err(e) = self.0.reload(usual_filter(filter)) {
            tracing::warn!("Failed to set tracing filter to \"{filter}\": {e}");
        }
        tracing::info!("Set tracing filter to \"{filter}\"");
    }
}

/// A panic is written to `crash.log` in the log folder, with where it was.  The
/// window has no console, so otherwise a panic would end the program with
/// nothing said.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = format!(
            "{} {info}\n{}\n\n",
            chrono::Local::now().to_rfc3339(),
            std::backtrace::Backtrace::force_capture()
        );
        if let Ok(dir) = log_dir() {
            use std::io::Write;
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                let _ = file.write_all(text.as_bytes());
            }
        }
        tracing::error!("panic: {info}");
        previous(info);
    }));
}

fn main() -> eframe::Result {
    let (_guard, reload_handle) = tracing_init().unwrap();
    install_panic_hook();

    let args = Args::parse();

    if !args.no_admin {
        #[cfg(any(windows, unix))]
        admin::ensure_admin();
    }

    // After the rights are there: the program that asks for them ends at once.
    session_log::start();
    tracing::info!("Irminsul {} started", env!("CARGO_PKG_VERSION"));

    let capture_backend = args.capture_backend;
    let minimized = args.minimized;
    let preview_popup = args.preview_popup;

    let mut viewport = egui::ViewportBuilder::default()
        // The same size for every tab; it can be made bigger or smaller.
        .with_inner_size(achievement_tab::TAB_WINDOW_SIZE)
        .with_min_inner_size([760., 480.])
        .with_resizable(true)
        .with_decorations(false)
        // The map over the game is a window whose pictures are see-through at
        // places, which needs the windows to be made so that they can be.
        .with_transparent(true)
        .with_icon(
            // NOTE: Adding an icon is optional
            eframe::icon_data::from_png_bytes(&include_bytes!("../assets/icon-256.png")[..])
                .expect("Failed to load icon"),
        );
    if minimized {
        // eframe shows the window once it painted its first frame, whatever is
        // asked for.  Out of sight, that doesn't show until the app hides it
        // properly.
        viewport = viewport.with_position([
            window_visibility::HIDDEN_POSITION,
            window_visibility::HIDDEN_POSITION,
        ]);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Irminsul",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::IrminsulApp::new(
                cc,
                reload_handle,
                capture_backend,
                minimized,
                preview_popup,
            )))
        }),
    );
    session_log::finish();
    result
}

fn log_dir() -> Result<PathBuf> {
    let mut dir = eframe::storage_dir(APP_ID).context("Storage dir not found")?;
    dir.push("log");
    println!("Log folder: {}", dir.display());
    Ok(dir)
}

fn open_log_dir() -> Result<()> {
    let dir = log_dir()?;
    open::that(dir)?;
    Ok(())
}

fn tracing_init() -> Result<(tracing_appender::non_blocking::WorkerGuard, ReloadHandle)> {
    let appender = tracing_appender::rolling::Builder::new()
        .filename_prefix("log")
        .rotation(Rotation::DAILY)
        .max_log_files(7)
        .build(log_dir()?)?;
    let (non_blocking_appender, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::new(usual_filter(TracingLevel::default().get_filter()));
    let (filter, reload_handle) = reload::Layer::new(filter);
    let writer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_appender)
        .with_ansi(false)
        .with_filter(filter);
    // The background log of the run takes more, whatever level is chosen (see `session_log`).
    let background = tracing_subscriber::fmt::layer()
        .with_writer(session_log::Writer)
        .with_ansi(false)
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new("%Y-%m-%d %H:%M:%S%.3f".to_owned()))
        .with_filter(EnvFilter::new(session_log::FILTER));
    tracing_subscriber::registry()
        .with(writer)
        .with(background)
        .init();
    tracing::info!("Tracing initialized and logging to file.");

    Ok((guard, ReloadHandle(reload_handle)))
}

impl PlayerPlace {
    /// The way the player faces on the map (a unit step to the right and down), if
    /// told: the map has x against the game's z, and y against its x.
    pub fn facing(&self) -> Option<[f32; 2]> {
        let turn = self.yaw?.to_radians();
        Some([-turn.cos(), -turn.sin()])
    }
}
