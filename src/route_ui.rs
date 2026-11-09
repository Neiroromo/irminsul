//! The route mode of the map: choosing what to farm, the button that plans the route, the list of
//! its stages, and drawing it over the map (see `route` for the planning).

use std::collections::{BTreeSet, HashSet};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use egui::{Color32, Context, Painter, Pos2, Rect, RichText, Shape, Stroke, Ui, Vec2, vec2};

use crate::achievement_tab::DIM;
use crate::atlas::MapDef;
use crate::icons::Icons;
use crate::markers::{self, Data};
use crate::roster_tab::{Cell, item_cell};
use crate::route::{self, Route, Spot};

/// The colours of the stages, one after another.
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0xff, 0x6b, 0x6b),
    Color32::from_rgb(0x4d, 0xd0, 0xe1),
    Color32::from_rgb(0xff, 0xd5, 0x4f),
    Color32::from_rgb(0x9c, 0xcc, 0x65),
    Color32::from_rgb(0xba, 0x68, 0xc8),
    Color32::from_rgb(0xff, 0x8a, 0x65),
    Color32::from_rgb(0x64, 0xb5, 0xf6),
    Color32::from_rgb(0xf0, 0x62, 0x92),
];

/// How long the planning may improve the route.
const BUDGET: Duration = Duration::from_millis(2500);

fn stage_color(stage: usize) -> Color32 {
    PALETTE[stage % PALETTE.len()]
}

/// A material that can be chosen.
struct Entry {
    id: u32,
    en: String,
    ru: String,
    icon: String,
    rank: u32,
}

/// A route that was planned.
pub struct Done {
    pub map: u32,
    pub route: Route,
    pub teleports: bool,
}

/// What the panel asks the map to do.
pub enum Action {
    None,
    /// Plan the route.
    Generate,
    /// Put this place (left, top, right, bottom) in view.
    Focus([f32; 4]),
}

#[derive(Default)]
pub struct RouteState {
    pub active: bool,
    /// The materials to farm and the kinds of marker (enemies and the like).
    pub items: BTreeSet<u32>,
    pub kinds: BTreeSet<u32>,
    pub teleports: bool,
    /// The kinds of marker the chosen materials are got from, and the markers of the domains.
    pub labels: BTreeSet<u32>,
    pub domains: HashSet<u32>,
    search: String,
    catalog: Option<Vec<Entry>>,
    pub result: Option<Done>,
    /// The stage being walked through (the buttons Start / Next / Back); none: the whole route is shown.
    pub step: Option<usize>,
    job: Option<Receiver<Done>>,
    /// What to tell under the button.
    pub message: String,
}

