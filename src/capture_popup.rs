//! A small window that slides down from the top of the screen when the game
//! starts loading, and shows how far capturing the game's data has come.
//!
//! It is a window of its own, so it can show over the game while Irminsul's
//! main window is hidden: always on top, never takes the focus, and lets mouse
//! clicks through.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, Context, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, StrokeKind,
    ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2,
};

use crate::CaptureProgress;
use crate::achievement_data::Language;

const WIDTH: f32 = 440.;
const HEIGHT: f32 = 132.;
/// Gap between the top of the screen and the popup when it is shown.
const TOP_MARGIN: f32 = 18.;
const CORNER_RADIUS: f32 = 20.;
/// How long it takes to slide in and out.
const SLIDE: Duration = Duration::from_millis(450);
/// How long it stays after the data was captured, or the capture failed.
const SUCCESS_HOLD: Duration = Duration::from_millis(2400);
const FAILURE_HOLD: Duration = Duration::from_secs(7);
/// It goes away by itself if the data never completes.
const TIMEOUT: Duration = Duration::from_secs(90);

const CARD: Color32 = Color32::from_rgb(22, 25, 44);
const CARD_SUCCESS: Color32 = Color32::from_rgb(31, 157, 85);
const CARD_FAILURE: Color32 = Color32::from_rgb(165, 58, 58);
const RING_LOADING: Color32 = Color32::from_rgb(155, 180, 255);
const RING_DONE: Color32 = Color32::from_rgb(74, 222, 128);

fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

fn ease_in(t: f32) -> f32 {
    t.powi(3)
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn fraction(elapsed: Duration, total: Duration) -> f32 {
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
}

pub struct CapturePopup {
    /// The connection to the game server the popup was last shown for.
    session: u64,
    shown_at: Option<Instant>,
    /// When everything was captured, or the capture failed.
    finished_at: Option<Instant>,
    closing_at: Option<Instant>,
    /// How full each ring is drawn, easing towards where the capture is.
    fill: [f32; 3],
    last_frame: Instant,
    /// Whether the popup's window got its rounded corners yet.
    rounded: bool,
    /// A made up connection being shown, and whether it is to fail.
    preview: Option<(Instant, bool)>,
    previews: u64,
}

/// Everything the drawing needs, so it can happen in the popup's own window.
struct Look {
    fill: [f32; 3],
    done: [bool; 3],
    labels: [&'static str; 3],
    title: &'static str,
    success: f32,
    failure: f32,
}

impl CapturePopup {
    pub fn new() -> Self {
        Self {
            session: 0,
            shown_at: None,
            finished_at: None,
            closing_at: None,
            fill: [0.; 3],
            last_frame: Instant::now(),
            rounded: false,
            preview: None,
            previews: 0,
        }
    }

    /// Plays a made up connection, to see what the popup looks like.
    pub fn start_preview(&mut self, failure: bool) {
        self.previews += 1;
        self.preview = Some((Instant::now(), failure));
    }

    /// The progress of the made up connection, if one is playing.
    fn preview_progress(&mut self, now: Instant) -> Option<CaptureProgress> {
        let (started, failure) = self.preview?;
        let elapsed = (now - started).as_secs_f32();
        if elapsed > 30. {
            self.preview = None;
            return None;
        }
        Some(CaptureProgress {
            // Far from the numbers real connections count up from.
            session: u64::MAX - self.previews,
            key_found: !failure && elapsed > 1.4,
            items: !failure && elapsed > 2.3,
            characters: !failure && elapsed > 2.7,
            achievements: !failure && elapsed > 3.2,
            failed: failure && elapsed > 2.0,
        })
    }

    /// Call every frame.  Shows the popup for each new connection of the game,
    /// follows `progress` and hides it again when it is done.
    pub fn update(
        &mut self,
        ctx: &Context,
        progress: &CaptureProgress,
        language: Language,
        enabled: bool,
    ) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;

        let preview = self.preview_progress(now);
        let progress = preview.as_ref().unwrap_or(progress);

        if progress.session != self.session {
            self.session = progress.session;
            if enabled && progress.session != 0 {
                self.shown_at = Some(now);
                self.finished_at = None;
                self.closing_at = None;
                self.fill = [0.; 3];
                self.rounded = false;
            }
        }
        let Some(shown_at) = self.shown_at else {
            return;
        };

        let done = [progress.items, progress.characters, progress.achievements];
        let all_done = done.iter().all(|done| *done);
        let failed = progress.failed && !all_done;

        // The rings fill up while data is awaited, and jump to full when it is in.
        for (fill, done) in self.fill.iter_mut().zip(done) {
            let (target, rate) = match (done, progress.key_found) {
                (true, _) => (1., 7.),
                (false, true) => (0.85, 1.2),
                (false, false) => (0.3, 1.5),
            };
            *fill += (target - *fill) * (1. - (-rate * dt).exp());
        }

        if self.finished_at.is_none() {
            let rings_full = self.fill.iter().all(|fill| *fill > 0.97);
            if (all_done && rings_full) || failed {
                self.finished_at = Some(now);
            }
        }
        if self.closing_at.is_none() {
            let hold = if failed { FAILURE_HOLD } else { SUCCESS_HOLD };
            let finished_long_enough = self.finished_at.is_some_and(|at| now - at >= hold);
            if finished_long_enough || now - shown_at >= TIMEOUT || !enabled {
                self.closing_at = Some(now);
            }
        }

        let hidden_y = -(HEIGHT + 24.);
        let mut y = lerp(hidden_y, TOP_MARGIN, ease_out(fraction(now - shown_at, SLIDE)));
        if let Some(closing_at) = self.closing_at {
            let t = fraction(now - closing_at, SLIDE);
            if t >= 1. {
                self.shown_at = None;
                return;
            }
            y = lerp(TOP_MARGIN, hidden_y, ease_in(t));
        }

        let blend = self
            .finished_at
            .map_or(0., |at| ease_out(fraction(now - at, Duration::from_millis(400))));
        let tr = |en: &'static str, ru: &'static str| match language {
            Language::En => en,
            Language::Ru => ru,
        };
        let finished = self.finished_at.is_some();
        let look = Look {
            fill: self.fill,
            done,
            labels: [
                tr("Items", "Предметы"),
                tr("Characters", "Персонажи"),
                tr("Achievements", "Достижения"),
            ],
            title: match (finished, failed) {
                (true, true) => tr(
                    "Couldn't read the data. Log in again.",
                    "Данные не прочитаны. Перезайдите в игру.",
                ),
                (true, false) => tr("Data captured successfully", "Данные успешно захвачены"),
                (false, _) => tr("Capturing data…", "Захват данных…"),
            },
            success: if failed { 0. } else { blend },
            failure: if failed { blend } else { 0. },
        };

        // The window comes into being by the call below, so it can be rounded
        // from the next frame on.
        if !self.rounded {
            self.rounded = round_corners();
        }

        let x = (Self::screen_width(ctx) - WIDTH) / 2.;
        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("capture_popup"),
            ViewportBuilder::default()
                .with_title("Irminsul capture")
                .with_inner_size([WIDTH, HEIGHT])
                .with_position([x, y])
                .with_decorations(false)
                .with_always_on_top()
                .with_taskbar(false)
                .with_active(false)
                .with_resizable(false)
                .with_mouse_passthrough(true),
            move |ctx, _class| {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(x, y)));
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| draw(ui, &look, ui.max_rect()));
            },
        );
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    /// Width of the main screen the popup is centered on, in points.
    fn screen_width(ctx: &Context) -> f32 {
        #[cfg(windows)]
        {
            use windows::Win32::UI::HiDpi::GetDpiForSystem;
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN};
            let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
            if width > 0 {
                // The scale of the main screen, not of whatever monitor the
                // main window is on.
                let scale = unsafe { GetDpiForSystem() } as f32 / 96.;
                return width as f32 / scale.max(1.);
            }
        }
        ctx.input(|input| input.viewport().monitor_size)
            .map_or(1920., |size| size.x)
    }
}

