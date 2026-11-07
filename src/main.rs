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
mod autostart;
mod capture;
mod capture_popup;
mod changes;
mod changes_popup;
mod debug_timeline;
mod good;
mod game_launch;
mod icons;
mod live_data;
mod overlay;
mod planner_tab;
mod probe;
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

impl ReloadHandle {
    pub fn set_filter(&mut self, filter: &str) {
        if let Err(e) = self.0.reload(filter) {
            tracing::warn!("Failed to set tracing filter to \"{filter}\": {e}");
        }
        tracing::info!("Set tracing filter to \"{filter}\"");
    }
}

fn main() -> eframe::Result {
    let (_guard, reload_handle) = tracing_init().unwrap();

    let args = Args::parse();

    if !args.no_admin {
        #[cfg(any(windows, unix))]
        admin::ensure_admin();
    }

    let capture_backend = args.capture_backend;
    let minimized = args.minimized;
    let preview_popup = args.preview_popup;

    let mut viewport = egui::ViewportBuilder::default()
        // The same size for every tab; it can be made bigger or smaller.
        .with_inner_size(achievement_tab::TAB_WINDOW_SIZE)
        .with_min_inner_size([760., 480.])
        .with_resizable(true)
        .with_decorations(false)
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
    eframe::run_native(
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
    )
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

    let filter = EnvFilter::new(TracingLevel::default().get_filter());
    let (filter, reload_handle) = reload::Layer::new(filter);
    let writer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_appender)
        .with_ansi(false);
    tracing_subscriber::registry()
        .with(filter)
        .with(writer)
        .init();
    tracing::info!("Tracing initialized and logging to file.");

    Ok((guard, ReloadHandle(reload_handle)))
}
