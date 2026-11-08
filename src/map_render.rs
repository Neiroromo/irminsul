//! Draws the map for the window over the game, in memory.
//!
//! The window over the game has to be see-through at places, by degrees, which
//! the window of the program's own drawing can't be on this machine (its
//! drawing surface has no room for how see-through a place is).  So the map is
//! drawn here, to a picture that has it, and given to a window that shows such
//! pictures.  The picture is in the form that window wants: blue, green, red and
//! how much of it is seen, each of the three already taken by how much.

use std::sync::Arc;

use egui::ColorImage;

use crate::map_tiles::{MapInfo, TILE, TileKey, grid};

/// What a chest on the map looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconLook {
    Closed,
    Opened,
    Likely,
    Faint,
}

#[derive(Clone, Copy, Debug)]
pub struct Icon {
    /// Where it is on the full size map.
    pub at: [f32; 2],
    pub tier: u8,
    pub look: IconLook,
}

/// The picture of a floor of an underground place, laid on the map.
pub struct FloorPicture {
    /// Where it is on the full size map: left, top, right, bottom.
    pub rect: [f32; 4],
    pub image: Arc<ColorImage>,
}

/// A marker of the map, to be drawn: where, its picture (a dot of its colour
/// until the picture is there), and whether it is collected (fainter).
pub struct MarkDraw {
    pub at: [f32; 2],
    pub image: Option<Arc<ColorImage>>,
    pub color: [u8; 3],
    pub done: bool,
}

/// The levels there are where the player is, for the buttons that choose one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelsView {
    pub current: usize,
    pub max: usize,
    /// The buttons are shown (and can be clicked); if not, the level is only
    /// told, when it is not the surface.
    pub buttons: bool,
}

const BUTTON: i32 = 30;
const BUTTON_GAP: i32 = 6;
const BUTTON_MARGIN: i32 = 16;

/// Where the buttons of the levels are in a picture of this width: x, y, width,
/// height, for 0 (farthest right) to `max`.
pub fn level_buttons(width: usize, max: usize) -> Vec<[i32; 4]> {
    (0..=max.min(12))
        .map(|number| {
            let left = width as i32 - BUTTON_MARGIN - BUTTON - number as i32 * (BUTTON + BUTTON_GAP);
            [left, BUTTON_MARGIN, BUTTON, BUTTON]
        })
        .collect()
}

pub struct Params<'a> {
    pub width: usize,
    pub height: usize,
    /// The place of the full size map that is in the middle of the picture.
    pub center: [f32; 2],
    /// Pixels of the picture to a pixel of the full size map.
    pub scale: f32,
    /// How seen the middle is: 1 is all.
    pub opacity: f32,
    /// How far in from the edge the fading begins: 0 is none.
    pub vignette: f32,
    /// The map is being moved: it is shown with a frame, and is not empty
    /// anywhere, so it can be taken hold of anywhere.
    pub edit: bool,
    pub info: &'a MapInfo,
    pub tiles: &'a dyn Fn(TileKey) -> Option<Arc<ColorImage>>,
    pub icons: &'a [Icon],
    /// Where the player is on the full size map, the way they face (a unit step on
    /// the map), and how far around them what is followed is looked at (pixels of
    /// the full size map; drawn as a circle if given).
    pub player: Option<[f32; 2]>,
    pub facing: Option<[f32; 2]>,
    pub range: Option<f32>,
    /// The floors laid on the map, and how dark the map under them is made.
    pub floors: &'a [FloorPicture],
    /// The markers of the kinds that are chosen, and whether to draw them as dots
    /// (when the map is far).
    pub markers: &'a [MarkDraw],
    pub dots: bool,
    pub dim: f32,
    pub levels: Option<LevelsView>,
    /// The place the player is in is not on any map: nothing is drawn but a dark
    /// ground, the player in the middle, and a question mark.
    pub unknown: bool,
}

/// The colour of a kind of chest.
pub fn tier_rgb(tier: u8) -> [u8; 3] {
    match tier {
        1 => [0xe8, 0xea, 0xee],
        2 => [0x6f, 0xe0, 0x7a],
        3 => [0xb8, 0x82, 0xff],
        4 => [0xff, 0xc2, 0x4a],
        _ => [0x66, 0xd9, 0xe8],
    }
}

const OPENED: [u8; 3] = [0x70, 0x74, 0x7e];
const EDGE: [u8; 3] = [0x1b, 0x1d, 0x27];

/// How much of the map is seen at a place: all of it near the middle, less and
/// less toward the edges, none at them.
pub fn fade(x: f32, y: f32, width: f32, height: f32, vignette: f32) -> f32 {
    let (half_x, half_y) = (width / 2., height / 2.);
    if half_x <= 0. || half_y <= 0. {
        return 0.;
    }
    let ring = (((x - half_x) / half_x).powi(2) + ((y - half_y) / half_y).powi(2)).sqrt();
    let inner = (1. - vignette.clamp(0., 1.)).clamp(0., 0.98);
    let t = ((ring - inner) / (1. - inner)).clamp(0., 1.);
    1. - t * t * (3. - 2. * t)
}

