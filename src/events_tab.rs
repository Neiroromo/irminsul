//! The events tab: a timeline of the events of the game, the wishes and the periods of the
//! Spiral Abyss and the Imaginarium Theater, with today marked, the versions of the game on
//! top, and a card of an event when it is clicked.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as Span, Local, NaiveTime, TimeZone, Timelike};
use egui::epaint::{Mesh, Vertex};
use egui::{
    Align2, Color32, ColorImage, Context, CornerRadius, FontId, Frame, Margin, Rect, RichText, ScrollArea, Sense, Stroke,
    TextureHandle, TextureId, TextureOptions, Ui, pos2, vec2,
};
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::achievement_data::Language;
use crate::achievement_tab::DIM;
use crate::events::{self, Event, Kind, Version};
use crate::roster_tab::language_choice;

/// Pixels in a day.
const DAY: f32 = 34.;
/// The height of a bar, and of a row (a bar and the space under it).
const BAR: f32 = 34.;
const ROW: f32 = 46.;
/// The strip of the versions, and the scale of the days under it.
const STRIP: f32 = 18.;
/// What is above the first row: the strip, the months and days, and some room.
const HEADER: f32 = 104.;
/// How many days before today and after it are shown.
const BEFORE: i64 = 21;
const AFTER: i64 = 84;
/// How wide the cover on a bar is, at most.
const COVER: f32 = 260.;
/// The background of the page, as on the model.
const PAGE: Color32 = Color32::from_rgb(0x26, 0x28, 0x4c);
const ORANGE: Color32 = Color32::from_rgb(0xff, 0xb4, 0x3c);

#[derive(Default, Serialize, Deserialize)]
struct Prefs {
    #[serde(default)]
    hide_astral: bool,
}

fn prefs_file() -> Option<PathBuf> {
    Some(eframe::storage_dir(APP_ID)?.join("events_prefs.json"))
}

// ---------------------------------------------------------------------------
// The pictures of the events.

struct Cover {
    texture: TextureHandle,
    /// The colour of the picture on average.
    average: Color32,
    size: [f32; 2],
}

struct Loaded {
    url: String,
    image: ColorImage,
    average: [u8; 3],
}

/// The covers: fetched in the background, kept on disk, made into textures when they come.
struct Covers {
    ctx: Context,
    request_tx: Sender<String>,
    loaded_rx: Receiver<Loaded>,
    covers: HashMap<String, Cover>,
    requested: HashSet<String>,
}

impl Covers {
    fn new(ctx: &Context) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<String>();
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let worker = ctx.clone();
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            let Ok(client) = reqwest::Client::builder().user_agent("Irminsul").timeout(Duration::from_secs(30)).build() else {
                return;
            };
            while let Ok(url) = request_rx.recv() {
                if let Some(loaded) = runtime.block_on(load(&client, &url)) {
                    let _ = loaded_tx.send(loaded);
                    worker.request_repaint();
                }
            }
        });
        Self { ctx: ctx.clone(), request_tx, loaded_rx, covers: HashMap::new(), requested: HashSet::new() }
    }

    /// Takes what has arrived.
    fn poll(&mut self) {
        while let Ok(loaded) = self.loaded_rx.try_recv() {
            let size = [loaded.image.size[0] as f32, loaded.image.size[1] as f32];
            let texture = self.ctx.load_texture(format!("event_cover_{}", loaded.url), loaded.image, TextureOptions::LINEAR);
            let [r, g, b] = loaded.average;
            self.covers.insert(loaded.url, Cover { texture, average: Color32::from_rgb(r, g, b), size });
        }
    }

    /// The cover of an address, asking for it the first time.
    fn get(&mut self, url: &str) -> Option<&Cover> {
        if url.is_empty() {
            return None;
        }
        if !self.covers.contains_key(url) && self.requested.insert(url.to_owned()) {
            let _ = self.request_tx.send(url.to_owned());
        }
        self.covers.get(url)
    }
}

