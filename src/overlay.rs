//! A floating window with the materials to farm, to keep over the game.
//!
//! It is always on top and mouse clicks go through it, so it does not get in
//! the way.  Its size is that of what it shows: it grows and shrinks with the
//! pictures.  While Alt is held it can be used: it widens by a strip on its
//! right with the buttons (to close it, move it, set how see-through it is, and
//! resize it), and a layer over the pictures says it can be moved.  Resizing it
//! makes the pictures fill it.  What it shows follows the numbers of the planner
//! and what is in the inventory, as they change; what is all there is, is taken
//! off with a fade, and the rest move up to fill its place.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use egui::{
    Align2, Color32, Context, CursorIcon, FontId, Frame, Id, Margin, Order, Pos2, Rect, RichText,
    Sense, Vec2, ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2,
};
use serde::{Deserialize, Serialize};

use crate::icons::Icons;
use crate::live_data;
use crate::planner_tab::{Family, chain_top, family_percent};
use crate::roster_tab::{BAD, Cell, MORA, OK, compact, item_cell, paint_cell_scaled};
use crate::upgrade_store;

const TITLE: &str = "Irminsul overlay";
const CARD: Color32 = Color32::from_rgb(22, 25, 44);
const STRIP_FILL: Color32 = Color32::from_rgb(14, 16, 30);
/// How big a picture is meant to be before scaling.
const WANTED_CELL: f32 = 72.;
const GAP: f32 = 6.;
const MARGIN: f32 = 6.;
/// The strip of buttons, which is there only while Alt is held.  The window is
/// never taller for it: the buttons go in as many columns as the height needs.
const STRIP_PAD: f32 = 4.;
const STRIP_GAP: f32 = 4.;
const STRIP_BUTTON: f32 = 28.;
/// The corner of the window that resizes it, a square this big.
const CORNER: f32 = 22.;
/// The most columns a new window has.
const START_COLUMNS: usize = 4;
/// How fast a picture fades, per second, and how fast the others move up.
const FADE_RATE: f32 = 2.6;
const MOVE_RATE: f32 = 11.;
const AMBER: Color32 = Color32::from_rgb(0xff, 0xc3, 0x4a);

fn default_one() -> f32 {
    1.
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OverlaySettings {
    /// Not kept: the window is not there when the program starts.
    #[serde(skip)]
    pub open: bool,
    /// The materials it shows, picked when it was made.
    pub items: Vec<u32>,
    pub pos: Option<[f32; 2]>,
    /// Of what it shows, without the strip of buttons.
    pub size: [f32; 2],
    /// 0.25 to 1.
    pub opacity: f32,
    /// How big the pictures are, 0.4 to 3.  Resizing the window changes it.
    #[serde(default = "default_one")]
    pub icon_scale: f32,
    /// How big the text in them is, 0.6 to 1.7.
    #[serde(default = "default_one")]
    pub text_scale: f32,
    /// Shown only while Genshin Impact is the window in front: not over
    /// Irminsul or any other window.
    #[serde(default)]
    pub only_in_focus: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            open: false,
            items: Vec::new(),
            pos: None,
            size: [340., 300.],
            opacity: 0.9,
            icon_scale: 1.,
            text_scale: 1.,
            only_in_focus: false,
        }
    }
}

/// The list the user picks from before the window is made.
struct Choice {
    /// Each material that is needed, and whether it is picked.
    entries: Vec<(u32, bool)>,
}

/// What has been shown of a picture: how much of it is there (it fades out
/// when it is done) and where it is, moving to where it belongs.
struct Anim {
    fade: f32,
    pos: Pos2,
    /// The number that is shown (per cent, or how many there are): it goes to the real one a
    /// step at a time, through the ones between.
    value: f32,
}

/// A picture in the window, one material or a whole chain of them.
struct Entry {
    /// What tells it from the others.
    key: u64,
    icon_id: u32,
    label: String,
    color: Color32,
    /// All of it is there.
    done: bool,
    /// The number in the label (per cent, or how many there are) and, if it is of the kind
    /// "there/needed", how many are needed: for showing the number going to its value.
    value: f32,
    need: Option<u64>,
}

/// A line of the list to pick from.
struct Row {
    /// The entries of the choice it is of: one, or all of a chain.
    members: Vec<usize>,
    icon_id: u32,
    name: String,
    label: String,
    color: Color32,
    /// Not all of it is there.
    missing: bool,
}

pub struct Overlay {
    pub settings: OverlaySettings,
    choice: Option<Choice>,
    /// Where the window was made, which it keeps being asked for, so that what
    /// the user did to it is not undone.
    spawn: Option<([f32; 2], Option<[f32; 2]>)>,
    last_alt: Option<bool>,
    applied_alpha: Option<u8>,
    slider_open: bool,
    /// Geometry or opacity changed and is not saved yet.
    dirty: bool,
    anim: HashMap<u64, Anim>,
    /// How many pictures there were, to see when the window is to change size.
    last_count: Option<usize>,
    /// Frames to wait after the size was set before taking the window's size as
    /// what the user made it.
    settle: u8,
    /// Columns the pictures are in.
    columns: usize,
}

