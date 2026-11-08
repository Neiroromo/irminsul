//! A small window in the corner of the screen that says what changed: how much
//! mora was spent, which items came and how many.
//!
//! Like the capture popup it is a window of its own: always on top, never takes
//! the focus, lets clicks through, so it can show over the game.  Changes are
//! given to it as batches; it adds up what is about the same thing and takes a
//! line away some seconds after the last change to it.

use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, Context, CornerRadius, FontId, Rect, Stroke, StrokeKind, ViewportBuilder,
    ViewportCommand, ViewportId, pos2, vec2,
};

use crate::achievement_data::Language;
use crate::changes::{Change, ChangeBatch, ChangeKind, Feed};
use crate::icons::Icons;
use crate::live_data;
use crate::planner_tab::grouped;
use crate::upgrade_store;
use crate::window_visibility;

const WIDTH: f32 = 340.;
const HEADER: f32 = 30.;
const ROW: f32 = 34.;
const PADDING: f32 = 10.;
/// How many lines are shown at a time; more are shown in turns, in a window
/// that stays as tall as for this many.
const PAGE: usize = 6;
/// How long a page is shown when there are more.
const PAGE_TIME: Duration = Duration::from_millis(2800);
/// How long a page takes to fade in and out.
const PAGE_FADE: Duration = Duration::from_millis(220);
const TOP_MARGIN: f32 = 18.;
const RIGHT_MARGIN: f32 = 24.;
const CORNER_RADIUS: f32 = 16.;
/// How long a line stays after its last change.
const HOLD: Duration = Duration::from_secs(5);
const SLIDE: Duration = Duration::from_millis(260);
const TITLE: &str = "Irminsul changes";

const CARD: Color32 = Color32::from_rgb(22, 25, 44);
const GAIN: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
const LOSS: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);

fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

pub struct ChangesPopup {
    feed: Feed,
    /// The newest batch taken so far.
    seen: u64,
    shown_at: Option<Instant>,
    closing_at: Option<Instant>,
    /// What the corners of the window were cut for.
    region: Option<(i32, i32, u32)>,
    /// There are more lines than fit, so they are shown a page at a time.
    paged: bool,
    /// When the page being shown began.
    page_since: Option<Instant>,
    /// A made up run of changes being played, and when it began.
    preview: Option<(Instant, usize)>,
}

/// What the preview plays: seconds after it began, and the changes.
fn preview_steps() -> Vec<(f32, Vec<Change>)> {
    let item = |id: u32, delta: i64| Change {
        kind: ChangeKind::Item(id),
        delta,
    };
    vec![
        (
            0.0,
            vec![
                Change {
                    kind: ChangeKind::Mora,
                    delta: -12_500,
                },
                item(101273, 3),
            ],
        ),
        (1.6, vec![item(101273, 2)]),
        // Crafting: three of one go, one of the next comes.
        (3.2, vec![item(104161, -3), item(104162, 1)]),
        (
            4.6,
            vec![
                Change {
                    kind: ChangeKind::Primogems,
                    delta: 60,
                },
                Change {
                    kind: ChangeKind::Note("Talent level up".to_owned()),
                    delta: 0,
                },
            ],
        ),
        // Chests in view, then one opened.
        (
            6.2,
            vec![
                Change {
                    kind: ChangeKind::ChestSeen(1),
                    delta: 2,
                },
                Change {
                    kind: ChangeKind::ChestSeen(2),
                    delta: 1,
                },
            ],
        ),
        (
            7.8,
            vec![
                Change {
                    kind: ChangeKind::ChestOpened(70_211_002),
                    delta: 1,
                },
                Change {
                    kind: ChangeKind::ChestTotals {
                        opened: 4,
                        known: 41,
                    },
                    delta: 41,
                },
            ],
        ),
    ]
}

impl ChangesPopup {
    pub fn new() -> Self {
        Self {
            feed: Feed::default(),
            seen: 0,
            shown_at: None,
            closing_at: None,
            region: None,
            paged: false,
            page_since: None,
            preview: None,
        }
    }

    /// Plays made up changes, to see what the popup looks like.
    pub fn start_preview(&mut self) {
        self.preview = Some((Instant::now(), 0));
    }