/// A picture with a colour and a share of it that is seen, for each place.
struct Canvas {
    width: usize,
    height: usize,
    rgb: Vec<[u8; 3]>,
    /// 0 where nothing was drawn, 255 where something was.
    drawn: Vec<u8>,
}

impl Canvas {
    fn blend(&mut self, x: i32, y: i32, colour: [u8; 3], share: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let at = y as usize * self.width + x as usize;
        let share = share.clamp(0., 1.);
        for channel in 0..3 {
            self.rgb[at][channel] =
                (self.rgb[at][channel] as f32 * (1. - share) + colour[channel] as f32 * share) as u8;
        }
        let seen = self.drawn[at] as f32 / 255.;
        self.drawn[at] = ((seen + (1. - seen) * share) * 255.) as u8;
    }

    fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, colour: [u8; 3], share: f32) {
        for y in y0.floor() as i32..y1.ceil() as i32 {
            for x in x0.floor() as i32..x1.ceil() as i32 {
                self.blend(x, y, colour, share);
            }
        }
    }

    /// How far a place is from a rectangle with round corners: below 0 inside, above 0 outside.
    fn round_distance(px: f32, py: f32, (x0, y0, x1, y1): (f32, f32, f32, f32), radius: f32) -> f32 {
        let (cx, cy) = ((x0 + x1) / 2., (y0 + y1) / 2.);
        let (hx, hy) = ((x1 - x0) / 2. - radius, (y1 - y0) / 2. - radius);
        let (dx, dy) = ((px - cx).abs() - hx, (py - cy).abs() - hy);
        dx.max(0.).hypot(dy.max(0.)) + dx.max(dy).min(0.) - radius
    }

    /// A rectangle with round corners, filled; and a line of `width` around it on the
    /// outside (`outside`) or on the inside, as the program's own map draws them.
    #[allow(clippy::too_many_arguments)]
    fn round_rect(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        fill: Option<([u8; 3], f32)>,
        line: Option<([u8; 3], f32, f32, bool)>,
    ) {
        let reach = line.map_or(0., |(_, _, width, _)| width) + 2.;
        for py in (rect.1 - reach).floor() as i32..(rect.3 + reach).ceil() as i32 {
            for px in (rect.0 - reach).floor() as i32..(rect.2 + reach).ceil() as i32 {
                let d = Self::round_distance(px as f32 + 0.5, py as f32 + 0.5, rect, radius);
                if let Some((colour, share)) = fill {
                    let cover = (0.5 - d).clamp(0., 1.);
                    if cover > 0. {
                        self.blend(px, py, colour, share * cover);
                    }
                }
                if let Some((colour, share, width, outside)) = line {
                    let (inner, outer) = if outside { (0., width) } else { (-width, 0.) };
                    let cover = ((d - inner + 0.5).clamp(0., 1.)) * ((outer - d + 0.5).clamp(0., 1.));
                    if cover > 0. {
                        self.blend(px, py, colour, share * cover);
                    }
                }
            }
        }
    }

    /// A horizontal line `width` thick, its ends cut square, centred on `y`.
    fn hline(&mut self, x0: f32, x1: f32, y: f32, width: f32, colour: [u8; 3], share: f32) {
        for py in (y - width).floor() as i32..(y + width).ceil() as i32 {
            let cover = (width / 2. + 0.5 - (py as f32 + 0.5 - y).abs()).clamp(0., 1.);
            if cover <= 0. {
                continue;
            }
            for px in x0.floor() as i32..x1.ceil() as i32 {
                self.blend(px, py, colour, share * cover);
            }
        }
    }

    /// A line around the inside of a rectangle, `width` thick.
    fn frame_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, colour: [u8; 3], share: f32) {
        self.fill_rect(x0, y0, x1, y0 + width, colour, share);
        self.fill_rect(x0, y1 - width, x1, y1, colour, share);
        self.fill_rect(x0, y0 + width, x0 + width, y1 - width, colour, share);
        self.fill_rect(x1 - width, y0 + width, x1, y1 - width, colour, share);
    }

    fn disc(&mut self, x: f32, y: f32, radius: f32, colour: [u8; 3], share: f32) {
        for py in (y - radius - 1.).floor() as i32..=(y + radius + 1.).ceil() as i32 {
            for px in (x - radius - 1.).floor() as i32..=(x + radius + 1.).ceil() as i32 {
                let distance = ((px as f32 + 0.5 - x).powi(2) + (py as f32 + 0.5 - y).powi(2)).sqrt();
                // The rim a little soft, so it is not jagged.
                let cover = (radius + 0.5 - distance).clamp(0., 1.);
                if cover > 0. {
                    self.blend(px, py, colour, share * cover);
                }
            }
        }
    }

    /// A ring that may be very large: only the rows and the stretches of them that
    /// the ring is on are looked at.
    fn wide_ring(&mut self, x: f32, y: f32, radius: f32, thickness: f32, colour: [u8; 3], share: f32) {
        let reach = thickness / 2. + 1.;
        let top = ((y - radius - reach).floor() as i32).max(0);
        let bottom = ((y + radius + reach).ceil() as i32).min(self.height as i32 - 1);
        for py in top..=bottom {
            let dy = py as f32 + 0.5 - y;
            let outer = radius + reach;
            if dy.abs() > outer {
                continue;
            }
            let far = (outer * outer - dy * dy).sqrt();
            let inner = radius - reach;
            let near = if dy.abs() < inner { (inner * inner - dy * dy).sqrt() } else { 0. };
            for (from, to) in [(x - far, x - near), (x + near, x + far)] {
                let (left, right) = ((from.floor() as i32).max(0), (to.ceil() as i32).min(self.width as i32 - 1));
                for px in left..=right {
                    let distance = ((px as f32 + 0.5 - x).powi(2) + dy * dy).sqrt();
                    let cover = (thickness / 2. + 0.5 - (distance - radius).abs()).clamp(0., 1.);
                    if cover > 0. {
                        self.blend(px, py, colour, share * cover);
                    }
                }
            }
        }
    }

    /// A filled shape of corners, its edges soft (four samples to a pixel).
    fn polygon(&mut self, corners: &[[f32; 2]], colour: [u8; 3], share: f32) {
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for [x, y] in corners {
            left = left.min(*x);
            right = right.max(*x);
            top = top.min(*y);
            bottom = bottom.max(*y);
        }
        let inside = |x: f32, y: f32| {
            let mut crossings = false;
            let mut previous = corners[corners.len() - 1];
            for corner in corners {
                if (corner[1] > y) != (previous[1] > y)
                    && x < (previous[0] - corner[0]) * (y - corner[1]) / (previous[1] - corner[1]) + corner[0]
                {
                    crossings = !crossings;
                }
                previous = *corner;
            }
            crossings
        };
        for py in top.floor() as i32..=bottom.ceil() as i32 {
            for px in left.floor() as i32..=right.ceil() as i32 {
                let mut hits = 0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    if inside(px as f32 + ox, py as f32 + oy) {
                        hits += 1;
                    }
                }
                if hits > 0 {
                    self.blend(px, py, colour, share * hits as f32 / 4.);
                }
            }
        }
    }

    fn ring(&mut self, x: f32, y: f32, radius: f32, thickness: f32, colour: [u8; 3], share: f32) {
        for py in (y - radius - 2.).floor() as i32..=(y + radius + 2.).ceil() as i32 {
            for px in (x - radius - 2.).floor() as i32..=(x + radius + 2.).ceil() as i32 {
                let distance = ((px as f32 + 0.5 - x).powi(2) + (py as f32 + 0.5 - y).powi(2)).sqrt();
                let cover = (thickness / 2. + 0.5 - (distance - radius).abs()).clamp(0., 1.);
                if cover > 0. {
                    self.blend(px, py, colour, share * cover);
                }
            }
        }
    }
}

