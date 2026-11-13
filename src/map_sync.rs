//! Finding which part of the map the game shows in its own big map, from a picture of the
//! screen: so that the markers can be drawn over it where they belong.
//!
//! The picture of the map in the game is the one of the tiles the program has (the official
//! interactive map is made of it).  The big map of the game is not turned, so what is looked
//! for is where its middle is on the map and how big a unit of the map is on the screen.  The
//! picture of the screen, made small and grey, is held up to the same part of the tiles, made
//! as small, and how alike they are is counted (the correlation of the two, which does not
//! mind the game making the map darker or tinting it).  The signs, names and fog the game
//! draws over its map make it less alike, but not somewhere else more alike.
//!
//! When the map is opened it has the player in its middle, so only the zoom is looked for
//! (`acquire`).  After that it is followed from one picture to the next (`track`): moved a
//! little, zoomed a little.
//!
//! Nothing here reads the game: only what is on the screen.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::map_tiles::{MapInfo, TILE, TileKey};

/// A grey picture; a place where nothing is known is not a number.
#[derive(Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

/// What of the map the game shows: the place of the full size map in the middle of the
/// game's window, and how much of the height of the window a unit of the map takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub center: [f32; 2],
    pub zoom: f32,
}

impl View {
    /// Where a place of the full size map is in a window of this size.
    pub fn to_screen(&self, at: [f32; 2], width: f32, height: f32) -> [f32; 2] {
        let scale = self.zoom * height;
        [width / 2. + (at[0] - self.center[0]) * scale, height / 2. + (at[1] - self.center[1]) * scale]
    }
}

/// A view that was found, and how alike the screen and the map are there (1 is the same).
#[derive(Clone, Copy, Debug)]
pub struct Found {
    pub view: View,
    pub score: f32,
}

/// The least and the most a unit of the map takes of the height of the window in the game's
/// map (measured: about 0.00021 zoomed all the way out, 0.0024 all the way in), with room.
pub const ZOOM_LEAST: f32 = 0.0002;
pub const ZOOM_MOST: f32 = 0.0035;
/// Less alike than this, and it is not taken as found (the pictures of the game that were
/// tried are 0.7 to 0.85 alike at the right place, and under 0.32 at any wrong one).
pub const GOOD: f32 = 0.4;

/// A view found with nothing known before is taken only if it is this alike.
pub const SURE: f32 = 0.5;

/// The part of the window that is held up to the map: the rest has the buttons and the
/// writing of the game (left, top, right, bottom, as shares of the window).
const RECT: [f32; 4] = [0.07, 0.15, 0.92, 0.87];

/// What the map looks like at a place.
pub trait Ground {
    /// How light the full size map is at a place, 0 to 255, looked at with pixels this many
    /// units big; not a number where it is not known.
    fn at(&self, x: f32, y: f32, units: f32) -> f32;

    /// The part of the map around a place, as a picture whose pixels are `units` big.
    fn patch(&self, center: [f32; 2], units: f32, w: usize, h: usize) -> Gray {
        let mut px = Vec::with_capacity(w * h);
        let (left, top) = (center[0] - w as f32 / 2. * units, center[1] - h as f32 / 2. * units);
        for y in 0..h {
            for x in 0..w {
                px.push(self.at(left + (x as f32 + 0.5) * units, top + (y as f32 + 0.5) * units, units));
            }
        }
        Gray { w, h, px }
    }
}