/// The columns and the size of a picture that make `count` pictures fill a space
/// the most.  A grid whose last row is as full as the others is chosen over one
/// with a gap in it, if it is nearly as big, so that there is no empty place.
fn fit(count: usize, width: f32, height: f32) -> (usize, f32) {
    let count = count.max(1);
    let cell_of = |columns: usize| {
        let rows = count.div_ceil(columns);
        ((width - GAP * (columns as f32 - 1.)) / columns as f32)
            .min((height - GAP * (rows as f32 - 1.)) / rows as f32)
    };
    let mut best = (1, f32::MIN);
    for columns in 1..=count {
        let cell = cell_of(columns);
        if cell > best.1 {
            best = (columns, cell);
        }
    }
    let whole = (1..=count)
        .filter(|columns| count % columns == 0)
        .map(|columns| (columns, cell_of(columns)))
        .filter(|(_, cell)| *cell >= best.1 * 0.85)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let (columns, cell) = whole.unwrap_or(best);
    (columns, cell.max(16.))
}

/// The size of the window for pictures of a size in a number of columns.
fn grid_size(count: usize, columns: usize, cell: f32) -> Vec2 {
    let columns = columns.clamp(1, count.max(1));
    let rows = count.max(1).div_ceil(columns);
    vec2(
        columns as f32 * cell + (columns as f32 - 1.) * GAP + 2. * MARGIN,
        rows as f32 * cell + (rows as f32 - 1.) * GAP + 2. * MARGIN,
    )
}

impl Overlay {
    pub fn new(mut settings: OverlaySettings) -> Self {
        // It is not there when the program starts.
        settings.open = false;
        Self {
            settings,
            choice: None,
            spawn: None,
            last_alt: None,
            applied_alpha: None,
            slider_open: false,
            dirty: false,
            anim: HashMap::new(),
            last_count: None,
            settle: 0,
            columns: START_COLUMNS,
        }
    }

    pub fn is_open(&self) -> bool {
        self.settings.open
    }

    pub fn is_choosing(&self) -> bool {
        self.choice.is_some()
    }

    /// Starts picking what the window is to show: everything that is needed.
    /// If the window is up, what it shows is picked; if not, what is missing.
    pub fn start_choosing(&mut self, needs: &BTreeMap<u32, u32>, owned: &HashMap<u32, u32>) {
        let showing = self.settings.open && !self.settings.items.is_empty();
        let mut entries: Vec<(u32, bool)> = needs
            .iter()
            .filter(|(_, need)| **need > 0)
            .map(|(id, need)| {
                let picked = if showing {
                    self.settings.items.contains(id)
                } else {
                    owned.get(id).copied().unwrap_or(0) < *need
                };
                (*id, picked)
            })
            .collect();
        entries.sort_by_key(|(id, _)| (*id != MORA, *id));
        self.choice = Some(Choice { entries });
    }