/// A colour of the picture of a tile, by where in it (in pixels of the tile).
fn sample(image: &ColorImage, u: f32, v: f32) -> [f32; 3] {
    let (w, h) = (image.size[0] as i32, image.size[1] as i32);
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |ix: i32, iy: i32| -> [f32; 3] {
        let pixel = image.pixels[iy.clamp(0, h - 1) as usize * w as usize + ix.clamp(0, w - 1) as usize];
        [pixel.r() as f32, pixel.g() as f32, pixel.b() as f32]
    };
    let (a, b, c, d) = (
        at(x0 as i32, y0 as i32),
        at(x0 as i32 + 1, y0 as i32),
        at(x0 as i32, y0 as i32 + 1),
        at(x0 as i32 + 1, y0 as i32 + 1),
    );
    let mut out = [0.; 3];
    for channel in 0..3 {
        let top = a[channel] * (1. - fx) + b[channel] * fx;
        let bottom = c[channel] * (1. - fx) + d[channel] * fx;
        out[channel] = top * (1. - fy) + bottom * fy;
    }
    out
}

/// Which tiles the picture needs, for asking for them: those of the level that
/// fits the zoom, and of the coarsest level, which are under them until they
/// come.
pub fn needed_tiles(info: &MapInfo, center: [f32; 2], scale: f32, width: f32, height: f32) -> Vec<TileKey> {
    let wanted = wanted_level(info, scale);
    let mut keys = Vec::new();
    for level in [info.max_level, wanted] {
        let span = TILE * 2f32.powi(level as i32);
        let (columns, rows) = grid(info, level);
        let min = [center[0] - width / 2. / scale, center[1] - height / 2. / scale];
        let max = [center[0] + width / 2. / scale, center[1] + height / 2. / scale];
        let first_x = ((min[0] / span).floor() as i32).max(0);
        let last_x = ((max[0] / span).floor() as i32).min(columns as i32 - 1);
        let first_y = ((min[1] / span).floor() as i32).max(0);
        let last_y = ((max[1] / span).floor() as i32).min(rows as i32 - 1);
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                keys.push(TileKey {
                    level,
                    x: x as u32,
                    y: y as u32,
                });
            }
        }
        if wanted == info.max_level {
            break;
        }
    }
    keys
}