impl RouteState {
    pub fn new() -> Self {
        Self { teleports: true, ..Self::default() }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn has_targets(&self) -> bool {
        !self.items.is_empty() || !self.kinds.is_empty()
    }

    /// Works out the kinds and the domains the choice stands for.
    pub fn resolve(&mut self, data: &Data) {
        let found = data.where_is(&self.items.iter().copied().collect::<Vec<_>>());
        self.labels = self.kinds.iter().copied().chain(found.labels).collect();
        self.domains = found.points.into_iter().collect();
    }

    /// Takes the result of the planning, when it is done.
    pub fn poll(&mut self) {
        let Some(job) = &self.job else {
            return;
        };
        if let Ok(done) = job.try_recv() {
            self.message = String::new();
            self.result = Some(done);
            self.step = None;
            self.job = None;
        }
    }

    /// Plans the route in the background.
    pub fn start(&mut self, ctx: &Context, map: u32, stops: Vec<Spot>, waypoints: Vec<Spot>, start: Option<Spot>) {
        let (sender, receiver) = channel();
        let teleports = !waypoints.is_empty();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let route = route::plan(&stops, &route::Params { waypoints: &waypoints, start }, BUDGET);
            let _ = sender.send(Done { map, route, teleports });
            ctx.request_repaint();
        });
        self.job = Some(receiver);
        self.step = None;
        self.message = String::new();
    }

    fn catalog(&mut self, data: &Data) -> &[Entry] {
        self.catalog.get_or_insert_with(|| {
            let upgrade = crate::upgrade_store::data();
            let live = crate::live_data::current();
            let mut seen = HashSet::new();
            let mut list = Vec::new();
            for (id, material) in upgrade.materials.iter().chain(live.materials.iter()) {
                if seen.insert(*id) && data.routable(*id) {
                    list.push(Entry {
                        id: *id,
                        en: material.name.get(false).to_owned(),
                        ru: material.name.get(true).to_owned(),
                        icon: material.icon.clone(),
                        rank: material.rarity,
                    });
                }
            }
            list.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.en.cmp(&b.en)));
            list
        })
    }

    /// The panel that takes the place of the filters while the mode is on.
    #[allow(clippy::too_many_arguments)]
    pub fn panel(&mut self, ui: &mut Ui, icons: &Icons, data: &Data, def: Option<&MapDef>, map: u32, russian: bool) -> Action {
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let mut action = Action::None;
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Route", "Маршрут")).size(22.).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Leave the mode", "Выйти из режима")).clicked() {
                    self.active = false;
                }
            });
        });
        ui.label(
            RichText::new(tr(
                "Choose what to farm: materials, or kinds of marker (enemies, plants ...). The route goes over all their markers on this map.",
                "Выберите, что фармить: материалы или виды меток (мобы, растения ...). Маршрут проходит по всем их меткам на этой карте.",
            ))
            .size(12.)
            .color(DIM),
        );
        ui.add_space(4.);
        ui.checkbox(&mut self.teleports, tr("Count the teleports", "Учитывать точки телепорта"));
        ui.add_space(4.);

        // What is chosen.
        if self.has_targets() {
            let mut remove_item = None;
            let mut remove_kind = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6., 6.);
                let upgrade = crate::upgrade_store::data();
                let live = crate::live_data::current();
                for id in &self.items {
                    let Some(material) = upgrade.materials.get(id).or_else(|| live.materials.get(id)) else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        let cell = Cell { icon: &material.icon, rarity: material.rarity.max(1), stars: false, label: None, selected: false };
                        item_cell(ui, icons, 26., &cell);
                        if ui.small_button(format!("{}  ✕", material.name.get(russian))).on_hover_text(tr("Click to remove", "Клик: убрать")).clicked() {
                            remove_item = Some(*id);
                        }
                    });
                }
                for id in &self.kinds {
                    let Some(label) = data.label(*id).filter(|label| !label.name(russian).is_empty()) else {
                        continue;
                    };
                    if ui.small_button(format!("{}  ✕", label.name(russian))).clicked() {
                        remove_kind = Some(*id);
                    }
                }
            });
            if let Some(id) = remove_item {
                self.items.remove(&id);
                changed = true;
            }
            if let Some(id) = remove_kind {
                self.kinds.remove(&id);
                changed = true;
            }
            if ui.small_button(tr("Clear the choice", "Сбросить выбор")).clicked() {
                self.items.clear();
                self.kinds.clear();
                changed = true;
            }
        } else {
            ui.label(RichText::new(tr("Nothing is chosen yet.", "Пока ничего не выбрано.")).color(DIM));
        }

        ui.add_space(4.);
        let generate = ui.add_enabled(
            self.has_targets() && !self.busy(),
            egui::Button::new(RichText::new(tr("Generate the route", "Сгенерировать маршрут")).size(16.).strong())
                .min_size(vec2(ui.available_width(), 34.)),
        );
        if generate.clicked() {
            action = Action::Generate;
        }
        if self.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr("Planning ...", "Строю маршрут ..."));
            });
        }
        if !self.message.is_empty() {
            ui.label(RichText::new(&self.message).color(Color32::from_rgb(0xff, 0xc2, 0x4a)));
        }

        // The result.
        if let Some(done) = self.result.as_ref().filter(|done| done.map == map) {
            ui.add_space(6.);
            let route = &done.route;
            ui.label(
                RichText::new(format!(
                    "{}: {}  ·  {}: {}  ·  {}: {}",
                    tr("Stages", "Этапов"),
                    route.stages.len(),
                    tr("Places", "Точек"),
                    route.stops(),
                    tr("Time", "Время"),
                    duration(route.seconds, russian),
                ))
                .strong(),
            );
            ui.label(
                RichText::new(format!("{}: {:.1} {}", tr("Walk", "Пешком"), route.metres / 1000., tr("km", "км"))).size(12.).color(DIM),
            );
            ui.add_space(4.);
            let count = route.stages.len();
            let mut go: Option<Option<usize>> = None;
            ui.horizontal(|ui| {
                let back_label = format!("◀  {}", tr("Back", "Назад"));
                if ui.add_enabled(self.step.is_some(), egui::Button::new(back_label).min_size(vec2(90., 30.))).clicked() {
                    go = Some(match self.step {
                        Some(0) | None => None,
                        Some(step) => Some(step - 1),
                    });
                }
                match self.step {
                    None => {
                        if ui.add(egui::Button::new(RichText::new(format!("▶  {}", tr("Start", "Начать"))).strong()).min_size(vec2(120., 30.))).clicked() {
                            go = Some(Some(0));
                        }
                    }
                    Some(step) if step + 1 < count => {
                        if ui.add(egui::Button::new(RichText::new(format!("{}  ▶", tr("Next step", "Следующий шаг"))).strong()).min_size(vec2(140., 30.))).clicked() {
                            go = Some(Some(step + 1));
                        }
                    }
                    Some(_) => {
                        if ui.add(egui::Button::new(RichText::new(format!("✔  {}", tr("Finish", "Завершить"))).strong()).min_size(vec2(140., 30.))).clicked() {
                            go = Some(None);
                        }
                    }
                }
                if let Some(step) = self.step {
                    ui.label(RichText::new(format!("{} / {count}", step + 1)).strong());
                }
            });
            ui.add_space(2.);
            for (index, stage) in route.stages.iter().enumerate() {
                let color = stage_color(index);
                let current = self.step == Some(index);
                let first = stage.teleport.as_ref().or(stage.stops.first());
                let place = first.and_then(|spot| def.and_then(|def| region_name(def, spot.at, russian))).unwrap_or_default();
                let how = match &stage.teleport {
                    Some(waypoint) => {
                        let name = data.label(waypoint.label).map_or("", |label| label.name(russian));
                        format!("{} {}", tr("Teleport:", "Телепорт:"), name)
                    }
                    None => tr("Walk from where you are", "Пешком оттуда, где вы стоите").to_owned(),
                };
                let line = ui.horizontal(|ui| {
                    if current {
                        let rect = ui.max_rect().expand2(vec2(4., 2.));
                        ui.painter().rect_filled(rect, 4., color.gamma_multiply(0.22));
                    }
                    let (dot, _) = ui.allocate_exact_size(vec2(26., 26.), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 12., color);
                    ui.painter().circle_stroke(dot.center(), 12., Stroke::new(1.5_f32, Color32::WHITE));
                    ui.painter().text(dot.center(), egui::Align2::CENTER_CENTER, (index + 1).to_string(), egui::FontId::proportional(14.), Color32::BLACK);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("{how}{}", if place.is_empty() { String::new() } else { format!(" · {place}") })).size(13.).strong());
                        ui.label(
                            RichText::new(format!(
                                "{} {}  ·  {}  ·  {:.0} {}",
                                stage.stops.len(),
                                tr("places", "точек"),
                                duration(stage.seconds, russian),
                                stage.metres,
                                tr("m", "м"),
                            ))
                            .size(12.)
                            .color(DIM),
                        );
                    });
                });
                let response = line.response.interact(egui::Sense::click()).on_hover_text(tr("Click: show this stage", "Клик: показать этот этап"));
                if response.clicked() {
                    go = Some(Some(index));
                }
            }
            if let Some(step) = go {
                self.step = step;
                action = match step.and_then(|index| route.stages.get(index)) {
                    Some(stage) => Action::Focus(stage_focus(stage)),
                    None => Action::Focus(route_focus(route)),
                };
            }
            if !done.teleports {
                ui.label(RichText::new(tr("Walking only.", "Только пешком.")).size(12.).color(DIM));
            }
        }
        ui.add_space(8.);
        ui.separator();

        // What to choose from.
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(tr("Search a material or a kind of marker", "Поиск материала или вида метки"))
                .desired_width(ui.available_width()),
        );
        let needle = self.search.trim().to_lowercase();
        ui.add_space(4.);
        ui.label(RichText::new(tr("Materials", "Материалы")).strong());
        let mut toggled = None;
        let items = self.items.clone();
        {
            let catalog = self.catalog(data);
            let mut shown = 0;
            for entry in catalog {
                let name = if russian { &entry.ru } else { &entry.en };
                // Only what is searched is listed: the list is long.
                if needle.is_empty() || !(entry.en.to_lowercase().contains(&needle) || entry.ru.to_lowercase().contains(&needle)) {
                    continue;
                }
                shown += 1;
                if shown > 60 {
                    break;
                }
                ui.horizontal(|ui| {
                    let cell = Cell { icon: &entry.icon, rarity: entry.rank.max(1), stars: false, label: None, selected: false };
                    item_cell(ui, icons, 28., &cell);
                    if ui.selectable_label(items.contains(&entry.id), name).clicked() {
                        toggled = Some(entry.id);
                    }
                });
            }
            if needle.is_empty() {
                ui.label(RichText::new(tr("Type a name to find a material.", "Введите название, чтобы найти материал.")).size(12.).color(DIM));
            } else if shown == 0 {
                ui.label(RichText::new(tr("No such material on the map.", "Такого материала нет на карте.")).size(12.).color(DIM));
            }
        }
        if let Some(id) = toggled {
            if !self.items.remove(&id) {
                self.items.insert(id);
            }
            changed = true;
        }

        ui.add_space(6.);
        ui.label(RichText::new(tr("Kinds of marker (enemies, plants ...)", "Виды меток (мобы, растения ...)")).strong());
        for group in data.groups() {
            let kinds: Vec<&markers::Label> = data
                .children(group.id)
                .filter(|label| !label.name(russian).is_empty() && data.points_of(label.id).iter().any(|point| u32::from(point.map) == map))
                .filter(|label| needle.is_empty() || label.en.to_lowercase().contains(&needle) || label.ru.to_lowercase().contains(&needle))
                .collect();
            if kinds.is_empty() {
                continue;
            }
            let on = kinds.iter().filter(|label| self.kinds.contains(&label.id)).count();
            let mut header = egui::CollapsingHeader::new(RichText::new(format!("{}  ({on}/{})", group.name(russian), kinds.len())))
                .id_salt(("route_group", group.id));
            if !needle.is_empty() {
                header = header.open(Some(true));
            }
            header.show(ui, |ui| {
                for label in kinds {
                    let mut chosen = self.kinds.contains(&label.id);
                    if ui.checkbox(&mut chosen, label.name(russian)).changed() {
                        if chosen {
                            self.kinds.insert(label.id);
                        } else {
                            self.kinds.remove(&label.id);
                        }
                        changed = true;
                    }
                }
            });
        }

        if changed {
            self.resolve(data);
        }
        action
    }
}