fn cache_path(url: &str) -> Option<PathBuf> {
    let name: String = url.chars().rev().take(80).collect::<Vec<_>>().into_iter().rev().filter(|c| c.is_alphanumeric()).collect();
    Some(eframe::storage_dir(APP_ID)?.join("event_covers").join(name))
}

async fn load(client: &reqwest::Client, url: &str) -> Option<Loaded> {
    let path = cache_path(url);
    let bytes = match path.as_ref().and_then(|path| std::fs::read(path).ok()) {
        Some(bytes) => bytes,
        None => {
            let bytes = client.get(url).send().await.ok()?.error_for_status().ok()?.bytes().await.ok()?.to_vec();
            if let Some(path) = &path {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, &bytes);
            }
            bytes
        }
    };
    let picture = image::load_from_memory(&bytes).ok()?.thumbnail(720, 720).to_rgba8();
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let mut sum = [0u64; 3];
    for pixel in picture.pixels() {
        for (channel, total) in sum.iter_mut().enumerate() {
            *total += u64::from(pixel.0[channel]);
        }
    }
    let count = (width * height).max(1) as u64;
    let average = [(sum[0] / count) as u8, (sum[1] / count) as u8, (sum[2] / count) as u8];
    let image = ColorImage::from_rgba_unmultiplied([width, height], picture.as_raw());
    Some(Loaded { url: url.to_owned(), image, average })
}

// ---------------------------------------------------------------------------

pub struct EventsTab {
    /// The view is put at today once.
    placed: bool,
    covers: Covers,
    hide_astral: bool,
    /// The event whose card is open: its id and its beginning (the periods of the calendar have
    /// no id).
    open: Option<(u64, i64)>,
}

impl EventsTab {
    pub fn new(ctx: &Context) -> Self {
        events::refresh_in_background(ctx, false);
        let prefs: Prefs = prefs_file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { placed: false, covers: Covers::new(ctx), hide_astral: prefs.hide_astral, open: None }
    }

