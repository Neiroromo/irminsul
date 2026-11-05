//! Achievement tracker tab: browse every achievement, filter them and mark the
//! ones you have finished, either by hand or by syncing from captured game data.

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc as std_mpsc;
use std::thread;

use egui::text::LayoutJob;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, ScrollArea,
    Sense, TextEdit, UiBuilder, vec2,
};
use egui_notify::Toasts;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::Message;
use crate::achievement_data::{self, Achievement, AchievementDb, Language, UpdateOutcome};

const PILL: Color32 = Color32::from_rgb(0x1c, 0x1e, 0x3d);
const CARD: Color32 = Color32::from_rgb(0x2b, 0x2f, 0x5e);
const CARD_SELECTED: Color32 = Color32::from_rgb(0x4a, 0x7b, 0xff);
const DIM: Color32 = Color32::from_rgb(0x6a, 0x6e, 0x95);
const PRIMOGEM: Color32 = Color32::from_rgb(0x7f, 0xc8, 0xff);

/// Window size used while the tracker tab is open.
pub const TAB_WINDOW_SIZE: egui::Vec2 = egui::vec2(1100., 720.);
/// Background colour behind the tracker tab.
pub const TAB_BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x16, 0x30);

/// Tracker data that is kept between runs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AchievementProgress {
    pub done: BTreeSet<u32>,
    pub language: Language,
    pub only_unfinished: bool,
    /// The header counters show what is left instead of what is done.
    pub show_remaining: bool,
}

impl Default for AchievementProgress {
    fn default() -> Self {
        Self {
            done: BTreeSet::new(),
            language: Language::default(),
            only_unfinished: false,
            show_remaining: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum TypeFilter {
    Commissions,
    Mondstadt,
    Liyue,
    Inazuma,
    Sumeru,
}

impl TypeFilter {
    const ALL: [TypeFilter; 5] = [
        TypeFilter::Commissions,
        TypeFilter::Mondstadt,
        TypeFilter::Liyue,
        TypeFilter::Inazuma,
        TypeFilter::Sumeru,
    ];

    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (TypeFilter::Commissions, Language::Ru) => "Поручения",
            (TypeFilter::Commissions, Language::En) => "Commissions",
            (TypeFilter::Mondstadt, Language::Ru) => "Мондштадт",
            (TypeFilter::Mondstadt, Language::En) => "Mondstadt",
            (TypeFilter::Liyue, Language::Ru) => "Ли Юэ",
            (TypeFilter::Liyue, Language::En) => "Liyue",
            (TypeFilter::Inazuma, Language::Ru) => "Инадзума",
            (TypeFilter::Inazuma, Language::En) => "Inazuma",
            (TypeFilter::Sumeru, Language::Ru) => "Сумеру",
            (TypeFilter::Sumeru, Language::En) => "Sumeru",
        }
    }

    fn from_region(region: &str) -> Option<TypeFilter> {
        TypeFilter::ALL
            .into_iter()
            .find(|ty| ty.region_key() == Some(region))
    }

    fn region_key(self) -> Option<&'static str> {
        match self {
            TypeFilter::Commissions => None,
            TypeFilter::Mondstadt => Some("mondstadt"),
            TypeFilter::Liyue => Some("liyue"),
            TypeFilter::Inazuma => Some("inazuma"),
            TypeFilter::Sumeru => Some("sumeru"),
        }
    }
}

enum UpdateState {
    Checking,
    UpToDate,
    Updated,
    Failed,
}

pub struct AchievementTab {
    db_ru: Option<AchievementDb>,
    db_en: Option<AchievementDb>,

    query: String,
    versions: BTreeSet<String>,
    types: BTreeSet<TypeFilter>,
    selected: usize,

    sync_rx: Option<oneshot::Receiver<Vec<u32>>>,

    /// `None` until the first update check has been started.
    update_state: Option<UpdateState>,
    update_rx: Option<std_mpsc::Receiver<anyhow::Result<UpdateOutcome>>>,
}

fn load_db(language: Language) -> Option<AchievementDb> {
    match AchievementDb::load(language) {
        Ok(db) => Some(db),
        Err(e) => {
            tracing::error!("unable to load achievement data: {e}");
            None
        }
    }
}

/// Lays out `text` on a single line, eliding it with an ellipsis if it does
/// not fit in `width`.
fn elided(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    ui.painter().layout_job(job)
}