/// The view that has a stage, with some room around.
fn stage_focus(stage: &route::Stage) -> [f32; 4] {
    bounds(stage.teleport.iter().chain(&stage.stops))
}

fn route_focus(route: &Route) -> [f32; 4] {
    bounds(route.stages.iter().flat_map(|stage| stage.teleport.iter().chain(&stage.stops)))
}

fn bounds<'a>(spots: impl Iterator<Item = &'a Spot>) -> [f32; 4] {
    let mut rect = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for spot in spots {
        rect = [rect[0].min(spot.at[0]), rect[1].min(spot.at[1]), rect[2].max(spot.at[0]), rect[3].max(spot.at[1])];
    }
    let pad = ((rect[2] - rect[0]).max(rect[3] - rect[1]) * 0.1).max(30.);
    [rect[0] - pad, rect[1] - pad, rect[2] + pad, rect[3] + pad]
}

/// The smallest region of the map that has the place.
fn region_name(def: &MapDef, at: [f32; 2], russian: bool) -> Option<String> {
    def.top_anchors()
        .filter(|anchor| anchor.rect[0] <= at[0] && at[0] <= anchor.rect[2] && anchor.rect[1] <= at[1] && at[1] <= anchor.rect[3])
        .min_by(|a, b| {
            let area = |rect: &[f32; 4]| (rect[2] - rect[0]) * (rect[3] - rect[1]);
            area(&a.rect).total_cmp(&area(&b.rect))
        })
        .map(|anchor| anchor.name(russian).to_owned())
}