    /// The window to pick from, which is a window of its own, so it can be put
    /// anywhere, outside the program's window too.  The sliders in it act on
    /// the floating window at once.  Chains of materials are one line each when
    /// they are shown as per cent.  Returns whether the settings changed and
    /// are to be saved.
    #[allow(clippy::too_many_arguments)]
    pub fn choosing_ui(
        &mut self,
        ctx: &Context,
        icons: &Icons,
        owned: &HashMap<u32, u32>,
        needs: &BTreeMap<u32, u32>,
        families: &[Family],
        percent: bool,
        russian: bool,
    ) -> bool {
        if self.choice.is_none() {
            return false;
        }
        let tr = |en: &'static str, ru: &'static str| if russian { ru } else { en };
        let data = upgrade_store::data();
        let live = live_data::current();
        let mut confirmed = false;
        let mut cancelled = false;
        let mut changed = false;
        let settings = &mut self.settings;
        let choice = self.choice.as_mut().unwrap();

        // The lines: one for each material, or for each chain.
        let have = |id: u32| owned.get(&id).copied().unwrap_or(0) as u64;
        let need = |id: u32| needs.get(&id).copied().unwrap_or(0) as u64;
        let mut rows: Vec<Row> = Vec::new();
        let mut taken: HashSet<usize> = HashSet::new();
        for (index, (id, _)) in choice.entries.iter().enumerate() {
            let family = percent
                .then(|| {
                    families
                        .iter()
                        .position(|family| family.ids.len() >= 2 && family.ids.contains(id))
                })
                .flatten();
            let name_of = |id: u32| {
                data.materials
                    .get(&id)
                    .or_else(|| live.materials.get(&id))
                    .map_or_else(|| format!("#{id}"), |m| m.name.get(russian).to_owned())
            };
            match family {
                Some(family_index) => {
                    if !taken.insert(family_index) {
                        continue;
                    }
                    let family = &families[family_index];
                    let members: Vec<usize> = choice
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, (other, _))| family.ids.contains(other))
                        .map(|(other, _)| other)
                        .collect();
                    let whole = (family_percent(family, need, have) * 100.).floor() as u32;
                    let top = chain_top(family, need);
                    rows.push(Row {
                        members,
                        icon_id: top,
                        name: name_of(top),
                        label: format!("{whole}%"),
                        color: if whole >= 100 { OK } else { AMBER },
                        missing: whole < 100,
                    });
                }
                None => rows.push(Row {
                    members: vec![index],
                    icon_id: *id,
                    name: name_of(*id),
                    label: format!("{}/{}", compact(have(*id)), compact(need(*id))),
                    color: if have(*id) >= need(*id) { OK } else { BAD },
                    missing: have(*id) < need(*id),
                }),
            }
        }

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("overlay_choice_window"),
            ViewportBuilder::default()
                .with_title("Irminsul floating window")
                .with_inner_size([500., 680.])
                .with_min_inner_size([380., 360.]),
            |ctx, _class| {
                icons.poll();
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(tr(
                                "Pick what the window is to show. What you do not need to watch \
                                 now, take off.",
                                "Выберите, что покажет окно. Что сейчас не нужно отслеживать, \
                                 уберите.",
                            ))
                            .size(13.),
                        )
                        .wrap(),
                    );
                    ui.separator();

                    // How it looks, which the window follows at once.
                    egui::Grid::new("overlay_look").num_columns(2).show(ui, |ui| {
                        ui.label(tr("Opacity", "Прозрачность"));
                        changed |= ui
                            .add(egui::Slider::new(&mut settings.opacity, 0.25..=1.0).show_value(false))
                            .changed();
                        ui.end_row();
                        ui.label(tr("Icon size", "Размер иконок"));
                        changed |= ui
                            .add(egui::Slider::new(&mut settings.icon_scale, 0.4..=3.0).show_value(false))
                            .changed();
                        ui.end_row();
                        ui.label(tr("Text size", "Размер текста"));
                        changed |= ui
                            .add(egui::Slider::new(&mut settings.text_scale, 0.6..=1.7).show_value(false))
                            .changed();
                        ui.end_row();
                    });
                    changed |= ui
                        .checkbox(
                            &mut settings.only_in_focus,
                            tr("Only over the game", "Только поверх игры"),
                        )
                        .changed();
                    ui.label(
                        RichText::new(tr(
                            "The darker band under the text grows and darkens with the text.",
                            "Тёмная подложка под текстом растёт и темнеет вместе с текстом.",
                        ))
                        .size(11.)
                        .color(Color32::from_gray(0x90)),
                    );
                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button(tr("Missing", "Недостающее")).clicked() {
                            for row in &rows {
                                for member in &row.members {
                                    choice.entries[*member].1 = row.missing;
                                }
                            }
                        }
                        if ui.button(tr("All", "Все")).clicked() {
                            choice.entries.iter_mut().for_each(|(_, picked)| *picked = true);
                        }
                        if ui.button(tr("None", "Никакие")).clicked() {
                            choice.entries.iter_mut().for_each(|(_, picked)| *picked = false);
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height((ui.available_height() - 50.).max(120.))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for row in &rows {
                                let material = data
                                    .materials
                                    .get(&row.icon_id)
                                    .or_else(|| live.materials.get(&row.icon_id));
                                // A chain is picked if any of it is.
                                let mut picked = row
                                    .members
                                    .iter()
                                    .any(|member| choice.entries[*member].1);
                                ui.horizontal(|ui| {
                                    let cell = Cell {
                                        icon: material.map_or("", |m| m.icon.as_str()),
                                        rarity: material.map_or(1, |m| m.rarity.max(1)),
                                        stars: false,
                                        label: None,
                                        selected: false,
                                    };
                                    item_cell(ui, icons, 34., &cell);
                                    ui.label(RichText::new(&row.name).size(14.));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.checkbox(&mut picked, "").changed() {
                                                for member in &row.members {
                                                    choice.entries[*member].1 = picked;
                                                }
                                            }
                                            ui.label(
                                                RichText::new(&row.label)
                                                    .size(13.)
                                                    .strong()
                                                    .color(row.color),
                                            );
                                        },
                                    );
                                });
                            }
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        let any = choice.entries.iter().any(|(_, picked)| *picked);
                        if ui
                            .add_enabled(any, egui::Button::new(tr("Create window", "Создать окно")))
                            .clicked()
                        {
                            confirmed = true;
                        }
                        if ui.button(tr("Close", "Закрыть")).clicked() {
                            cancelled = true;
                        }
                    });
                });
                if ctx.input(|input| input.viewport().close_requested()) {
                    cancelled = true;
                }
            },
        );

        if changed {
            // The size of the pictures was changed: the window fits to them.
            self.last_count = None;
        }
        if confirmed {
            if let Some(choice) = self.choice.take() {
                self.settings.items = choice
                    .entries
                    .into_iter()
                    .filter(|(_, picked)| *picked)
                    .map(|(id, _)| id)
                    .collect();
                self.settings.open = true;
                self.spawn = None;
                self.anim.clear();
                self.last_count = None;
                return true;
            }
        }
        if cancelled {
            self.choice = None;
        }
        changed
    }

    /// Call every frame.  Returns whether the settings changed and are to be
    /// saved.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        ctx: &Context,
        icons: &Icons,
        owned: &HashMap<u32, u32>,
        needs: &BTreeMap<u32, u32>,
        families: &[Family],
        percent: bool,
        russian: bool,
    ) -> bool {
        if !self.settings.open {
            self.spawn = None;
            self.applied_alpha = None;
            self.last_alt = None;
            self.slider_open = false;
            self.last_count = None;
            watch(false, false, ctx);
            return false;
        }
        let alt = alt_down();
        // Out of sight when something else is in front, if that is what is
        // wanted.  It is hidden, not closed, so it comes back as it was.  A
        // thread of its own sees to it, so that it does not wait for a frame.
        watch(true, self.settings.only_in_focus, ctx);
        let entries = entries(&self.settings.items, owned, needs, families, percent);
        let wanted_cell = WANTED_CELL * self.settings.icon_scale.clamp(0.4, 3.);

        // The first size: the pictures at their size in a few columns.
        let first_columns = entries.len().clamp(1, START_COLUMNS);
        let start = *self.spawn.get_or_insert_with(|| {
            let size = grid_size(entries.len(), first_columns, wanted_cell);
            ([size.x, size.y], self.settings.pos)
        });
        let mut builder = ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(start.0)
            .with_min_inner_size([60., 60.])
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_active(false)
            // Only the corner resizes it, and only while Alt is held.
            .with_resizable(false)
            .with_mouse_passthrough(true);
        if let Some(pos) = start.1 {
            builder = builder.with_position(pos);
        }

        let text_scale = self.settings.text_scale;
        let mut opacity = self.settings.opacity;
        let mut close = false;
        let mut slider_open = self.slider_open;
        let was_alt = self.last_alt;
        let mut content = self.settings.size;
        let mut position: Option<Pos2> = None;
        let mut new_scale: Option<f32> = None;
        let anim = &mut self.anim;
        let last_count = self.last_count;
        let mut settle = self.settle;
        let mut columns = self.columns;
        let mut shown_count = 0;
        let mut user_resized = false;

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("overlay_window"),
            builder,
            |ctx, _class| {
                icons.poll();
                let dt = ctx.input(|input| input.stable_dt).min(0.1);
                let viewport = ctx.input(|input| input.viewport().clone());
                let inner = viewport.inner_rect.map(|rect| rect.size());
                if let Some(outer) = viewport.outer_rect {
                    position = Some(outer.min);
                }

                if was_alt != Some(alt) {
                    // Clicks go through it unless Alt is held, and it is wider by
                    // the strip of buttons.
                    ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(!alt));
                    if was_alt.is_some() {
                        // Wider by the strip, and not taller.
                        let width = content[0] + if alt { strip_layout(content[1]).2 } else { 0. };
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(width, content[1])));
                        settle = 4;
                    }
                }
                settle = settle.saturating_sub(1);

                // What the user made of the size, while resizing with Alt held.
                if alt && settle == 0 {
                    if let Some(inner) = inner {
                        let size = [(inner.x - strip_layout(inner.y).2).max(40.), inner.y];
                        if (size[0] - content[0]).abs() > 1. || (size[1] - content[1]).abs() > 1. {
                            content = size;
                            user_resized = true;
                        }
                    }
                }

                let mut count = 0;
                let window_height = inner.map_or(content[1], |i| i.y);
                let layout = strip_layout(window_height);
                let mut opacity_button: Option<(Rect, bool)> = None;
                // How many pictures there are in the grid, for resizing it.
                let in_grid = entries
                    .iter()
                    .filter(|entry| anim.get(&entry.key).is_none_or(|state| state.fade > 0.004))
                    .count();
                if alt {
                    egui::SidePanel::right("overlay_strip")
                        .exact_width(layout.2)
                        .resizable(false)
                        .frame(Frame::new().fill(STRIP_FILL).inner_margin(Margin::ZERO))
                        .show(ctx, |ui| {
                            let result = strip(
                                ui,
                                layout,
                                inner.unwrap_or(vec2(content[0] + layout.2, content[1])),
                            );
                            close = result.close;
                            opacity_button = Some((result.opacity_rect, result.opacity_hovered));
                            // The window is made as big as the pictures that
                            // fill the space the corner was dragged to, so that
                            // nothing is left empty.
                            if let Some(wanted) = result.resize_to
                                && in_grid > 0
                            {
                                let height = wanted.y.max(40.);
                                let width = (wanted.x - strip_layout(height).2).max(40.);
                                let (fit_columns, cell) =
                                    fit(in_grid, width - 2. * MARGIN, height - 2. * MARGIN);
                                let size = grid_size(in_grid, fit_columns, cell);
                                columns = fit_columns;
                                content = [size.x, size.y];
                                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(
                                    size.x + strip_layout(size.y).2,
                                    size.y,
                                )));
                                settle = 2;
                                user_resized = true;
                            }
                        });
                }
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(CARD).inner_margin(Margin::same(MARGIN as i8)))
                    .show(ctx, |ui| {
                        let (moving, shown, used_columns, cell) = draw_entries(
                            ui, icons, anim, &entries, text_scale, dt, russian, columns,
                        );
                        count = shown;
                        columns = used_columns;
                        let _ = cell;
                        if moving {
                            ctx.request_repaint_after(Duration::from_millis(16));
                        }
                    });
                if alt {
                    egui::Area::new(Id::new("overlay_alt_layer"))
                        .order(Order::Foreground)
                        .fixed_pos(Pos2::ZERO)
                        .show(ctx, |ui| {
                            let rect = Rect::from_min_size(
                                Pos2::ZERO,
                                vec2(inner.map_or(content[0], |i| i.x) - layout.2, window_height),
                            );
                            drag_layer(ui, rect, russian);
                            if let Some((button, hovered)) = opacity_button {
                                opacity_slider(ui, rect, button, hovered, &mut opacity, &mut slider_open);
                            }
                        });
                }

                // The window is the size of what it shows: when a picture goes
                // (or the pictures change size) it follows, unless the user is
                // resizing it.
                shown_count = count;
                if !alt && settle == 0 && last_count != Some(count) && count > 0 {
                    let size = grid_size(count, columns.min(count), wanted_cell);
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                    content = [size.x, size.y];
                    settle = 4;
                }
                ctx.request_repaint_after(Duration::from_millis(50));
            },
        );

        self.last_alt = Some(alt);
        self.slider_open = slider_open;
        self.settle = settle;
        self.columns = columns;
        self.last_count = Some(shown_count);
        if (opacity - self.settings.opacity).abs() > f32::EPSILON {
            self.settings.opacity = opacity;
            self.dirty = true;
        }
        // What the user made of the size is what the pictures fill: how big they
        // are in it is what is kept.
        if user_resized && shown_count > 0 {
            let (_, cell) = fit(
                shown_count,
                content[0] - 2. * MARGIN,
                content[1] - 2. * MARGIN,
            );
            new_scale = Some((cell / WANTED_CELL).clamp(0.4, 3.));
        }
        if self.settle == 0 {
            if (content[0] - self.settings.size[0]).abs() > 1.5
                || (content[1] - self.settings.size[1]).abs() > 1.5
            {
                self.settings.size = content;
                self.dirty = true;
            }
            if let Some(scale) = new_scale
                && (scale - self.settings.icon_scale).abs() > 0.02
            {
                self.settings.icon_scale = scale;
                self.dirty = true;
            }
            if let Some(pos) = position {
                let moved = self
                    .settings
                    .pos
                    .is_none_or(|old| (old[0] - pos.x).abs() > 1.5 || (old[1] - pos.y).abs() > 1.5);
                if moved {
                    self.settings.pos = Some([pos.x, pos.y]);
                    self.dirty = true;
                }
            }
        }

        // How see-through it is is a property of the window as a whole.
        let alpha = (self.settings.opacity.clamp(0.25, 1.) * 255.) as u8;
        if self.applied_alpha != Some(alpha) && set_window_alpha(alpha) {
            self.applied_alpha = Some(alpha);
        }

        if close {
            self.settings.open = false;
            self.dirty = false;
            return true;
        }
        // Saved when Alt is let go, not while the window is being moved.
        if self.dirty && !alt {
            self.dirty = false;
            return true;
        }
        false
    }
}

