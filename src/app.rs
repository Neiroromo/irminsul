use std::fmt::Display;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::thread;

use anyhow::{Context as _, Result, anyhow};
use chrono::Local;
use egui::{
    Button, Color32, Context, DragValue, Frame, Id, Key, KeyboardShortcut, Margin, Modal,
    Modifiers, PointerButton, RichText, Sense, ViewportBuilder, ViewportCommand, ViewportId,
};
use egui_file_dialog::{DialogState, FileDialog};
use egui_notify::Toasts;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::achievement_tab::{
    AchievementProgress, AchievementTab, TAB_BACKGROUND,
};
use crate::autostart;
use crate::capture_popup::CapturePopup;
use crate::changes_popup::ChangesPopup;
use crate::probe;
use crate::monitor::Monitor;
use crate::profiles::{self, ProfileSummary};
use crate::roster_tab::RosterTab;
use crate::tray::{Tray, TrayCommand};
use crate::window_visibility;
use crate::player_data::ExportSettings;
use crate::update::check_for_app_update;
use crate::{
    AppState, ConfirmationType, Message, ReloadHandle, State, TracingLevel, admin, capture,
    open_log_dir, wish,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SavedAppState {
    export_settings: ExportSettings,
    #[serde(default)]
    auto_start_capture: bool,
    /// Closing the window hides it in the tray instead of quitting.
    #[serde(default)]
    close_to_tray: bool,
    /// Show the progress popup when the game connects and loads data.
    #[serde(default = "default_true")]
    show_capture_popup: bool,
    /// Show what changed (mora, items) as it happens.
    #[serde(default = "default_true")]
    show_changes_popup: bool,
    /// Numbers the traffic probe looks for.
    #[serde(default)]
    probe_needles: String,
    /// Keep the Power Tools window over all others.
    #[serde(default)]
    power_tools_on_top: bool,
    /// HoYoPlay's launcher program, if HoYoPlay is not where it is installed by
    /// default.
    #[serde(default)]
    hoyoplay_dir: String,
    /// Hide the window in the tray when the game is started from here.
    #[serde(default)]
    minimize_on_launch: bool,
    /// Keep HoYoPlay's window out of sight (in its tray) when it is started from here.
    #[serde(default = "yes")]
    hide_launcher: bool,
    log_raw_packets: bool,
    #[serde(default)]
    tracing_level: TracingLevel,
    #[serde(default)]
    achievement_progress: AchievementProgress,
    /// The account whose data the exports used last.
    #[serde(default)]
    active_profile: Option<u32>,
}

fn default_true() -> bool {
    true
}

fn window_level(on_top: bool) -> egui::viewport::WindowLevel {
    if on_top {
        egui::viewport::WindowLevel::AlwaysOnTop
    } else {
        egui::viewport::WindowLevel::Normal
    }
}

impl Default for SavedAppState {
    fn default() -> Self {
        Self {
            export_settings: ExportSettings {
                include_characters: true,
                include_artifacts: true,
                include_weapons: true,
                include_materials: true,
                fake_initialize_4th_line: false,
                min_character_level: 1,
                min_character_ascension: 0,
                min_character_constellation: 0,
                min_artifact_level: 0,
                min_artifact_rarity: 5,
                min_weapon_level: 1,
                min_weapon_refinement: 0,
                min_weapon_ascension: 0,
                min_weapon_rarity: 3,
            },
            auto_start_capture: false,
            close_to_tray: false,
            show_capture_popup: true,
            show_changes_popup: true,
            probe_needles: String::new(),
            power_tools_on_top: false,
            hoyoplay_dir: String::new(),
            minimize_on_launch: false,
            hide_launcher: true,
            log_raw_packets: false,
            tracing_level: Default::default(),
            achievement_progress: Default::default(),
            active_profile: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainTab {
    Export,
    Achievements,
    Characters,
    Materials,
    Planner,
    Chests,
    Map,
    Events,
}

#[derive(Clone, Debug)]
enum OptimizerExportTarget {
    None,
    Clipboard,
    File,
}

pub struct IrminsulApp {
    ui_message_tx: mpsc::UnboundedSender<Message>,
    state_rx: watch::Receiver<AppState>,
    wish_url_rx: watch::Receiver<Option<String>>,
    log_packets_tx: watch::Sender<bool>,
    tracing_reload_handle: ReloadHandle,

    toasts: Toasts,

    power_tools_open: bool,
    /// What to write with the mark of the background log.
    background_log_note: String,

    capture_settings_open: bool,

    optimizer_settings_open: bool,
    optimizer_export_rx: Option<oneshot::Receiver<Result<String>>>,
    optimizer_save_dialog: Option<FileDialog>,
    optimizer_save_path: Option<PathBuf>,
    optimizer_export_target: OptimizerExportTarget,

    achievement_export_rx: Option<oneshot::Receiver<Result<String>>>,
    achievement_save_dialog: Option<FileDialog>,
    achievement_save_path: Option<PathBuf>,
    achievement_export_target: OptimizerExportTarget,

    main_tab: MainTab,
    achievement_tab: AchievementTab,
    roster_tab: RosterTab,
    chests_tab: crate::chests_tab::ChestsTab,
    events_tab: crate::events_tab::EventsTab,
    map_tab: crate::map_tab::MapTab,

    restarting: bool,
    /// Set when quitting for real, as opposed to hiding in the tray.
    quitting: bool,
    tray: Option<Tray>,
    /// Frames left to hide the window in, when it was asked to start hidden.
    hide_after_start: u8,
    autostart_enabled: bool,
    capture_popup: CapturePopup,
    changes_popup: ChangesPopup,
    /// Picking the launcher's program.
    launcher_dialog: Option<FileDialog>,
    /// Whether the game is running, and when that was looked at.
    game_running: bool,
    game_checked: Option<std::time::Instant>,
    /// How starting the game through HoYoPlay is going.
    launch_status: crate::game_launch::SharedStatus,
    /// What is running and is to be closed before the game is started, and where
    /// HoYoPlay is, while the user is asked.
    launch_confirm: Option<(PathBuf, Vec<crate::game_launch::ProcessInfo>)>,

    /// The account of the data in use, as last seen, to notice a change.
    last_profile_uid: Option<u32>,
    profiles_open: bool,
    profile_list: Vec<ProfileSummary>,
    profiles_seen_version: Option<u64>,
    profile_delete_confirm: Option<u32>,

    saved_state: SavedAppState,
}

trait ToastError<T> {
    fn toast_error(self, app: &mut IrminsulApp) -> Option<T>;
}

impl<T, E: Display> ToastError<T> for std::result::Result<T, E> {
    fn toast_error(self, app: &mut IrminsulApp) -> Option<T> {
        match self {
            Ok(val) => Some(val),
            Err(e) => {
                tracing::error!("{e}");
                app.toasts.error(e.to_string());
                None
            }
        }
    }
}

fn start_async_runtime(
    egui_ctx: Context,
    log_packets_rx: watch::Receiver<bool>,
    capture_backend: capture::BackendType,
) -> (
    mpsc::UnboundedSender<Message>,
    watch::Receiver<AppState>,
    watch::Receiver<Option<String>>,
) {
    tracing::info!("starting tokio async");
    let (ui_message_tx, mut ui_message_rx) = mpsc::unbounded_channel::<Message>();

    let (state_tx, state_rx) = watch::channel(AppState::new());
    let (wish_url_tx, wish_url_rx) = watch::channel(None);
    let mut updater_state_rx = state_rx.clone();
    let updater_ctx = egui_ctx.clone();
    thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            // Before starting the monitor, check for updates if not in debug mode
            tracing::info!("Checking for update");
            if let Err(e) = check_for_app_update(&state_tx, &mut ui_message_rx).await {
                tracing::error!("error checking for update: {e}");
            }

            // Check for wish URL
            tokio::spawn(async move {
                let Ok(mut wish) = wish::Wish::new(wish_url_tx).await else {
                    tracing::error!("Failed to create new wish monitor");
                    return;
                };

                if let Err(e) = wish.monitor().await {
                    tracing::error!("Error monitoring for wishes: {e}");
                }
            });

            // Notify egui of state changes.
            tokio::spawn(async move {
                loop {
                    let _ = updater_state_rx.changed().await;
                    updater_ctx.request_repaint();
                }
            });
            tracing::info!("Starting monitor");
            let monitor = match Monitor::new(
                state_tx,
                ui_message_rx,
                log_packets_rx,
                capture_backend,
            )
            .await
            {
                Ok(monitor) => monitor,
                Err(e) => {
                    tracing::error!("error loading monitor task: {e}");
                    return;
                }
            };
            monitor.run().await;
        });
    });
    tracing::info!("started tokio");
    (ui_message_tx, state_rx, wish_url_rx)
}