/// Cuts the corners of the popup's window.  The window can't be transparent
/// where the card is rounded, so the corners are taken off the window itself.
/// Returns whether it was done.
#[cfg(windows)]
fn round_corners() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};
    use windows::core::{PCWSTR, w};

    unsafe {
        let Ok(hwnd) = FindWindowW(PCWSTR::null(), w!("Irminsul capture")) else {
            return false;
        };
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let scale = GetDpiForSystem() as f32 / 96.;
        let diameter = (2. * CORNER_RADIUS * scale).round() as i32;
        let region = CreateRoundRectRgn(
            0,
            0,
            rect.right - rect.left + 1,
            rect.bottom - rect.top + 1,
            diameter,
            diameter,
        );
        // The window owns the region from here on.
        SetWindowRgn(hwnd, Some(region), true) != 0
    }
}

#[cfg(not(windows))]
fn round_corners() -> bool {
    true
}

fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    from.lerp_to_gamma(to, t.clamp(0., 1.))
}

fn draw(ui: &mut egui::Ui, look: &Look, rect: Rect) {
    let painter = ui.painter();

    let card = mix(
        mix(CARD, CARD_SUCCESS, look.success),
        CARD_FAILURE,
        look.failure,
    )
    .gamma_multiply(0.96);
    let rounding = CornerRadius::same(CORNER_RADIUS as u8);
    painter.rect_filled(rect, rounding, card);
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
        StrokeKind::Inside,
    );

    painter.text(
        pos2(rect.center().x, rect.top() + 26.),
        Align2::CENTER_CENTER,
        look.title,
        FontId::proportional(if look.failure > 0.5 { 15. } else { 17. }),
        Color32::WHITE,
    );

    let ring_done = mix(RING_DONE, Color32::WHITE, look.success);
    let radius = 19.;
    for (index, label) in look.labels.iter().enumerate() {
        let center = pos2(rect.center().x + (index as f32 - 1.) * 130., rect.top() + 70.);

        painter.circle_stroke(center, radius, Stroke::new(5.0_f32, Color32::from_white_alpha(45)));

        let fill = look.fill[index].clamp(0., 1.);
        let color = if look.done[index] { ring_done } else { RING_LOADING };
        if fill > 0.001 {
            painter.add(arc(center, radius, fill, Stroke::new(5.0_f32, color)));
        }
        if look.done[index] {
            painter.add(Shape::line(
                vec![
                    center + vec2(-7., 1.),
                    center + vec2(-2., 6.),
                    center + vec2(8., -6.),
                ],
                Stroke::new(3.0_f32, ring_done),
            ));
        }

        painter.text(
            pos2(center.x, rect.top() + 108.),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13.),
            Color32::from_white_alpha(215),
        );
    }
}

/// An arc around `center`, starting at the top and going clockwise.
fn arc(center: Pos2, radius: f32, fraction: f32, stroke: Stroke) -> Shape {
    let steps = ((64. * fraction) as usize).max(2);
    let points = (0..=steps)
        .map(|step| {
            let angle = -FRAC_PI_2 + TAU * fraction * step as f32 / steps as f32;
            center + radius * vec2(angle.cos(), angle.sin())
        })
        .collect();
    Shape::line(points, stroke)
}