fn wanted_level(info: &MapInfo, scale: f32) -> u32 {
    (-scale.log2()).floor().clamp(0., info.max_level as f32) as u32
}

/// The picture: blue, green, red and the share that is seen, each of the three
/// already taken by it, row by row from the top.
pub fn render(p: &Params) -> Vec<u8> {
    let (w, h) = (p.width, p.height);
    let mut canvas = Canvas {
        width: w,
        height: h,
        rgb: vec![[0; 3]; w * h],
        drawn: vec![0; w * h],
    };
    let (half_w, half_h) = (w as f32 / 2., h as f32 / 2.);
    if p.unknown {
        canvas.rgb.fill([0x14, 0x1a, 0x28]);
        canvas.drawn.fill(255);
    }
    let wanted = wanted_level(p.info, p.scale);
    let coarse = p.info.max_level;

    // The tiles of the level that fits; where one is not there yet, the part of
    // the coarsest that is where it will be.
    let span = TILE * 2f32.powi(wanted as i32);
    let (columns, rows) = grid(p.info, wanted);
    let min = [p.center[0] - half_w / p.scale, p.center[1] - half_h / p.scale];
    let max = [p.center[0] + half_w / p.scale, p.center[1] + half_h / p.scale];
    let first_x = ((min[0] / span).floor() as i32).max(0);
    let last_x = ((max[0] / span).floor() as i32).min(columns as i32 - 1);
    let first_y = ((min[1] / span).floor() as i32).max(0);
    let last_y = ((max[1] / span).floor() as i32).min(rows as i32 - 1);
    for ty in first_y..=last_y {
        for tx in first_x..=last_x {
            let own = (p.tiles)(TileKey {
                level: wanted,
                x: tx as u32,
                y: ty as u32,
            });
            // The picture used, the span it covers, and where its tile begins.
            let (image, used_span, origin) = match own {
                Some(image) => (image, span, [tx as f32 * span, ty as f32 * span]),
                None if wanted < coarse => {
                    let ratio = 1u32 << (coarse - wanted);
                    let coarse_span = TILE * 2f32.powi(coarse as i32);
                    let key = TileKey {
                        level: coarse,
                        x: tx as u32 / ratio,
                        y: ty as u32 / ratio,
                    };
                    let Some(image) = (p.tiles)(key) else {
                        continue;
                    };
                    (
                        image,
                        coarse_span,
                        [key.x as f32 * coarse_span, key.y as f32 * coarse_span],
                    )
                }
                None => continue,
            };
            let x0 = (half_w + (tx as f32 * span - p.center[0]) * p.scale).floor().max(0.) as usize;
            let x1 = ((half_w + ((tx + 1) as f32 * span - p.center[0]) * p.scale).ceil().max(0.) as usize).min(w);
            let y0 = (half_h + (ty as f32 * span - p.center[1]) * p.scale).floor().max(0.) as usize;
            let y1 = ((half_h + ((ty + 1) as f32 * span - p.center[1]) * p.scale).ceil().max(0.) as usize).min(h);
            let pixels = image.size[0] as f32 / used_span;
            for py in y0..y1 {
                let map_y = p.center[1] + (py as f32 + 0.5 - half_h) / p.scale;
                let v = (map_y - origin[1]) * pixels;
                for px in x0..x1 {
                    let map_x = p.center[0] + (px as f32 + 0.5 - half_w) / p.scale;
                    let u = (map_x - origin[0]) * pixels;
                    let colour = sample(&image, u, v);
                    let at = py * w + px;
                    canvas.rgb[at] = [colour[0] as u8, colour[1] as u8, colour[2] as u8];
                    canvas.drawn[at] = 255;
                }
            }
        }
    }

    // Underground: the map darker, and the floors on it.
    if p.dim > 0. {
        let keep = 1. - p.dim.clamp(0., 1.);
        for colour in &mut canvas.rgb {
            for channel in colour.iter_mut() {
                *channel = (*channel as f32 * keep) as u8;
            }
        }
    }
    for floor in p.floors {
        let [left, top, right, bottom] = floor.rect;
        if right <= left || bottom <= top {
            continue;
        }
        let x0 = (half_w + (left - p.center[0]) * p.scale).floor().max(0.) as usize;
        let x1 = ((half_w + (right - p.center[0]) * p.scale).ceil().max(0.) as usize).min(w);
        let y0 = (half_h + (top - p.center[1]) * p.scale).floor().max(0.) as usize;
        let y1 = ((half_h + (bottom - p.center[1]) * p.scale).ceil().max(0.) as usize).min(h);
        let (image_w, image_h) = (floor.image.size[0] as f32, floor.image.size[1] as f32);
        for py in y0..y1 {
            let map_y = p.center[1] + (py as f32 + 0.5 - half_h) / p.scale;
            let v = (map_y - top) / (bottom - top) * image_h;
            for px in x0..x1 {
                let map_x = p.center[0] + (px as f32 + 0.5 - half_w) / p.scale;
                let u = (map_x - left) / (right - left) * image_w;
                let [r, g, b, a] = sample_rgba(&floor.image, u, v);
                let share = a / 255.;
                if share <= 0. {
                    continue;
                }
                let colour = [r, g, b];
                canvas.blend(px as i32, py as i32, [colour[0] as u8, colour[1] as u8, colour[2] as u8], share);
            }
        }
    }

    // The markers of the kinds that are chosen.
    for mark in p.markers {
        let x = half_w + (mark.at[0] - p.center[0]) * p.scale;
        let y = half_h + (mark.at[1] - p.center[1]) * p.scale;
        if x < -12. || y < -12. || x > w as f32 + 12. || y > h as f32 + 12. {
            continue;
        }
        let strength = if mark.done { 0.35 } else { 1. };
        match (&mark.image, p.dots) {
            (Some(image), false) => {
                let size = 20.;
                let (image_w, image_h) = (image.size[0] as f32, image.size[1] as f32);
                for py in (y - size / 2.).floor() as i32..(y + size / 2.).ceil() as i32 {
                    for px in (x - size / 2.).floor() as i32..(x + size / 2.).ceil() as i32 {
                        let u = (px as f32 + 0.5 - (x - size / 2.)) / size * image_w;
                        let v = (py as f32 + 0.5 - (y - size / 2.)) / size * image_h;
                        let [r, g, b, a] = sample_rgba(image, u, v);
                        let colour = [r, g, b];
                        canvas.blend(px, py, [colour[0] as u8, colour[1] as u8, colour[2] as u8], a / 255. * strength);
                    }
                }
            }
            (None, false) => {
                canvas.disc(x, y, 5., mark.color, strength);
                canvas.ring(x, y, 5., 1., [0, 0, 0], 0.6 * strength);
            }
            (_, true) => canvas.disc(x, y, 2., mark.color, strength),
        }
        if mark.done && !p.dots {
            canvas.ring(x, y, 10., 1.5, [0x4a, 0xde, 0x80], 0.8);
        }
    }

    // The chests.
    for icon in p.icons {
        let x = half_w + (icon.at[0] - p.center[0]) * p.scale;
        let y = half_h + (icon.at[1] - p.center[1]) * p.scale;
        if x < -12. || y < -12. || x > w as f32 + 12. || y > h as f32 + 12. {
            continue;
        }
        draw_chest(&mut canvas, x, y, icon.tier, icon.look);
    }

    // The player.
    if let Some(player) = p.player {
        let x = half_w + (player[0] - p.center[0]) * p.scale;
        let y = half_h + (player[1] - p.center[1]) * p.scale;
        if let Some(range) = p.range {
            canvas.wide_ring(x, y, range * p.scale, 2., [0x20, 0xc0, 0xff], 0.85);
        }
        if let Some([dx, dy]) = p.facing {
            // An arrow that points the way the player faces.
            let (nx, ny) = (-dy, dx);
            let shape = |grow: f32| {
                [
                    [x + dx * 12. * grow, y + dy * 12. * grow],
                    [x - dx * 8. * grow + nx * 8.5 * grow, y - dy * 8. * grow + ny * 8.5 * grow],
                    [x - dx * 3. * grow, y - dy * 3. * grow],
                    [x - dx * 8. * grow - nx * 8.5 * grow, y - dy * 8. * grow - ny * 8.5 * grow],
                ]
            };
            canvas.polygon(&shape(1.35), [255, 255, 255], 1.);
            canvas.polygon(&shape(1.), [0x20, 0xc0, 0xff], 1.);
        } else {
            canvas.disc(x, y, 8., [0x20, 0xc0, 0xff], 0.35);
            canvas.ring(x, y, 8., 2., [255, 255, 255], 1.);
            canvas.disc(x, y, 3.5, [0x20, 0xc0, 0xff], 1.);
        }
    }

    // The share that is seen, and the colours taken by it.
    let mut out = vec![0u8; w * h * 4];
    for py in 0..h {
        for px in 0..w {
            let at = py * w + px;
            let drawn = canvas.drawn[at] as f32 / 255.;
            let mut share = p.opacity.clamp(0., 1.) * fade(px as f32 + 0.5, py as f32 + 0.5, w as f32, h as f32, p.vignette) * drawn;
            if p.edit {
                // Not empty anywhere, so that the window can be taken hold of
                // anywhere, and darker, so that it is seen that it can be moved.
                share = share.max(0.30);
            }
            let rgb = canvas.rgb[at];
            let base = at * 4;
            out[base] = (rgb[2] as f32 * share) as u8;
            out[base + 1] = (rgb[1] as f32 * share) as u8;
            out[base + 2] = (rgb[0] as f32 * share) as u8;
            out[base + 3] = (share * 255.) as u8;
        }
    }
    if let Some(levels) = p.levels {
        draw_levels(&mut out, w, h, levels);
    }
    if p.unknown {
        // A question mark where the buttons of the levels are.
        let [x, y, size, _] = level_buttons(w, 0)[0];
        for py in y.max(0)..(y + size).min(h as i32) {
            for px in x.max(0)..(x + size).min(w as i32) {
                let rim = px < x + 2 || px >= x + size - 2 || py < y + 2 || py >= y + size - 2;
                put(&mut out, w, px as usize, py as usize, if rim { [0xff, 0xc3, 0x4a] } else { [0x16, 0x1e, 0x30] });
            }
        }
        put_glyph(&mut out, w, (x + size / 2, y + size / 2), [0b111, 0b001, 0b011, 0b000, 0b010], 3, [255, 255, 255]);
    }
    if p.edit {
        // A frame of amber, as seen as it can be.
        let amber = [0xff, 0xc3, 0x4a];
        for thickness in 0..2usize {
            for x in 0..w {
                for y in [thickness, h.saturating_sub(1 + thickness)] {
                    put(&mut out, w, x, y, amber);
                }
            }
            for y in 0..h {
                for x in [thickness, w.saturating_sub(1 + thickness)] {
                    put(&mut out, w, x, y, amber);
                }
            }
        }
    }
    out
}