impl IrminsulApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        mut tracing_reload_handle: ReloadHandle,
        capture_backend: capture::BackendType,
        minimized: bool,
        preview_popup: bool,
    ) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        egui_material_icons::initialize(&cc.egui_ctx);

        let saved_state: SavedAppState = if let Some(storage) = cc.storage {
            eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default()
        } else {
            Default::default()
        };

        tracing_reload_handle.set_filter(saved_state.tracing_level.get_filter());
        let (log_packets_tx, log_packets_rx) = watch::channel(saved_state.log_raw_packets);
        probe::set_needles(&saved_state.probe_needles);
        let (ui_message_tx, state_rx, wish_url_rx) =
            start_async_runtime(cc.egui_ctx.clone(), log_packets_rx, capture_backend);

        // Started minimized, there is nobody to press the button.
        if saved_state.auto_start_capture || minimized {
            if let Err(e) = ui_message_tx.send(Message::StartCapture) {
                tracing::error!("Failed to send auto start message: {e}");
            }
        }

        let toasts = Toasts::default().with_anchor(egui_notify::Anchor::BottomLeft);

        // Bring back the account that was in use last.
        let uid = std::env::var("IRMINSUL_SELECT_PROFILE")
            .ok()
            .and_then(|uid| uid.parse().ok())
            .or(saved_state.active_profile);
        if let Some(uid) = uid {
            let _ = ui_message_tx.send(Message::SelectProfile(Some(uid)));
        }

        let mut capture_popup = CapturePopup::new();
        if preview_popup {
            capture_popup.start_preview(false);
        }

        Self {
            saved_state,
            ui_message_tx,
            log_packets_tx,
            tracing_reload_handle,
            toasts,
            power_tools_open: false,
            background_log_note: String::new(),
            capture_settings_open: false,
            optimizer_settings_open: false,
            optimizer_export_rx: None,
            optimizer_save_dialog: None,
            optimizer_save_path: None,
            optimizer_export_target: OptimizerExportTarget::None,
            achievement_export_rx: None,
            achievement_save_dialog: None,
            achievement_save_path: None,
            achievement_export_target: OptimizerExportTarget::None,
            // For looking at a tab without a click.
            main_tab: match std::env::var("IRMINSUL_START_TAB").as_deref() {
                Ok("achievements") => MainTab::Achievements,
                Ok("characters") => MainTab::Characters,
                Ok("materials") => MainTab::Materials,
                Ok("planner") => MainTab::Planner,
                Ok("chests") => MainTab::Chests,
                Ok("map") => MainTab::Map,
                Ok("events") => MainTab::Events,
                _ => MainTab::Export,
            },
            achievement_tab: AchievementTab::new(),
            roster_tab: RosterTab::new(&cc.egui_ctx),
            chests_tab: crate::chests_tab::ChestsTab::new(),
            events_tab: crate::events_tab::EventsTab::new(&cc.egui_ctx),
            map_tab: crate::map_tab::MapTab::new(&cc.egui_ctx),
            restarting: false,
            quitting: false,
            tray: Tray::new(&cc.egui_ctx),
            hide_after_start: if minimized { 3 } else { 0 },
            autostart_enabled: autostart::is_enabled(),
            capture_popup,
            changes_popup: ChangesPopup::new(),
            launcher_dialog: None,
            game_running: false,
            game_checked: None,
            launch_status: Default::default(),
            launch_confirm: None,
            last_profile_uid: None,
            // For looking at the accounts window without a click.
            profiles_open: std::env::var_os("IRMINSUL_OPEN_PROFILES").is_some(),
            profile_list: Vec::new(),
            profiles_seen_version: None,
            profile_delete_confirm: None,
            state_rx,
            wish_url_rx,
        }
    }
}