/// Whether the big map of the game is open in a picture of its window: the round light
/// buttons it has are there, the settings at the bottom left and the cross at the top
/// right (the game puts them by the corners, sized by the height of the window).  Not only
/// that the two places are light (snow and sky are): the dark signs in them are looked for
/// too, the cog wheel in the one and the cross in the other.
pub fn map_open(pixels: &[u8], width: usize, height: usize, step: usize, rgb: [usize; 3]) -> bool {
    let h = height as f32;
    let colour = |x: f32, y: f32| -> Option<(i32, i32, i32)> {
        let (x, y) = (x.round() as i32, y.round() as i32);
        if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
            return None;
        }
        let at = (y as usize * width + x as usize) * step;
        Some((pixels[at + rgb[0]] as i32, pixels[at + rgb[1]] as i32, pixels[at + rgb[2]] as i32))
    };
    // The colour of the buttons: light, a little warm, not a colour.
    let light = |x: f32, y: f32| colour(x, y).is_some_and(|(r, g, b)| r > 195 && g > 190 && b > 175 && r >= b && r - b < 50);
    // The colour of the signs in them: a dark blue grey.
    let dark = |x: f32, y: f32| colour(x, y).is_some_and(|(r, g, b)| r < 125 && g < 125 && b < 140);
    // How many of 16 places around a middle are light, or dark.
    let around = |cx: f32, cy: f32, radius: f32, is: &dyn Fn(f32, f32) -> bool| {
        (0..16)
            .filter(|k| {
                let turn = *k as f32 / 16. * std::f32::consts::TAU;
                is(cx + radius * turn.cos(), cy + radius * turn.sin())
            })
            .count()
    };
    let raw = std::env::var_os("IRMINSUL_SYNC_RAW").is_some();
    // The settings: a light disc, a light middle, the cog wheel between.
    let (gx, gy) = (0.0498 * h, h - 0.0551 * h);
    let cog = (around(gx, gy, 0.0185 * h, &light), around(gx, gy, 0.0089 * h, &dark), light(gx, gy));
    // The cross: a light disc, a dark middle, dark along the two slants.
    let (cx, cy) = (width as f32 - 0.0702 * h, 0.0444 * h);
    let slant = 0.0058 * h;
    let arms = [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)].iter().filter(|(dx, dy)| dark(cx + dx * slant, cy + dy * slant)).count();
    let cross = (around(cx, cy, 0.0185 * h, &light), dark(cx, cy), arms, light(cx - 0.0125 * h, cy) && light(cx + 0.0125 * h, cy));
    if raw {
        println!("   the settings: {cog:?}, the cross: {cross:?}");
    }
    cog.0 >= 13 && cog.1 >= 6 && cog.2 && cross.0 >= 11 && cross.1 && cross.2 >= 3 && cross.3
}

/// Makes a picture of the screen small and grey.  `step` is how many bytes a pixel has, and
/// `rgb` where the red, green and blue are in it.
pub fn reduce(pixels: &[u8], width: usize, height: usize, step: usize, rgb: [usize; 3], tw: usize, th: usize) -> Gray {
    let mut px = vec![0f32; tw * th];
    for ty in 0..th {
        let (y0, y1) = (ty * height / th, ((ty + 1) * height / th).max(ty * height / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * width / tw, ((tx + 1) * width / tw).max(tx * width / tw + 1));
            // Not every pixel of a big box: enough of them.
            let (sy, sx) = (((y1 - y0) / 4).max(1), ((x1 - x0) / 4).max(1));
            let (mut sum, mut count) = (0f32, 0f32);
            let mut y = y0;
            while y < y1.min(height) {
                let mut x = x0;
                while x < x1.min(width) {
                    let at = (y * width + x) * step;
                    sum += 0.299 * pixels[at + rgb[0]] as f32 + 0.587 * pixels[at + rgb[1]] as f32 + 0.114 * pixels[at + rgb[2]] as f32;
                    count += 1.;
                    x += sx;
                }
                y += sy;
            }
            px[ty * tw + tx] = if count > 0. { sum / count } else { f32::NAN };
        }
    }
    Gray { w: tw, h: th, px }
}

