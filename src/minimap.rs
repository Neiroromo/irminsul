//! Which floor the player is on, told by the minimap of the game.
//!
//! The game draws the map of the floor the player is on in its minimap (the
//! surface, or the picture of an underground place), round, in the top left.  The
//! places the program thinks the player may be on are known (the surface, and the
//! floors whose picture covers the place), and so are their pictures (the tiles
//! of the map, and the pictures of the floors).  So the minimap is taken from the
//! screen, and the picture of each of those floors around the player is held up to
//! it: the one that has the same colours is the floor.
//!
//! Only the circle of the minimap is taken from the screen, a few times a second at
//! most, and only while the game is the window in front; it is not kept (but for
//! the one that is shown in the settings, to set the place of the circle), and
//! it is not sent anywhere.  It is off unless it is turned on.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{ColorImage, Context};
use serde::{Deserialize, Serialize};

use crate::APP_ID;
use crate::map_render::sample_rgba;
use crate::map_tiles::{MapInfo, MapTiles, TILE, TileKey};
use crate::marker_icons::MarkerIcons;
use crate::markers::Data;

/// How often the screen is looked at.
const EVERY: Duration = Duration::from_millis(1500);
/// How far around the player the pictures are held up to the minimap, in metres.
const RADIUS_METRES: f32 = 125.;
/// The colour a place with no picture is given (the ground of the map).
const GROUND: [u8; 3] = [18, 24, 40];
/// How much more alike the best floor must be than the next, and at least how alike.
const MARGIN: f32 = 0.04;
const LEAST: f32 = 0.45;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Where the circle of the minimap is, in the window of the game, as fractions of
    /// its height from its top left: the middle, and the radius.
    pub x: f32,
    pub y: f32,
    pub radius: f32,
}

impl Default for Settings {
    fn default() -> Self {
        // Measured on a screen of 1440 lines: the middle at (178, 152), a radius of 140.
        Self {
            enabled: false,
            x: 0.1236,
            y: 0.1056,
            radius: 0.0972,
        }
    }
}

/// `IRMINSUL_MINIMAP` replaces the file.
fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_MINIMAP") {
        return Some(path.into());
    }
    eframe::storage_dir(APP_ID).map(|dir| dir.join("minimap.json"))
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

/// A square picture, red, green and blue of each place.
pub struct Crop {
    pub size: usize,
    pub rgb: Vec<u8>,
}

/// How many of each colour there are (four levels of each of red, green, blue),
/// as shares of the whole.  The same whatever way the picture is turned, which the
/// minimap is as the camera turns.
pub type Signature = [f32; 64];

fn bin(red: u8, green: u8, blue: u8) -> usize {
    ((red >> 6) as usize) * 16 + ((green >> 6) as usize) * 4 + (blue >> 6) as usize
}

fn normalised(mut counts: [f32; 64], total: f32) -> Option<Signature> {
    if total < 50. {
        return None;
    }
    for count in &mut counts {
        *count /= total;
    }
    Some(counts)
}

/// The colours of the minimap: its circle, a little in from the edge (where the
/// frame is).
pub fn signature_of(crop: &Crop) -> Option<Signature> {
    let half = crop.size as f32 / 2.;
    let mut counts = [0f32; 64];
    let mut total = 0.;
    for y in 0..crop.size {
        for x in 0..crop.size {
            let (dx, dy) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
            if dx * dx + dy * dy > (half * 0.9).powi(2) {
                continue;
            }
            let at = (y * crop.size + x) * 3;
            counts[bin(crop.rgb[at], crop.rgb[at + 1], crop.rgb[at + 2])] += 1.;
            total += 1.;
        }
    }
    normalised(counts, total)
}

/// How alike two signatures are, 0 to 1 (the Bhattacharyya coefficient).
pub fn similarity(a: &Signature, b: &Signature) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x * y).sqrt()).sum()
}

/// The colours a source of pictures gives around a place of the map, `radius`
/// pixels of the map around it, `at` pixels of the full size map; `colour_at` gives
/// the colour at a place (None if it is not there to be had yet).  None if too much
/// of it is not there.
fn around(at: [f32; 2], radius: f32, colour_at: &dyn Fn(f32, f32) -> Option<[u8; 3]>) -> Option<Signature> {
    let mut counts = [0f32; 64];
    let (mut total, mut missing) = (0., 0.);
    let step = (radius / 40.).max(1.);
    let mut y = -radius;
    while y <= radius {
        let mut x = -radius;
        while x <= radius {
            if x * x + y * y <= radius * radius {
                match colour_at(at[0] + x, at[1] + y) {
                    Some(colour) => counts[bin(colour[0], colour[1], colour[2])] += 1.,
                    None => missing += 1.,
                }
                total += 1.;
            }
            x += step;
        }
        y += step;
    }
    if missing * 10. > total * 3. {
        return None;
    }
    normalised(counts, total - missing)
}

