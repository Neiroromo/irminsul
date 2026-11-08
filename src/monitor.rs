use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use anime_game_data::AnimeGameData;
use anyhow::{Context, Result, anyhow};
use auto_artifactarium::{
    ConnectionPacket, GameCommand, GamePacket, GameSniffer, SnifferEvent, matches_achievement_packet, matches_avatar_packet,
    matches_avatar_props, matches_avatar_stats, matches_scene_arrival, matches_teleport_target, matches_gadget_use, matches_gadgets_in_view, matches_item_changes,
    matches_avatar_motion, matches_quest_list, StoryStatus, matches_quest_updates, matches_story_update, matches_item_packet, matches_location_of_player, matches_talent_level, matches_player_mora,
    matches_player_props, matches_uid_of_player,
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

    pub fn update_chest_marks(&mut self, marks: std::sync::Arc<crate::chest_watch::ChestMarks>) {
        self.app_state.chest_marks = marks;
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_chests(&mut self, chests: std::sync::Arc<crate::chests::Snapshot>) {
        self.app_state.chests = chests;
        let _ = self.state_tx.send(self.app_state.clone());
    }

    pub fn update_player(&mut self, player: crate::PlayerPlace) {
        self.app_state.player = Some(player);
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
    /// The id the game puts on the player's place, when it is not the account's
    /// (`location_uid`), and one that is told instead of it, and how many times.
    location_uid: Option<u32>,
    foreign_location: Option<(u32, u8)>,
    /// What was said of the player's place in this connection, once each.
    location_said: bool,
    location_dumped: bool,
    /// A place with the account's own id was told in this connection.
    location_matched: bool,
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
    /// The program joined a connection that was running (no login was seen): the stored data
    /// of the account is what there is, and is not cleared while the account is not known.
    resumed: bool,
    /// A session key was found and is not kept yet.
    session_key_changed: bool,
    /// Where the player was last told by the game to be (scene, floor, how far the radar
    /// looks), which the movement the client sends is put on, and when it was last used.
    motion_scene: Option<(u32, u32, f32)>,
    motion_at: Instant,
    /// How many commands of each kind came, and since when (told in the timeline).
    command_counts: std::collections::BTreeMap<u16, u32>,
    counts_since: std::time::Instant,
    capture_cancel_token: Option<CancellationToken>,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    packet_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    status_tx: mpsc::UnboundedSender<CaptureStatus>,
    status_rx: mpsc::UnboundedReceiver<CaptureStatus>,
    capture_backend: BackendType,
    /// Whether the mora has been seen in the latest connection: the first time
    /// is what there was at login, not a change.
    mora_seen_in_session: bool,
    /// The chests that were seen and opened, of the account in use.
    chests: crate::chests::Tracker,
    /// Which floor of the map the player is on.
    floors: crate::levels::FloorLocator,
    /// Which map each scene of the game is.
    scenes: crate::scenes::Scenes,
    /// The state of the chests of the markers of the other maps, told by the game.
    watch: crate::chest_watch::ChestWatch,
    watch_published: u64,
    /// The state of the things done once (oculi, seelies, puzzles), told by the game.
    objects: crate::object_watch::ObjectWatch,
    /// The quests that are done, told by the game, for the markers of the quests.
    quests: crate::quest_watch::QuestWatch,
    /// Puzzles done and things picked up, for the markers of them.
    puzzles: crate::puzzle_watch::PuzzleWatch,
    /// The things put in view that are written in the census, with their state.
    census_seen: std::collections::HashMap<(u32, u32), Option<u32>>,
    /// The version of the chests that the UI has.
    chests_published: u64,
    /// How many times the game has said where the player is.
    player_reports: u64,
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
        let session_key_changed = false;
        let resumed = false;
        let motion_scene = None;
        let motion_at = Instant::now();
        let command_counts = Default::default();
        let counts_since = std::time::Instant::now();
        let sniffer = GameSniffer::new().set_initial_keys(keys.clone()).set_retained_key(load_session_key());
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
            location_uid: None,
            foreign_location: None,
            location_said: false,
            location_dumped: false,
            location_matched: false,
            session_nickname: None,
            captured: Captured::default(),
            unsaved: false,
            last_data: None,
            active_uid: None,
            active_nickname: None,
            timeline: DebugTimeline::new(),
            sniffer,
            session_key_changed,
            resumed,
            motion_scene,
            motion_at,
            command_counts,
            counts_since,
            capture_cancel_token: None,
            packet_tx,
            packet_rx,
            status_tx,
            status_rx,
            capture_backend,
            mora_seen_in_session: false,
            chests: crate::chests::Tracker::new(),
            floors: crate::levels::FloorLocator::new(),
            scenes: crate::scenes::Scenes::new(),
            watch: crate::chest_watch::ChestWatch::new(),
            watch_published: 0,
            objects: crate::object_watch::ObjectWatch::new(),
            quests: crate::quest_watch::QuestWatch::new(),
            puzzles: crate::puzzle_watch::PuzzleWatch::new(),
            census_seen: Default::default(),
            chests_published: u64::MAX,
            player_reports: 0,
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
        self.resumed = false;
        self.motion_scene = None;
        self.location_uid = None;
        self.foreign_location = None;
        self.location_said = false;
        self.location_dumped = false;
        self.location_matched = false;
        self.session_nickname = None;
        self.captured = Captured::default();
        self.last_data = None;
        self.chests.start_session();
        self.watch.start_session();
        self.objects.start_session();
        self.puzzles.start_session();
        self.census_seen.clear();
    }

    /// The account of this connection became known after data of it was taken:
    /// that data is of it.
    fn learned_uid(&mut self, uid: u32) {
        self.session_uid = Some(uid);
        self.record(format!("player id {uid} (found next to the name)"));
        // What is loaded is of another account: it is dropped, not stored under this one.
        let other_account = self.active_uid.is_some_and(|active| active != uid);
        if other_account {
            self.unsaved = false;
            self.select_profile(Some(uid));
            if let Some(name) = &self.session_nickname {
                self.active_nickname = Some(name.clone());
            }
            self.chests_for_session();
            self.publish_chests();
            return;
        }
        if self.active_uid.is_none() {
            self.active_uid = Some(uid);
            self.active_nickname = self.session_nickname.clone();
            let profile = self.active_profile();
            self.app_state.update_profile(profile);
        }
        self.chests_for_session();
        self.publish_chests();
        // What was captured so far can be stored now.
        self.unsaved = true;
        self.last_data = Some(Instant::now());
    }

    /// Tells the UI of the chests, if they changed.
    fn publish_chests(&mut self) {
        let snapshot = self.chests.snapshot();
        if snapshot.version != self.chests_published {
            self.chests_published = snapshot.version;
            self.app_state.update_chests(snapshot);
        }
    }

    /// Gadgets of the world came into view: the chests among them are noted,
    /// and the user is told of the ones that were not told of yet.
    fn chests_in_view(&mut self, gadgets: &[auto_artifactarium::SceneGadget]) {
        self.chests_for_session();
        self.take_census(gadgets);
        self.floors.observe(gadgets);
        self.scenes.note_gadgets(gadgets);
        // Chests of other scenes (Enkanomiya and the like) are not of the world: their
        // places are not kept as places of the world.
        let world = self.scenes.scene_now().is_none_or(|scene| scene == 3);
        // The chests of the markers of the other maps.
        if let Some(scene) = self.scenes.scene_now()
            && let Some(scene_map) = self.scenes.map_of(scene)
        {
            self.watch.gadgets(scene, scene_map, gadgets);
            self.objects.gadgets(scene, scene_map, gadgets);
            self.puzzles.gadgets(scene, scene_map, gadgets);
            self.publish_chest_marks();
        }
        let report = self.chests.gadgets_in_scene(gadgets, world);
        if !report.new_by_tier.is_empty() {
            let text: Vec<String> = report
                .new_by_tier
                .iter()
                .map(|(tier, count)| format!("tier {tier} x{count}"))
                .collect();
            self.record(format!("FOUND chests in view: {}", text.join(", ")));
            for detail in &report.details {
                self.record(format!("  chest: {detail}"));
            }
            self.app_state.push_changes(
                report
                    .new_by_tier
                    .into_iter()
                    .map(|(tier, count)| crate::changes::Change {
                        kind: crate::changes::ChangeKind::ChestSeen(tier),
                        delta: count as i64,
                    })
                    .collect(),
            );
        }
        self.publish_chests();
    }

    /// Writes down every thing the game puts in view (what kind it is, the state it is
    /// in, where), once, and again when its state changes: what tells which of the
    /// things on the map are there or done.
    fn take_census(&mut self, gadgets: &[auto_artifactarium::SceneGadget]) {
        let scene = self.scenes.scene_now().unwrap_or(0);
        let scene_map = self.scenes.map_of(scene);
        for gadget in gadgets {
            if self.census_seen.get(&(scene, gadget.entity_id)) == Some(&gadget.state) {
                continue;
            }
            self.census_seen.insert((scene, gadget.entity_id), gadget.state);
            let place = match (gadget.position, scene_map) {
                (Some(p), Some(map)) => {
                    let at = map.to_map(p);
                    format!("pos={:.1},{:.1},{:.1} map={} at={:.1},{:.1}", p[0], p[1], p[2], map.map, at[0], at[1])
                }
                (Some(p), None) => format!("pos={:.1},{:.1},{:.1} map=-", p[0], p[1], p[2]),
                _ => "pos=-".to_owned(),
            };
            crate::probe::census(&format!(
                "scene={scene} e={} g={} grp={} cfg={} st={:?} {place}",
                gadget.entity_id, gadget.gadget_id, gadget.group_id, gadget.config_id, gadget.state
            ));
        }
    }

    /// Makes the chests held those of the account of this session.  When the
    /// account is not known (yet) they are held without one, and go to it when
    /// it is.
    /// Gives the maps what the game said of the chests of their markers, if it is new.
    fn publish_chest_marks(&mut self) {
        let version = self
            .watch
            .version()
            .wrapping_add(self.objects.version().rotate_left(20))
            .wrapping_add(self.quests.version().rotate_left(40))
            .wrapping_add(self.puzzles.version().rotate_left(50));
        if version != self.watch_published {
            self.watch_published = version;
            let mut marks = self.watch.snapshot();
            marks.version = version;
            // Only the chests of the other maps are given to the maps for now: what the
            // watches of the oculi, seelies, puzzles, gathering and quests say is kept apart,
            // not used (see `EXTRA_MARKS` and docs/tracking-logic.md).
            if EXTRA_MARKS {
                let objects = self.objects.snapshot();
                let quests = self.quests.snapshot();
                let puzzles = self.puzzles.snapshot();
                // What a puzzle's own record says is done is not undone by an older sighting
                // of its objects kept by the watch of the things done once.
                let done: std::collections::HashSet<u32> = puzzles.opened.iter().map(|(marker, _)| *marker).collect();
                marks.opened.extend(puzzles.opened);
                marks.opened.extend(objects.opened);
                marks.closed.extend(objects.closed);
                marks.opened.extend(quests.opened);
                marks.closed.extend(quests.closed);
                marks.closed.retain(|marker| !done.contains(marker));
            }
            self.app_state.update_chest_marks(std::sync::Arc::new(marks));
        }
    }

    fn chests_for_session(&mut self) {
        self.watch.use_account(self.session_uid);
        self.objects.use_account(self.session_uid);
        self.quests.use_account(self.session_uid);
        self.puzzles.use_account(self.session_uid);
        match self.session_uid {
            Some(uid) if self.chests.account().is_none() => self.chests.adopt_account(uid),
            Some(uid) => self.chests.use_account(Some(uid)),
            None => self.chests.use_account(None),
        }
    }

    /// A chest was opened: the user is told, and it is stored.
    fn chest_opened(&mut self, opened: crate::chests::Opened) {
        self.record(format!(
            "FOUND chest opened: gadget {} ({} of {})",
            opened.gadget, opened.totals.0, opened.totals.1
        ));
        self.app_state.push_changes(vec![
            crate::changes::Change {
                kind: crate::changes::ChangeKind::ChestOpened(opened.gadget),
                delta: 1,
            },
            crate::changes::Change {
                kind: crate::changes::ChangeKind::ChestTotals {
                    opened: opened.totals.0,
                    known: opened.totals.1,
                },
                delta: opened.totals.1.max(1) as i64,
            },
        ]);
        self.chests.save_if_dirty();
        self.publish_chests();
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
        // The chests shown are those of the same account.
        self.chests.use_account(uid);
        self.publish_chests();
    }

    fn publish_roster(&mut self) {
        let roster = self.player_data.roster();
        self.app_state.update_roster(roster);
    }

    /// Before data captured in this connection is taken: if it is of another
    /// account than the data held, that is replaced by the stored data of it,
    /// so nothing of two accounts gets mixed.
    fn switch_to_session_account(&mut self) {
        // The account is not known (yet): what is loaded stays as it is, and is not cleared.
        // (The program started while the game ran: the account is told by the first place.)
        if (self.session_uid.is_none() && self.resumed) || self.session_uid == self.active_uid {
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
        self.chests.save_if_dirty();
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
                    // Ids have nine digits, or ten for the newer Asian accounts.
                    if self.session_uid.is_none() && (100_000_000..=u32::MAX).contains(&uid) {
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
                SnifferEvent::SessionKeyResumed => {
                    self.resumed = true;
                    self.app_state.update_capture_error(None);
                    // What comes now is kept as it comes, as after a login.
                    crate::probe::session_started();
                }
                SnifferEvent::SessionKeyFound { .. } => {
                    self.session_key_changed = true;
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
        self.sniffer = GameSniffer::new().set_initial_keys(self.keys.clone()).set_retained_key(load_session_key());
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
        // The key of the connection is kept, so that the program can be started again
        // (updated) while the game is running without logging in again.
        if self.session_key_changed {
            self.session_key_changed = false;
            if let Some(key) = self.sniffer.session_key() {
                save_session_key(key);
            }
        }
        let commands = match received {
            Some(GamePacket::Commands(commands)) => commands,
            Some(GamePacket::Connection(ConnectionPacket::HandshakeRequested)) => {
                self.mora_seen_in_session = false;
                crate::probe::session_started();
                self.player_data.start_session();
                return;
            }
            _ => return,
        };

        if self.counts_since.elapsed() >= Duration::from_secs(20) {
            let text: Vec<String> = self.command_counts.iter().map(|(id, count)| format!("{id}x{count}")).collect();
            self.command_counts.clear();
            self.counts_since = std::time::Instant::now();
            self.record(format!("commands in 20 s: {}", text.join(" ")));
        }

        let log_packets = *self.log_packet_rx.borrow_and_update();

        let mut updated = self.app_state.app_state.updated.clone();
        let mut has_new_data = false;

        for command in commands {
            *self.command_counts.entry(command.command_id).or_insert(0) += 1;
            crate::probe::observe(command.command_id, &command.proto_data);
            let _span = tracing::info_span!("packet id {}", command.command_id);

            // The state of the quests: written down (the whole list comes when logging in), so
            // that what is done can be told from what is not.
            for update in guarded("quest updates", || Some(matches_quest_updates(&command))).unwrap_or_default() {
                crate::probe::census(&format!(
                    "quest id={} state={} story={}",
                    update.quest, update.state, update.parent
                ));
            }

            let list = guarded("quest list", || Some(matches_quest_list(&command))).unwrap_or_default();
            if !list.is_empty() {
                self.chests_for_session();
                self.quests.apply_list(&list);
                self.publish_chest_marks();
                let count = |status: StoryStatus| list.iter().filter(|entry| entry.status == status).count();
                self.record(format!(
                    "quest list: {} stories, {} done, {} in progress, {} offered",
                    list.len(),
                    count(StoryStatus::Done),
                    count(StoryStatus::InProgress),
                    count(StoryStatus::Available)
                ));
                for entry in &list {
                    let open: Vec<String> = entry.open_steps.iter().map(|(quest, state)| format!("{quest}:{state}")).collect();
                    crate::probe::census(&format!(
                        "queststory id={} status={:?} since={} steps={} open={}",
                        entry.story,
                        entry.status,
                        entry.since,
                        entry.steps,
                        open.join(",")
                    ));
                }
            }
            if let Some((story, quests)) = guarded("story update", || Some(matches_story_update(&command))).flatten() {
                let list: Vec<String> = quests.iter().map(|(quest, state)| format!("{quest}:{state}")).collect();
                crate::probe::census(&format!("story id={story} quests={}", list.join(",")));
                self.chests_for_session();
                self.quests.apply_story(story, &quests);
                self.publish_chest_marks();
            }

            // How the player moves, as the client says it to the server, several times a
            // second: the map follows that (the game's own report comes every 6 seconds).
            if command.from_client
                && self.motion_at.elapsed() >= Duration::from_millis(250)
                && let Some((scene, floor, range)) = self.motion_scene
                && let Some(motion) = guarded("avatar motion", || matches_avatar_motion(&command))
            {
                self.motion_at = Instant::now();
                self.chests.note_position(scene, motion.position);
                self.puzzles.player_at(scene, motion.position);
                let placed = self.scenes.map_of(scene).map(|scene_map| (scene_map.map, scene_map.to_map(motion.position), scene_map.s));
                self.player_reports += 1;
                self.app_state.update_player(crate::PlayerPlace {
                    scene,
                    position: motion.position,
                    floor,
                    map: placed.map_or(0, |placed| placed.0),
                    at: placed.map_or([0., 0.], |placed| placed.1),
                    scale: placed.map_or(1., |placed| placed.2),
                    yaw: motion.yaw,
                    range,
                    count: self.player_reports,
                });
            }

            // The account, if the login did not say which it is: the game says it
            // beside the name of the player.
            if self.session_uid.is_none()
                && let Some(name) = self.session_nickname.clone()
                && let Some(uid) = guarded("uid by name", || matches_uid_of_player(&command, &name))
            {
                self.learned_uid(uid);
            }

            // Where the player is, said every few seconds: the way walked tells
            // what the game has sent, and the map follows the player.
            let expected = self.session_uid;
            let location = guarded("player location", || matches_location_of_player(&command, expected));
            if location.is_none() && command.command_id == 20584 && !self.location_dumped {
                // The message that says where the player is came, and could not be
                // read: its make-up is kept, so that it can be looked at.
                self.location_dumped = true;
                let bytes: Vec<String> = command.proto_data.iter().take(160).map(|b| format!("{b:02x}")).collect();
                self.record(format!(
                    "player location (command 20584) not read; {} bytes: {}",
                    command.proto_data.len(),
                    bytes.join("")
                ));
            }
            if let Some(location) = location {
                if self.session_uid.is_none() {
                    // The first place told after the login is the player's own.
                    self.learned_uid(location.uid);
                }
                // The place is of the account's player.  If the game puts another id on
                // it, and it is always that one, it is the account's all the same: the
                // place is followed whatever the id is.
                if self.session_uid == Some(location.uid) {
                    self.location_matched = true;
                }
                if self.session_uid != Some(location.uid) && self.location_uid.is_none() && !self.location_matched {
                    let count = match self.foreign_location {
                        Some((uid, count)) if uid == location.uid => count + 1,
                        _ => 1,
                    };
                    self.foreign_location = Some((location.uid, count));
                    if count >= 3 {
                        self.location_uid = Some(location.uid);
                        self.record(format!(
                            "player place is told with id {}, not the account's {:?}: followed all the same",
                            location.uid, self.session_uid
                        ));
                    }
                }
                if !self.location_said {
                    self.location_said = true;
                    self.record(format!(
                        "player place read: scene {}, id {}, account {:?}",
                        location.scene, location.uid, self.session_uid
                    ));
                }
                if self.session_uid == Some(location.uid) || self.location_uid == Some(location.uid) {
                    self.chests_for_session();
                    let changed = self.chests.player_at(location.scene, location.position);
                    self.player_reports += 1;
                    let (placed, said) = self.scenes.locate(location.scene, location.position);
                    if let Some(text) = said {
                        self.record(text);
                    }
                    // The floor of the map the player is on (the surface is 0).
                    let floor = match (placed, self.scenes.map_of(location.scene)) {
                        (Some(placed), Some(scene_map)) => {
                            // The teleports gone to in this scene before are anchors.
                            let places = self.scenes.teleport_places(location.scene);
                            self.floors.seed_anchors(scene_map, &places);
                            let floor = self.floors.locate(placed.map, scene_map, location.position);
                            // A chest marker walked by with no chest sent is opened.
                            self.watch.player_at(location.scene, scene_map, location.position, floor);
                            self.objects.player_at(location.scene, scene_map, location.position, floor);
                            self.puzzles.report(location.scene, scene_map, location.position, floor);
                            self.publish_chest_marks();
                            floor
                        }
                        _ => 0,
                    };
                    crate::probe::track(&format!(
                        "scene={} pos={:.1},{:.1},{:.1} yaw={} floor={floor} map={}",
                        location.scene,
                        location.position[0],
                        location.position[1],
                        location.position[2],
                        location.yaw.map_or("-".to_owned(), |yaw| format!("{yaw:.0}")),
                        placed.map_or(0, |placed| placed.map),
                    ));
                    self.motion_scene = Some((
                        location.scene,
                        floor,
                        if placed.is_some_and(|placed| placed.map != crate::atlas::WORLD) {
                            self.watch.range(location.scene)
                        } else {
                            crate::chest_watch::radar()
                        },
                    ));
                    self.app_state.update_player(crate::PlayerPlace {
                        scene: location.scene,
                        position: location.position,
                        floor,
                        map: placed.map_or(0, |placed| placed.map),
                        at: placed.map_or([0., 0.], |placed| placed.at),
                        scale: placed.map_or(1., |placed| placed.scale),
                        yaw: location.yaw,
                        range: if placed.is_some_and(|placed| placed.map != crate::atlas::WORLD) {
                            self.watch.range(location.scene)
                        } else {
                            crate::chest_watch::radar()
                        },
                        count: self.player_reports,
                    });
                    if changed {
                        self.publish_chests();
                    }
                }
            }

            // A teleport: to which point of which scene, and where the player comes in.
            if let Some((scene, point)) = guarded("teleport target", || matches_teleport_target(&command)) {
                self.scenes.note_teleport(scene, point);
            }
            if let Some((scene, position)) = guarded("scene arrival", || matches_scene_arrival(&command))
                && let Some(text) = self.scenes.note_arrival(scene, position)
            {
                self.record(text);
                // The movement the client says is of the new place, which is not told yet.
                self.motion_scene = None;
                // A teleport: the floor of the waypoint is where the player is.
                if let Some(scene_map) = self.scenes.map_of(scene) {
                    self.floors.note_arrival(scene_map, position);
                    self.objects.entered(scene, position);
                    self.puzzles.entered(scene, position);

                }
            }

            // Chests that came into view (closed ones), and chests opened.
            if let Some(gadgets) = guarded("gadgets in view", || matches_gadgets_in_view(&command)) {
                self.chests_in_view(&gadgets);
            }
            if let Some(event) = guarded("gadget use", || matches_gadget_use(&command)) {
                self.chests_for_session();
                if let Some(opened) = self.chests.interaction(&event) {
                    self.chest_opened(opened);
                }
                self.watch.interaction(&event);
                // A chest opened that is of a group of a puzzle finishes it.
                if event.kind == 3 && !event.begins {
                    self.puzzles.chest_opened(event.entity_id);
                }
                self.publish_chest_marks();
            }
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
                    // Rewards are the proof that a chest that began to open did.
                    if let Some(opened) = self.chests.rewards() {
                        self.chest_opened(opened);
                    }
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
            // The first of a kind (a flower picked for the first time) is a stack
            // that is new: the message is as strict as for the others, so it is
            // taken as well.
            if let Some(updates) = guarded("item changes", || matches_item_changes(&command)) {
                let applied = self.player_data.apply_item_changes(&updates);
                let changes = applied.stacks;
                let weapons = applied.weapons;
                if !changes.is_empty() || !weapons.is_empty() {
                    self.switch_to_session_account();
                    let weapon_ids: Vec<u32> = weapons.iter().map(|weapon| weapon.item_id).collect();
                    self.record(format!("FOUND item changes: {changes:?} weapons {weapon_ids:?}"));
                    self.unsaved = true;
                    has_new_data = true;
                    if let Some(opened) = self.chests.rewards() {
                        self.chest_opened(opened);
                    }
                    // Something picked up by a marker of its name: that marker is picked.
                    if self.captured.items
                        && let Some((scene, floor, _)) = self.motion_scene
                        && let Some(scene_map) = self.scenes.map_of(scene)
                        && let Some(position) = self.puzzles.position(scene)
                    {
                        for (id, delta) in &changes {
                            if (1..=3).contains(delta)
                                && let Some(item) = crate::live_data::item(*id)
                            {
                                self.puzzles.picked_up(&item.name.en, scene_map, position, floor);
                            }
                        }
                        self.publish_chest_marks();
                    }
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

/// Where the session key of the connection is kept.
/// Whether the marks of the oculi, seelies, puzzles, gathering and quests are given to the
/// maps.  Off: only the chests are (they are what works well).
const EXTRA_MARKS: bool = false;

fn session_key_path() -> Option<std::path::PathBuf> {
    eframe::storage_dir(crate::APP_ID).map(|dir| dir.join("session_key.bin"))
}

fn load_session_key() -> Option<Vec<u8>> {
    let key = std::fs::read(session_key_path()?).ok()?;
    (!key.is_empty()).then_some(key)
}

fn save_session_key(key: &[u8]) {
    if let Some(path) = session_key_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, key);
    }
}