/// How alike the picture of the screen is to the map, the map being `margin` pixels bigger on
/// every side and the screen put `dx`, `dy` off its middle.
fn alike(shot: &Gray, map: &Gray, margin: i32, dx: i32, dy: i32) -> Option<f32> {
    let (x0, x1) = ((RECT[0] * shot.w as f32) as usize, (RECT[2] * shot.w as f32) as usize);
    let (y0, y1) = ((RECT[1] * shot.h as f32) as usize, (RECT[3] * shot.h as f32) as usize);
    let (ox, oy) = ((margin + dx) as usize, (margin + dy) as usize);
    let (mut n, mut s, mut ss, mut r, mut rr, mut sr) = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
    for y in y0..y1 {
        let shot_row = &shot.px[y * shot.w + x0..y * shot.w + x1];
        let map_row = &map.px[(y + oy) * map.w + ox + x0..(y + oy) * map.w + ox + x1];
        for (a, b) in shot_row.iter().zip(map_row) {
            if b.is_nan() || a.is_nan() {
                continue;
            }
            n += 1.;
            s += a;
            ss += a * a;
            r += b;
            rr += b * b;
            sr += a * b;
        }
    }
    let all = ((x1 - x0) * (y1 - y0)) as f32;
    if n < all * 0.6 {
        return None;
    }
    let (vs, vr) = (ss - s * s / n, rr - r * r / n);
    if vs <= 1. || vr <= 1. {
        return None;
    }
    Some((sr - s * r / n) / (vs * vr).sqrt())
}

/// The best place of the screen on the map around a view, moved by up to `margin` pixels of
/// the small picture each way: the view there, and how alike.
fn slide(shot: &Gray, ground: &dyn Ground, view: View, margin: i32) -> Option<Found> {
    // A unit of the map is this many pixels of the small picture.
    let scale = view.zoom * shot.h as f32;
    let units = 1. / scale;
    let m = margin as usize;
    let map = ground.patch(view.center, units, shot.w + 2 * m, shot.h + 2 * m);
    let mut best: Option<(f32, i32, i32)> = None;
    for dy in -margin..=margin {
        for dx in -margin..=margin {
            if let Some(score) = alike(shot, &map, margin, dx, dy)
                && best.is_none_or(|(most, _, _)| score > most)
            {
                best = Some((score, dx, dy));
            }
        }
    }
    best.map(|(score, dx, dy)| Found {
        view: View { center: [view.center[0] + dx as f32 * units, view.center[1] + dy as f32 * units], zoom: view.zoom },
        score,
    })
}