/// What the window shows: each material of the list, or, when chains of
/// materials are shown as per cent, one picture for each chain.
fn entries(
    items: &[u32],
    owned: &HashMap<u32, u32>,
    needs: &BTreeMap<u32, u32>,
    families: &[Family],
    percent: bool,
) -> Vec<Entry> {
    let have = |id: u32| owned.get(&id).copied().unwrap_or(0) as u64;
    let need = |id: u32| needs.get(&id).copied().unwrap_or(0) as u64;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in items {
        let family = percent
            .then(|| {
                families
                    .iter()
                    .position(|family| family.ids.len() >= 2 && family.ids.contains(id))
            })
            .flatten();
        match family {
            Some(index) => {
                if !seen.insert(index) {
                    continue;
                }
                let family = &families[index];
                let fraction = family_percent(family, need, have);
                let whole = (fraction * 100.).floor() as u32;
                out.push(Entry {
                    key: (1 << 40) | index as u64,
                    icon_id: chain_top(family, need),
                    label: format!("{whole}%"),
                    color: if whole >= 100 { OK } else { AMBER },
                    done: whole >= 100,
                    value: whole as f32,
                    need: None,
                });
            }
            None => {
                let (have, need) = (have(*id), need(*id));
                out.push(Entry {
                    key: *id as u64,
                    icon_id: *id,
                    label: format!("{}/{}", compact(have), compact(need)),
                    color: if have >= need { OK } else { BAD },
                    done: have >= need,
                    value: have as f32,
                    need: Some(need),
                });
            }
        }
    }
    out
}

