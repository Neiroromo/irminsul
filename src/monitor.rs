use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use anime_game_data::AnimeGameData;
use anyhow::{Context, Result, anyhow};
use auto_artifactarium::{
    ConnectionPacket, GameCommand, GamePacket, GameSniffer, SnifferEvent, matches_achievement_packet, matches_avatar_packet,
    matches_avatar_props, matches_avatar_stats, matches_item_changes, matches_item_packet, matches_talent_level,
    matches_player_mora, matches_player_props,
};
use base64::prelude::*;
use chrono::prelude::*;
use flate2::read::GzDecoder;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::capture::{BackendType, CaptureError, create_capture};
use crate::debug_timeline::DebugTimeline;
use crate::player_data::{PlayerData, Roster};
use crate::profiles::{self, Profile};
use crate::{APP_ID, ActiveProfile, AppState, CaptureProgress, DataUpdated, Message, State};

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

    pub fn update_profile(&mut self, profile: Option<ActiveProfile>) {
        self.app_state.profile = profile;
        self.app_state.profiles_version += 1;
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_roster(&mut self, roster: Roster) {
        self.app_state.roster = std::sync::Arc::new(roster);
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_progress(&mut self, update: impl FnOnce(&mut CaptureProgress)) {
        update(&mut self.app_state.capture_progress);
        let _ = self.state_tx.send(self.app_state.clone());
    }

    /// Tells the UI what changed.
    pub fn push_changes(&mut self, changes: Vec<crate::changes::Change>) {
        if changes.is_empty() {
            return;
        }
        let id = self.app_state.changes.last().map_or(1, |batch| batch.id + 1);
        self.app_state
            .changes
            .push(crate::changes::ChangeBatch { id, changes });
        let extra = self.app_state.changes.len().saturating_sub(16);
        self.app_state.changes.drain(..extra);
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

/// What was captured from the game in a connection.
#[derive(Clone, Copy, Default)]
struct Captured {
    items: bool,
    characters: bool,
    achievements: bool,
}

impl Captured {
    fn all(self) -> bool {
        self.items && self.characters && self.achievements
    }
}

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
    /// The account of the latest connection to the game, once it is known.
    session_uid: Option<u32>,
    session_nickname: Option<String>,
    /// What was captured in the latest connection.
    captured: Captured,
    /// There is captured data that is not stored yet, and when it came.
    unsaved: bool,
    last_data: Option<Instant>,
    /// The account the data held for the exports belongs to.
    active_uid: Option<u32>,
    active_nickname: Option<String>,
    timeline: DebugTimeline,
    sniffer: GameSniffer,
    capture_cancel_token: Option<CancellationToken>,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    packet_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    status_tx: mpsc::UnboundedSender<CaptureStatus>,
    status_rx: mpsc::UnboundedReceiver<CaptureStatus>,
    capture_backend: BackendType,
    /// Whether the mora has been seen in the latest connection: the first time
    /// is what there was at login, not a change.
    mora_seen_in_session: bool,
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
            session_uid: None,
            session_nickname: None,
            captured: Captured::default(),
            unsaved: false,
            last_data: None,
            active_uid: None,
            active_nickname: None,
            timeline: DebugTimeline::new(),
            sniffer,
            capture_cancel_token: None,
            packet_tx,
            packet_rx,
            status_tx,
            status_rx,
            capture_backend,
            mora_seen_in_session: false,
        })
    }

    pub async fn run(mut self) {
        self.app_state.update_app_state(State::Main);

        // Data that stays unsaved because not everything came is stored
        // after a while.
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            #[rustfmt::skip]
                tokio::select! {
                    _ = tick.tick() => self.save_if_settled(),
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
            Message::DebugMark(label) => {
                crate::probe::note(&label);
                self.record(format!("MARK: {label}"));
            }
            Message::SelectProfile(uid) => {
                // An account that has nothing stored (any more) is not one
                // to use.
                let stored = uid.is_some_and(|uid| matches!(Profile::load(uid), Ok(Some(_))));
                self.select_profile(uid.filter(|_| stored));
                self.record(format!("selected profile {uid:?}"));
            }
            Message::DeleteProfile(uid) => {
                if let Err(e) = profiles::delete(uid) {
                    tracing::error!("unable to delete the profile {uid}: {e:#}");
                }
                if self.active_uid == Some(uid) {
                    self.select_profile(None);
                } else {
                    let profile = self.active_profile();
                    self.app_state.update_profile(profile);
                }
                self.record(format!("deleted profile {uid}"));
            }
            Message::FinishedAchievements(reply_tx) => {
                let _ = reply_tx.send(self.player_data.finished_achievement_ids());
            }
            _ => (),
        }
    }

    fn active_profile(&self) -> Option<ActiveProfile> {
        self.active_uid.map(|uid| ActiveProfile {
            uid,
            nickname: self.active_nickname.clone(),
        })
    }

    /// A new connection to the game: what was captured in the last one is
    /// stored, and the account is not known until the game tells.
    fn start_account_session(&mut self) {
        if self.unsaved {
            self.save_session();
        }
        self.session_uid = None;
        self.session_nickname = None;
        self.captured = Captured::default();
        self.last_data = None;
    }

    /// Makes the stored data of `uid` the one the exports use, or none.
    fn select_profile(&mut self, uid: Option<u32>) {
        self.active_uid = uid;
        self.active_nickname = None;
        let stored = uid.map(Profile::load);
        match stored {
            Some(Ok(Some(profile))) => {
                self.active_nickname = profile.nickname.clone();
                self.player_data.load_profile(&profile);
            }
            Some(Err(e)) => {
                tracing::error!("unable to load the profile: {e:#}");
                self.player_data.clear();
            }
            Some(Ok(None)) | None => self.player_data.clear(),
        }

        // What there is now counts as what was received.
        let now = Instant::now();
        let (items, characters, achievements) = self.player_data.availability();
        let mut updated = self.app_state.app_state.updated.clone();
        updated.items_updated = items.then_some(now);
        updated.characters_updated = characters.then_some(now);
        updated.achievements_updated = achievements.then_some(now);
        self.app_state.update_timestamps(updated);

        let profile = self.active_profile();
        self.app_state.update_profile(profile);
        self.publish_roster();
    }

    fn publish_roster(&mut self) {
        let roster = self.player_data.roster();
        self.app_state.update_roster(roster);
    }

    /// Before data captured in this connection is taken: if it is of another
    /// account than the data held, that is replaced by the stored data of it,
    /// so nothing of two accounts gets mixed.
    fn switch_to_session_account(&mut self) {
        if self.session_uid == self.active_uid {
            return;
        }
        self.select_profile(self.session_uid);
        if let Some(name) = &self.session_nickname {
            self.active_nickname = Some(name.clone());
        }
        self.record(format!("data is of account {:?}", self.session_uid));
    }

    fn save_if_settled(&mut self) {
        let settled = self
            .last_data
            .is_some_and(|at| at.elapsed() > Duration::from_secs(6));
        if self.unsaved && settled {
            self.save_session();
        }
    }

    /// Stores what was captured in this connection in the file of the account.
    fn save_session(&mut self) {
        self.unsaved = false;
        let Some(uid) = self.session_uid else {
            self.record("captured data is not stored: the account is not known");
            return;
        };
        if self.active_uid != Some(uid) {
            return;
        }

        let mut profile = match Profile::load(uid) {
            Ok(profile) => profile.unwrap_or_default(),
            Err(e) => {
                tracing::warn!("replacing a profile that can't be read: {e:#}");
                Profile::default()
            }
        };
        profile.uid = uid;
        let captured = self.captured;
        self.player_data.fill_profile(
            &mut profile,
            captured.items,
            captured.characters,
            captured.achievements,
        );
        if let Some(name) = &self.session_nickname {
            profile.nickname = Some(name.clone());
        }

        match profile.save() {
            Ok(()) => {
                let summary = profile.summary();
                self.record(format!(
                    "stored profile {uid} ({} items, {} characters, {} achievements)",
                    summary.items, summary.characters, summary.achievements
                ));
            }
            Err(e) => {
                tracing::error!("unable to store the profile {uid}: {e:#}");
                self.record(format!("unable to store the profile {uid}"));
                return;
            }
        }
        self.active_nickname = profile.nickname.clone();
        let active = self.active_profile();
        self.app_state.update_profile(active);
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
                SnifferEvent::PlayerUid(uid) => self.session_uid = Some(uid),
                // Only a fallback: the token response is the better source.
                SnifferEvent::HeaderUserId(uid) => {
                    if self.session_uid.is_none() && (100_000_000..=999_999_999).contains(&uid) {
                        self.session_uid = Some(uid);
                    }
                }
                SnifferEvent::PlayerNickname(name) => {
                    self.session_nickname = Some(name);
                    if self.session_uid.is_some() {
                        self.unsaved = true;
                        self.last_data = Some(Instant::now());
                    }
                }
                SnifferEvent::HandshakeRequested => {
                    // The game sends a burst of these for one connection.
                    let new_connection = self
                        .session_started
                        .is_none_or(|at| at.elapsed() > Duration::from_secs(3));
                    if new_connection {
                        self.start_account_session();
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
                self.mora_seen_in_session = false;
                self.player_data.start_session();
                return;
            }
            _ => return,
        };

        let log_packets = *self.log_packet_rx.borrow_and_update();

        let mut updated = self.app_state.app_state.updated.clone();
        let mut has_new_data = false;

        for command in commands {
            crate::probe::observe(command.command_id, &command.proto_data);
            let _span = tracing::info_span!("packet id {}", command.command_id);
            if log_packets {
                if let Err(e) = log_command(&command) {
                    tracing::info!("error logging command {e}");
                }
            }

            // What the game says about the player, for the log, so that what is
            // read from it can be checked.
            if let Some(props) = guarded("player props", || matches_player_props(&command)) {
                let text: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
                tracing::info!("Player properties: {}", text.join(" "));
            }

            // The mora is not among the items; it comes with the player's
            // properties, now and each time it changes.
            if let Some(mora) = guarded("mora", || matches_player_mora(&command)) {
                if let Some(before) = self.player_data.process_mora(mora) {
                    self.switch_to_session_account();
                    tracing::info!("Found mora: {mora}");
                    self.record(format!("FOUND mora {mora}"));
                    self.unsaved = true;
                    has_new_data = true;
                    // Not the first time in this connection: it was spent or earned.
                    if self.mora_seen_in_session
                        && let Some(before) = before
                    {
                        self.app_state.push_changes(vec![crate::changes::Change {
                            kind: crate::changes::ChangeKind::Mora,
                            delta: mora as i64 - before as i64,
                        }]);
                    }
                }
                self.mora_seen_in_session = true;
            }

            // A stack of materials that changed, as the game says it in a small
            // message: what there is now.  Taken in even before the lists, which
            // it is not, being at most a few items.
            if let Some(updates) = guarded("item changes", || matches_item_changes(&command))
                && updates.iter().all(|item| self.player_data.knows_item(item.item_id))
            {
                let applied = self.player_data.apply_item_changes(&updates);
                let changes = applied.stacks;
                let weapons = applied.weapons;
                if !changes.is_empty() || !weapons.is_empty() {
                    self.switch_to_session_account();
                    let weapon_ids: Vec<u32> = weapons.iter().map(|weapon| weapon.item_id).collect();
                    self.record(format!("FOUND item changes: {changes:?} weapons {weapon_ids:?}"));
                    self.unsaved = true;
                    has_new_data = true;
                    // Said only when the full list of this connection is in, so
                    // that what is shown was a change and not a correction.
                    if self.captured.items {
                        let mut shown: Vec<crate::changes::Change> = changes
                            .into_iter()
                            .map(|(id, delta)| crate::changes::Change {
                                kind: crate::changes::ChangeKind::Item(id),
                                delta,
                            })
                            .collect();
                        for weapon in &weapons {
                            if weapon.leveled {
                                shown.push(crate::changes::Change {
                                    kind: crate::changes::ChangeKind::Weapon(weapon.item_id),
                                    delta: weapon.level as i64,
                                });
                            }
                            if weapon.refined {
                                shown.push(crate::changes::Change {
                                    kind: crate::changes::ChangeKind::Refinement(weapon.item_id),
                                    delta: weapon.refinement as i64,
                                });
                            }
                        }
                        self.app_state.push_changes(shown);
                    }
                }
                continue;
            }

            // A talent that went up a level.
            if let Some(update) = guarded("talent", || matches_talent_level(&command))
                && let Some((avatar, skill, level)) = self.player_data.apply_talent_level(&update)
            {
                self.record(format!("FOUND talent {skill} of {avatar} at {level}"));
                self.unsaved = true;
                has_new_data = true;
                if self.captured.characters {
                    self.app_state.push_changes(vec![crate::changes::Change {
                        kind: crate::changes::ChangeKind::Talent(avatar, skill),
                        delta: level as i64,
                    }]);
                }
                continue;
            }

            // A character's level, experience or ascension that changed, and the
            // stats that go with it.
            if let Some(update) = guarded("avatar props", || matches_avatar_props(&command))
                && let Some((id, level)) = self
                    .player_data
                    .apply_avatar_props(&update.guids, &update.props)
            {
                self.record(format!("FOUND avatar {id} props {:?}", update.props));
                self.unsaved = true;
                has_new_data = true;
                if self.captured.characters
                    && let Some(level) = level
                {
                    self.app_state.push_changes(vec![crate::changes::Change {
                        kind: crate::changes::ChangeKind::Level(id),
                        delta: level as i64,
                    }]);
                }
                continue;
            }
            if let Some((guids, stats)) = guarded("avatar stats", || matches_avatar_stats(&command))
                && self.player_data.apply_avatar_stats(&guids, &stats)
            {
                self.unsaved = true;
                has_new_data = true;
                continue;
            }

            // Avatars are matched first: they are strict about what they accept,
            // while an item match is looser and could claim an avatar list.
            if let Some(avatars) = guarded("avatar packet", || matches_avatar_packet(&command)) {
                self.switch_to_session_account();
                if self.player_data.process_characters(&avatars) {
                    self.captured.characters = true;
                    tracing::info!("Found avatar packet with {} avatars", avatars.len());
                    self.record(format!("FOUND avatar packet, {} avatars", avatars.len()));
                    updated.characters_updated = Some(Instant::now());
                    self.app_state.update_progress(|progress| progress.characters = true);
                    has_new_data = true;
                } else {
                    self.record(format!("ignored partial avatar list, {}", avatars.len()));
                }
            } else if let Some(items) = guarded("item packet", || matches_item_packet(&command)) {
                self.switch_to_session_account();
                if self.player_data.process_items(&items) {
                    self.captured.items = true;
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
                self.switch_to_session_account();
                self.player_data.process_achievements(&achievements);
                self.captured.achievements = true;
                updated.achievements_updated = Some(Instant::now());
                self.app_state.update_progress(|progress| progress.achievements = true);
                has_new_data = true;
            }
        }

        if has_new_data {
            self.publish_roster();
            self.unsaved = true;
            self.last_data = Some(Instant::now());
            if self.captured.all() {
                self.save_session();
            }
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