impl eframe::App for IrminsulApp {
    /// Called by the framework to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.saved_state);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.style_mut(|style| {
            style.interaction.selectable_labels = false;
            style.interaction.tooltip_delay = 0.25;
        });

        self.handle_tray(ctx);
        self.follow_profile();
        self.toasts.show(ctx);
        if let Some(optimizer_save_dialog) = &mut self.optimizer_save_dialog {
            optimizer_save_dialog.update(ctx);
        }
        if let Some(launcher_dialog) = &mut self.launcher_dialog {
            launcher_dialog.update(ctx);
        }
        if let Some(achievement_save_dialog) = &mut self.achievement_save_dialog {
            achievement_save_dialog.update(ctx);
        }

        // The tabs other than the export tab draw over a plain background.
        let tracker_open = matches!(self.state_rx.borrow().state, State::Main)
            && self.main_tab != MainTab::Export;

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                if tracker_open {
                    ui.painter()
                        .rect_filled(ui.ctx().screen_rect(), 0., TAB_BACKGROUND);
                } else {
                    egui::Image::new(egui::include_image!("../assets/background.webp"))
                        .paint_at(ui, ui.ctx().screen_rect());
                }
            });

            ui.vertical(|ui| {
                self.title_bar(ui);
                ui.add_space(25.);

                // Handle power tools here instead of main UI to allow it to be opened
                // in other app states.
                let power_tools_shortcut = KeyboardShortcut {
                    modifiers: Modifiers {
                        command: true,
                        shift: true,
                        ..Default::default()
                    },
                    logical_key: Key::P,
                };
                ui.ctx().input_mut(|i| {
                    if i.consume_shortcut(&power_tools_shortcut) {
                        self.power_tools_open = true;
                    }
                });

                // The accounts, which are opened from the title bar, on any tab.
                if self.profiles_open {
                    let state = self.state_rx.borrow().clone();
                    let modal = Modal::new(Id::new("Accounts")).show(ui.ctx(), |ui| {
                        self.profiles_modal(ui, &state);
                    });
                    if modal.should_close() {
                        self.profiles_open = false;
                        self.profile_delete_confirm = None;
                    }
                }

                if tracker_open {
                    // "Show on the map" of a material: the map is opened with where it is got.
                    if let Some(request) = crate::map_tab::take_request() {
                        let russian = self.saved_state.achievement_progress.language == crate::achievement_data::Language::Ru;
                        self.map_tab.show_only(request, russian);
                        self.main_tab = MainTab::Map;
                    }
                    // "Add to the route" of a material.
                    self.map_tab.route_requests(crate::map_tab::take_route_requests());
                    self.map_tab.route_mirror();
                    let state = self.state_rx.borrow_and_update().clone();
                    Frame::new()
                        .inner_margin(Margin::symmetric(20, 0))
                        .show(ui, |ui| match self.main_tab {
                            MainTab::Characters => self.roster_tab.characters_ui(
                                ui,
                                &state.roster,
                                &mut self.saved_state.achievement_progress.language,
                            ),
                            MainTab::Planner => self.roster_tab.planner_ui(
                                ui,
                                &state.roster,
                                &mut self.saved_state.achievement_progress.language,
                            ),
                            MainTab::Materials => self.roster_tab.materials_ui(
                                ui,
                                &state.roster,
                                &mut self.saved_state.achievement_progress.language,
                            ),
                            MainTab::Events => self.events_tab.ui(
                                ui,
                                &mut self.saved_state.achievement_progress.language,
                            ),
                            MainTab::Chests => self.chests_tab.ui(
                                ui,
                                &state.chests,
                                &mut self.saved_state.achievement_progress.language,
                            ),
                            MainTab::Map => self.map_tab.ui(
                                ui,
                                &state.chests,
                                &state.chest_marks,
                                state.player,
                                &mut self.saved_state.achievement_progress.language,
                            ),
                            _ => self.achievement_tab.ui(
                                ui,
                                &mut self.saved_state.achievement_progress,
                                state.updated.achievements_updated.is_some(),
                                state.profile.as_ref(),
                                &self.ui_message_tx,
                                &mut self.toasts,
                            ),
                        });
                    return;
                }

                ui.horizontal(|ui| {
                    // Where the controls go on the artwork, which is as wide as
                    // the window; the artwork was drawn for a window 800 wide.
                    ui.add_space(525. / 800. * ui.ctx().screen_rect().width());
                    let state = self.state_rx.borrow_and_update().clone();
                    ui.vertical(|ui| match state.state {
                        State::Starting => (),
                        State::CheckingForUpdate => self.checking_for_update_ui(ui),
                        State::WaitingForUpdateConfirmation(status) => {
                            self.waiting_for_update_confirmation_ui(ui, status)
                        }
                        State::Updating => self.updating_ui(ui),
                        State::Updated { needs_caps } => self.updated_ui(ui, needs_caps),
                        State::CheckingForData => self.checking_for_data_ui(ui),
                        State::WaitingForDownloadConfirmation(confirmation_type) => {
                            self.waiting_for_download_confirmation_ui(ui, confirmation_type)
                        }
                        State::Downloading => self.load_data_ui(ui),
                        State::Main => self.main_ui(ui, &state),
                    });
                });
            });
        });

        // Power Tools is a window of its own, to be put anywhere on the desktop,
        // next to the game.
        if self.power_tools_open {
            let mut close = false;
            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("power_tools_window"),
                {
                    let builder = ViewportBuilder::default()
                        .with_title("Irminsul Power Tools")
                        .with_inner_size([560., 760.])
                        .with_min_inner_size([420., 320.]);
                    if self.saved_state.power_tools_on_top {
                        builder.with_always_on_top()
                    } else {
                        builder
                    }
                },
                |ctx, _class| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.power_tools_modal(ui));
                    });
                    close = ctx.input(|input| input.viewport().close_requested());
                },
            );
            if close {
                self.power_tools_open = false;
            }
        }
        let progress = self.state_rx.borrow().capture_progress.clone();
        let updated = self.state_rx.borrow().updated.clone();
        self.capture_popup.update(
            ctx,
            &progress,
            &updated,
            self.saved_state.achievement_progress.language,
            self.saved_state.show_capture_popup,
        );
        let roster = self.state_rx.borrow().roster.clone();
        self.roster_tab.overlay_ui(
            ctx,
            &roster,
            self.saved_state.achievement_progress.language,
        );
        {
            let (chests, marks, player, key_lost, mobs) = {
                let state = self.state_rx.borrow();
                (state.chests.clone(), state.chest_marks.clone(), state.player, state.capture_progress.failed, state.mobs.clone())
            };
            self.map_tab.overlay_frame(
                ctx,
                &chests,
                &marks,
                player,
                self.saved_state.achievement_progress.language,
                key_lost,
                &mobs,
            );
        }
        let changes = self.state_rx.borrow().changes.clone();
        self.changes_popup.update(
            ctx,
            &changes,
            self.roster_tab.icons(),
            self.saved_state.achievement_progress.language,
            self.saved_state.show_changes_popup,
        );
    }
}

impl IrminsulApp {
    /// The data in use came from another account: show the marks of that one.
    fn follow_profile(&mut self) {
        let uid = self.state_rx.borrow().profile.as_ref().map(|profile| profile.uid);
        if uid != self.last_profile_uid {
            self.last_profile_uid = uid;
            self.saved_state.active_profile = uid;
            self.saved_state
                .achievement_progress
                .switch_profile(uid.unwrap_or(0));
        }
    }

    /// Part of the names of exported files: the account they are of.
    fn file_tag(app_state: &AppState) -> String {
        app_state
            .profile
            .as_ref()
            .map(|profile| format!("{}_", profile.uid))
            .unwrap_or_default()
    }

    fn hides_to_tray(&self) -> bool {
        self.tray.is_some() && self.saved_state.close_to_tray
    }

    fn set_window_visible(&mut self, visible: bool) {
        self.hide_after_start = 0;
        window_visibility::set_visible(visible);
    }