/// Draws the pictures so that they fill the space.  A picture whose material is
/// all there is fades and shrinks, and when it has gone the others slide into
/// its place.  Returns whether anything is still moving, how many pictures are
/// in the grid, the columns they are in and how big each is.
#[allow(clippy::too_many_arguments)]
fn draw_entries(
    ui: &mut egui::Ui,
    icons: &Icons,
    anim: &mut HashMap<u64, Anim>,
    entries: &[Entry],
    text_scale: f32,
    dt: f32,
    russian: bool,
    previous_columns: usize,
) -> (bool, usize, usize, f32) {
    let data = upgrade_store::data();
    let live = live_data::current();
    let mut moving = false;

    // How far each has faded: towards gone if it is done, towards there if not.
    for entry in entries {
        let target = if entry.done { 0. } else { 1. };
        let state = anim.entry(entry.key).or_insert(Anim {
            fade: target,
            pos: Pos2::ZERO,
            value: entry.value,
        });
        let step = FADE_RATE * dt;
        if (state.fade - target).abs() > 0.004 {
            state.fade += (target - state.fade).clamp(-step, step);
            moving = true;
        } else {
            state.fade = target;
        }
        // The number goes to its value through the ones between: at least 14 a second, and
        // all of the way in half a second at most.
        let gap = (entry.value - state.value).abs();
        if gap > 0.01 {
            let speed = (gap / 0.5).max(14.);
            let step = speed * dt;
            state.value += (entry.value - state.value).clamp(-step, step);
            moving = true;
        } else {
            state.value = entry.value;
        }
    }
    anim.retain(|key, _| entries.iter().any(|entry| entry.key == *key));

    // What is still in the grid: not done, or not yet gone.
    let shown: Vec<&Entry> = entries
        .iter()
        .filter(|entry| anim[&entry.key].fade > 0.004)
        .collect();

    let area = ui.available_rect_before_wrap();
    if entries.is_empty() {
        ui.painter().text(
            area.center(),
            Align2::CENTER_CENTER,
            if russian { "Нечего отслеживать" } else { "Nothing to watch" },
            FontId::proportional(14.),
            Color32::from_gray(0xa0),
        );
        return (false, 0, previous_columns, WANTED_CELL);
    }
    if shown.is_empty() {
        ui.painter().text(
            area.center(),
            Align2::CENTER_CENTER,
            if russian { "Всё собрано" } else { "All collected" },
            FontId::proportional((area.width() * 0.12).clamp(14., 26.)),
            OK,
        );
        return (moving, 0, previous_columns, WANTED_CELL);
    }

    let (columns, cell) = fit(shown.len(), area.width(), area.height());
    for (index, entry) in shown.iter().enumerate() {
        let (column, row) = (index % columns, index / columns);
        let target = area.min + vec2(column as f32 * (cell + GAP), row as f32 * (cell + GAP));
        let state = anim.get_mut(&entry.key).unwrap();
        if state.pos == Pos2::ZERO {
            state.pos = target;
        }
        state.pos += (target - state.pos) * (1. - (-MOVE_RATE * dt).exp());
        if (state.pos - target).length() < 0.4 {
            state.pos = target;
        } else {
            moving = true;
        }

        let material = data
            .materials
            .get(&entry.icon_id)
            .or_else(|| live.materials.get(&entry.icon_id));
        let cell_look = Cell {
            icon: material.map_or("", |m| m.icon.as_str()),
            rarity: material.map_or(1, |m| m.rarity.max(1)),
            stars: false,
            label: Some((
                match entry.need {
                    None => format!("{}%", anim[&entry.key].value.round().max(0.) as u32),
                    Some(need) => format!("{}/{}", compact(anim[&entry.key].value.round().max(0.) as u64), compact(need)),
                },
                entry.color,
            )),
            selected: false,
        };
        // Fading shrinks the picture around its middle.
        let fade = anim[&entry.key].fade;
        let side = cell * (0.7 + 0.3 * fade);
        let rect = Rect::from_center_size(anim[&entry.key].pos + vec2(cell, cell) / 2., vec2(side, side));
        ui.set_opacity(fade);
        paint_cell_scaled(ui, icons, rect, &cell_look, false, text_scale);
        ui.set_opacity(1.);
    }
    (moving, shown.len(), columns, cell)
}