    /// Takes in the batches not seen yet.
    pub fn ingest(&mut self, batches: &[ChangeBatch], enabled: bool, now: Instant) {
        let before = self.seen;
        for batch in batches.iter().filter(|batch| batch.id > before) {
            self.seen = batch.id;
            if enabled {
                self.push(&batch.changes, now);
            }
        }
    }

    /// Takes in a batch, as if the monitor had sent it.
    pub fn push(&mut self, batch: &[Change], now: Instant) {
        for change in batch {
            self.feed.add(change, now);
        }
    }

    /// Call every frame.
    pub fn update(
        &mut self,
        ctx: &Context,
        batches: &[ChangeBatch],
        icons: &Icons,
        language: Language,
        enabled: bool,
    ) {
        let now = Instant::now();

        self.ingest(batches, enabled, now);
        if let Some((started, next)) = self.preview {
            let steps = preview_steps();
            let elapsed = (now - started).as_secs_f32();
            if let Some((at, changes)) = steps.get(next)
                && elapsed >= *at
            {
                self.push(changes, now);
                self.preview = Some((started, next + 1));
            } else if next >= steps.len() {
                self.preview = None;
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // More lines than fit are shown a page at a time: when a page has been
        // shown it is taken away and the next is shown.  The window keeps its
        // height meanwhile.
        let count = self.feed.lines.len();
        if count == 0 {
            self.paged = false;
            self.page_since = None;
        } else if count > PAGE && !self.paged {
            self.paged = true;
            self.page_since = Some(now);
        }
        if self.paged {
            let since = *self.page_since.get_or_insert(now);
            if now - since >= PAGE_TIME {
                let shown = PAGE.min(self.feed.lines.len());
                self.feed.lines.drain(..shown);
                self.page_since = Some(now);
                // What comes next has its time from now.
                for line in self.feed.lines.iter_mut().take(PAGE) {
                    line.updated = now;
                }
                if self.feed.lines.is_empty() {
                    self.paged = false;
                    self.page_since = None;
                }
            }
        } else {
            self.feed.expire(now, HOLD);
        }

        // Slide in when there is something to show, out when there is not.
        if self.feed.is_empty() {
            if self.shown_at.is_some() && self.closing_at.is_none() {
                self.closing_at = Some(now);
            }
        } else {
            if self.shown_at.is_none() {
                self.shown_at = Some(now);
                self.region = None;
            }
            self.closing_at = None;
        }
        let Some(shown_at) = self.shown_at else {
            return;
        };

        let rows = if self.paged { PAGE } else { self.feed.lines.len().min(PAGE) } as f32;
        let height = HEADER + rows * ROW + PADDING;
        let hidden_y = -(height + 24.);
        let mut y = hidden_y + (TOP_MARGIN - hidden_y) * ease_out(fraction(now - shown_at, SLIDE));
        if let Some(closing_at) = self.closing_at {
            let t = fraction(now - closing_at, SLIDE);
            if t >= 1. {
                self.shown_at = None;
                self.closing_at = None;
                return;
            }
            y = TOP_MARGIN + (hidden_y - TOP_MARGIN) * t.powi(3);
        }

        let russian = language == Language::Ru;
        let lines: Vec<DrawLine> = self
            .feed
            .lines
            .iter()
            .take(PAGE)
            .map(|line| DrawLine::new(&line.kind, line.delta, russian))
            .collect();
        let hidden_lines = self.feed.lines.len().saturating_sub(PAGE);

        // A page fades in and, if more follow, out.
        let page_alpha = match (self.paged, self.page_since) {
            (true, Some(since)) => {
                let age = now - since;
                let fade_in = fraction(age, PAGE_FADE);
                let fade_out = fraction(PAGE_TIME.saturating_sub(age), PAGE_FADE);
                fade_in.min(fade_out)
            }
            _ => 1.,
        };

        // The corners are cut for the window as it is, every frame it changes.
        window_visibility::round_window(TITLE, CORNER_RADIUS, &mut self.region);

        let x = Self::screen_width(ctx) - WIDTH - RIGHT_MARGIN;
        let title = if russian { "Изменения" } else { "Changes" };
        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("changes_popup"),
            ViewportBuilder::default()
                .with_title(TITLE)
                .with_inner_size([WIDTH, height])
                .with_position([x, y])
                .with_decorations(false)
                .with_always_on_top()
                .with_taskbar(false)
                .with_active(false)
                .with_resizable(false)
                .with_mouse_passthrough(true),
            |ctx, _class| {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(WIDTH, height)));
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(x, y)));
                icons.poll();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        draw(ui, ui.max_rect(), title, &lines, hidden_lines, page_alpha, icons)
                    });
            },
        );
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    /// Width of the main screen, in points.
    fn screen_width(ctx: &Context) -> f32 {
        #[cfg(windows)]
        {
            use windows::Win32::UI::HiDpi::GetDpiForSystem;
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN};
            let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
            if width > 0 {
                let scale = unsafe { GetDpiForSystem() } as f32 / 96.;
                return width as f32 / scale.max(1.);
            }
        }
        ctx.input(|input| input.viewport().monitor_size)
            .map_or(1920., |size| size.x)
    }
}