fn duration(seconds: f32, russian: bool) -> String {
    let total = seconds.round().max(0.) as u32;
    let (minutes, rest) = (total / 60, total % 60);
    if russian { format!("{minutes} мин {rest} с") } else { format!("{minutes} min {rest} s") }
}

/// A line with a dark edge, so that it shows on any map.
fn line(painter: &Painter, a: Pos2, b: Pos2, color: Color32, active: bool) {
    let (edge, width) = if active { (200, 4.0_f32) } else { (50, 2.5_f32) };
    painter.line_segment([a, b], Stroke::new(width + 3.5, Color32::from_black_alpha(edge)));
    painter.line_segment([a, b], Stroke::new(width, color));
}

/// An arrow head at a place, along a direction.
fn arrow(painter: &Painter, at: Pos2, direction: Vec2, color: Color32) {
    let side = vec2(-direction.y, direction.x);
    let points = vec![at + direction * 9., at - direction * 6. + side * 7., at - direction * 6. - side * 7.];
    painter.add(Shape::convex_polygon(points, color, Stroke::new(1.5_f32, Color32::from_black_alpha(230))));
}

/// A dashed jump from one place to another, with arrows along it and one at its end.
fn dashed(painter: &Painter, a: Pos2, b: Pos2, color: Color32) {
    let length = (b - a).length();
    if length < 1. {
        return;
    }
    let direction = (b - a) / length;
    let count = ((length / 16.) as usize).clamp(1, 400);
    for step in 0..count {
        let from = a + (b - a) * (step as f32 / count as f32);
        let to = a + (b - a) * ((step as f32 + 0.55) / count as f32);
        painter.line_segment([from, to], Stroke::new(5.5_f32, Color32::from_black_alpha(200)));
        painter.line_segment([from, to], Stroke::new(3.0_f32, Color32::WHITE));
    }
    let along = (length / 220.) as usize;
    for k in 0..=along {
        let at = a + direction * length * ((k as f32 + 0.5) / (along as f32 + 1.));
        arrow(painter, at, direction, color);
    }
    if length > 60. {
        arrow(painter, b - direction * 24., direction, color);
    }
}