    fn save_prefs(&self) {
        if let Some(path) = prefs_file() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(text) = serde_json::to_string(&Prefs { hide_astral: self.hide_astral }) {
                let _ = std::fs::write(path, text);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, language: &mut Language) {
        let russian = *language == Language::Ru;
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        events::refresh_in_background(ui.ctx(), false);
        self.covers.poll();

        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Event timeline", "Лента событий")).size(34.).strong());
            ui.add_space(8.);
            let status = events::fetch_status();
            if ui.add_enabled(!status.running, egui::Button::new(tr("Refresh", "Обновить"))).clicked() {
                events::refresh_in_background(ui.ctx(), true);
            }
            let before = self.hide_astral;
            ui.checkbox(&mut self.hide_astral, tr("Hide the Astral Plane", "Скрыть Астральный предел"));
            if before != self.hide_astral {
                self.save_prefs();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                language_choice(ui, language);
                let text = if status.running {
                    tr("Updating…", "Обновление…").to_owned()
                } else if let Some(error) = &status.error {
                    format!("{} {error}", tr("Could not update:", "Не удалось обновить:"))
                } else if events::fetched_at() > 0 {
                    let time = Local.timestamp_opt(events::fetched_at(), 0).single();
                    format!(
                        "{} {}",
                        tr("Updated", "Обновлено"),
                        time.map(|time| time.format("%d.%m %H:%M").to_string()).unwrap_or_default()
                    )
                } else {
                    tr("Not fetched yet", "Ещё не загружено").to_owned()
                };
                ui.label(RichText::new(text).size(13.).color(DIM));
            });
        });
        ui.label(
            RichText::new(tr(
                "Times are local. The Abyss and the Theater come from the calendar; the rest is from the official announcements of the game.",
                "Время местное. Бездна и Театр считаются по календарю, остальное берётся из официальных объявлений игры.",
            ))
            .size(12.)
            .color(DIM),
        );
        ui.add_space(6.);

        let now = chrono::Utc::now().timestamp();
        let today = Local::now().date_naive();
        let origin_day = today - Span::days(BEFORE);
        let origin = local_midnight(origin_day);
        let days = BEFORE + AFTER;
        let all = events::all(now);
        let versions = events::versions(now);
        let (from, to) = (origin, origin + days * 86400);
        let mut shown: Vec<&Event> = all
            .iter()
            .filter(|event| event.end > from && event.start < to)
            .filter(|event| !(self.hide_astral && event.kind == Kind::Astral))
            .collect();
        shown.sort_by_key(|event| order_key(event));
        let lanes = assign_lanes(&shown);
        let lane_count = lanes.iter().copied().max().map_or(1, |lane| lane + 1);

        let x_of = |time: i64| (time - origin) as f32 / 86400. * DAY;
        let width = days as f32 * DAY;
        let height = HEADER + lane_count as f32 * ROW + 16.;
        let first = !self.placed;
        self.placed = true;

        let mut clicked: Option<(u64, i64)> = None;
        Frame::new().fill(PAGE).corner_radius(CornerRadius::same(12)).inner_margin(Margin::ZERO).show(ui, |ui| {
            let mut area = ScrollArea::both().auto_shrink([false, false]).id_salt("events_timeline");
            if first {
                area = area.horizontal_scroll_offset(((BEFORE as f32 - 3.) * DAY).max(0.));
            }
            area.show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(width, height.max(ui.available_height())), Sense::hover());
                let painter = ui.painter_at(rect);
                let view = ui.clip_rect();
                let top = rect.top() + 8.;

                // The days: weekends are shaded, the first of a month has its name.
                for day in 0..days {
                    let date = origin_day + Span::days(day);
                    let x = rect.left() + day as f32 * DAY;
                    if x + DAY < view.left() || x > view.right() {
                        continue;
                    }
                    let weekend = date.weekday().number_from_monday() >= 6;
                    painter.line_segment(
                        [pos2(x, top + STRIP + 4.), pos2(x, rect.bottom())],
                        Stroke::new(1., Color32::from_white_alpha(if date.day() == 1 { 46 } else { 14 })),
                    );
                    let color = if weekend { ORANGE } else { Color32::from_white_alpha(190) };
                    painter.text(
                        pos2(x + DAY / 2., top + STRIP + 36.),
                        Align2::CENTER_CENTER,
                        weekday_short(date.weekday().num_days_from_monday() as usize, russian),
                        FontId::proportional(12.),
                        color,
                    );
                    painter.text(
                        pos2(x + DAY / 2., top + STRIP + 56.),
                        Align2::CENTER_CENTER,
                        date.day().to_string(),
                        FontId::proportional(14.),
                        if date == today { Color32::WHITE } else { color },
                    );
                    if date.day() == 1 || day == 0 {
                        painter.text(
                            pos2(x.max(view.left()) + 8., top + STRIP + 16.),
                            Align2::LEFT_CENTER,
                            month_name(date.month0() as usize, russian),
                            FontId::proportional(15.),
                            ORANGE,
                        );
                    }
                }

                // The versions, in a thin strip above the days, with the primogems of each.
                for (index, version) in versions.iter().enumerate() {
                    let end = versions.get(index + 1).map_or(version.start + 42 * 86400, |next| next.start);
                    let (left, right) = (rect.left() + x_of(version.start), rect.left() + x_of(end));
                    let strip = Rect::from_min_max(pos2(left + 1., top), pos2(right - 1., top + STRIP));
                    if strip.right() < view.left() || strip.left() > view.right() {
                        continue;
                    }
                    let current = version.start <= now && now < end;
                    let fill = if current { Color32::from_rgb(0x5b, 0x6b, 0xd8) } else { Color32::from_rgb(0x3a, 0x3e, 0x74) };
                    painter.rect_filled(strip, CornerRadius::same(4), fill);
                    let total: u32 = all.iter().filter(|event| event.start >= version.start && event.start < end).map(|event| event.primogems).sum();
                    let label = if total > 0 {
                        format!("{} {} · ≈{} {}", tr("Version", "Версия"), version.name, grouped(total), tr("primogems", "примогемов"))
                    } else {
                        format!("{} {}", tr("Version", "Версия"), version.name)
                    };
                    let clip = strip.intersect(view).shrink2(vec2(3., 0.));
                    if clip.width() > 20. {
                        painter.with_clip_rect(clip).text(
                            pos2(strip.left().max(view.left()) + 8., strip.center().y),
                            Align2::LEFT_CENTER,
                            label,
                            FontId::proportional(12.),
                            Color32::WHITE,
                        );
                    }
                    let response = ui.interact(strip.intersect(view), ui.id().with(("version", index)), Sense::hover());
                    response.on_hover_text(version_tip(version, end, total, russian));
                }

                // The bars.
                for (event, lane) in shown.iter().zip(&lanes) {
                    let left = rect.left() + x_of(event.start);
                    let right = rect.left() + x_of(event.end);
                    let y = top + HEADER + *lane as f32 * ROW;
                    let bar = Rect::from_min_max(pos2(left, y), pos2(right.max(left + 6.), y + BAR));
                    if bar.right() < view.left() || bar.left() > view.right() {
                        continue;
                    }
                    let cover = self.covers.get(&event.banner);
                    let fill = cover.map_or_else(|| kind_color(event), |cover| cover.average);
                    painter.rect_filled(bar, CornerRadius::same(5), fill);
                    if let Some(cover) = cover {
                        paint_cover(&painter, bar, cover.texture.id(), cover.size, fill);
                    }
                    // The title stays in view while the bar is: it begins at the left edge of what is seen.
                    let text_color = text_color(fill);
                    let clip = bar.intersect(view);
                    if clip.width() > 24. {
                        painter.with_clip_rect(clip.shrink2(vec2(4., 0.))).text(
                            pos2(bar.left().max(view.left()) + 10., bar.center().y),
                            Align2::LEFT_CENTER,
                            event.short_title(russian),
                            FontId::proportional(15.),
                            text_color,
                        );
                    }
                    let response = ui.interact(clip, ui.id().with(("event", event.id, event.start, event.kind as u8)), Sense::click());
                    if response.hovered() {
                        painter.rect_filled(bar, CornerRadius::same(5), Color32::from_white_alpha(22));
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if response.clicked() {
                        clicked = Some((event.id, event.start));
                    }
                }

                // Today: a line, with the time on it.
                let line = rect.left() + x_of(now);
                painter.line_segment(
                    [pos2(line, top + STRIP + 4.), pos2(line, rect.bottom())],
                    Stroke::new(1.5, Color32::from_white_alpha(210)),
                );
                let clock = Local::now().format("%H:%M:%S").to_string();
                let label = Rect::from_center_size(pos2(line, top + STRIP + 14.), vec2(66., 20.));
                painter.rect_filled(label, CornerRadius::same(10), Color32::from_gray(0x55));
                painter.text(label.center(), Align2::CENTER_CENTER, clock, FontId::proportional(12.), Color32::WHITE);
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            });
        });

        if clicked.is_some() {
            self.open = clicked;
        }
        self.card(ui.ctx(), &all, russian, now);
    }

    /// The card of the event that was clicked: its cover, name, dates, how long it lasts, the
    /// article and the text.
    fn card(&mut self, ctx: &Context, all: &[Event], russian: bool, now: i64) {
        let Some((id, start)) = self.open else {
            return;
        };
        let Some(event) = all.iter().find(|event| event.id == id && event.start == start) else {
            self.open = None;
            return;
        };
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let modal = egui::Modal::new(egui::Id::new("event_card")).show(ctx, |ui| {
                ui.set_width(560.);
                if let Some(cover) = self.covers.get(&event.banner) {
                    let width = 560.;
                    let height = width * cover.size[1] / cover.size[0].max(1.);
                    ui.add(egui::Image::new((cover.texture.id(), vec2(width, height))).corner_radius(CornerRadius::same(8)));
                    ui.add_space(6.);
                }
                ui.label(RichText::new(event.title(russian)).size(19.).strong());
                ui.add_space(4.);
                ui.label(format!("{} {}", tr("Begins:", "Начало:"), format_time(event.start)));
                ui.label(format!("{} {}", tr("Ends:", "Окончание:"), format_time(event.end)));
                ui.label(RichText::new(countdown(event, now, russian)).size(17.).strong().color(ORANGE));
                if event.primogems > 0 {
                    ui.label(format!(
                        "{} {}",
                        tr("Primogems named in the description:", "Примогемы, указанные в описании:"),
                        grouped(event.primogems)
                    ));
                }
                if !event.link.is_empty() {
                    ui.hyperlink_to(tr("Official article", "Официальная статья"), &event.link);
                }
                let text = event.text(russian);
                if text.is_empty() {
                    if event.id == 0 {
                        ui.label(
                            RichText::new(tr(
                                "The dates are from the calendar: the periods change on the 1st and the 16th (the Theater on the 1st).",
                                "Даты рассчитаны по календарю: периоды меняются 1-го и 16-го числа (Театр — 1-го).",
                            ))
                            .color(DIM),
                        );
                    }
                } else {
                    ui.add_space(6.);
                    ScrollArea::vertical().max_height(260.).id_salt("event_text").show(ui, |ui| {
                        ui.label(text);
                    });
                }
            });
        ctx.request_repaint_after(Duration::from_millis(500));
        // Closed by Escape and by a press outside the window, like every popup.
        if modal.should_close() {
            self.open = None;
        }
    }
}