/// How the buttons go in the strip for a window that is `height` tall: the
/// columns, the size of a button, and the width of the strip.  The fewest
/// columns that fit the height are used, so the window is not made taller.  The
/// corner of the window, which resizes it, is kept free below them.
fn strip_layout(height: f32) -> (usize, f32, f32) {
    let one_column = 2. * STRIP_BUTTON + STRIP_GAP + 2. * STRIP_PAD + CORNER;
    if height >= one_column {
        let width = STRIP_BUTTON + 2. * STRIP_PAD;
        return (1, STRIP_BUTTON, width);
    }
    // The two buttons side by side, as small as the height makes them.
    let button = STRIP_BUTTON.min((height - 2. * STRIP_PAD).max(14.));
    (2, button, 2. * button + STRIP_GAP + 2. * STRIP_PAD)
}

struct StripResult {
    close: bool,
    opacity_rect: Rect,
    opacity_hovered: bool,
    /// The size of the window the corner is being dragged to.
    resize_to: Option<Vec2>,
}

/// The strip on the right of the window while Alt is held: the buttons to close
/// it and for how see-through it is.  The strip can be dragged to move the
/// window, like the rest of it, and the corner of the window, bottom right,
/// resizes it.  `window` is the size of the window now.
fn strip(ui: &mut egui::Ui, layout: (usize, f32, f32), window: Vec2) -> StripResult {
    let (columns, size, _) = layout;
    let rect = ui.max_rect();

    // The strip drags the window, like what is over the pictures.  It comes
    // first, so the buttons and the corner, which come after, are above it.
    let background = ui.interact(rect, Id::new("overlay_strip_drag"), Sense::drag());
    if background.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    background.on_hover_cursor(CursorIcon::Grab);

    let place = |index: usize| {
        let (column, row) = (index % columns, index / columns);
        Rect::from_min_size(
            pos2(
                rect.left() + STRIP_PAD + column as f32 * (size + STRIP_GAP),
                rect.top() + STRIP_PAD + row as f32 * (size + STRIP_GAP),
            ),
            vec2(size, size),
        )
    };
    let font = FontId::proportional((size * 0.65).clamp(10., 20.));

    let close = ui
        .put(
            place(0),
            egui::Button::new(
                RichText::new(egui_material_icons::icons::ICON_CLOSE).font(font.clone()),
            ),
        )
        .clicked();

    // How see-through: its slider is shown over the pictures, by the caller.
    let opacity_rect = place(1);
    let opacity_response = ui.put(
        opacity_rect,
        egui::Button::new(RichText::new(egui_material_icons::icons::ICON_OPACITY).font(font)),
    );

    // The corner of the window.  A small triangle shows where it is, brighter
    // when the pointer is over it.
    let corner = Rect::from_min_size(rect.right_bottom() - vec2(CORNER, CORNER), vec2(CORNER, CORNER));
    let corner_response = ui.interact(corner, Id::new("overlay_corner"), Sense::drag());
    let shade = if corner_response.hovered() || corner_response.dragged() { 230 } else { 90 };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            pos2(corner.right() - 2., corner.top() + 4.),
            pos2(corner.right() - 2., corner.bottom() - 2.),
            pos2(corner.left() + 4., corner.bottom() - 2.),
        ],
        Color32::from_white_alpha(shade),
        egui::Stroke::NONE,
    ));

    // Dragging the corner: it follows the pointer, as far from it as it was when
    // the drag began.  The window itself is set by the caller.
    let key = Id::new("overlay_resize_offset");
    let pointer = ui.input(|input| input.pointer.interact_pos());
    if corner_response.drag_started()
        && let Some(pointer) = pointer
    {
        ui.data_mut(|data| data.insert_temp(key, window - pointer.to_vec2()));
    }
    let resize_to = if corner_response.dragged() {
        pointer
            .zip(ui.data(|data| data.get_temp::<Vec2>(key)))
            .map(|(pointer, offset)| pointer.to_vec2() + offset)
    } else {
        None
    };
    corner_response.on_hover_cursor(CursorIcon::ResizeNwSe);

    StripResult {
        close,
        opacity_rect,
        opacity_hovered: opacity_response.hovered(),
        resize_to,
    }
}