/// A colour of the picture of a floor (with how much of it is seen), by where in
/// it (in pixels of the picture).
pub(crate) fn sample_rgba(image: &ColorImage, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (image.size[0] as i32, image.size[1] as i32);
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |ix: i32, iy: i32| -> [f32; 4] {
        let pixel = image.pixels[iy.clamp(0, h - 1) as usize * w as usize + ix.clamp(0, w - 1) as usize];
        // Not taken by the share it has: that is done when it is laid on.
        let [r, g, b, a] = pixel.to_srgba_unmultiplied();
        [r as f32, g as f32, b as f32, a as f32]
    };
    let (a, b, c, d) = (
        at(x0 as i32, y0 as i32),
        at(x0 as i32 + 1, y0 as i32),
        at(x0 as i32, y0 as i32 + 1),
        at(x0 as i32 + 1, y0 as i32 + 1),
    );
    let mut out = [0.; 4];
    for channel in 0..4 {
        let top = a[channel] * (1. - fx) + b[channel] * fx;
        let bottom = c[channel] * (1. - fx) + d[channel] * fx;
        out[channel] = top * (1. - fy) + bottom * fy;
    }
    out
}

/// The digits, three across and five down, a bit for each place.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// A glyph of three across and five down, centred at a place.
fn put_glyph(out: &mut [u8], width: usize, centre: (i32, i32), rows: [u8; 5], size: i32, colour: [u8; 3]) {
    let (left, top) = (centre.0 - 3 * size / 2, centre.1 - 5 * size / 2);
    for (row, bits) in rows.iter().enumerate() {
        for column in 0..3 {
            if bits & (0b100 >> column) != 0 {
                for dy in 0..size {
                    for dx in 0..size {
                        let (x, y) = (left + column * size + dx, top + row as i32 * size + dy);
                        if x >= 0 && y >= 0 {
                            put(out, width, x as usize, y as usize, colour);
                        }
                    }
                }
            }
        }
    }
}