/// The cover at the right edge of a bar, fading into its fill to the left.
fn paint_cover(painter: &egui::Painter, bar: Rect, texture: TextureId, size: [f32; 2], fill: Color32) {
    let _ = fill;
    let width = bar.width().min(COVER);
    let area = Rect::from_min_max(pos2(bar.right() - width, bar.top()), bar.right_bottom());
    // The picture covers the area: cut at the sides it is too wide or too tall in.
    let scale = (area.width() / size[0]).max(area.height() / size[1]);
    let (shown_w, shown_h) = (area.width() / (size[0] * scale), area.height() / (size[1] * scale));
    let (u0, v0) = ((1. - shown_w) / 2., (1. - shown_h) / 2.);
    const STEPS: usize = 16;
    let mut mesh = Mesh::with_texture(texture);
    for step in 0..=STEPS {
        let t = step as f32 / STEPS as f32;
        // The fade is slow at first and then quick: smoothstep.
        let alpha = (t * t * (3. - 2. * t) * 255.) as u8;
        let color = Color32::from_white_alpha(alpha);
        let x = area.left() + area.width() * t;
        let u = u0 + shown_w * t;
        mesh.vertices.push(Vertex { pos: pos2(x, area.top()), uv: pos2(u, v0), color });
        mesh.vertices.push(Vertex { pos: pos2(x, area.bottom()), uv: pos2(u, v0 + shown_h), color });
    }
    for step in 0..STEPS as u32 {
        let base = step * 2;
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    painter.add(mesh);
}

fn version_tip(version: &Version, end: i64, total: u32, russian: bool) -> String {
    let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
    let mut text = format!(
        "{} {}\n{} — {}",
        tr("Version", "Версия"),
        version.name,
        format_time(version.start),
        format_time(end)
    );
    if !version.known {
        text += &format!("\n{}", tr("The dates are counted by six weeks from the known versions.", "Даты посчитаны по шесть недель от известных версий."));
    }
    if total > 0 {
        text += &format!(
            "\n≈ {} {}\n{}",
            grouped(total),
            tr("primogems", "примогемов"),
            tr(
                "Only what the descriptions of the events name; quests, exploration, the Abyss and the Theater are not counted.",
                "Только то, что указано в описаниях событий; задания, исследование, Бездна и Театр не учтены."
            )
        );
    }
    text
}

/// How long is left (or until it begins); never below zero.
fn countdown(event: &Event, now: i64, russian: bool) -> String {
    let clock = |seconds: i64| {
        let seconds = seconds.max(0);
        let (days, hours, minutes, rest) = (seconds / 86400, seconds % 86400 / 3600, seconds % 3600 / 60, seconds % 60);
        if days > 0 {
            format!("{days} {} {hours:02}:{minutes:02}:{rest:02}", if russian { "д" } else { "d" })
        } else {
            format!("{hours:02}:{minutes:02}:{rest:02}")
        }
    };
    if now < event.start {
        format!("{} {}", if russian { "До начала:" } else { "Starts in:" }, clock(event.start - now))
    } else if now < event.end {
        format!("{} {}", if russian { "До окончания:" } else { "Ends in:" }, clock(event.end - now))
    } else {
        (if russian { "Событие завершено" } else { "The event has ended" }).to_owned()
    }
}

fn grouped(number: u32) -> String {
    let text = number.to_string();
    let mut out = String::new();
    for (index, digit) in text.chars().enumerate() {
        if index > 0 && (text.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(digit);
    }
    out
}

fn local_midnight(date: chrono::NaiveDate) -> i64 {
    Local
        .from_local_datetime(&date.and_time(NaiveTime::MIN))
        .earliest()
        .map_or(0, |time| time.timestamp())
}

fn format_time(time: i64) -> String {
    DateTime::from_timestamp(time, 0)
        .map(|time| time.with_timezone(&Local))
        .map(|time| format!("{:02}.{:02}.{} {:02}:{:02}", time.day(), time.month(), time.year(), time.hour(), time.minute()))
        .unwrap_or_default()
}

/// The order of the rows: the short events first and the long ones last; the Stygian Onslaught,
/// the Spiral Abyss and the Imaginarium Theater always at the bottom.
fn order_key(event: &Event) -> (u8, i64, i64) {
    let stygian = event.en.contains("Stygian") || event.ru.contains("Мрачный натиск");
    let rank = match event.kind {
        _ if stygian => 1,
        Kind::Abyss => 2,
        Kind::Theater => 3,
        _ => 0,
    };
    // The ones that are the same thing over and over (the Abyss, a weapon wish) keep their order
    // in time, whatever the length of each.
    let length = if rank > 1 { 0 } else { event.end - event.start };
    (rank, length, event.start)
}

/// The row of each event, in the order given: an event has a row of its own, except the ones
/// that follow each other and are the same thing (the periods of the Abyss, the weapon wishes),
/// which lie on one row.  So the rows keep the order of the events.
fn assign_lanes(events: &[&Event]) -> Vec<usize> {
    let mut rows: Vec<(String, i64)> = Vec::new();
    let mut lanes = Vec::with_capacity(events.len());
    for event in events {
        let key = row_key(event);
        let found = rows.iter().position(|(title, end)| *title == key && *end <= event.start + 60);
        let lane = found.unwrap_or_else(|| {
            rows.push((key.clone(), i64::MIN));
            rows.len() - 1
        });
        rows[lane] = (key, event.end);
        lanes.push(lane);
    }
    lanes
}

/// What events that follow each other share: the Abyss, the Theater, and the kind of wish.
fn row_key(event: &Event) -> String {
    match event.kind {
        Kind::Abyss => "abyss".to_owned(),
        Kind::Theater => "theater".to_owned(),
        Kind::Banner => {
            let title = event.en.to_lowercase();
            if title.contains("weapon") || title.contains("divine") || title.contains("epitome") {
                "banner-weapon".to_owned()
            } else {
                format!("banner-{}", event.id)
            }
        }
        _ => format!("event-{}", event.id),
    }
}

/// The fill of a bar whose cover is not there (yet).
fn kind_color(event: &Event) -> Color32 {
    match event.kind {
        Kind::Abyss => Color32::from_rgb(0x3b, 0x82, 0xd6),
        Kind::Theater => Color32::from_rgb(0x6f, 0x6c, 0xe0),
        Kind::Astral => Color32::from_rgb(0x9c, 0x7c, 0xc8),
        Kind::Banner => Color32::from_rgb(0x7f, 0xb6, 0xf5),
        Kind::Event => Color32::from_rgb(0xb4, 0x9a, 0x6e),
    }
}

/// Dark text on light bars, light on dark.
fn text_color(background: Color32) -> Color32 {
    let luma = 0.299 * background.r() as f32 + 0.587 * background.g() as f32 + 0.114 * background.b() as f32;
    if luma > 150. { Color32::from_rgb(0x1a, 0x1d, 0x2a) } else { Color32::WHITE }
}

fn weekday_short(index: usize, russian: bool) -> &'static str {
    const RU: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];
    const EN: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    if russian { RU[index % 7] } else { EN[index % 7] }
}