/// The slider of how see-through the window is, over the pictures, by the
/// button that shows it.  It is there while the button or the slider is hovered.
fn opacity_slider(
    ui: &mut egui::Ui,
    content: Rect,
    button: Rect,
    hovered: bool,
    opacity: &mut f32,
    open: &mut bool,
) {
    let width = (content.width() - 12.).min(170.);
    if width < 40. {
        *open = false;
        return;
    }
    let height = 26.;
    let top = (button.center().y - height / 2.).clamp(
        content.top() + 2.,
        (content.bottom() - height - 2.).max(content.top() + 2.),
    );
    let slider = Rect::from_min_size(pos2(content.right() - width - 4., top), vec2(width, height));
    // The slider and the button are one place to hover, with what is between.
    let place = slider.union(button).expand(4.);
    let pointer = ui.input(|input| input.pointer.hover_pos());
    *open = hovered || (*open && pointer.is_some_and(|p| place.contains(p)));
    if *open {
        ui.painter()
            .rect_filled(slider.expand(3.), 6, Color32::from_black_alpha(210));
        ui.put(slider, egui::Slider::new(opacity, 0.25..=1.0).show_value(false));
    }
}

/// A layer over the pictures while Alt is held, which says that the window can
/// be moved, and does it when it is dragged.
fn drag_layer(ui: &mut egui::Ui, rect: Rect, russian: bool) {
    let drag = ui.allocate_rect(rect, Sense::click_and_drag());
    ui.painter()
        .rect_filled(rect, 0, Color32::from_black_alpha(130));
    ui.painter().text(
        rect.center() - vec2(0., 6.),
        Align2::CENTER_CENTER,
        egui_material_icons::icons::ICON_OPEN_WITH,
        FontId::proportional((rect.width().min(rect.height()) * 0.35).clamp(22., 90.)),
        Color32::from_white_alpha(200),
    );
    if rect.width() > 110. && rect.height() > 110. {
        ui.painter().text(
            rect.center() + vec2(0., (rect.width().min(rect.height()) * 0.22).clamp(22., 56.)),
            Align2::CENTER_CENTER,
            if russian { "Перетащите окно" } else { "Drag to move" },
            FontId::proportional(13.),
            Color32::from_white_alpha(200),
        );
    }
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    drag.on_hover_cursor(CursorIcon::Grab);
}

/// Whether a window with this title is the game's.
fn is_game_window(title: &str) -> bool {
    title == "Genshin Impact" || title == "原神"
}

/// Whether the file is the game's program, by its name.
fn is_game_program(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase();
    name == "genshinimpact.exe" || name == "yuanshen.exe"
}

/// Tells the thread that shows and hides the window whether the window is up and
/// whether it is only to be shown while the game is in front.  The
/// thread is started the first time.
fn watch(open: bool, only_in_focus: bool, ctx: &Context) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static OPEN: AtomicBool = AtomicBool::new(false);
    static ONLY_IN_FOCUS: AtomicBool = AtomicBool::new(false);
    static STARTED: std::sync::Once = std::sync::Once::new();

    OPEN.store(open, Ordering::Relaxed);
    ONLY_IN_FOCUS.store(only_in_focus, Ordering::Relaxed);
    let ctx = ctx.clone();
    STARTED.call_once(|| {
        std::thread::spawn(move || {
            // Whether the thread has hidden the window, which it shows again.
            let mut hidden = false;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let wanted = !OPEN.load(Ordering::Relaxed)
                    || !ONLY_IN_FOCUS.load(Ordering::Relaxed)
                    || game_in_front();
                if wanted != hidden {
                    continue;
                }
                if show_window(wanted) {
                    hidden = !wanted;
                    if wanted {
                        // It has to be drawn again to be seen.
                        ctx.request_repaint();
                        ctx.request_repaint_of(ViewportId::from_hash_of("overlay_window"));
                    }
                }
            }
        });
    });
}

/// Shows or hides the window, without it taking the focus.  Returns whether the
/// window was there.
#[cfg(windows)]
fn show_window(show: bool) -> bool {
    use windows::Win32::Graphics::Gdi::{
        RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_HIDE, SW_SHOWNA, ShowWindow};
    use windows::core::{HSTRING, PCWSTR};

    unsafe {
        let title = HSTRING::from(TITLE);
        let Ok(window) = FindWindowW(PCWSTR::null(), &title) else {
            return false;
        };
        let _ = ShowWindow(window, if show { SW_SHOWNA } else { SW_HIDE });
        if show {
            let _ = RedrawWindow(
                Some(window),
                None,
                None,
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
        }
        true
    }
}

#[cfg(not(windows))]
fn show_window(_show: bool) -> bool {
    true
}

/// Whether the window in front is the game's.
fn game_in_front() -> bool {
    what_is_in_front() == crate::hotkey::FRONT_GAME
}

/// What is in front: the game, one of our windows, or something else.
#[cfg(windows)]
pub(crate) fn what_is_in_front() -> u8 {
    use crate::hotkey::{FRONT_GAME, FRONT_OTHER, FRONT_US};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    use windows::core::PWSTR;

    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return FRONT_OTHER;
        }
        let mut process = 0u32;
        GetWindowThreadProcessId(window, Some(&mut process));
        if process == std::process::id() {
            return FRONT_US;
        }
        // The game's window says what it is, which does not need the right to
        // look into its program, which a protected one may not give.
        let mut title = [0u16; 128];
        let length = GetWindowTextW(window, &mut title).max(0) as usize;
        let title = String::from_utf16_lossy(&title[..length.min(title.len())]);
        if is_game_window(&title) {
            return FRONT_GAME;
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process) else {
            return FRONT_OTHER;
        };
        let mut buffer = [0u16; 520];
        let mut length = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if queried && is_game_program(&String::from_utf16_lossy(&buffer[..length as usize])) {
            FRONT_GAME
        } else {
            FRONT_OTHER
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn what_is_in_front() -> u8 {
    crate::hotkey::FRONT_GAME
}

/// Whether Alt is held, in any window: the overlay is not the one with the
/// focus.
#[cfg(windows)]
fn alt_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_MENU};
    unsafe { (GetAsyncKeyState(VK_MENU.0 as i32) as u16) & 0x8000 != 0 }
}