    /// Closing the window: quit, or hide in the tray where it keeps capturing.
    fn close_window(&mut self, ctx: &egui::Context) {
        if self.hides_to_tray() {
            self.set_window_visible(false);
        } else {
            self.quitting = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        // eframe shows the window after its first frame even if it was asked to
        // start hidden, and sets its style while at it, so when started in the
        // tray it is hidden a few times over.
        if self.hide_after_start > 0 {
            self.hide_after_start -= 1;
            window_visibility::set_visible(false);
        }

        let commands = self.tray.as_ref().map(Tray::commands).unwrap_or_default();
        for command in commands {
            match command {
                TrayCommand::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        // Closing by other means than the close button (Alt+F4, the taskbar).
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested && !self.quitting && !self.restarting && self.hides_to_tray() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.set_window_visible(false);
        }

        // A hidden window can't be answered, so bring it up for the questions
        // that block everything else.
        let needs_answer = matches!(
            self.state_rx.borrow().state,
            State::WaitingForUpdateConfirmation(_)
                | State::WaitingForDownloadConfirmation(_)
                | State::Updated { needs_caps: true }
        );
        if needs_answer && !window_visibility::is_visible() {
            self.set_window_visible(true);
        }
    }

    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let state = self.state_rx.borrow().clone();
        let in_main = matches!(state.state, State::Main);
        if self.profiles_seen_version != Some(state.profiles_version) {
            self.profiles_seen_version = Some(state.profiles_version);
            self.profile_list = profiles::list();
        }
        let active = state.profile.clone();
        let known = self.profile_list.clone();
        let mut close_clicked = false;
        let (tabs_width, button_width) = egui::Sides::new().show(
            ui,
            |ui| {
                if in_main {
                    ui.add_space(8.);
                    for (tab, label) in [
                        (MainTab::Export, "Export"),
                        (MainTab::Achievements, "Achievements"),
                        (MainTab::Characters, "Characters"),
                        (MainTab::Materials, "Materials"),
                        (MainTab::Planner, "Planner"),
                        (MainTab::Map, "Map"),
                        (MainTab::Events, "Events"),
                    ] {
                        ui.selectable_value(
                            &mut self.main_tab,
                            tab,
                            RichText::new(label).size(16.),
                        );
                    }
                }
                ui.min_rect().width()
            },
            |ui| {
                let button = ui.add(
                    Button::new(RichText::new(egui_material_icons::icons::ICON_CLOSE).size(24.))
                        .frame(false),
                );
                if button.clicked() {
                    close_clicked = true;
                }

                // The account, by the close button: the name and the UID, which
                // copy the UID when clicked, and an arrow that opens the accounts
                // there are, to go from one to another.
                if in_main {
                    let arrow = ui
                        .add(
                            Button::new(
                                RichText::new(egui_material_icons::icons::ICON_KEYBOARD_ARROW_DOWN)
                                    .size(22.),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Accounts");
                    egui::Popup::menu(&arrow).show(|ui| {
                        ui.set_min_width(260.);
                        if known.is_empty() {
                            ui.label(
                                RichText::new("No account yet: log in to the game while capture is running")
                                    .size(13.),
                            );
                        }
                        for profile in &known {
                            let name = match &profile.nickname {
                                Some(name) => format!("{name} ({})", profile.uid),
                                None => profile.uid.to_string(),
                            };
                            let selected = active.as_ref().map(|a| a.uid) == Some(profile.uid);
                            if ui
                                .selectable_label(selected, RichText::new(name).size(15.))
                                .clicked()
                                && !selected
                            {
                                let _ = self
                                    .ui_message_tx
                                    .send(Message::SelectProfile(Some(profile.uid)));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Manage accounts...").clicked() {
                            self.profiles_open = true;
                            self.profiles_seen_version = None;
                            ui.close();
                        }
                    });

                    match &active {
                        Some(profile) => {
                            let uid = profile.uid.to_string();
                            let mut copy = false;
                            // Right to left: the UID is next to the arrow, the name
                            // before it.
                            copy |= ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(&uid)
                                            .size(15.)
                                            .color(Color32::from_gray(0xc0)),
                                    )
                                    .sense(Sense::click()),
                                )
                                .on_hover_text("Click to copy the UID")
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked();
                            if let Some(name) = &profile.nickname {
                                copy |= ui
                                    .add(
                                        egui::Label::new(RichText::new(name).size(15.).strong())
                                            .sense(Sense::click()),
                                    )
                                    .on_hover_text("Click to copy the UID")
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked();
                            }
                            if copy {
                                ui.ctx().copy_text(uid);
                                self.toasts.info("UID copied");
                            }
                        }
                        None => {
                            ui.label(RichText::new("No account yet").size(14.).color(Color32::from_gray(0x90)));
                        }
                    }
                }
                ui.min_rect().width()
            },
        );
        if close_clicked {
            self.close_window(ui.ctx());
        }

        let app_rect = ui.max_rect();

        let title_bar_height = 32.0;
        let title_bar_rect = {
            let mut rect = app_rect;
            rect.max.y = rect.min.y + title_bar_height;
            rect.min.x += tabs_width;
            rect.max.x -= button_width;
            rect
        };

        let response = ui.interact(
            title_bar_rect,
            Id::new("title_bar"),
            Sense::click_and_drag(),
        );

        if response.drag_started_by(PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
    }

    fn checking_for_update_ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Checking for Irminsul updates".to_string());
        });
    }

