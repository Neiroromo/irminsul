use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use anime_game_data::AnimeGameData;
use anyhow::{Context, Result, anyhow};
use auto_artifactarium::{
    ConnectionPacket, GameCommand, GamePacket, GameSniffer, SnifferEvent, matches_achievement_packet, matches_avatar_packet,
    matches_item_packet,
};
use base64::prelude::*;
use chrono::prelude::*;
use flate2::read::GzDecoder;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::capture::{BackendType, CaptureError, create_capture};
use crate::debug_timeline::DebugTimeline;
use crate::player_data::PlayerData;
use crate::{APP_ID, AppState, CaptureProgress, DataUpdated, Message, State};

struct AppStateManager {
    app_state: AppState,
    state_tx: watch::Sender<AppState>,
}

impl AppStateManager {
    fn new(app_state: AppState, state_tx: watch::Sender<AppState>) -> Self {
        Self {
            app_state,
            state_tx,
        }
    }

    pub fn update_app_state(&mut self, state: State) {
        self.app_state.state = state;
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_capturing_state(&mut self, capturing: bool) {
        self.app_state.capturing = capturing;
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_capture_error(&mut self, error: Option<String>) {
        if self.app_state.capture_error != error {
            self.app_state.capture_error = error;
            let _ = self.state_tx.send(self.app_state.clone());
        }
    }

    pub fn update_progress(&mut self, update: impl FnOnce(&mut CaptureProgress)) {
        update(&mut self.app_state.capture_progress);
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_debug_events(&mut self, events: Vec<String>) {
        self.app_state.debug_events = events;
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_timestamps(&mut self, updated: DataUpdated) {
        self.app_state.updated = updated;
        let _ = self.state_tx.send(self.app_state.clone());
    }
}

/// First delay before retrying a failed packet capture, doubled up to the max.
const CAPTURE_RETRY_MIN: Duration = Duration::from_secs(1);
const CAPTURE_RETRY_MAX: Duration = Duration::from_secs(15);
/// A capture that ran at least this long is not counted as a quick failure.
const CAPTURE_STABLE_AFTER: Duration = Duration::from_secs(10);

enum CaptureStatus {
    Running,
    Error(String),
}

pub struct Monitor {
    app_state: AppStateManager,
    ui_message_rx: mpsc::UnboundedReceiver<Message>,
    log_packet_rx: watch::Receiver<bool>,
    player_data: PlayerData,
    keys: HashMap<u16, Vec<u8>>,
    /// When the capture progress of the latest connection to the game began.
    session_started: Option<Instant>,
    timeline: DebugTimeline,
    sniffer: GameSniffer,
    capture_cancel_token: Option<CancellationToken>,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    packet_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    status_tx: mpsc::UnboundedSender<CaptureStatus>,
    status_rx: mpsc::UnboundedReceiver<CaptureStatus>,
    capture_backend: BackendType,
}

impl Monitor {
    pub async fn new(
        state_tx: watch::Sender<AppState>,
        mut ui_message_rx: mpsc::UnboundedReceiver<Message>,
        log_packet_rx: watch::Receiver<bool>,
        capture_backend: BackendType,
    ) -> Result<Self> {
        let mut app_state = AppStateManager::new(state_tx.borrow().clone(), state_tx.clone());
        let game_data = get_database(&mut app_state, &mut ui_message_rx).await?;
        let player_data = PlayerData::new(game_data);
        let keys = load_keys()?;
        let sniffer = GameSniffer::new().set_initial_keys(keys.clone());
        let (packet_tx, packet_rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = mpsc::unbounded_channel();

        Ok(Self {
            app_state,
            player_data,
            ui_message_rx,
            log_packet_rx,
            keys,
            session_started: None,
            timeline: DebugTimeline::new(),
            sniffer,
            capture_cancel_token: None,
            packet_tx,
            packet_rx,
            status_tx,
            status_rx,
            capture_backend,
        })
    }

    pub async fn run(mut self) {
        self.app_state.update_app_state(State::Main);

        loop {
            #[rustfmt::skip]
                tokio::select! {
                    Some(packet) = self.packet_rx.recv() => self.handle_packet(packet),
                    Some(status) = self.status_rx.recv() => self.handle_capture_status(status),
                    Some(msg) = self.ui_message_rx.recv() => self.handle_ui_msg(msg),
                }
        }
    }

    fn handle_ui_msg(&mut self, msg: Message) {
        match msg {
            Message::StartCapture => {
                if self.capture_cancel_token.is_some() {
                    tracing::warn!("Capture start request with an existing cancel token");
                }

                // Spawn capture task.
                let cancel_token = CancellationToken::new();
                tokio::spawn(capture_task(
                    cancel_token.clone(),
                    self.packet_tx.clone(),
                    self.status_tx.clone(),
                    self.capture_backend,
                ));
                self.capture_cancel_token = Some(cancel_token);
                self.record("capture started");
                self.app_state.update_capture_error(None);
                self.app_state.update_capturing_state(true);
            }
            Message::StopCapture => {
                let Some(cancel_token) = self.capture_cancel_token.take() else {
                    tracing::warn!("Capture stop request with no current cancel token");
                    return;
                };
                cancel_token.cancel();
                self.record("capture stopped");
                self.app_state.update_capture_error(None);
                self.app_state.update_capturing_state(false);
            }
            Message::ExportGenshinOptimizer(settings, reply_tx) => {
                let _ = reply_tx.send(self.player_data.export_genshin_optimizer(&settings));
            }
            Message::ExportAchievements(reply_tx) => {
                let _ = reply_tx.send(self.player_data.export_achievements());
            }
            Message::DebugMark(label) => self.record(format!("MARK: {label}")),
            Message::FinishedAchievements(reply_tx) => {
                let _ = reply_tx.send(self.player_data.finished_achievement_ids());
            }
            _ => (),
        }
    }

    fn handle_capture_status(&mut self, status: CaptureStatus) {
        // Ignore reports from a capture that has been stopped meanwhile.
        if self.capture_cancel_token.is_none() {
            return;
        }
        match status {
            CaptureStatus::Running => self.app_state.update_capture_error(None),
            CaptureStatus::Error(error) => {
                self.record(format!("capture problem: {error}"));
                self.app_state.update_capture_error(Some(error));
            }
        }
    }

    /// Adds a line to the debug timeline and shows it in the UI.
    fn record(&mut self, text: impl AsRef<str>) {
        self.timeline.record(text);
        self.app_state.update_debug_events(self.timeline.recent());
    }

    /// Moves what the sniffer noticed since the last call to the timeline.
    fn record_sniffer_events(&mut self) {
        let events = self.sniffer.take_events();
        if events.is_empty() {
            return;
        }
        for event in events {
            self.timeline.record(event.to_string());
            match event {
                SnifferEvent::UndecryptedSample(bytes) => self.timeline.save_sample(&bytes),
                SnifferEvent::HandshakeRequested => {
                    // The game sends a burst of these for one connection.
                    let new_connection = self
                        .session_started
                        .is_none_or(|at| at.elapsed() > Duration::from_secs(3));
                    if new_connection {
                        self.session_started = Some(Instant::now());
                        self.app_state.update_progress(|progress| {
                            *progress = CaptureProgress {
                                session: progress.session + 1,
                                ..Default::default()
                            };
                        });
                    }
                }
                SnifferEvent::TokenResponseUnreadable { .. } | SnifferEvent::KeySearchFailed { .. } => {
                    self.app_state.update_progress(|progress| progress.failed = true);
                    self.app_state.update_capture_error(Some(
                        "The game's login response could not be read, so nothing can be \
                         decrypted. Log out of the game and log in again."
                            .to_string(),
                    ));
                }
                SnifferEvent::SessionKeyFound { .. } => {
                    self.app_state.update_progress(|progress| progress.key_found = true);
                    self.app_state.update_capture_error(None);
                }
                _ => (),
            }
        }
        self.app_state.update_debug_events(self.timeline.recent());
    }

    /// The sniffer panicked on a packet, so its state can't be trusted: start
    /// over.  The session key is lost with it, which needs a new login.
    fn handle_sniffer_panic(&mut self, panic: Box<dyn std::any::Any + Send>) {
        tracing::error!(
            "packet decoder panicked, resetting it: {}",
            panic_message(&panic)
        );
        self.sniffer = GameSniffer::new().set_initial_keys(self.keys.clone());
        self.record("packet decoder panicked and was reset");
        self.app_state.update_capture_error(Some(
            "Packet decoding failed. Log in to the game again while capture is running."
                .to_string(),
        ));
    }

    fn handle_packet(&mut self, packet: Vec<u8>) {
        let received = match catch_unwind(AssertUnwindSafe(|| self.sniffer.receive_packet(packet)))
        {
            Ok(received) => received,
            Err(panic) => return self.handle_sniffer_panic(panic),
        };
        self.record_sniffer_events();
        let commands = match received {
            Some(GamePacket::Commands(commands)) => commands,
            Some(GamePacket::Connection(ConnectionPacket::HandshakeRequested)) => {
                self.player_data.start_session();
                return;
            }
            _ => return,
        };

        let log_packets = *self.log_packet_rx.borrow_and_update();

        let mut updated = self.app_state.app_state.updated.clone();
        let mut has_new_data = false;

        for command in commands {
            let _span = tracing::info_span!("packet id {}", command.command_id);
            if log_packets {
                if let Err(e) = log_command(&command) {
                    tracing::info!("error logging command {e}");
                }
            }

            // Avatars are matched first: they are strict about what they accept,
            // while an item match is looser and could claim an avatar list.
            if let Some(avatars) = guarded("avatar packet", || matches_avatar_packet(&command)) {
                if self.player_data.process_characters(&avatars) {
                    tracing::info!("Found avatar packet with {} avatars", avatars.len());
                    self.record(format!("FOUND avatar packet, {} avatars", avatars.len()));
                    updated.characters_updated = Some(Instant::now());
                    self.app_state.update_progress(|progress| progress.characters = true);
                    has_new_data = true;
                } else {
                    self.record(format!("ignored partial avatar list, {}", avatars.len()));
                }
            } else if let Some(items) = guarded("item packet", || matches_item_packet(&command)) {
                if self.player_data.process_items(&items) {
                    tracing::info!("Found item packet with {} items", items.len());
                    self.record(format!("FOUND item packet, {} items", items.len()));
                    updated.items_updated = Some(Instant::now());
                    self.app_state.update_progress(|progress| progress.items = true);
                    has_new_data = true;
                } else {
                    self.record(format!("ignored partial item list, {}", items.len()));
                }
            } else if let Some(achievements) =
                guarded("achievement packet", || matches_achievement_packet(&command))
            {
                tracing::info!(
                    "Found achievement packet with {} achievements ({} finished)",
                    achievements.len(),
                    achievements.iter().filter(|a| a.status >= 2).count()
                );
                self.record(format!(
                    "FOUND achievement packet, {} achievements",
                    achievements.len()
                ));
                self.player_data.process_achievements(&achievements);
                updated.achievements_updated = Some(Instant::now());
                self.app_state.update_progress(|progress| progress.achievements = true);
                has_new_data = true;
            }
        }

        if has_new_data {
            // Data is arriving, so whatever went wrong earlier is over.
            self.app_state.update_capture_error(None);
            self.app_state.update_timestamps(updated);
        }
    }
}

async fn get_database(
    app_state: &mut AppStateManager,
    _ui_message_rx: &mut mpsc::UnboundedReceiver<Message>,
) -> Result<AnimeGameData> {
    app_state.update_app_state(State::CheckingForData);

    static DATABASE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/game_data.gz"));
    let reader = GzDecoder::new(DATABASE);
    let db = anime_game_data::AnimeGameData::new_from_reader(reader)?;

    Ok(db)
}

/// Runs packet capture until cancelled.  If the capture can't be created or
/// stops by itself it is retried with a growing delay, and the problem is
/// reported through `status_tx` so it shows in the UI.
async fn capture_task(
    cancel_token: CancellationToken,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    status_tx: mpsc::UnboundedSender<CaptureStatus>,
    backend: BackendType,
) {
    let mut retry_delay = CAPTURE_RETRY_MIN;
    loop {
        let started = Instant::now();
        let error = match run_capture(&cancel_token, &packet_tx, &status_tx, backend).await {
            Ok(()) => break,
            Err(e) => e,
        };
        tracing::error!("{error:#}");
        if started.elapsed() >= CAPTURE_STABLE_AFTER {
            retry_delay = CAPTURE_RETRY_MIN;
        }
        let _ = status_tx.send(CaptureStatus::Error(format!(
            "{error:#}. Retrying in {}s...",
            retry_delay.as_secs()
        )));

        tokio::select!(
            _ = tokio::time::sleep(retry_delay) => (),
            _ = cancel_token.cancelled() => break,
        );
        retry_delay = (retry_delay * 2).min(CAPTURE_RETRY_MAX);
    }
    tracing::info!("ending capture");
}

/// Returns `Ok` when cancelled and an error if the capture failed.
async fn run_capture(
    cancel_token: &CancellationToken,
    packet_tx: &mpsc::UnboundedSender<Vec<u8>>,
    status_tx: &mpsc::UnboundedSender<CaptureStatus>,
    backend: BackendType,
) -> Result<()> {
    let mut capture = create_capture(backend)
        .map_err(|e| anyhow!("Error creating packet capture using {:?}: {e}", backend))?;
    tracing::info!("starting capture");
    let _ = status_tx.send(CaptureStatus::Running);
    loop {
        let packet = tokio::select!(
            packet = capture.next_packet() => packet,
            _ = cancel_token.cancelled() => return Ok(()),
        );
        match packet {
            Ok(packet) => {
                if packet_tx.send(packet).is_err() {
                    // The monitor is gone, nothing left to capture for.
                    return Ok(());
                }
            }
            // These never recover, and would otherwise make this loop spin.
            Err(e @ (CaptureError::CaptureClosed | CaptureError::ChannelClosed)) => {
                return Err(anyhow!("Packet capture stopped: {e}"));
            }
            Err(e) => tracing::error!("Error receiving packet: {e}"),
        }
    }
}

/// Runs a packet matcher, treating a panic in it as "no match" so one
/// malformed packet can't take the monitor down.
fn guarded<T>(what: &str, matcher: impl FnOnce() -> Option<T>) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(matcher)) {
        Ok(result) => result,
        Err(panic) => {
            tracing::error!("{what} matcher panicked: {}", panic_message(&panic));
            None
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

fn log_command(command: &GameCommand) -> Result<()> {
    let mut packet_log_path = eframe::storage_dir(APP_ID).context("Storage dir not found")?;
    packet_log_path.push("packet_log");
    fs::create_dir_all(&packet_log_path)?;

    let now = Local::now();
    packet_log_path.push(format!(
        "{}-{}.bin",
        now.format("%Y-%m-%d_%H-%M-%S%.f"),
        command.command_id
    ));

    let file = fs::File::create(&packet_log_path)
        .with_context(|| format!("can't create file {packet_log_path:?}"))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(&command.proto_data)?;

    Ok(())
}

fn load_keys() -> Result<HashMap<u16, Vec<u8>>> {
    let keys: HashMap<u16, String> = serde_json::from_slice(include_bytes!("../keys/gi.json"))?;

    keys.iter()
        .map(|(key, value)| -> Result<_, _> { Ok((*key, BASE64_STANDARD.decode(value)?)) })
        .collect::<Result<HashMap<_, _>>>()
}