/// What a floor looks like around a place of the map (relative to its origin).
fn floor_look(image: &ColorImage, rect: [f32; 4], at: [f32; 2], radius: f32) -> Option<Signature> {
    let (width, height) = (image.size[0] as f32, image.size[1] as f32);
    around(at, radius, &|x, y| {
        if x < rect[0] || x > rect[2] || y < rect[1] || y > rect[3] {
            return Some(GROUND);
        }
        let [r, g, b, a] = sample_rgba(
            image,
            (x - rect[0]) / (rect[2] - rect[0]) * width,
            (y - rect[1]) / (rect[3] - rect[1]) * height,
        );
        // What is not of the floor is the ground.
        let share = (a / 255.).clamp(0., 1.);
        let mix = |colour: f32, ground: u8| (colour * share + f32::from(ground) * (1. - share)) as u8;
        Some([mix(r, GROUND[0]), mix(g, GROUND[1]), mix(b, GROUND[2])])
    })
}

/// What the surface looks like around a place of the map, from its tiles (the
/// finest level).
fn surface_look(tiles: &MapTiles, info: &MapInfo, at: [f32; 2], radius: f32) -> Option<Signature> {
    let origin = info.origin;
    around(at, radius, &|x, y| {
        let (px, py) = (origin[0] + x, origin[1] + y);
        if px < 0. || py < 0. || px >= info.size[0] || py >= info.size[1] {
            return Some(GROUND);
        }
        let (tx, ty) = ((px / TILE).floor() as u32, (py / TILE).floor() as u32);
        let image = tiles.image(TileKey { level: 0, x: tx, y: ty })?;
        let (u, v) = (
            (((px - tx as f32 * TILE) / TILE) * image.size[0] as f32) as usize,
            (((py - ty as f32 * TILE) / TILE) * image.size[1] as f32) as usize,
        );
        let pixel = image.pixels.get(v.min(image.size[1] - 1) * image.size[0] + u.min(image.size[0] - 1))?;
        Some([pixel.r(), pixel.g(), pixel.b()])
    })
}

/// The minimap, taken from the screen: its circle's square.
#[cfg(windows)]
pub fn capture(settings: &Settings) -> Option<Crop> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, ClientToScreen, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetForegroundWindow};

    if crate::overlay::what_is_in_front() != crate::hotkey::FRONT_GAME {
        return None;
    }
    unsafe {
        let window = GetForegroundWindow();
        let mut client = RECT::default();
        GetClientRect(window, &mut client).ok()?;
        let height = (client.bottom - client.top) as f32;
        if height < 300. {
            return None;
        }
        let mut origin = POINT { x: 0, y: 0 };
        if !ClientToScreen(window, &mut origin).as_bool() {
            return None;
        }
        let side = (settings.radius * 2. * height).round() as i32;
        if !(40..=900).contains(&side) {
            return None;
        }
        let left = origin.x + ((settings.x - settings.radius) * height).round() as i32;
        let top = origin.y + ((settings.y - settings.radius) * height).round() as i32;

        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0);
        let result = match bitmap {
            Ok(bitmap) if !bits.is_null() => {
                let old = SelectObject(memory, bitmap.into());
                let copied = BitBlt(memory, 0, 0, side, side, Some(screen), left, top, SRCCOPY | CAPTUREBLT).is_ok();
                let crop = copied.then(|| {
                    let pixels = std::slice::from_raw_parts(bits as *const u8, (side * side * 4) as usize);
                    let mut rgb = Vec::with_capacity((side * side * 3) as usize);
                    for pixel in pixels.chunks_exact(4) {
                        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                    }
                    Crop { size: side as usize, rgb }
                });
                SelectObject(memory, old);
                let _ = DeleteObject(bitmap.into());
                crop
            }
            _ => None,
        };
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        result
    }
}

#[cfg(not(windows))]
pub fn capture(_settings: &Settings) -> Option<Crop> {
    None
}

/// What the watching has made of the screen, for the settings to tell.
#[derive(Default)]
pub struct Report {
    /// The floors held up to the minimap, and how alike each is (0 is the surface).
    pub alike: Vec<(u32, f32)>,
    /// What was concluded, in words.
    pub text: String,
}

pub struct Watcher {
    pub settings: Settings,
    last: Option<Instant>,
    /// The floor that came out best the last time, and how many times in a row.
    streak: Option<(u32, u8)>,
    pub preview: Option<ColorImage>,
    pub report: Report,
}

impl Watcher {
    pub fn new() -> Self {
        Self {
            settings: Settings::load(),
            last: None,
            streak: None,
            preview: None,
            report: Report::default(),
        }
    }