    fn waiting_for_update_confirmation_ui(&self, ui: &mut egui::Ui, version: String) {
        ui.label(format!(
            "Update {} available.  Download and install?",
            version
        ));

        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("Yes")).clicked() {
                if let Err(e) = self.ui_message_tx.send(Message::UpdateAcknowledged) {
                    tracing::error!("Unable to send UI message: {e}");
                }
            }
            if ui.add(egui::Button::new("No")).clicked() {
                if let Err(e) = self.ui_message_tx.send(Message::UpdateCanceled) {
                    tracing::error!("Unable to send UI message: {e}");
                }
            }
        });
    }

    fn updating_ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Downloading and updating...".to_string());
            ui.spinner();
        });
    }

    /// Relaunch Irminsul and close this instance.
    ///
    /// Uses the executable's own path rather than argv[0], which may be a bare
    /// name that only resolves through PATH, or be relative to a directory we
    /// are no longer in.
    fn restart(&mut self, ui: &mut egui::Ui) {
        match std::env::current_exe() {
            Ok(exe_path) => {
                let _ = std::process::Command::new(exe_path).spawn();
            }
            Err(e) => tracing::error!("unable to find Irminsul to restart it: {e}"),
        }
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        self.restarting = true;
        self.quitting = true;
    }

    fn updated_ui(&mut self, ui: &mut egui::Ui, needs_caps: bool) {
        if needs_caps {
            return self.update_reset_permissions_ui(ui);
        }

        ui.horizontal(|ui| {
            ui.label("Updated. Restarting...".to_string());
        });
        if !self.restarting {
            self.restart(ui);
        }
    }

    /// Shown when the update dropped the packet capture permissions the user
    /// had granted.  Restarting without re-granting them just lands in the
    /// missing permissions dialog, so ask here instead of restarting straight
    /// away.
    fn update_reset_permissions_ui(&mut self, ui: &mut egui::Ui) {
        let exe_path = admin::current_exe_path();
        let command = admin::setcap_command(&exe_path);

        ui.label("Updated.  The update reset Irminsul's packet capture");
        ui.label("permissions.  Re-grant them with:");
        ui.add_space(5.0);
        ui.label(&command);
        ui.add_space(5.0);

        ui.horizontal(|ui| {
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(command.clone());
            }
            if ui.button("Restart").clicked() && !self.restarting {
                self.restart(ui);
            }
        });
    }

    fn checking_for_data_ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Checking for game data updates".to_string());
        });
    }

    fn waiting_for_download_confirmation_ui(
        &self,
        ui: &mut egui::Ui,
        confirmation_type: ConfirmationType,
    ) {
        let label = match confirmation_type {
            ConfirmationType::Initial => "Irminsul needs to download initial data",
            ConfirmationType::Update => "New data available",
        };
        ui.label(label.to_string());
        if ui.add(egui::Button::new("Download")).clicked() {
            if let Err(e) = self.ui_message_tx.send(Message::DownloadAcknowledged) {
                tracing::error!("Unable to send UI message{e}");
            }
        }
    }

    fn load_data_ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Downloading Data".to_string());
            ui.spinner();
        });
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, app_state: &AppState) {
        // The program picked for the launcher; the dialog is done when it is not
        // open any more.
        if let Some(dialog) = &mut self.launcher_dialog {
            if let Some(path) = dialog.take_picked() {
                self.saved_state.hoyoplay_dir = path.to_string_lossy().into_owned();
            }
            if !matches!(dialog.state(), DialogState::Open) {
                self.launcher_dialog = None;
            }
        }
        // The settings wait while the dialog to pick a program is open, which
        // they would be in the way of.
        if self.capture_settings_open && self.launcher_dialog.is_none() {
            let modal = Modal::new(Id::new("Settings")).show(ui.ctx(), |ui| {
                self.capture_settings_modal(ui);
            });
            if modal.should_close() {
                self.capture_settings_open = false;
            }
        }

        if self.optimizer_settings_open {
            let modal = Modal::new(Id::new("Optimizer Settings")).show(ui.ctx(), |ui| {
                self.optimizer_settings_modal(ui);
            });
            if modal.should_close() {
                self.optimizer_settings_open = false;
            }
        }
        if let Some((dir, running)) = self.launch_confirm.clone() {
            let modal = Modal::new(Id::new("Launch confirm")).show(ui.ctx(), |ui| {
                ui.set_width(380.);
                ui.heading("Restart Genshin Impact");
                ui.separator();
                ui.label("These are running and are closed first:");
                for process in &running {
                    ui.label(format!("   {} ({})", process.name, process.pid));
                }
                ui.add_space(4.);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "What is not saved in the game is lost. Then HoYoPlay is started,                              and when it is up, the game.",
                        )
                        .size(12.),
                    )
                    .wrap(),
                );
                ui.add_space(6.);
                ui.horizontal(|ui| {
                    if ui.button("Close them and start").clicked() {
                        crate::game_launch::run(dir.clone(), self.launch_status.clone(), self.saved_state.hide_launcher);
                        self.launch_confirm = None;
                        self.minimize_for_launch(ui.ctx());
                    }
                    if ui.button("Cancel").clicked() {
                        self.launch_confirm = None;
                    }
                });
            });
            if modal.should_close() {
                self.launch_confirm = None;
            }
        }
        self.capture_ui(ui, app_state);
        ui.separator();
        self.genshin_optimizer_ui(ui, app_state);
        ui.separator();
        self.wish_ui(ui);
        ui.separator();
        self.achievement_ui(ui, app_state);
    }

    fn capture_ui(&mut self, ui: &mut egui::Ui, app_state: &AppState) {
        ui.vertical(|ui| {
            egui::Sides::new().show(
                ui,
                |ui| {
                    Self::section_header(ui, "Packet Capture");
                },
                |ui| {
                    if ui
                        .button(egui_material_icons::icons::ICON_SETTINGS)
                        .clicked()
                    {
                        self.capture_settings_open = true;
                    }

                    if app_state.capturing {
                        if ui.button(egui_material_icons::icons::ICON_PAUSE).clicked() {
                            let _ = self.ui_message_tx.send(Message::StopCapture);
                        }
                    } else if ui
                        .button(egui_material_icons::icons::ICON_PLAY_ARROW)
                        .clicked()
                    {
                        let _ = self.ui_message_tx.send(Message::StartCapture);
                    }
                },
            );
        });
        if let Some(error) = &app_state.capture_error {
            ui.add(
                egui::Label::new(
                    RichText::new(error)
                        .size(12.)
                        .color(Color32::from_rgb(0xff, 0x6b, 0x6b)),
                )
                .wrap(),
            );
        }

        // The game is started through HoYoPlay, so that the capture sees it.
        let status = self
            .launch_status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default();
        let busy = matches!(status, crate::game_launch::Status::Working(_));
        // Is the game running: looked at about once a second while this is shown.
        let looked = self
            .game_checked
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(1000));
        if !looked {
            self.game_running = crate::game_launch::game_running();
            self.game_checked = Some(std::time::Instant::now());
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(1000));
        let running = self.game_running;
        ui.checkbox(&mut self.saved_state.minimize_on_launch, "Minimize on launch")
            .on_hover_text("Hides this window in the tray when the game is started from here.");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(if running {
                        "Relaunch Genshin Impact"
                    } else {
                        "Launch Genshin Impact"
                    }),
                )
                .on_hover_text(
                    "Closes Genshin Impact and HoYoPlay if they are running, starts HoYoPlay,                      and when it is up, the game. The game started by itself is not seen by                      the capture.",
                )
                .clicked()
            {
                self.start_game_launch(ui.ctx());
            }
            match &status {
                crate::game_launch::Status::Working(text) => {
                    ui.add(egui::Spinner::new().size(14.));
                    ui.label(RichText::new(format!("{text}...")).size(12.));
                }
                // Not working and not failed: what the game is doing now.
                crate::game_launch::Status::Done(_) | crate::game_launch::Status::Idle => {
                    if running {
                        ui.label(
                            RichText::new("Genshin Impact is running")
                                .size(12.)
                                .color(Color32::from_rgb(0x4a, 0xde, 0x80)),
                        );
                    } else {
                        ui.label(
                            RichText::new("Genshin Impact is not running")
                                .size(12.)
                                .color(Color32::from_gray(0x90)),
                        );
                    }
                }
                crate::game_launch::Status::Failed(text) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .size(12.)
                                .color(Color32::from_rgb(0xff, 0x6b, 0x6b)),
                        )
                        .wrap(),
                    );
                }
            }
        });
        if busy {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    /// The button that starts the game: if something of it is running, the user is
    /// asked first.
    fn start_game_launch(&mut self, ctx: &egui::Context) {
        let Some(dir) = crate::game_launch::hoyoplay_dir(&self.saved_state.hoyoplay_dir) else {
            if let Ok(mut status) = self.launch_status.lock() {
                *status = crate::game_launch::Status::Failed(
                    "HoYoPlay was not found. Give its launcher in Settings.".to_owned(),
                );
            }
            return;
        };
        let running = crate::game_launch::find_running(&dir);
        if running.is_empty() {
            crate::game_launch::run(dir, self.launch_status.clone(), self.saved_state.hide_launcher);
            self.minimize_for_launch(ctx);
        } else {
            self.launch_confirm = Some((dir, running));
        }
    }

    /// Hides the window in the tray, if that is what is wanted when the game is
    /// started from here.
    fn minimize_for_launch(&mut self, ctx: &egui::Context) {
        if !self.saved_state.minimize_on_launch {
            return;
        }
        if self.tray.is_some() {
            self.set_window_visible(false);
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }

    /// The accounts captured so far, to choose the one to use.
    fn profiles_modal(&mut self, ui: &mut egui::Ui, app_state: &AppState) {
        ui.set_width(460.0);
        ui.heading("Accounts");
        ui.separator();

        if self.profiles_seen_version != Some(app_state.profiles_version) {
            self.profiles_seen_version = Some(app_state.profiles_version);
            self.profile_list = profiles::list();
        }
        let active = app_state.profile.as_ref().map(|profile| profile.uid);

        if self.profile_list.is_empty() {
            ui.label(
                "Nothing captured yet. Log in to the game while capture is running, and the \
                 account is stored here.",
            );
        }
        for profile in self.profile_list.clone() {
            ui.horizontal(|ui| {
                let name = match &profile.nickname {
                    Some(name) => format!("{name} ({})", profile.uid),
                    None => profile.uid.to_string(),
                };
                let selected = active == Some(profile.uid);
                if ui.radio(selected, RichText::new(name).size(15.)).clicked() && !selected {
                    let _ = self
                        .ui_message_tx
                        .send(Message::SelectProfile(Some(profile.uid)));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirming = self.profile_delete_confirm == Some(profile.uid);
                    let label = if confirming {
                        RichText::new("Delete?").color(Color32::from_rgb(0xff, 0x6b, 0x6b))
                    } else {
                        RichText::new(egui_material_icons::icons::ICON_DELETE)
                    };
                    if ui.button(label).on_hover_text("Delete the stored data").clicked() {
                        if confirming {
                            self.profile_delete_confirm = None;
                            let _ = self.ui_message_tx.send(Message::DeleteProfile(profile.uid));
                        } else {
                            self.profile_delete_confirm = Some(profile.uid);
                        }
                    }
                });
            });

            let updated = profile
                .updated_at
                .as_deref()
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                .map(|time| time.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "never".to_owned());
            ui.horizontal(|ui| {
                ui.add_space(24.);
                ui.label(
                    RichText::new(format!(
                        "{} items, {} characters, {} achievements; updated {updated}",
                        profile.items, profile.characters, profile.achievements
                    ))
                    .size(12.),
                );
            });
            ui.add_space(4.);
        }

        ui.separator();
        egui::Sides::new().show(
            ui,
            |ui| {
                if ui.button("Open folder").clicked() {
                    match profiles::profiles_dir() {
                        Ok(dir) => {
                            let _ = std::fs::create_dir_all(&dir);
                            if let Err(e) = open::that(dir) {
                                tracing::error!("unable to open the profiles folder: {e}");
                            }
                        }
                        Err(e) => tracing::error!("{e}"),
                    }
                }
            },
            |ui| {
                if ui.button("Ok").clicked() {
                    ui.close()
                }
            },
        );
    }

    fn genshin_optimizer_ui(&mut self, ui: &mut egui::Ui, app_state: &AppState) {
        self.optimizer_handle_export(ui).toast_error(self);

        ui.vertical(|ui| {
            egui::Sides::new().show(
                ui,
                |ui| {
                    Self::section_header(ui, "Genshin Optimizer");
                },
                |ui| {
                    if ui
                        .button(egui_material_icons::icons::ICON_SETTINGS)
                        .clicked()
                    {
                        self.optimizer_settings_open = true;
                    }

                    ui.add_enabled_ui(
                        app_state.updated.characters_updated.is_some()
                            && app_state.updated.items_updated.is_some()
                            && self.optimizer_export_rx.is_none(),
                        |ui| {
                            if ui
                                .button(egui_material_icons::icons::ICON_DOWNLOAD)
                                .clicked()
                            {
                                let now = Local::now();
                                let mut optimizer_save_dialog = FileDialog::new()
                                    .add_file_filter_extensions("JSON files", vec!["json"])
                                    .default_file_name(&format!(
                                        "genshin_export_{}{}.json",
                                        Self::file_tag(app_state),
                                        now.format("%Y-%m-%d_%H-%M")
                                    ));
                                optimizer_save_dialog.save_file();
                                self.optimizer_save_dialog = Some(optimizer_save_dialog);
                            }

                            if let Some(optimizer_save_dialog) = &mut self.optimizer_save_dialog
                                && let Some(path) = optimizer_save_dialog.take_picked()
                            {
                                self.optimizer_save_path = Some(path);
                                self.genshin_optimizer_request_export(OptimizerExportTarget::File);
                            }

                            if ui
                                .button(egui_material_icons::icons::ICON_CONTENT_PASTE_GO)
                                .clicked()
                            {
                                self.genshin_optimizer_request_export(
                                    OptimizerExportTarget::Clipboard,
                                );
                            }
                        },
                    );
                },
            );
        });
    }

    fn genshin_optimizer_request_export(&mut self, target: OptimizerExportTarget) {
        let (tx, rx) = oneshot::channel();
        let _ = self.ui_message_tx.send(Message::ExportGenshinOptimizer(
            self.saved_state.export_settings.clone(),
            tx,
        ));
        self.optimizer_export_target = target;
        self.optimizer_export_rx = Some(rx);
    }

    fn wish_ui(&mut self, ui: &mut egui::Ui) {
        self.optimizer_handle_export(ui).toast_error(self);

        let wish_url = self.wish_url_rx.borrow_and_update().clone();
        ui.vertical(|ui| {
            egui::Sides::new().show(
                ui,
                |ui| {
                    Self::section_header(ui, "Wish History");
                    ui.label(egui_material_icons::icons::ICON_HELP)
                        .on_hover_text("Click the Copy icon to copy the wish URL to the clipboard.  Paste this into paimon.moe using the Manual auto-import method.");
                },
                |ui| {
                    ui.add_enabled_ui(wish_url.is_some(), |ui| {
                        if ui
                            .button(egui_material_icons::icons::ICON_CONTENT_PASTE_GO)
                            .clicked()
                        {
                            if let Some(url) = wish_url {
                                ui.ctx().copy_text(url);
                            }
                        }
                    });
                },
            );
        });
    }

    fn power_tools_modal(&mut self, ui: &mut egui::Ui) {
        ui.heading("Power Tools");
        if ui
            .checkbox(&mut self.saved_state.power_tools_on_top, "Keep this window on top of all others")
            .changed()
        {
            ui.ctx().send_viewport_cmd(ViewportCommand::WindowLevel(window_level(
                self.saved_state.power_tools_on_top,
            )));
        }
        ui.separator();
        if ui
            .checkbox(&mut self.saved_state.log_raw_packets, "Log raw packets")
            .changed()
        {
            let _ = self.log_packets_tx.send(self.saved_state.log_raw_packets);
        };
        let prev_level = self.saved_state.tracing_level;
        egui::ComboBox::from_label("Logging Level")
            .selected_text(format!("{}", self.saved_state.tracing_level))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.saved_state.tracing_level,
                    TracingLevel::Default,
                    "Default",
                );
                ui.selectable_value(
                    &mut self.saved_state.tracing_level,
                    TracingLevel::VerboseInfo,
                    "Verbose Info",
                );
                ui.selectable_value(
                    &mut self.saved_state.tracing_level,
                    TracingLevel::VerboseDebug,
                    "Verbose Debug",
                );
                ui.selectable_value(
                    &mut self.saved_state.tracing_level,
                    TracingLevel::VerboseTrace,
                    "Verbose Trace",
                );
            });
        if prev_level != self.saved_state.tracing_level {
            self.tracing_reload_handle
                .set_filter(self.saved_state.tracing_level.get_filter());
        }
        ui.end_row();
        ui.separator();
        self.background_log_ui(ui);
        ui.separator();
        self.debug_timeline_ui(ui);
        ui.separator();
        egui::Sides::new().show(
            ui,
            |_ui| {},
            |ui| {
                if ui.button("Ok").clicked() {
                    ui.close()
                }
            },
        );
    }

    /// The background log of the run: whether it is written, and the button that keeps it.
    fn background_log_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Background log").size(16.));
        ui.label(
            RichText::new(
                "Everything the program says is written to a log of this run, and thrown away \
                 when the program closes. Press the button when something went wrong: the log \
                 is then kept, with a mark at that moment.",
            )
            .size(12.),
        );
        let mut on = crate::session_log::enabled();
        if ui
            .checkbox(&mut on, "Write the background log from the start of the program")
            .on_hover_text("Holds from the next start of the program")
            .changed()
        {
            crate::session_log::set_enabled(on);
        }
        match crate::session_log::status() {
            Some((path, kept)) => {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.background_log_note)
                            .hint_text("What went wrong (optional)")
                            .desired_width(260.),
                    );
                    if ui.button("Keep this log (mark a problem)").clicked() {
                        let note = self.background_log_note.trim();
                        crate::session_log::keep(if note.is_empty() { "marked by the user" } else { note });
                        self.background_log_note.clear();
                    }
                });
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                if kept {
                    ui.label(RichText::new(format!("{name}: marked, it will be kept")).color(Color32::from_rgb(0x8b, 0xd4, 0x6e)));
                } else {
                    ui.label(RichText::new(format!("{name}: not marked, it will be removed when the program closes")).size(12.));
                }
            }
            None => {
                ui.label(RichText::new("No background log is being written in this run.").size(12.));
            }
        }
    }

    /// Markers for the debug timeline, and the latest lines of it.
    fn debug_timeline_ui(&mut self, ui: &mut egui::Ui) {
        const MARKS: [&str; 12] = [
            "HoYoPlay opened",
            "Play pressed",
            "Game window shown",
            "Login clicked",
            "Door clicked",
            "World loaded",
            "Logged out",
            "Chest opened",
            "Chest opened (primogems)",
            "Closed chest next to it",
            "Teleported away",
            "Teleported back",
        ];

        ui.label(RichText::new("Debug timeline").size(16.));
        ui.label(
            RichText::new(
                "Press these as you go through the game's launch and login. Each press is                  saved with the time, next to what the capture sees.",
            )
            .size(12.),
        );
        ui.horizontal_wrapped(|ui| {
            for mark in MARKS {
                if ui.button(mark).clicked() {
                    let _ = self.ui_message_tx.send(Message::DebugMark(mark.to_string()));
                }
            }
        });

        let lines = self.state_rx.borrow().debug_events.clone();
        Frame::new()
            .fill(Color32::from_black_alpha(80))
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ui.set_min_height(150.);
                if lines.is_empty() {
                    ui.label(RichText::new("Nothing recorded yet").size(11.));
                }
                for line in lines {
                    ui.add(egui::Label::new(RichText::new(line).monospace().size(11.)).truncate());
                }
            });

        ui.horizontal_wrapped(|ui| {
            if ui.button("Preview capture popup").clicked() {
                self.capture_popup.start_preview(false);
            }
            if ui.button("Preview failure").clicked() {
                self.capture_popup.start_preview(true);
            }
            if ui.button("Preview changes popup").clicked() {
                self.changes_popup.start_preview();
            }
        });

        ui.separator();
        ui.label(RichText::new("Traffic probe").size(16.));
        ui.label(
            RichText::new(
                "While on, commands that carry items, player properties, characters or the \
                 numbers below are written as lines of text to probe-<date>.log in the log \
                 folder: ids and counts. After each mark below, and for the first minutes of a \
                 game session, some messages are also kept whole, as hex, in probe-raw-<date>.log. \
                 Both stay on this computer; nothing is sent anywhere.",
            )
            .size(12.),
        );
        let mut probe_on = crate::probe::enabled();
        if ui.checkbox(&mut probe_on, "Record what the traffic says").changed() {
            crate::probe::set_enabled(probe_on);
            if probe_on {
                crate::probe::set_needles(&self.saved_state.probe_needles);
                crate::probe::note("probe on");
            }
        }
        ui.horizontal(|ui| {
            ui.label("Numbers to look for");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.saved_state.probe_needles)
                        .hint_text("e.g. 23813567, 120")
                        .desired_width(220.),
                )
                .changed()
            {
                crate::probe::set_needles(&self.saved_state.probe_needles);
            }
        });
        ui.separator();

        if ui.button("Open log folder").clicked() {
            thread::spawn(|| {
                let _ = open_log_dir();
            });
        }

        if ui.button("Open timeline folder").clicked() {
            match crate::debug_timeline::debug_dir() {
                Ok(dir) => {
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(e) = open::that(dir) {
                        tracing::error!("unable to open the timeline folder: {e}");
                    }
                }
                Err(e) => tracing::error!("{e}"),
            }
        }
    }

    /// A little bigger text, and more room between the items, for the windows of
    /// settings.
    fn roomier(ui: &mut egui::Ui) {
        use egui::{FontId, TextStyle};
        let style = ui.style_mut();
        style.spacing.item_spacing = egui::vec2(10., 14.);
        for (text_style, size) in [
            (TextStyle::Body, 16.),
            (TextStyle::Button, 16.),
            (TextStyle::Small, 13.),
            (TextStyle::Heading, 24.),
        ] {
            style.text_styles.insert(text_style, FontId::proportional(size));
        }
    }

    fn capture_settings_modal(&mut self, ui: &mut egui::Ui) {
        Self::roomier(ui);
        ui.set_width(460.0);
        ui.heading("Settings");
        ui.separator();
        ui.checkbox(
            &mut self.saved_state.auto_start_capture,
            "Start capture on Irminsul launch",
        );
        ui.checkbox(
            &mut self.saved_state.show_capture_popup,
            "Show a progress popup while the game's data is captured",
        );
        ui.checkbox(
            &mut self.saved_state.show_changes_popup,
            "Show a popup with what changed (mora, items) as you play",
        );
        ui.vertical(|ui| {
            ui.label("Hoyo Play Launcher exe");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.saved_state.hoyoplay_dir)
                        .hint_text("empty: found where it is installed by default")
                        .desired_width(340.),
                );
                if ui
                    .button(egui_material_icons::icons::ICON_FOLDER_OPEN)
                    .on_hover_text("Choose launcher.exe")
                    .clicked()
                {
                    let mut dialog = FileDialog::new()
                        .add_file_filter_extensions("Programs", vec!["exe"]);
                    dialog.pick_file();
                    self.launcher_dialog = Some(dialog);
                }
            });
        });
        ui.checkbox(
            &mut self.saved_state.hide_launcher,
            "Keep HoYoPlay's window out of sight (in its tray) when the game is started from here",
        );
        ui.add_enabled_ui(self.tray.is_some(), |ui| {
            ui.checkbox(
                &mut self.saved_state.close_to_tray,
                "Keep running in the tray when the window is closed",
            );

            let mut autostart = self.autostart_enabled;
            if ui
                .checkbox(&mut autostart, "Start with Windows, hidden in the tray")
                .on_hover_text(
                    "Starts Irminsul in the tray when you log on and captures right away, \
                     so it sees the game connect. Needs administrator rights.",
                )
                .changed()
            {
                match autostart::set_enabled(autostart) {
                    Ok(()) => {
                        self.autostart_enabled = autostart;
                        if autostart {
                            self.saved_state.close_to_tray = true;
                        }
                    }
                    Err(e) => {
                        tracing::error!("unable to change the startup setting: {e:#}");
                        self.toasts.error(format!("{e:#}"));
                    }
                }
            }
        });
        ui.separator();
        egui::Sides::new().show(
            ui,
            |_ui| {},
            |ui| {
                if ui.button("Ok").clicked() {
                    ui.close()
                }
            },
        );
    }

    fn optimizer_settings_modal(&mut self, ui: &mut egui::Ui) {
        Self::roomier(ui);
        ui.set_width(380.0);
        ui.heading("Genshin Optimizer Settings");
        ui.separator();
        ui.checkbox(
            &mut self.saved_state.export_settings.include_characters,
            "Characters",
        );
        ui.horizontal(|ui| {
            ui.add_space(20.);
            egui::Grid::new("char_options")
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Min level".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_character_level)
                            .range(1..=90),
                    );
                    ui.end_row();
                    ui.label("Min ascension".to_string());
                    ui.add(
                        DragValue::new(
                            &mut self.saved_state.export_settings.min_character_ascension,
                        )
                        .range(0..=6),
                    );
                    ui.end_row();
                    ui.label("Min constellation".to_string());
                    ui.add(
                        DragValue::new(
                            &mut self.saved_state.export_settings.min_character_constellation,
                        )
                        .range(0..=6),
                    );
                    ui.end_row();
                });
        });
        ui.checkbox(
            &mut self.saved_state.export_settings.include_artifacts,
            "Artifacts",
        );
        ui.horizontal(|ui| {
            ui.add_space(20.);
            egui::Grid::new("artifact_options")
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Min level".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_artifact_level)
                            .range(0..=20),
                    );
                    ui.end_row();
                    ui.label("Min rarity".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_artifact_rarity)
                            .range(0..=6),
                    );
                    ui.end_row();
                });
        });
        ui.checkbox(
            &mut self.saved_state.export_settings.include_weapons,
            "Weapons",
        );
        ui.horizontal(|ui| {
            ui.add_space(20.);
            egui::Grid::new("weapon_options")
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Min level".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_weapon_level)
                            .range(1..=90),
                    );
                    ui.end_row();

                    ui.label("Min refinement".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_weapon_refinement)
                            .range(1..=5),
                    );
                    ui.end_row();

                    ui.label("Min ascension".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_weapon_ascension)
                            .range(0..=6),
                    );
                    ui.end_row();

                    ui.label("Min rarity".to_string());
                    ui.add(
                        DragValue::new(&mut self.saved_state.export_settings.min_weapon_rarity)
                            .range(1..=5),
                    );
                    ui.end_row();
                });
        });
        ui.checkbox(
            &mut self.saved_state.export_settings.include_materials,
            "Materials",
        );
        ui.checkbox(
            &mut self.saved_state.export_settings.fake_initialize_4th_line,
            "Fake level-up 5* artifacts with unactivated stats (hover for more info)"
        ).on_hover_text(
            "Genshin Optimizer still internally treats 5* 3-liners like pre-6.0, where the new stat is \"hidden\" and unknown to GO's optimizer.\nThis is a temporary workaround by activating that last stat line, but to prevent unintended effects, the artifacts are set to level 4, mimicking the player leveling it up.\nThe last line *should* be the unlockable 4th line."
        );
        ui.separator();
        egui::Sides::new().show(
            ui,
            |_ui| {},
            |ui| {
                if ui.button("Ok").clicked() {
                    ui.close()
                }
            },
        );
    }

    fn optimizer_handle_export(&mut self, ui: &mut egui::Ui) -> Result<()> {
        let Some(rx) = self.optimizer_export_rx.take() else {
            return Ok(());
        };

        let json = rx.blocking_recv()??;

        match self.optimizer_export_target {
            OptimizerExportTarget::None => {
                tracing::warn!("Unexpected json export");
            }
            OptimizerExportTarget::Clipboard => {
                self.optimizer_save_to_clipboard(ui, json)?;
            }
            OptimizerExportTarget::File => {
                self.optimizer_save_to_file(json)?;
            }
        }

        self.optimizer_export_target = OptimizerExportTarget::None;
        Ok(())
    }

    fn optimizer_save_to_clipboard(&mut self, ui: &mut egui::Ui, json: String) -> Result<()> {
        ui.ctx().copy_text(json);
        self.toasts
            .info("Genshin Optimizer data copied to clipboard");
        Ok(())
    }

    fn optimizer_save_to_file(&mut self, json: String) -> Result<()> {
        let path = self
            .optimizer_save_path
            .take()
            .ok_or_else(|| anyhow!("No save file path set"))?;

        let file = File::create(&path).with_context(|| format!("Unable to open file {path:?}"))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(json.as_bytes())?;

        self.toasts.info("Genshin Optimizer data saved to file");
        Ok(())
    }

    fn achievement_ui(&mut self, ui: &mut egui::Ui, app_state: &AppState) {
        self.achievement_handle_export(ui).toast_error(self);

        ui.vertical(|ui| {
            egui::Sides::new().show(
                ui,
                |ui| {
                    Self::section_header(ui, "Achievement Export");
                },
                |ui| {
                    ui.add_enabled_ui(
                        app_state.updated.achievements_updated.is_some()
                            && self.achievement_export_rx.is_none(),
                        |ui| {
                            if ui
                                .button(egui_material_icons::icons::ICON_DOWNLOAD)
                                .clicked()
                            {
                                let now = Local::now();
                                let mut achievement_save_dialog = FileDialog::new()
                                    .add_file_filter_extensions("JSON files", vec!["json"])
                                    .default_file_name(&format!(
                                        "achievements_{}{}.json",
                                        Self::file_tag(app_state),
                                        now.format("%Y-%m-%d_%H-%M")
                                    ));
                                achievement_save_dialog.save_file();
                                self.achievement_save_dialog = Some(achievement_save_dialog);
                            }

                            if let Some(achievement_save_dialog) = &mut self.achievement_save_dialog
                                && let Some(path) = achievement_save_dialog.take_picked()
                            {
                                self.achievement_save_path = Some(path);
                                self.achievement_request_export(OptimizerExportTarget::File);
                            }

                            if ui
                                .button(egui_material_icons::icons::ICON_CONTENT_PASTE_GO)
                                .clicked()
                            {
                                self.achievement_request_export(OptimizerExportTarget::Clipboard);
                            }
                        },
                    );
                },
            );
        });
    }

    fn achievement_request_export(&mut self, target: OptimizerExportTarget) {
        let (tx, rx) = oneshot::channel();
        let _ = self.ui_message_tx.send(Message::ExportAchievements(tx));
        self.achievement_export_target = target;
        self.achievement_export_rx = Some(rx);
    }

    fn achievement_handle_export(&mut self, ui: &mut egui::Ui) -> Result<()> {
        let Some(rx) = self.achievement_export_rx.take() else {
            return Ok(());
        };

        let json = rx.blocking_recv()??;

        match self.achievement_export_target {
            OptimizerExportTarget::None => {
                tracing::warn!("Unexpected achievement export");
            }
            OptimizerExportTarget::Clipboard => {
                ui.ctx().copy_text(json);
                self.toasts.info("Achievement data copied to clipboard");
            }
            OptimizerExportTarget::File => {
                let path = self
                    .achievement_save_path
                    .take()
                    .ok_or_else(|| anyhow!("No save file path set"))?;

                let file =
                    File::create(&path).with_context(|| format!("Unable to open file {path:?}"))?;
                let mut writer = BufWriter::new(file);
                writer.write_all(json.as_bytes())?;

                self.toasts.info("Achievement data saved to file");
            }
        }

        self.achievement_export_target = OptimizerExportTarget::None;
        Ok(())
    }

    fn section_header(ui: &mut egui::Ui, name: &str) {
        ui.label(RichText::new(name).size(18.));
    }
}

fn yes() -> bool {
    true
}