/// A number, centred at a place, as big as `size` of a place of the font.
fn put_number(out: &mut [u8], width: usize, centre: (i32, i32), number: usize, size: i32, colour: [u8; 3]) {
    let digits: Vec<usize> = number.to_string().bytes().map(|digit| (digit - b'0') as usize).collect();
    let total = digits.len() as i32 * 4 * size - size;
    let mut left = centre.0 - total / 2;
    let top = centre.1 - 5 * size / 2;
    for digit in digits {
        for (row, bits) in DIGITS[digit].iter().enumerate() {
            for column in 0..3 {
                if bits & (0b100 >> column) != 0 {
                    for dy in 0..size {
                        for dx in 0..size {
                            let (x, y) = (left + column * size + dx, top + row as i32 * size + dy);
                            if x >= 0 && y >= 0 {
                                put(out, width, x as usize, y as usize, colour);
                            }
                        }
                    }
                }
            }
        }
        left += 4 * size;
    }
}

/// The buttons of the levels (or, when they are not to be clicked, the level the
/// player is on, if it is not the surface).
fn draw_levels(out: &mut [u8], width: usize, height: usize, levels: LevelsView) {
    let amber = [0xff, 0xc3, 0x4a];
    let navy = [0x16, 0x1e, 0x30];
    let rects = level_buttons(width, levels.max);
    let shown: Vec<(usize, [i32; 4])> = if levels.buttons {
        rects.into_iter().enumerate().collect()
    } else if levels.current > 0 {
        vec![(levels.current, rects[0])]
    } else {
        Vec::new()
    };
    for (number, [x, y, w, h]) in shown {
        let current = number == levels.current;
        for py in y.max(0)..(y + h).min(height as i32) {
            for px in x.max(0)..(x + w).min(width as i32) {
                let rim = px < x + 2 || px >= x + w - 2 || py < y + 2 || py >= y + h - 2;
                let colour = if rim || current { amber } else { navy };
                put(out, width, px as usize, py as usize, colour);
            }
        }
        put_number(out, width, (x + w / 2, y + h / 2), number, 3, if current { [0, 0, 0] } else { [255, 255, 255] });
    }
}