#[cfg(not(windows))]
fn alt_down() -> bool {
    false
}

/// Makes the whole window see-through by `alpha` of 255.  Returns whether the
/// window was there to do it to.
#[cfg(windows)]
fn set_window_alpha(alpha: u8) -> bool {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes,
        SetWindowLongPtrW, WS_EX_LAYERED,
    };
    use windows::core::{HSTRING, PCWSTR};

    unsafe {
        let title = HSTRING::from(TITLE);
        let Ok(hwnd) = FindWindowW(PCWSTR::null(), &title) else {
            return false;
        };
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if style & WS_EX_LAYERED.0 as isize == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
        }
        SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA).is_ok()
    }
}

#[cfg(not(windows))]
fn set_window_alpha(_alpha: u8) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gems() -> Vec<Family> {
        vec![Family {
            ids: vec![1, 2, 3, 4],
            mora: vec![0, 300, 900, 2700],
        }]
    }

    #[test]
    fn a_chain_is_one_picture_in_per_cent_mode() {
        let mut needs = BTreeMap::new();
        needs.insert(4, 6);
        needs.insert(202, 100);
        let mut owned = HashMap::new();
        // Nine of the third rarity make three of the top: half of what is needed.
        owned.insert(3, 9);
        let shown = entries(&[3, 4, 202], &owned, &needs, &gems(), true);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].label, "50%");
        assert_eq!(shown[0].icon_id, 4);
        assert!(!shown[0].done);
        // The same materials one by one when per cent is off.
        let single = entries(&[3, 4, 202], &owned, &needs, &gems(), false);
        assert_eq!(single.len(), 3);
    }

    #[test]
    fn a_chain_with_all_there_is_done() {
        let mut needs = BTreeMap::new();
        needs.insert(4, 2);
        let mut owned = HashMap::new();
        owned.insert(3, 6);
        let shown = entries(&[4], &owned, &needs, &gems(), true);
        assert!(shown[0].done);
        assert_eq!(shown[0].label, "100%");
    }

    #[test]
    fn a_material_with_enough_is_done() {
        let mut needs = BTreeMap::new();
        needs.insert(7, 10);
        let mut owned = HashMap::new();
        owned.insert(7, 10);
        assert!(entries(&[7], &owned, &needs, &[], false)[0].done);
        owned.insert(7, 9);
        assert!(!entries(&[7], &owned, &needs, &[], false)[0].done);
    }

    #[test]
    fn pictures_fill_the_space_the_most_they_can() {
        // Four pictures in a wide low space go in a row.
        let (columns, cell) = fit(4, 400., 90.);
        assert_eq!(columns, 4);
        assert!(cell > 80.);
        // In a tall narrow one they go in a column.
        assert_eq!(fit(4, 90., 400.).0, 1);
        // A bigger space gives bigger pictures.
        assert!(fit(6, 600., 400.).1 > fit(6, 300., 200.).1);
    }

    #[test]
    fn a_grid_with_full_rows_is_chosen_when_it_is_nearly_as_big() {
        // Five pictures in a wide space: five in a row has no gap; so does one
        // column of five.  Two rows would leave one empty.
        let (columns, _) = fit(5, 700., 180.);
        assert!(5 % columns == 0);
        // Four, where two by two is a little smaller than four in a row but
        // without gaps either way.
        let (columns, _) = fit(4, 300., 140.);
        assert!(4 % columns == 0);
    }

    #[test]
    fn the_buttons_fit_the_height_by_going_side_by_side() {
        // Tall enough for the two buttons in a column with the corner below.
        assert_eq!(strip_layout(200.).0, 1);
        // A short window has them side by side.
        assert_eq!(strip_layout(50.).0, 2);
        assert!(strip_layout(50.).2 > strip_layout(200.).2);
    }

    #[test]
    fn the_games_window_is_known_by_its_title() {
        assert!(is_game_window("Genshin Impact"));
        assert!(is_game_window("原神"));
        assert!(!is_game_window("Genshin Impact Wiki - Google Chrome"));
    }

    #[test]
    fn the_games_program_is_known_by_its_name() {
        assert!(is_game_program(r"C:\Program Files\Genshin Impact\Genshin Impact game\GenshinImpact.exe"));
        assert!(is_game_program(r"D:\Games\YuanShen.exe"));
        assert!(!is_game_program(r"C:\Windows\explorer.exe"));
        assert!(!is_game_program("GenshinImpactLauncher.exe"));
    }

    #[test]
    fn the_window_is_as_big_as_its_pictures() {
        let size = grid_size(6, 3, 70.);
        assert_eq!(size.x, 3. * 70. + 2. * GAP + 2. * MARGIN);
        assert_eq!(size.y, 2. * 70. + GAP + 2. * MARGIN);
        // Fewer pictures than columns make it narrower.
        assert_eq!(grid_size(2, 4, 70.).x, grid_size(2, 2, 70.).x);
    }
}