fn better(a: Option<Found>, b: Option<Found>) -> Option<Found> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.score > a.score { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// A picture of the screen, at the sizes it is looked at.
pub struct Shot {
    pub small: Gray,
    pub middle: Gray,
    pub big: Gray,
}

impl Shot {
    pub fn new(pixels: &[u8], width: usize, height: usize, step: usize, rgb: [usize; 3]) -> Self {
        let sized = |th: usize| reduce(pixels, width, height, step, rgb, (th * width / height.max(1)).max(8), th);
        Self { small: sized(90), middle: sized(240), big: sized(540) }
    }
}

/// Makes a view that was found with the small picture exact: the middle, then the big one,
/// each trying the zoom a little bigger and smaller and the place a little moved.
fn refine(shot: &Shot, ground: &dyn Ground, found: Found) -> Found {
    let mut best = found;
    for (picture, steps, margin) in [(&shot.middle, [0.97f32, 0.985, 1., 1.015, 1.03], 4), (&shot.big, [0.992, 0.996, 1., 1.004, 1.008], 2)] {
        // Again while the best zoom is the farthest that was tried: it may be farther still.
        for _ in 0..5 {
            let from = best.view;
            let mut round: Option<(Found, f32)> = None;
            for step in steps {
                if let Some(found) = slide(picture, ground, View { center: from.center, zoom: from.zoom * step }, margin)
                    && round.is_none_or(|(most, _)| found.score > most.score)
                {
                    round = Some((found, step));
                }
            }
            let Some((found, step)) = round else {
                break;
            };
            best = found;
            if step != steps[0] && step != steps[4] {
                break;
            }
        }
    }
    best
}

/// Finds the view when the map was just opened: its middle is at the player (or near), and
/// the zoom is what it was left at, which is not known.
pub fn acquire(shot: &Shot, ground: &dyn Ground, around: [f32; 2]) -> Option<Found> {
    anywhere(shot, ground, around, 8)
}

/// Finds the view when nothing is known of it but that the player is somewhere in it or near
/// (the map was moved before it was looked at): any zoom, up to half a screen and more away.
/// Slow (seconds).
pub fn acquire_wide(shot: &Shot, ground: &dyn Ground, around: [f32; 2]) -> Option<Found> {
    anywhere(shot, ground, around, 60)
}

fn anywhere(shot: &Shot, ground: &dyn Ground, around: [f32; 2], margin: i32) -> Option<Found> {
    let mut best: Option<Found> = None;
    let mut zoom = ZOOM_LEAST;
    while zoom <= ZOOM_MOST {
        best = better(best, slide(&shot.small, ground, View { center: around, zoom }, margin));
        zoom *= 1.035;
    }
    let best = best?;
    let exact = refine(shot, ground, best);
    // With nothing known before, it has to be surer.
    (exact.score >= SURE).then_some(exact)
}

/// Follows the view from the last picture to this one: the map may have been moved (as far
/// as about a third of the screen) and zoomed (a little).
pub fn track(shot: &Shot, ground: &dyn Ground, last: View) -> Option<Found> {
    // Nothing moved, most of the time: that is seen at once.
    if let Some(still) = slide(&shot.small, ground, last, 1)
        && still.score >= 0.55
        && still.view.center == last.center
    {
        let exact = refine(shot, ground, still);
        return (exact.score >= GOOD).then_some(exact);
    }
    let mut best: Option<Found> = None;
    for step in [1f32, 0.94, 1.06, 0.88, 1.13] {
        let zoom = (last.zoom * step).clamp(ZOOM_LEAST, ZOOM_MOST);
        // The same zoom is looked at far around; another zoom means the wheel, not a drag.
        let margin = if step == 1. { 30 } else { 12 };
        best = better(best, slide(&shot.small, ground, View { center: last.center, zoom }, margin));
        // Good already as it was: nothing was zoomed.
        if step == 1. && best.is_some_and(|found| found.score >= 0.6) {
            break;
        }
    }
    let best = best?;
    let exact = refine(shot, ground, best);
    (exact.score >= GOOD).then_some(exact)
}

/// Finds the view again after it was lost: about the zoom it had, anywhere within a screen
/// of where it was.
pub fn recover(shot: &Shot, ground: &dyn Ground, last: View) -> Option<Found> {
    let mut best: Option<Found> = None;
    let mut zoom = (last.zoom * 0.6).max(ZOOM_LEAST);
    while zoom <= (last.zoom * 1.7).min(ZOOM_MOST) {
        best = better(best, slide(&shot.small, ground, View { center: last.center, zoom }, 45));
        zoom *= 1.035;
    }
    let exact = refine(shot, ground, best?);
    (exact.score >= SURE).then_some(exact)
}

/// Looks for the view anywhere within `reach` units of a place, at any zoom: slow, for when
/// nothing else is known.
pub fn search(shot: &Shot, ground: &dyn Ground, around: [f32; 2], reach: f32) -> Option<Found> {
    let mut best: Option<Found> = None;
    let mut zoom = ZOOM_LEAST;
    while zoom <= ZOOM_MOST {
        let units = 1. / (zoom * shot.small.h as f32);
        let margin = ((reach / units) as i32).clamp(4, 160);
        best = better(best, slide(&shot.small, ground, View { center: around, zoom }, margin));
        zoom *= 1.035;
    }
    let best = best?;
    let exact = refine(shot, ground, best);
    if std::env::var_os("IRMINSUL_SYNC_RAW").is_some() {
        println!("  raw: coarse {best:?} exact {exact:?}");
    }
    (exact.score >= GOOD).then_some(exact)
}

/// The tiles of a map on the disk, grey, read as they are asked for.
pub struct DiskGround {
    dir: PathBuf,
    max_level: u32,
    size: [f32; 2],
    tiles: RefCell<HashMap<TileKey, Option<Rc<Vec<u8>>>>>,
}

impl DiskGround {
    pub fn new(dir: PathBuf, info: &MapInfo) -> Self {
        Self { dir, max_level: info.max_level, size: info.size, tiles: RefCell::default() }
    }

    /// Lets go of what was read, when it is a lot.
    pub fn trim(&self) {
        let mut tiles = self.tiles.borrow_mut();
        if tiles.len() > 1500 {
            tiles.clear();
        }
    }

    /// Forgets the tiles that were not there: they may have come since.
    pub fn retry_missing(&self) {
        self.tiles.borrow_mut().retain(|_, tile| tile.is_some());
    }

    fn tile(&self, key: TileKey) -> Option<Rc<Vec<u8>>> {
        let mut tiles = self.tiles.borrow_mut();
        tiles
            .entry(key)
            .or_insert_with(|| {
                let suffix = if key.level == 0 { "P0".to_owned() } else { format!("N{}", key.level) };
                let path = self.dir.join(format!("{}_{}_{suffix}.webp", key.x, key.y));
                let image = image::open(path).ok()?.to_luma8();
                (image.width() == TILE as u32 && image.height() == TILE as u32).then(|| Rc::new(image.into_raw()))
            })
            .clone()
    }

    /// The tile that has a place, of the level wanted or, if that is not there, of a
    /// coarser one: the tile, how many units it is across, and where it begins.
    fn resolve(&self, x: f32, y: f32, wanted: u32) -> Option<(Rc<Vec<u8>>, f32, [f32; 2])> {
        for level in wanted..=self.max_level {
            let span = TILE * 2f32.powi(level as i32);
            let key = TileKey { level, x: (x / span) as u32, y: (y / span) as u32 };
            if let Some(tile) = self.tile(key) {
                return Some((tile, span, [key.x as f32 * span, key.y as f32 * span]));
            }
        }
        None
    }

    fn level(&self, units: f32) -> u32 {
        (units.max(1.).log2().floor() as u32).min(self.max_level)
    }
}

impl Ground for DiskGround {
    fn at(&self, x: f32, y: f32, units: f32) -> f32 {
        if x < 0. || y < 0. || x >= self.size[0] || y >= self.size[1] {
            return f32::NAN;
        }
        match self.resolve(x, y, self.level(units)) {
            Some((tile, span, from)) => {
                let (u, v) = (
                    (((x - from[0]) / span * TILE) as usize).min(TILE as usize - 1),
                    (((y - from[1]) / span * TILE) as usize).min(TILE as usize - 1),
                );
                tile[v * TILE as usize + u] as f32
            }
            None => f32::NAN,
        }
    }

    /// As for any ground, but the tile is not looked up again for every place.
    fn patch(&self, center: [f32; 2], units: f32, w: usize, h: usize) -> Gray {
        let wanted = self.level(units);
        let fine = TILE * 2f32.powi(wanted as i32);
        let mut px = Vec::with_capacity(w * h);
        let (left, top) = (center[0] - w as f32 / 2. * units, center[1] - h as f32 / 2. * units);
        // The tile of the last place: which one of the level wanted it stands for, and it.
        let mut last: Option<((i32, i32), Option<(Rc<Vec<u8>>, f32, [f32; 2])>)> = None;
        for row in 0..h {
            let y = top + (row as f32 + 0.5) * units;
            for column in 0..w {
                let x = left + (column as f32 + 0.5) * units;
                if x < 0. || y < 0. || x >= self.size[0] || y >= self.size[1] {
                    px.push(f32::NAN);
                    continue;
                }
                let cell = ((x / fine) as i32, (y / fine) as i32);
                if last.as_ref().is_none_or(|(was, _)| *was != cell) {
                    last = Some((cell, self.resolve(x, y, wanted)));
                }
                px.push(match last.as_ref().and_then(|(_, tile)| tile.as_ref()) {
                    Some((tile, span, from)) => {
                        let (u, v) = (
                            (((x - from[0]) / span * TILE) as usize).min(TILE as usize - 1),
                            (((y - from[1]) / span * TILE) as usize).min(TILE as usize - 1),
                        );
                        tile[v * TILE as usize + u] as f32
                    }
                    None => f32::NAN,
                });
            }
        }
        Gray { w, h, px }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made up map: smooth hills of light and dark, different everywhere.
    struct Hills;

    impl Ground for Hills {
        fn at(&self, x: f32, y: f32, _units: f32) -> f32 {
            let wave = |a: f32, b: f32, c: f32| (x * a + y * b + c).sin();
            128. + 40. * wave(0.011, 0.004, 0.3) + 30. * wave(-0.003, 0.017, 1.1) + 25. * wave(0.031, 0.027, 2.) + 20. * wave(0.0021, -0.0013, 0.7)
        }
    }

    /// What the game would show of the made up map: a window 1280 by 720, as bytes.
    fn screen(view: View) -> Vec<u8> {
        let (w, h) = (1280usize, 720usize);
        let mut out = Vec::with_capacity(w * h * 3);
        let scale = view.zoom * h as f32;
        for y in 0..h {
            for x in 0..w {
                let at = [view.center[0] + (x as f32 - w as f32 / 2.) / scale, view.center[1] + (y as f32 - h as f32 / 2.) / scale];
                // Darker and tinted, as the game does.
                let light = (Hills.at(at[0], at[1], 1.) * 0.7 + 20.) as u8;
                out.extend_from_slice(&[light, light, light]);
            }
        }
        out
    }

    #[test]
    fn the_zoom_is_found_when_the_map_opens_on_the_player() {
        for zoom in [0.0003f32, 0.0009, 0.0021] {
            let truth = View { center: [5000., 7000.], zoom };
            let shot = Shot::new(&screen(truth), 1280, 720, 3, [0, 1, 2]);
            let found = acquire(&shot, &Hills, [5010., 6990.]).expect("found");
            assert!((found.view.zoom / zoom - 1.).abs() < 0.01, "{found:?}");
            let off = (found.view.center[0] - 5000.).hypot(found.view.center[1] - 7000.) * zoom * 720.;
            assert!(off < 8., "{off} pixels off: {found:?}");
        }
    }

    #[test]
    fn a_map_that_is_moved_and_zoomed_is_followed() {
        let start = View { center: [5000., 7000.], zoom: 0.001 };
        // Dragged a quarter of the screen, then zoomed in a little.
        let moved = View { center: [5000. + 250. / 0.72, 7000. - 120. / 0.72], zoom: 0.001 };
        let shot = Shot::new(&screen(moved), 1280, 720, 3, [0, 1, 2]);
        let found = track(&shot, &Hills, start).expect("followed");
        let off = (found.view.center[0] - moved.center[0]).hypot(found.view.center[1] - moved.center[1]) * 0.72;
        assert!(off < 4. && (found.view.zoom / 0.001 - 1.).abs() < 0.01, "{off}: {found:?}");
        let zoomed = View { center: moved.center, zoom: 0.00107 };
        let shot = Shot::new(&screen(zoomed), 1280, 720, 3, [0, 1, 2]);
        let found = track(&shot, &Hills, found.view).expect("followed");
        assert!((found.view.zoom / 0.00107 - 1.).abs() < 0.01, "{found:?}");
    }

    #[test]
    fn a_place_of_the_map_is_put_on_the_screen() {
        let view = View { center: [100., 200.], zoom: 0.001 };
        assert_eq!(view.to_screen([100., 200.], 1920., 1080.), [960., 540.]);
        assert_eq!(view.to_screen([200., 200.], 1920., 1080.), [960. + 108., 540.]);
    }

    /// The real thing: pictures of the game's map (`IRMINSUL_SYNC_SHOTS`, files apart by `;`),
    /// the tiles (`IRMINSUL_SYNC_TILES`), and about where the player was
    /// (`IRMINSUL_SYNC_AROUND`, a place of the full size map).  Writes each picture again with
    /// the waypoints of the map marked on it, to be looked at.
    #[test]
    #[ignore]
    fn the_pictures_of_the_game_are_placed() {
        let shots = std::env::var("IRMINSUL_SYNC_SHOTS").expect("the pictures");
        let dir = PathBuf::from(std::env::var("IRMINSUL_SYNC_TILES").expect("the tiles"));
        let around: Vec<f32> = std::env::var("IRMINSUL_SYNC_AROUND").expect("the place").split(',').filter_map(|n| n.trim().parse().ok()).collect();
        let reach: f32 = std::env::var("IRMINSUL_SYNC_REACH").ok().and_then(|n| n.parse().ok()).unwrap_or(1500.);
        let info = MapInfo::default();
        let ground = DiskGround::new(dir, &info);
        let data = crate::markers::data();
        for path in shots.split(';') {
            let image = image::open(path).expect("a picture").to_rgb8();
            let (w, h) = (image.width() as usize, image.height() as usize);
            println!("{path}: the map is open: {}", map_open(image.as_raw(), w, h, 3, [0, 1, 2]));
            if std::env::var_os("IRMINSUL_SYNC_OPEN_ONLY").is_some() {
                continue;
            }
            let shot = Shot::new(image.as_raw(), w, h, 3, [0, 1, 2]);
            let started = std::time::Instant::now();
            let found = search(&shot, &ground, [around[0], around[1]], reach);
            println!("{path}: {found:?} in {:?}", started.elapsed());
            let Some(found) = found else {
                continue;
            };
            let again = std::time::Instant::now();
            let followed = track(&shot, &ground, View { center: [found.view.center[0] + 90., found.view.center[1] - 60.], zoom: found.view.zoom * 1.04 });
            println!("  followed from nearby: {followed:?} in {:?}", again.elapsed());
            let again = std::time::Instant::now();
            let still = track(&shot, &ground, found.view);
            println!("  followed, nothing moved: {still:?} in {:?}", again.elapsed());
            let again = std::time::Instant::now();
            let opened = acquire(&shot, &ground, [around[0], around[1]]);
            println!("  as when just opened: {opened:?} in {:?}", again.elapsed());
            // The waypoints of the map, drawn as the program draws markers over the game.
            let marks: Vec<crate::map_render::MarkDraw> = [2u32, 3]
                .iter()
                .flat_map(|label| data.points_of(*label).iter())
                .filter(|point| point.map == 2 && point.floor == 0)
                .map(|point| crate::map_render::MarkDraw {
                    at: [info.origin[0] + point.x, info.origin[1] + point.y],
                    image: None,
                    color: [255, 0, 255],
                    done: false,
                })
                .collect();
            let view = found.view;
            let picture = crate::map_render::render_marks(w, h, &move |at| view.to_screen(at, w as f32, h as f32), &marks, &[], 26., false, 1.);
            let mut out = image.clone();
            for (at, pixel) in out.pixels_mut().enumerate() {
                let over = &picture[at * 4..at * 4 + 4];
                let seen = over[3] as f32 / 255.;
                for channel in 0..3 {
                    pixel.0[channel] = (pixel.0[channel] as f32 * (1. - seen) + over[2 - channel] as f32) as u8;
                }
            }
            out.save(format!("{path}.placed.png")).expect("written");
        }
    }
}