fn badge(painter: &Painter, at: Pos2, radius: f32, color: Color32, text: &str, size: f32) {
    painter.circle_filled(at, radius, color);
    painter.circle_stroke(at, radius, Stroke::new(2.0_f32, Color32::WHITE));
    painter.circle_stroke(at, radius + 1.5, Stroke::new(1.5_f32, Color32::from_black_alpha(220)));
    painter.text(at, egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(size), Color32::BLACK);
}

/// Draws the route over the map: lines with arrows (as thick at any zoom), numbered places, and
/// the number of each stage where it begins.  `to_screen` takes a place from the origin of the
/// map.
pub fn draw(state: &RouteState, painter: &Painter, area: Rect, to_screen: &dyn Fn(Vec2) -> Pos2, origin: Vec2, floors: &HashSet<u32>, map: u32) {
    let Some(done) = state.result.as_ref().filter(|done| done.map == map) else {
        return;
    };
    let place = |spot: &Spot| to_screen(origin + vec2(spot.at[0], spot.at[1]));
    let total = done.route.stops();
    // The numbers are legible only while there are not too many places.
    let numbered = total <= 300;
    let mut number = 0usize;
    let mut previous_end: Option<Pos2> = None;
    for (index, stage) in done.route.stages.iter().enumerate() {
        // While stepping, the stage that is walked stands out, the others are faint.
        let active = state.step.is_none_or(|step| step == index);
        let color = if active { stage_color(index) } else { stage_color(index).gamma_multiply(0.3) };
        let mut path: Vec<Pos2> = Vec::new();
        if let Some(waypoint) = &stage.teleport {
            path.push(place(waypoint));
        }
        path.extend(stage.stops.iter().map(&place));
        // The jump from where the last stage ended to the teleport.
        if let (Some(from), Some(to)) = (previous_end, path.first()) {
            if active && Rect::from_two_pos(from, *to).intersects(area) {
                dashed(painter, from, *to, color);
            }
        }
        for pair in path.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if !Rect::from_two_pos(a, b).expand(10.).intersects(area) {
                continue;
            }
            line(painter, a, b, color, active);
            let length = (b - a).length();
            if active && length > 34. {
                let direction = (b - a) / length;
                let count = (length / 170.) as usize;
                for k in 0..=count {
                    let at = a + direction * length * ((k as f32 + 0.5) / (count as f32 + 1.));
                    if area.contains(at) {
                        arrow(painter, at, direction, color);
                    }
                }
            }
        }
        previous_end = path.last().copied();
        // The places, numbered over all the stages.
        for stop in &stage.stops {
            number += 1;
            let at = place(stop);
            if !area.expand(20.).contains(at) {
                continue;
            }
            let dim = if floors.contains(&u32::from(stop.floor)) { 1. } else { 0.45 };
            let dim = if active { dim } else { dim * 0.8 };
            if numbered {
                let radius = if number < 100 { 9. } else { 11.5 };
                painter.circle_filled(at, radius, color.gamma_multiply(dim));
                painter.circle_stroke(at, radius, Stroke::new(1.5_f32, Color32::WHITE.gamma_multiply(dim)));
                painter.text(at, egui::Align2::CENTER_CENTER, number.to_string(), egui::FontId::proportional(if number < 100 { 11. } else { 10. }), Color32::BLACK);
            } else {
                painter.circle_filled(at, 4.5, color.gamma_multiply(dim));
                painter.circle_stroke(at, 4.5, Stroke::new(1.5_f32, Color32::WHITE.gamma_multiply(dim)));
            }
        }
        // The number of the stage, where it begins.
        let label = (index + 1).to_string();
        match (&stage.teleport, stage.stops.first()) {
            (Some(waypoint), _) => {
                let at = place(waypoint);
                if area.expand(30.).contains(at) {
                    badge(painter, at, 15., color, &label, 17.);
                }
            }
            (None, Some(first)) => {
                let at = place(first) + vec2(-20., -20.);
                if area.expand(30.).contains(at) {
                    painter.line_segment([at, place(first)], Stroke::new(2.0_f32, Color32::WHITE));
                    badge(painter, at, 14., color, &label, 16.);
                }
            }
            _ => {}
        }
    }
}