fn fraction(elapsed: Duration, total: Duration) -> f32 {
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
}

/// A line as it is drawn: what it is about, and how much.
struct DrawLine {
    icon: String,
    name: String,
    amount: String,
    color: Color32,
}

impl DrawLine {
    fn new(kind: &ChangeKind, delta: i64, russian: bool) -> Self {
        let data = upgrade_store::data();
        let live = live_data::current();
        // What the data built in knows, then what the upcoming content has, then
        // the list of all the items, which is fetched when it is needed.
        let material = |id: u32| {
            data.materials
                .get(&id)
                .or_else(|| live.materials.get(&id))
                .cloned()
                .or_else(|| live_data::item(id))
        };
        let (icon, name) = match kind {
            ChangeKind::Mora => (
                "UI_ItemIcon_202".to_owned(),
                if russian { "Мора" } else { "Mora" }.to_owned(),
            ),
            ChangeKind::Primogems => (
                "UI_ItemIcon_201".to_owned(),
                if russian { "Примогемы" } else { "Primogems" }.to_owned(),
            ),
            ChangeKind::Item(id) => match material(*id) {
                Some(material) => (material.icon.clone(), material.name.get(russian).to_owned()),
                None => (format!("UI_ItemIcon_{id}"), format!("#{id}")),
            },
            ChangeKind::Talent(avatar, skill) => match data.avatars.get(avatar) {
                Some(character) => {
                    let talent = data.depots.get(&character.default_depot).and_then(|depot| {
                        [&depot.normal, &depot.skill, &depot.burst]
                            .into_iter()
                            .find(|talent| talent.skill_id == *skill)
                    });
                    let name = match talent {
                        Some(talent) => format!(
                            "{}: {}",
                            character.name.get(russian),
                            talent.name.get(russian)
                        ),
                        None => character.name.get(russian).to_owned(),
                    };
                    (character.icon.clone(), name)
                }
                None => (String::new(), format!("#{avatar}")),
            },
            ChangeKind::Weapon(id) | ChangeKind::Refinement(id) => match data.weapons.get(id) {
                Some(weapon) => (weapon.icon.clone(), weapon.name.get(russian).to_owned()),
                None => (String::new(), format!("#{id}")),
            },
            ChangeKind::Level(id) => match data.avatars.get(id) {
                Some(avatar) => (avatar.icon.clone(), avatar.name.get(russian).to_owned()),
                None => (String::new(), format!("#{id}")),
            },
            ChangeKind::Note(text) => (String::new(), text.clone()),
            ChangeKind::ChestSeen(tier) => (
                String::new(),
                format!(
                    "{}: {}",
                    if russian { "Рядом" } else { "Nearby" },
                    crate::chests::tier_name(*tier, russian)
                ),
            ),
            ChangeKind::ChestOpened(gadget) => (
                String::new(),
                format!(
                    "{} {}",
                    crate::chests::kind_name(*gadget, russian),
                    if russian { "открыт" } else { "opened" }
                ),
            ),
            ChangeKind::ChestTotals { .. } => (
                String::new(),
                if russian { "Сундуков открыто" } else { "Chests opened" }.to_owned(),
            ),
        };
        let note = matches!(kind, ChangeKind::Note(_));
        let amount = if note {
            String::new()
        } else if let ChangeKind::ChestSeen(_) = kind {
            format!("×{delta}")
        } else if let ChangeKind::ChestTotals { opened, known } = kind {
            format!("{opened}/{known}")
        } else if matches!(kind, ChangeKind::Refinement(_)) {
            format!("R{delta}")
        } else if matches!(
            kind,
            ChangeKind::Level(_) | ChangeKind::Talent(..) | ChangeKind::Weapon(_)
        ) {
            format!("{} {delta}", if russian { "Ур." } else { "Lv." })
        } else if delta >= 0 {
            format!("+{}", grouped(delta as u64))
        } else {
            format!("-{}", grouped(delta.unsigned_abs()))
        };
        Self {
            icon,
            name,
            amount,
            color: if matches!(kind, ChangeKind::ChestSeen(_)) {
                Color32::from_rgb(0x6a, 0xb7, 0xff)
            } else if matches!(
                kind,
                ChangeKind::Level(_)
                    | ChangeKind::Talent(..)
                    | ChangeKind::Weapon(_)
                    | ChangeKind::Refinement(_)
                    | ChangeKind::ChestTotals { .. }
            ) {
                Color32::from_rgb(0xff, 0xc3, 0x4a)
            } else if delta < 0 {
                LOSS
            } else {
                GAIN
            },
        }
    }
}