    /// Call every frame.  Looks at the minimap now and then, and tells `levels` which
    /// floor it is of, when it is clear.
    pub fn frame(
        &mut self,
        ctx: &Context,
        player: Option<crate::PlayerPlace>,
        data: &Data,
        tiles: &MapTiles,
        floor_images: &MarkerIcons,
        info: &MapInfo,
    ) {
        if !self.settings.enabled {
            return;
        }
        ctx.request_repaint_after(EVERY);
        if self.last.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        let Some(player) = player.filter(|player| player.map != 0 && player.map == info.map) else {
            return;
        };
        self.last = Some(Instant::now());

        // The floors the player may be on: the surface, and those whose picture
        // covers the place.
        let Some(def) = data.atlas.map(player.map) else {
            return;
        };
        let mut candidates: Vec<(u32, &crate::atlas::Floor)> = Vec::new();
        for site in &def.sites {
            for floor in &site.floors {
                let rect = floor.rect;
                if rect[2] > rect[0] && player.at[0] >= rect[0] && player.at[0] <= rect[2] && player.at[1] >= rect[1] && player.at[1] <= rect[3] {
                    candidates.push((floor.id, floor));
                }
            }
        }
        // Nothing to choose between, but the pictures are asked for in any case.
        let radius = RADIUS_METRES * player.scale.max(0.1);
        let Some(crop) = capture(&self.settings) else {
            self.report.text = "the game is not in front, or its window can not be read".to_owned();
            return;
        };
        self.preview = Some(ColorImage::from_rgb([crop.size, crop.size], &crop.rgb));
        if candidates.is_empty() {
            self.report = Report { alike: Vec::new(), text: "no floor is here but the surface".to_owned() };
            self.streak = None;
            return;
        }
        let Some(minimap) = signature_of(&crop) else {
            return;
        };

        let mut alike: Vec<(u32, f32)> = Vec::new();
        if let Some(look) = surface_look(tiles, info, player.at, radius) {
            alike.push((0, similarity(&minimap, &look)));
        }
        for (id, floor) in &candidates {
            if let Some(image) = floor_images.image(*id, &floor.url)
                && let Some(look) = floor_look(&image, floor.rect, player.at, radius)
            {
                alike.push((*id, similarity(&minimap, &look)));
            }
        }
        alike.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let text = match alike.as_slice() {
            [best, second, ..] if best.1 >= LEAST && best.1 - second.1 >= MARGIN => {
                let (floor, share) = *best;
                let same = matches!(self.streak, Some((was, _)) if was == floor);
                let times = if same { self.streak.map_or(0, |(_, times)| times) + 1 } else { 1 };
                self.streak = Some((floor, times));
                // The same twice in a row.
                if times >= 2 {
                    crate::levels::set_vision(player.map, floor);
                }
                format!("floor {floor} ({share:.2}, next {:.2}), {times} in a row", second.1)
            }
            [_] | [] => {
                self.streak = None;
                "not enough pictures yet to choose".to_owned()
            }
            [best, ..] => {
                self.streak = None;
                format!("not clear ({:.2})", best.1)
            }
        };
        self.report = Report { alike, text };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop_of(colour: [u8; 3], size: usize) -> Crop {
        Crop { size, rgb: (0..size * size).flat_map(|_| colour).collect() }
    }

    #[test]
    fn what_has_the_same_colours_is_alike() {
        let green = signature_of(&crop_of([60, 150, 110], 100)).unwrap();
        let also_green = signature_of(&crop_of([60, 150, 110], 80)).unwrap();
        let orange = signature_of(&crop_of([220, 150, 60], 100)).unwrap();
        assert!(similarity(&green, &also_green) > 0.99);
        assert!(similarity(&green, &orange) < 0.1);
    }

    #[test]
    fn the_pictures_of_floors_are_told_by_their_colours() {
        // A floor of green with a part of it not there (the ground), held up around
        // its middle, is alike a minimap of green and ground, and not of orange.
        let size = 40;
        let mut pixels = vec![egui::Color32::from_rgb(60, 150, 110); size * size];
        for pixel in pixels.iter_mut().take(size * size / 4) {
            *pixel = egui::Color32::TRANSPARENT;
        }
        let image = ColorImage { size: [size, size], pixels, source_size: egui::vec2(size as f32, size as f32) };
        let look = floor_look(&image, [-20., -20., 20., 20.], [0., 0.], 18.).unwrap();
        let mut minimap = crop_of([60, 150, 110], 60);
        // A quarter of the minimap is ground too.
        for y in 0..15 {
            for x in 0..60 {
                let at = (y * 60 + x) * 3;
                minimap.rgb[at..at + 3].copy_from_slice(&GROUND);
            }
        }
        let wanted = signature_of(&minimap).unwrap();
        assert!(similarity(&wanted, &look) > 0.85, "{}", similarity(&wanted, &look));
        let orange = signature_of(&crop_of([220, 150, 60], 60)).unwrap();
        assert!(similarity(&orange, &look) < 0.3);
    }
}