fn month_name(index: usize, russian: bool) -> &'static str {
    const RU: [&str; 12] = [
        "Январь", "Февраль", "Март", "Апрель", "Май", "Июнь", "Июль", "Август", "Сентябрь", "Октябрь", "Ноябрь", "Декабрь",
    ];
    const EN: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
    ];
    if russian { RU[index % 12] } else { EN[index % 12] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: u64, start: i64, end: i64, kind: Kind, title: &str) -> Event {
        Event {
            id,
            en: title.into(),
            ru: String::new(),
            start,
            end,
            kind,
            banner: String::new(),
            text_en: String::new(),
            text_ru: String::new(),
            link: String::new(),
            primogems: 0,
        }
    }

    #[test]
    fn the_periods_of_the_abyss_lie_on_one_row_and_the_overlapping_ones_apart() {
        let a = event(0, 0, 100, Kind::Abyss, "Spiral Abyss");
        let b = event(0, 100, 200, Kind::Abyss, "Spiral Abyss");
        let c = event(7, 50, 300, Kind::Event, "Something");
        let d = event(8, 60, 400, Kind::Event, "Another");
        let mut all = vec![&a, &b, &c, &d];
        all.sort_by_key(|event| order_key(event));
        let lanes = assign_lanes(&all);
        let lane_of = |event: &Event| lanes[all.iter().position(|other| std::ptr::eq(*other, event)).expect("in")];
        assert_eq!(lane_of(&a), lane_of(&b), "the Abyss in a row");
        assert_ne!(lane_of(&c), lane_of(&d));
        // The shorter event is above the longer one, the Abyss below both.
        assert!(lane_of(&c) < lane_of(&d));
        assert!(lane_of(&d) < lane_of(&a));
    }

    #[test]
    fn the_long_come_last_and_the_periods_of_the_calendar_at_the_bottom() {
        let short = event(1, 0, 10, Kind::Event, "Short");
        let long = event(2, 0, 1000, Kind::Event, "Long");
        let stygian = event(3, 0, 5, Kind::Event, "Stygian Onslaught Event: x");
        let abyss = event(0, 0, 3, Kind::Abyss, "Spiral Abyss");
        let theater = event(0, 0, 4, Kind::Theater, "Imaginarium Theater");
        let mut all = vec![&theater, &long, &abyss, &stygian, &short];
        all.sort_by_key(|event| order_key(event));
        let titles: Vec<&str> = all.iter().map(|event| event.en.as_str()).collect();
        assert_eq!(titles, ["Short", "Long", "Stygian Onslaught Event: x", "Spiral Abyss", "Imaginarium Theater"]);
    }

    #[test]
    fn the_countdown_never_goes_below_zero() {
        let e = event(1, 100, 200, Kind::Event, "x");
        assert_eq!(countdown(&e, 150, false), "Ends in: 00:00:50");
        assert_eq!(countdown(&e, 200, false), "The event has ended");
        assert_eq!(countdown(&e, 5000, true), "Событие завершено");
        assert_eq!(countdown(&e, 40, false), "Starts in: 00:01:00");
        assert_eq!(countdown(&event(1, 0, 90_000, Kind::Event, "x"), 0, true), "До окончания: 1 д 01:00:00");
    }

    #[test]
    fn numbers_are_grouped() {
        assert_eq!(grouped(1234), "1 234");
        assert_eq!(grouped(600), "600");
        assert_eq!(grouped(1_000_000), "1 000 000");
    }
}