fn draw(
    ui: &mut egui::Ui,
    rect: Rect,
    title: &str,
    lines: &[DrawLine],
    hidden_lines: usize,
    alpha: f32,
    icons: &Icons,
) {
    let painter = ui.painter();
    let rounding = CornerRadius::same(CORNER_RADIUS as u8);
    painter.rect_filled(rect, rounding, CARD.gamma_multiply(0.96));
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
        StrokeKind::Inside,
    );
    painter.text(
        pos2(rect.left() + 16., rect.top() + HEADER / 2. + 2.),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(13.),
        Color32::from_white_alpha(170),
    );
    if hidden_lines > 0 {
        painter.text(
            pos2(rect.right() - 16., rect.top() + HEADER / 2. + 2.),
            Align2::RIGHT_CENTER,
            format!("+{hidden_lines}"),
            FontId::proportional(12.),
            Color32::from_white_alpha(140),
        );
    }

    // The lines are what fades, not the card.
    ui.set_opacity(alpha);
    let painter = ui.painter();
    for (index, line) in lines.iter().enumerate() {
        let top = rect.top() + HEADER + index as f32 * ROW;
        let middle = top + ROW / 2.;
        let mut text_left = rect.left() + 16.;
        if !line.icon.is_empty() {
            if let Some(texture) = icons.get(&line.icon) {
                painter.image(
                    texture.id(),
                    Rect::from_center_size(pos2(text_left + 13., middle), vec2(26., 26.)),
                    Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                    Color32::WHITE,
                );
            }
            text_left += 36.;
        }
        let amount = painter.text(
            pos2(rect.right() - 16., middle),
            Align2::RIGHT_CENTER,
            &line.amount,
            FontId::proportional(17.),
            line.color,
        );
        // The name takes what the amount leaves.
        let room = (amount.left() - 10. - text_left).max(20.);
        let galley = painter.layout(
            line.name.clone(),
            FontId::proportional(15.),
            Color32::WHITE,
            room,
        );
        painter.galley(
            pos2(text_left, middle - galley.size().y.min(18.) / 2.),
            galley,
            Color32::WHITE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_is_taken_once() {
        let mut popup = ChangesPopup::new();
        let batch = ChangeBatch {
            id: 1,
            changes: vec![Change {
                kind: ChangeKind::Item(1),
                delta: 2,
            }],
        };
        let now = Instant::now();
        popup.ingest(std::slice::from_ref(&batch), true, now);
        popup.ingest(std::slice::from_ref(&batch), true, now);
        assert_eq!(popup.feed.lines[0].delta, 2);
    }

    #[test]
    fn amounts_show_their_sign() {
        let gain = DrawLine::new(&ChangeKind::Mora, 12_500, true);
        assert_eq!(gain.amount, "+12 500");
        let loss = DrawLine::new(&ChangeKind::Mora, -12_500, true);
        assert_eq!(loss.amount, "-12 500");
        assert_eq!(loss.color, LOSS);
    }
}