/// The places of the chosen kinds on a map (and the domains chosen), that are to be visited:
/// `skip` leaves out what is already collected.
pub fn targets(state: &RouteState, data: &Data, map: u32, skip: &dyn Fn(&markers::Point, &markers::Label) -> bool) -> Vec<Spot> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut take = |point: &markers::Point| {
        if u32::from(point.map) == map && seen.insert(point.id) {
            out.push(Spot { id: point.id, label: point.label, at: [point.x, point.y], floor: point.floor });
        }
    };
    for label_id in &state.labels {
        let Some(label) = data.label(*label_id) else {
            continue;
        };
        for point in data.points_of(*label_id) {
            if !skip(point, label) {
                take(point);
            }
        }
    }
    for point in data.points_of(markers::DOMAIN_LABEL) {
        if state.domains.contains(&point.id) {
            take(point);
        }
    }
    out
}

/// The waypoints of a map that can be teleported to.
pub fn waypoints(data: &Data, map: u32, labels: &[u32]) -> Vec<Spot> {
    labels
        .iter()
        .flat_map(|label| data.points_of(*label))
        .filter(|point| u32::from(point.map) == map)
        .map(|point| Spot { id: point.id, label: point.label, at: [point.x, point.y], floor: point.floor })
        .collect()
}