/// One place of the picture, fully seen.
fn put(out: &mut [u8], width: usize, x: usize, y: usize, colour: [u8; 3]) {
    let at = (y * width + x) * 4;
    if at + 3 < out.len() {
        out[at] = colour[2];
        out[at + 1] = colour[1];
        out[at + 2] = colour[0];
        out[at + 3] = 255;
    }
}

/// A small chest: a box with a lid, in the colour of its kind, grey if it is
/// opened, an outline only if it is not found yet.
fn draw_chest(canvas: &mut Canvas, x: f32, y: f32, tier: u8, look: IconLook) {
    let half = match tier {
        4 | 5 => 8.,
        3 => 7.5,
        _ => 7.,
    };
    // The same as the chest of the map of the program (`map_tab::draw_chest`): round corners
    // (3), the lid at 38 %, the same colours and the same strengths.
    let rect = (x - half, y - half * 0.8, x + half, y + half * 0.8);
    let (x0, y0, x1, y1) = rect;
    let colour = tier_rgb(tier);
    let lid = y0 + (y1 - y0) * 0.38;
    match look {
        IconLook::Faint => {
            canvas.round_rect(rect, 3., Some(([0x15, 0x1a, 0x24], 0.59)), Some((colour, 0.7, 1.2, false)));
            canvas.hline(x0, x1, lid, 1.0, colour, 0.7);
        }
        _ => {
            let (fill, share) = match look {
                IconLook::Opened => (OPENED, 0.8),
                IconLook::Likely => (OPENED, 0.5),
                _ => (colour, 1.),
            };
            canvas.round_rect(rect, 3., Some((fill, share)), Some((EDGE, 1., 1.5, true)));
            canvas.hline(x0, x1, lid, 1.3, EDGE, 1.);
            canvas.disc(x, lid, 1.6, EDGE, 1.);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile that is all of one colour.
    fn solid(r: u8, g: u8, b: u8) -> Arc<ColorImage> {
        Arc::new(ColorImage::new(
            [256, 256],
            vec![egui::Color32::from_rgb(r, g, b); 256 * 256],
        ))
    }

    fn params<'a>(
        info: &'a MapInfo,
        tiles: &'a dyn Fn(TileKey) -> Option<Arc<ColorImage>>,
        icons: &'a [Icon],
    ) -> Params<'a> {
        Params {
            width: 120,
            height: 100,
            center: [info.origin[0], info.origin[1]],
            scale: 1. / 16.,
            opacity: 0.8,
            vignette: 0.5,
            edit: false,
            info,
            tiles,
            icons,
            player: None,
            facing: None,
            range: None,
            floors: &[],
            markers: &[],
            dots: false,
            unknown: false,
            dim: 0.,
            levels: None,
        }
    }

    fn pixel(out: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * width + x) * 4;
        [out[at], out[at + 1], out[at + 2], out[at + 3]]
    }

    #[test]
    fn the_middle_is_seen_and_the_edges_are_empty() {
        let info = MapInfo::default();
        let tiles = |_: TileKey| Some(solid(200, 100, 50));
        let out = render(&params(&info, &tiles, &[]));
        assert_eq!(out.len(), 120 * 100 * 4);
        // The middle: 80% of the colour, as blue, green, red, with the share.
        let [b, g, r, a] = pixel(&out, 120, 60, 50);
        assert!((a as i32 - 204).abs() <= 2, "alpha {a}");
        assert!((r as i32 - (200. * 0.8) as i32).abs() <= 3, "red {r}");
        assert!((g as i32 - (100. * 0.8) as i32).abs() <= 3);
        assert!((b as i32 - (50. * 0.8) as i32).abs() <= 3);
        // The corners are nothing at all.
        assert_eq!(pixel(&out, 120, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&out, 120, 119, 99)[3], 0);
        // And the further out the less.
        assert!(pixel(&out, 120, 90, 50)[3] > pixel(&out, 120, 110, 50)[3]);
        // Nothing has more of a colour than it is seen.
        for chunk in out.chunks(4) {
            assert!(chunk[0] <= chunk[3] && chunk[1] <= chunk[3] && chunk[2] <= chunk[3]);
        }
    }

    #[test]
    fn where_there_is_no_tile_there_is_nothing() {
        let info = MapInfo::default();
        let tiles = |_: TileKey| None;
        let out = render(&params(&info, &tiles, &[]));
        assert!(out.chunks(4).all(|pixel| pixel[3] == 0));
    }

    #[test]
    fn a_tile_that_has_not_come_is_the_coarse_one_in_its_place() {
        let info = MapInfo::default();
        // Only the coarsest level is there; the zoom wants a finer one.
        let coarse = info.max_level;
        let tiles = move |key: TileKey| (key.level == coarse).then(|| solid(10, 200, 10));
        let mut p = params(&info, &tiles, &[]);
        p.scale = 0.5;
        let out = render(&p);
        let [_, g, _, a] = pixel(&out, 120, 60, 50);
        assert!(a > 150 && g > 100);
    }

    #[test]
    fn a_chest_is_drawn_where_it_is_in_the_colour_of_its_kind() {
        let info = MapInfo::default();
        let tiles = |_: TileKey| Some(solid(40, 40, 40));
        // A golden chest in the middle.
        let icons = [Icon {
            at: [info.origin[0], info.origin[1]],
            tier: 4,
            look: IconLook::Closed,
        }];
        let mut p = params(&info, &tiles, &icons);
        p.vignette = 0.0;
        p.opacity = 1.;
        let out = render(&p);
        let [b, g, r, _] = pixel(&out, 120, 60, 50);
        assert!(r > 200 && g > 150 && b < 120, "{r} {g} {b}");
        // The same chest opened is grey.
        let icons = [Icon { look: IconLook::Opened, ..icons[0] }];
        let out = render(&params(&info, &tiles, &icons));
        let [b, g, r, _] = pixel(&out, 120, 60, 50);
        assert!((r as i32 - g as i32).abs() < 40 && (g as i32 - b as i32).abs() < 40);
    }

    #[test]
    fn a_floor_is_laid_on_a_darker_map() {
        let info = MapInfo::default();
        let tiles = |_: TileKey| Some(solid(200, 200, 200));
        // A floor that is red, covering the right half of the picture.
        let floor = FloorPicture {
            rect: [info.origin[0], info.origin[1] - 800., info.origin[0] + 1000., info.origin[1] + 800.],
            image: Arc::new(ColorImage::new([4, 4], vec![egui::Color32::from_rgb(255, 0, 0); 16])),
        };
        let floors = [floor];
        let mut p = params(&info, &tiles, &[]);
        p.floors = &floors;
        p.dim = 0.75;
        p.vignette = 0.;
        p.opacity = 1.;
        let out = render(&p);
        // Left of the middle: the map, darker (200 * 0.25 = 50).
        let [_, _, r, _] = pixel(&out, 120, 20, 50);
        assert!((r as i32 - 50).abs() <= 3, "dark map {r}");
        // Right of the middle: the floor, red.
        let [b, _, r, _] = pixel(&out, 120, 100, 50);
        assert!(r > 240 && b < 10, "floor {r} {b}");
    }

    #[test]
    fn a_marker_is_drawn_where_it_is_and_fainter_when_collected() {
        let info = MapInfo::default();
        let tiles = |_: TileKey| Some(solid(0, 0, 0));
        let marks = [
            MarkDraw { at: [info.origin[0], info.origin[1]], image: None, color: [255, 0, 0], done: false },
            MarkDraw { at: [info.origin[0] + 400., info.origin[1]], image: None, color: [255, 0, 0], done: true },
        ];
        let mut p = params(&info, &tiles, &[]);
        p.markers = &marks;
        p.vignette = 0.;
        p.opacity = 1.;
        let out = render(&p);
        // Scale is 1/16: 400 pixels of the map are 25 of the picture.
        let [_, _, strong, _] = pixel(&out, 120, 60, 50);
        let [_, _, faint, _] = pixel(&out, 120, 85, 50);
        assert!(strong > 200, "{strong}");
        assert!(faint > 40 && faint < 130, "{faint}");
    }

    #[test]
    fn the_buttons_of_the_levels_are_numbered_from_the_right() {
        let buttons = level_buttons(400, 3);
        assert_eq!(buttons.len(), 4);
        assert!(buttons[0][0] > buttons[1][0] && buttons[1][0] > buttons[3][0], "0 is farthest right");
        assert!(buttons[0][0] + buttons[0][2] < 400);
        let info = MapInfo::default();
        let tiles = |_: TileKey| Some(solid(10, 10, 10));
        let mut p = params(&info, &tiles, &[]);
        p.width = 400;
        p.levels = Some(LevelsView { current: 2, max: 3, buttons: true });
        let out = render(&p);
        // The current level is amber, seen in full.
        let [x, y, w, h] = buttons[2];
        let [b, g, r, a] = pixel(&out, 400, (x + 5) as usize, (y + h / 2) as usize);
        assert_eq!(a, 255);
        assert!(r > 200 && g > 150 && b < 120, "{r} {g} {b} {w}");
    }

    #[test]
    fn while_it_is_moved_it_is_not_empty_anywhere() {
        let info = MapInfo::default();
        let tiles = |_: TileKey| None;
        let mut p = params(&info, &tiles, &[]);
        p.edit = true;
        let out = render(&p);
        assert!(out.chunks(4).all(|pixel| pixel[3] >= 70));
    }

    #[test]
    fn the_tiles_needed_are_those_in_view() {
        let info = MapInfo::default();
        let keys = needed_tiles(&info, [info.origin[0], info.origin[1]], 0.5, 400., 300.);
        assert!(keys.iter().any(|key| key.level == info.max_level));
        assert!(keys.iter().any(|key| key.level == 1));
        assert!(keys.len() < 40);
    }
}