/// A clickable counter; returns whether it was clicked.
fn pill(ui: &mut egui::Ui, id: &'static str, text: impl Into<String>) -> bool {
    let frame = Frame::new()
        .fill(PILL)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(RichText::new(text.into()).size(15.));
        });
    let rect = frame.response.rect;
    let response = ui
        .interact(rect, ui.id().with(id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(12), Color32::from_white_alpha(14));
    }
    response.clicked()
}

/// Unfinished achievements and their primogems.
type Remaining = (u32, u32);

/// One row of a filter dropdown: a checkbox with the remaining achievement
/// count and primogems on the right, in two separate columns.
fn filter_option(ui: &mut egui::Ui, label: &str, on: &mut bool, (count, primos): Remaining) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = ui.checkbox(on, label).changed();
        // Fixed height: a plain `with_layout` would take all the vertical
        // space available inside the scroll area.
        let right = vec2(ui.available_width(), ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(right, Layout::right_to_left(Align::Center), |ui| {
            let (count_color, primos_color) = if count == 0 {
                (DIM, DIM)
            } else {
                (Color32::from_gray(0xd0), PRIMOGEM)
            };
            ui.allocate_ui_with_layout(vec2(70., 18.), Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{primos} {}",
                        egui_material_icons::icons::ICON_AUTO_AWESOME
                    ))
                    .color(primos_color),
                );
            });
            ui.separator();
            ui.allocate_ui_with_layout(vec2(36., 18.), Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(count.to_string()).color(count_color));
            });
        });
    });
    changed
}

fn matches(
    chain: &[Achievement],
    versions: &BTreeSet<String>,
    types: &BTreeSet<TypeFilter>,
    query: &str,
) -> bool {
    let first = &chain[0];
    if !versions.is_empty() && !versions.contains(&first.ver) {
        return false;
    }
    if !types.is_empty() {
        let Some(region) = &first.region else {
            return false;
        };
        let any_region = types.contains(&TypeFilter::Commissions);
        let region_match = types
            .iter()
            .any(|ty| ty.region_key() == Some(region.as_str()));
        if !any_region && !region_match {
            return false;
        }
    }
    let query = query.trim().to_lowercase();
    query.is_empty()
        || chain.iter().any(|achievement| {
            achievement.name.to_lowercase().contains(&query)
                || achievement.desc.to_lowercase().contains(&query)
        })
}

fn start_update_check(
    ctx: &egui::Context,
    update_state: &mut Option<UpdateState>,
    update_rx: &mut Option<std_mpsc::Receiver<anyhow::Result<UpdateOutcome>>>,
) {
    let (tx, rx) = std_mpsc::channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let _ = tx.send(achievement_data::check_for_update());
        ctx.request_repaint();
    });
    *update_rx = Some(rx);
    *update_state = Some(UpdateState::Checking);
}

impl AchievementTab {
    pub fn new() -> Self {
        Self {
            db_ru: load_db(Language::Ru),
            db_en: load_db(Language::En),
            query: String::new(),
            versions: BTreeSet::new(),
            types: BTreeSet::new(),
            selected: 0,
            sync_rx: None,
            update_state: None,
            update_rx: None,
        }
    }

    fn poll_update(&mut self, toasts: &mut Toasts, language: Language) {
        let Some(rx) = &self.update_rx else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std_mpsc::TryRecvError::Empty) => return,
            Err(std_mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("update check stopped")),
        };
        self.update_rx = None;
        self.update_state = Some(match outcome {
            Ok(UpdateOutcome::UpToDate) => UpdateState::UpToDate,
            Ok(UpdateOutcome::Updated) => {
                self.db_ru = load_db(Language::Ru);
                self.db_en = load_db(Language::En);
                toasts.info(match language {
                    Language::Ru => "База достижений обновлена",
                    Language::En => "Achievement database updated",
                });
                UpdateState::Updated
            }
            Err(e) => {
                tracing::warn!("achievement data update failed: {e:#}");
                UpdateState::Failed
            }
        });
    }

    fn poll_sync(&mut self, progress: &mut AchievementProgress, toasts: &mut Toasts) {
        let Some(mut rx) = self.sync_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(ids) => {
                let before = progress.done.len();
                progress.done.extend(ids);
                let added = progress.done.len() - before;
                let language = progress.language;
                toasts.info(match language {
                    Language::Ru => format!("Загружено из игры, новых достижений: {added}"),
                    Language::En => format!("Synced from game, {added} new achievements"),
                });
            }
            Err(oneshot::error::TryRecvError::Empty) => self.sync_rx = Some(rx),
            Err(oneshot::error::TryRecvError::Closed) => (),
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        progress: &mut AchievementProgress,
        can_sync: bool,
        ui_message_tx: &mpsc::UnboundedSender<Message>,
        toasts: &mut Toasts,
    ) {
        self.poll_sync(progress, toasts);
        if self.sync_rx.is_some() {
            ui.ctx().request_repaint();
        }
        if self.update_state.is_none() {
            start_update_check(ui.ctx(), &mut self.update_state, &mut self.update_rx);
        }
        self.poll_update(toasts, progress.language);

        let language = progress.language;
        let tr = |en: &'static str, ru: &'static str| match language {
            Language::En => en,
            Language::Ru => ru,
        };

        let db = match language {
            Language::Ru => &self.db_ru,
            Language::En => &self.db_en,
        };
        let Some(db) = db else {
            ui.label(tr(
                "Unable to load achievement data",
                "Не удалось загрузить данные достижений",
            ));
            return;
        };

        if self.selected >= db.categories.len() {
            self.selected = 0;
        }

        // The header counters cover everything the version, type and search
        // filters let through, across all categories.
        let all = || {
            db.categories
                .iter()
                .flat_map(|c| c.entries.iter())
                .filter(|chain| matches(chain, &self.versions, &self.types, &self.query))
                .flatten()
        };
        let total_count = all().count();
        let done_count = all().filter(|a| progress.done.contains(&a.id)).count();
        let total_primos: u32 = all().map(|a| a.reward).sum();
        let done_primos: u32 = all()
            .filter(|a| progress.done.contains(&a.id))
            .map(|a| a.reward)
            .sum();

        // Header.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr("Achievements", "Достижения"))
                    .size(34.)
                    .strong(),
            );
            // Both counters share one mode; clicking either switches both.
            let primogem = egui_material_icons::icons::ICON_AUTO_AWESOME;
            let (count_text, primos_text) = if progress.show_remaining {
                (
                    (total_count - done_count).to_string(),
                    format!("{} {primogem}", total_primos - done_primos),
                )
            } else {
                (
                    format!("{done_count} {} {total_count}", tr("of", "из")),
                    format!(
                        "{done_primos} {} {total_primos} {primogem}",
                        tr("of", "из")
                    ),
                )
            };
            let clicked_count = pill(ui, "count_pill", count_text);
            let clicked_primos = pill(ui, "primos_pill", primos_text);
            if clicked_count || clicked_primos {
                progress.show_remaining = !progress.show_remaining;
            }
            ui.add_space(12.);
            ui.checkbox(
                &mut progress.only_unfinished,
                tr("Show only unfinished", "Показывать только незавершённые"),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("achievement_language")
                    .width(60.)
                    .selected_text(match language {
                        Language::Ru => "RU",
                        Language::En => "EN",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut progress.language, Language::Ru, "RU");
                        ui.selectable_value(&mut progress.language, Language::En, "EN");
                    });
                let checking = matches!(self.update_state, Some(UpdateState::Checking));
                if ui
                    .add_enabled(
                        !checking,
                        egui::Button::new(egui_material_icons::icons::ICON_REFRESH),
                    )
                    .on_hover_text(tr(
                        "Check for achievement database updates",
                        "Проверить обновления базы достижений",
                    ))
                    .clicked()
                {
                    start_update_check(ui.ctx(), &mut self.update_state, &mut self.update_rx);
                }
                let status = match self.update_state {
                    Some(UpdateState::Checking) => {
                        ui.spinner();
                        tr("Checking for updates...", "Проверка обновлений...")
                    }
                    Some(UpdateState::UpToDate) => tr("Database is up to date", "База актуальна"),
                    Some(UpdateState::Updated) => tr("Database updated", "База обновлена"),
                    Some(UpdateState::Failed) => {
                        tr("Update check failed", "Не удалось проверить обновления")
                    }
                    None => "",
                };
                ui.label(RichText::new(status).color(DIM));
                ui.add_enabled_ui(can_sync && self.sync_rx.is_none(), |ui| {
                    if ui
                        .button(format!(
                            "{} {}",
                            egui_material_icons::icons::ICON_SYNC,
                            tr("Sync from game", "Из игры")
                        ))
                        .on_hover_text(tr(
                            "Mark achievements finished in the captured game data",
                            "Отметить достижения, выполненные в перехваченных данных игры",
                        ))
                        .clicked()
                    {
                        let (tx, rx) = oneshot::channel();
                        let _ = ui_message_tx.send(Message::FinishedAchievements(tx));
                        self.sync_rx = Some(rx);
                    }
                });
            });
        });
        ui.add_space(6.);

        // What is left per version and per type.  Each list is counted with the
        // other filters applied but not its own, so its options show what
        // selecting them would give.
        let no_versions = BTreeSet::new();
        let no_types = BTreeSet::new();
        let mut version_stats: HashMap<&str, Remaining> = HashMap::new();
        let mut type_stats: HashMap<TypeFilter, Remaining> = HashMap::new();
        for chain in db.categories.iter().flat_map(|c| c.entries.iter()) {
            let first = &chain[0];
            let left = chain
                .iter()
                .filter(|a| !progress.done.contains(&a.id))
                .fold((0, 0), |(count, primos), a| (count + 1, primos + a.reward));
            if matches(chain, &no_versions, &self.types, &self.query) {
                let stats = version_stats.entry(first.ver.as_str()).or_default();
                stats.0 += left.0;
                stats.1 += left.1;
            }
            if matches(chain, &self.versions, &no_types, &self.query)
                && let Some(region) = &first.region
            {
                for ty in [Some(TypeFilter::Commissions), TypeFilter::from_region(region)]
                    .into_iter()
                    .flatten()
                {
                    let stats = type_stats.entry(ty).or_default();
                    stats.0 += left.0;
                    stats.1 += left.1;
                }
            }
        }

        // Filters.
        ui.horizontal(|ui| {
            let combo_width = 200.;
            let search_width = (ui.available_width() - 2. * combo_width - 24.).max(120.);
            ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text(tr("Search achievements", "Поиск достижений"))
                    .desired_width(search_width),
            );

            let version_label = if self.versions.is_empty() {
                tr("Version", "Версия").to_owned()
            } else {
                self.versions.iter().cloned().collect::<Vec<_>>().join(", ")
            };
            egui::ComboBox::from_id_salt("achievement_version")
                .width(combo_width)
                .truncate()
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .selected_text(version_label)
                .show_ui(ui, |ui| {
                    ui.set_min_width(260.);
                    if ui.button(tr("Clear", "Сбросить")).clicked() {
                        self.versions.clear();
                    }
                    ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                        for version in db.versions.iter().rev() {
                            let mut on = self.versions.contains(version);
                            let stats = version_stats
                                .get(version.as_str())
                                .copied()
                                .unwrap_or_default();
                            if filter_option(ui, version, &mut on, stats) {
                                if on {
                                    self.versions.insert(version.clone());
                                } else {
                                    self.versions.remove(version);
                                }
                            }
                        }
                    });
                });

            let type_label = if self.types.is_empty() {
                tr("Type", "Тип").to_owned()
            } else {
                self.types
                    .iter()
                    .map(|ty| ty.label(language))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            egui::ComboBox::from_id_salt("achievement_type")
                .width(combo_width)
                .truncate()
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .selected_text(type_label)
                .show_ui(ui, |ui| {
                    ui.set_min_width(260.);
                    if ui.button(tr("Clear", "Сбросить")).clicked() {
                        self.types.clear();
                    }
                    for ty in TypeFilter::ALL {
                        let mut on = self.types.contains(&ty);
                        let stats = type_stats.get(&ty).copied().unwrap_or_default();
                        if filter_option(ui, ty.label(language), &mut on, stats) {
                            if on {
                                self.types.insert(ty);
                            } else {
                                self.types.remove(&ty);
                            }
                        }
                    }
                });
        });
        ui.add_space(8.);

        // Body: category list on the left, achievements of the selected one
        // on the right.  Leave room at the bottom for the footer icons.
        let body_height = (ui.available_height() - 30.).max(100.);
        let list_width = 300.;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(list_width, body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("achievement_categories")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.;
                            let width = ui.available_width() - 12.;
                            for (index, category) in db.categories.iter().enumerate() {
                                let achievements = || category.entries.iter().flatten();
                                let total = achievements().count();
                                let done = achievements()
                                    .filter(|a| progress.done.contains(&a.id))
                                    .count();
                                let primos: u32 = achievements()
                                    .filter(|a| progress.done.contains(&a.id))
                                    .map(|a| a.reward)
                                    .sum();
                                let percent = if total == 0 { 0 } else { done * 100 / total };

                                let (rect, response) =
                                    ui.allocate_exact_size(vec2(width, 62.), Sense::click());
                                let selected = index == self.selected;
                                let fill = if selected {
                                    CARD_SELECTED
                                } else if response.hovered() {
                                    CARD.gamma_multiply(1.3)
                                } else {
                                    CARD
                                };
                                let painter = ui.painter();
                                painter.rect_filled(rect, CornerRadius::same(12), fill);

                                let galley = elided(
                                    ui,
                                    &category.name,
                                    FontId::proportional(16.),
                                    Color32::WHITE,
                                    width - 24.,
                                );
                                painter.galley(
                                    rect.left_top() + vec2(12., 9.),
                                    galley,
                                    Color32::WHITE,
                                );
                                painter.text(
                                    rect.left_top() + vec2(12., 34.),
                                    Align2::LEFT_TOP,
                                    format!("{done}/{total} ({percent}%)"),
                                    FontId::proportional(14.),
                                    Color32::from_gray(0xd0),
                                );
                                painter.text(
                                    rect.right_bottom() - vec2(12., 9.),
                                    Align2::RIGHT_BOTTOM,
                                    format!(
                                        "{primos} {}",
                                        egui_material_icons::icons::ICON_AUTO_AWESOME
                                    ),
                                    FontId::proportional(14.),
                                    PRIMOGEM,
                                );

                                if response.clicked() {
                                    self.selected = index;
                                }
                            }
                        });
                },
            );

            ui.add_space(4.);
            let list_width = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(list_width, body_height),
                Layout::top_down(Align::LEFT),
                |ui| {
                    let category = &db.categories[self.selected];
                    let entries: Vec<&Vec<Achievement>> = category
                        .entries
                        .iter()
                        .filter(|chain| matches(chain, &self.versions, &self.types, &self.query))
                        .filter(|chain| {
                            !progress.only_unfinished
                                || chain.iter().any(|a| !progress.done.contains(&a.id))
                        })
                        .collect();

                    ScrollArea::vertical()
                        .id_salt("achievement_list")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.;
                            if entries.is_empty() {
                                ui.label(
                                    RichText::new(tr("Nothing found", "Ничего не найдено"))
                                        .color(DIM),
                                );
                            }
                            let width = ui.available_width() - 12.;
                            for chain in entries {
                                Self::chain_ui(ui, chain, width, progress);
                            }
                        });
                },
            );
        });
    }

    fn chain_ui(
        ui: &mut egui::Ui,
        chain: &[Achievement],
        width: f32,
        progress: &mut AchievementProgress,
    ) {
        const MARGIN: i8 = 14;
        Frame::new()
            .fill(CARD)
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(MARGIN, 10))
            .show(ui, |ui| {
                ui.set_width(width - 2. * f32::from(MARGIN));
                for (index, achievement) in chain.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(4.);
                        ui.separator();
                        ui.add_space(4.);
                    }
                    // Later stages are dimmed until the previous one is done.
                    let dimmed = index > 0 && !progress.done.contains(&chain[index - 1].id);
                    Self::achievement_row(ui, achievement, dimmed, progress);
                }
            });
    }

    fn achievement_row(
        ui: &mut egui::Ui,
        achievement: &Achievement,
        dimmed: bool,
        progress: &mut AchievementProgress,
    ) {
        let right_width = 130.;
        let left_width = ui.available_width() - right_width;
        let (name_color, text_color, primo_color) = if dimmed {
            (DIM, DIM, DIM)
        } else {
            (Color32::WHITE, Color32::from_gray(0xc4), PRIMOGEM)
        };

        let row_top = ui.cursor().min;
        let row_right = row_top.x + ui.available_width();
        let left = ui.allocate_ui_with_layout(
            vec2(left_width, 0.),
            Layout::top_down(Align::LEFT),
            |ui| {
                ui.set_min_width(left_width);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&achievement.name)
                            .size(16.)
                            .strong()
                            .color(name_color),
                    );
                    ui.label(
                        RichText::new(format!(" {} ", achievement.ver))
                            .size(12.)
                            .color(text_color)
                            .background_color(PILL),
                    );
                });
                ui.label(RichText::new(&achievement.desc).color(text_color));
            },
        );

        let right_rect = egui::Rect::from_min_size(
            egui::pos2(row_right - right_width + 4., row_top.y),
            vec2(right_width - 4., left.response.rect.height().max(32.)),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(right_rect)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.spacing_mut().icon_width = 28.;
                let mut done = progress.done.contains(&achievement.id);
                if ui.checkbox(&mut done, "").changed() {
                    if done {
                        progress.done.insert(achievement.id);
                    } else {
                        progress.done.remove(&achievement.id);
                    }
                }
                ui.label(
                    RichText::new(egui_material_icons::icons::ICON_AUTO_AWESOME)
                        .size(20.)
                        .color(primo_color),
                );
                ui.label(
                    RichText::new(achievement.reward.to_string())
                        .size(16.)
                        .color(text_color),
                );
            },
        );
        // Make sure the parent layout continues below the taller of the sides.
        ui.advance_cursor_after_rect(right_rect.union(left.response.rect));
    }
}
