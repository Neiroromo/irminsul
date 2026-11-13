//! What is drawn over the big map of the game besides the markers: the sign that the place of
//! the map is being looked for, and the panel of the settings of the markers (shown while
//! the key of the map over the game is held).  Both are pictures made in memory: blue, green,
//! red and the share that is seen (the three taken by it), row by row from the top.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

const NAVY: [u8; 3] = [0x16, 0x1e, 0x30];
const AMBER: [u8; 3] = [0xff, 0xc3, 0x4a];
const WHITE: [u8; 3] = [0xf2, 0xf2, 0xf2];
const DIM: [u8; 3] = [0xa8, 0xb0, 0xc0];

fn font() -> Option<&'static FontRef<'static>> {
    static FONT: OnceLock<Option<FontRef<'static>>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).ok()).as_ref()
}

struct Picture {
    width: usize,
    height: usize,
    px: Vec<u8>,
}

impl Picture {
    fn new(width: usize, height: usize) -> Self {
        Self { width, height, px: vec![0; width * height * 4] }
    }

    /// Lays a colour on a place by a share.
    fn blend(&mut self, x: i32, y: i32, colour: [u8; 3], share: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let at = (y as usize * self.width + x as usize) * 4;
        let share = share.clamp(0., 1.);
        for channel in 0..3 {
            self.px[at + channel] = (self.px[at + channel] as f32 * (1. - share) + colour[2 - channel] as f32 * share) as u8;
        }
        self.px[at + 3] = (self.px[at + 3] as f32 * (1. - share) + 255. * share) as u8;
    }

    fn rect(&mut self, [x, y, w, h]: [i32; 4], colour: [u8; 3], share: f32) {
        for py in y..y + h {
            for px in x..x + w {
                self.blend(px, py, colour, share);
            }
        }
    }

    /// A picture, made to fit a place.
    fn image(&mut self, [x, y, w, h]: [i32; 4], image: &egui::ColorImage) {
        let (image_w, image_h) = (image.size[0] as f32, image.size[1] as f32);
        for py in 0..h {
            for px in 0..w {
                let [r, g, b, a] = crate::map_render::sample_rgba(image, (px as f32 + 0.5) / w as f32 * image_w, (py as f32 + 0.5) / h as f32 * image_h);
                self.blend(x + px, y + py, [r as u8, g as u8, b as u8], a / 255.);
            }
        }
    }

    /// An arrow in a square place, turned by so many degrees to the right from pointing up.
    fn pointer(&mut self, [x, y, size, _]: [i32; 4], degrees: f32, colour: [u8; 3]) {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let half = size as f32 / 2.;
        // In the arrow's own place (pointing up, the middle at 0, 0, from -1 to 1): a head
        // and a shaft.
        let inside = |px: f32, py: f32| {
            let (dx, dy) = ((px - half) / half, (py - half) / half);
            // Turned back, so that the arrow points up.
            let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            let head = (-0.85..=0.05).contains(&v) && u.abs() <= (v + 0.85) * 0.75;
            let shaft = (0.05..=0.8).contains(&v) && u.abs() <= 0.2;
            head || shaft
        };
        for py in 0..size {
            for px in 0..size {
                // Four places in each, for a smooth edge.
                let mut share = 0.;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    if inside(px as f32 + ox, py as f32 + oy) {
                        share += 0.25;
                    }
                }
                if share > 0. {
                    self.blend(x + px, y + py, colour, share);
                }
            }
        }
    }

    /// A small arrow that points up or down, its top left at a place.
    fn arrow(&mut self, x: i32, y: i32, up: bool, colour: [u8; 3]) {
        for row in 0..6i32 {
            let half = if up { row } else { 5 - row };
            self.rect([x + 5 - half, y + row, half * 2 + 1, 1], colour, 1.);
        }
    }

    fn frame(&mut self, [x, y, w, h]: [i32; 4], colour: [u8; 3]) {
        self.rect([x, y, w, 2], colour, 1.);
        self.rect([x, y + h - 2, w, 2], colour, 1.);
        self.rect([x, y, 2, h], colour, 1.);
        self.rect([x + w - 2, y, 2, h], colour, 1.);
    }

    /// How wide a line of text is.
    fn text_width(text: &str, size: f32) -> f32 {
        let Some(font) = font() else {
            return 0.;
        };
        let scaled = font.as_scaled(PxScale::from(size));
        text.chars().map(|letter| scaled.h_advance(font.glyph_id(letter))).sum()
    }

    /// Writes a line of text, its top left at a place.
    fn text(&mut self, x: f32, y: f32, text: &str, size: f32, colour: [u8; 3]) {
        let Some(font) = font() else {
            return;
        };
        let scaled = font.as_scaled(PxScale::from(size));
        let mut pen = x;
        for letter in text.chars() {
            let id = font.glyph_id(letter);
            let glyph = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(pen, y + scaled.ascent()));
            pen += scaled.h_advance(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, cover| {
                    self.blend(bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, colour, cover);
                });
            }
        }
    }

    fn text_centred(&mut self, rect: [i32; 4], text: &str, size: f32, colour: [u8; 3]) {
        let width = Self::text_width(text, size);
        self.text(rect[0] as f32 + (rect[2] as f32 - width) / 2., rect[1] as f32 + (rect[3] as f32 - size) / 2. - 1., text, size, colour);
    }
}

/// The sign that the place of the map is being looked for: the picture and its size.
/// `phase` goes on with the time, for the dots.
pub fn looking(phase: u32, russian: bool) -> (Vec<u8>, usize, usize) {
    let text = if russian { "Совмещаю отметки с картой" } else { "Placing the markers on the map" };
    let size = 17.;
    let width = Picture::text_width(text, size) as usize + 64;
    let mut picture = Picture::new(width, 34);
    picture.rect([0, 0, width as i32, 34], NAVY, 0.86);
    picture.frame([0, 0, width as i32, 34], AMBER);
    picture.text(14., 7., text, size, WHITE);
    for dot in 0..3 {
        let share = if dot <= phase % 3 { 1. } else { 0.25 };
        picture.rect([width as i32 - 44 + dot as i32 * 10, 21, 4, 4], AMBER, share);
    }
    (picture.px, width, 34)
}

/// What is told when the map over the game is asked for and the traffic of the game can not
/// be read: the picture and its size.
pub fn no_key(russian: bool) -> (Vec<u8>, usize, usize) {
    let (first, second) = if russian {
        ("Ключ не расшифрован: данные игры не читаются", "Перезапустите Genshin Impact")
    } else {
        ("The key was not found: the game's data can not be read", "Start Genshin Impact again")
    };
    let width = Picture::text_width(first, 18.).max(Picture::text_width(second, 18.)) as usize + 36;
    let height = 70;
    let mut picture = Picture::new(width, height);
    let red = [0xff, 0x6b, 0x6b];
    picture.rect([0, 0, width as i32, height as i32], NAVY, 0.94);
    picture.frame([0, 0, width as i32, height as i32], red);
    picture.text_centred([0, 8, width as i32, 26], first, 18., WHITE);
    picture.text_centred([0, 36, width as i32, 26], second, 18., red);
    (picture.px, width, height)
}

/// A line of the list of the monsters near: the picture of its kind, its name, how many of
/// them, how far the nearest is (metres), and how much higher (or, negative, lower) it is.
#[derive(Clone, PartialEq)]
pub struct NearRow {
    pub icon: Option<std::sync::Arc<egui::ColorImage>>,
    pub name: String,
    pub count: usize,
    pub metres: i32,
    pub up: i32,
    /// Which way to go to it, from the way the player faces: degrees to the right (0 is
    /// straight on, 90 to the right, 180 behind), if the way the player faces is known.
    pub turn: Option<i32>,
}

/// The list of the monsters near: the picture and its size.  `moving`: it can be moved now,
/// which is shown by its frame (and said, if there is nothing in it).
pub fn nearby(rows: &[NearRow], russian: bool, moving: bool) -> (Vec<u8>, usize, usize) {
    const ROW: i32 = 38;
    let width = 300usize;
    let height = (rows.len().max(1) as i32 * ROW + 12) as usize;
    let mut picture = Picture::new(width, height);
    picture.rect([0, 0, width as i32, height as i32], NAVY, if moving { 0.95 } else { 0.88 });
    picture.frame([0, 0, width as i32, height as i32], if moving { AMBER } else { [0x4a, 0x58, 0x7a] });
    if rows.is_empty() {
        let text = if russian { "Мобы рядом: перетащите мышью" } else { "Enemies near: drag with the mouse" };
        picture.text_centred([0, 6, width as i32, ROW], text, 16., WHITE);
    }
    let unit = if russian { "м" } else { "m" };
    for (index, row) in rows.iter().enumerate() {
        let top = 6 + index as i32 * ROW;
        if let Some(icon) = &row.icon {
            picture.image([10, top + 3, 32, 32], icon);
        } else {
            picture.rect([18, top + 11, 16, 16], [0xff, 0x6b, 0x6b], 1.);
        }
        // How far, at the right; over it or under it, by an arrow and how much.
        let far = format!("{} {unit}", row.metres);
        let far_width = Picture::text_width(&far, 18.);
        picture.text(width as f32 - 12. - far_width, top as f32 + 2., &far, 18., WHITE);
        if row.up.abs() >= 3 {
            let tall = format!("{}", row.up.abs());
            let tall_width = Picture::text_width(&tall, 13.);
            let x = width as f32 - 12. - tall_width;
            picture.text(x, top as f32 + 21., &tall, 13., AMBER);
            picture.arrow(x as i32 - 12, top + 24, row.up > 0, AMBER);
        }
        // Which way to go, before the numbers.
        let numbers = far_width.max(46.);
        if let Some(turn) = row.turn {
            let left = (width as f32 - 12. - numbers - 30.) as i32;
            picture.pointer([left, top + 7, 24, 24], turn as f32, [0x5a, 0xd0, 0xff]);
        }
        let name = if row.count > 1 { format!("{} ×{}", row.name, row.count) } else { row.name.clone() };
        // As much of the name as fits before the arrow and the numbers.
        let room = width as f32 - 50. - numbers - 22. - if row.turn.is_some() { 30. } else { 0. };
        let mut shown = name.clone();
        while Picture::text_width(&shown, 16.) > room && shown.chars().count() > 4 {
            shown = shown.chars().take(shown.chars().count() - 2).collect::<String>() + "…";
        }
        picture.text(50., top as f32 + 9., &shown, 16., WHITE);
    }
    (picture.px, width, height)
}

/// What the panel shows.
#[derive(Clone, PartialEq)]
pub struct PanelState {
    pub on: bool,
    /// The list of the monsters near is on.
    pub mobs: bool,
    /// The size of the markers, 1 is the usual.
    pub size: f32,
    pub status: String,
    pub russian: bool,
}

pub const PANEL_WIDTH: usize = 320;
pub const PANEL_HEIGHT: usize = 232;
/// What the buttons of the panel are, in the order of `panel_buttons`.
pub const BUTTON_TOGGLE: usize = 0;
pub const BUTTON_SMALLER: usize = 1;
pub const BUTTON_BIGGER: usize = 2;
pub const BUTTON_MOBS: usize = 3;

/// Where the buttons are in the panel: x, y, width, height.
pub fn panel_buttons() -> Vec<[i32; 4]> {
    vec![
        [16, 50, PANEL_WIDTH as i32 - 32, 36],
        [PANEL_WIDTH as i32 - 16 - 36 - 8 - 36, 102, 36, 32],
        [PANEL_WIDTH as i32 - 16 - 36, 102, 36, 32],
        [16, 148, PANEL_WIDTH as i32 - 32, 36],
    ]
}

/// The panel of the settings of the markers over the game's map.
pub fn panel(state: &PanelState) -> Vec<u8> {
    let tr = |en: &'static str, ru: &'static str| if state.russian { ru } else { en };
    let mut picture = Picture::new(PANEL_WIDTH, PANEL_HEIGHT);
    let all = [0, 0, PANEL_WIDTH as i32, PANEL_HEIGHT as i32];
    picture.rect(all, NAVY, 0.94);
    picture.frame(all, AMBER);
    picture.text(16., 14., tr("Markers on the game's map", "Отметки на карте игры"), 19., WHITE);
    let buttons = panel_buttons();
    // On or off.
    let toggle = buttons[BUTTON_TOGGLE];
    if state.on {
        picture.rect(toggle, AMBER, 1.);
        picture.text_centred(toggle, tr("On (click to turn off)", "Включены (нажмите, чтобы выключить)"), 16., [0, 0, 0]);
    } else {
        picture.rect(toggle, [0x2a, 0x34, 0x4c], 1.);
        picture.frame(toggle, DIM);
        picture.text_centred(toggle, tr("Off (click to turn on)", "Выключены (нажмите, чтобы включить)"), 16., WHITE);
    }
    // The size.
    picture.text(16., 109., &format!("{}: {:.0}%", tr("Size of the markers", "Размер отметок"), state.size * 100.), 16., WHITE);
    for (button, sign) in [(buttons[BUTTON_SMALLER], "−"), (buttons[BUTTON_BIGGER], "+")] {
        picture.rect(button, [0x2a, 0x34, 0x4c], 1.);
        picture.frame(button, AMBER);
        picture.text_centred(button, sign, 22., WHITE);
    }
    // The list of the monsters near.
    let mobs = buttons[BUTTON_MOBS];
    if state.mobs {
        picture.rect(mobs, AMBER, 1.);
        picture.text_centred(mobs, tr("Enemies near: shown", "Мобы рядом: показываются"), 16., [0, 0, 0]);
    } else {
        picture.rect(mobs, [0x2a, 0x34, 0x4c], 1.);
        picture.frame(mobs, DIM);
        picture.text_centred(mobs, tr("Enemies near: hidden", "Мобы рядом: скрыты"), 16., WHITE);
    }
    picture.text(16., 196., &state.status, 14., DIM);
    picture.px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_is_drawn_with_its_writing() {
        let state = PanelState { on: true, mobs: true, size: 1., status: "слежу за картой".to_owned(), russian: true };
        let picture = panel(&state);
        assert_eq!(picture.len(), PANEL_WIDTH * PANEL_HEIGHT * 4);
        // The frame is there, and so is writing (something light inside, not only the ground).
        assert_eq!(picture[3], 255);
        let light = picture.chunks_exact(4).filter(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200).count();
        assert!(light > 100, "{light}");
        // Off looks different.
        assert!(panel(&PanelState { on: false, ..state }) != picture);
        for [x, y, w, h] in panel_buttons() {
            assert!(x >= 0 && y >= 0 && (x + w) as usize <= PANEL_WIDTH && (y + h) as usize <= PANEL_HEIGHT);
        }
    }

    /// Writes the panel and the sign as pictures to look at (`IRMINSUL_PANEL_OUT`, a folder).
    #[test]
    #[ignore]
    fn the_pictures_are_written_to_look_at() {
        let dir = std::path::PathBuf::from(std::env::var("IRMINSUL_PANEL_OUT").expect("a folder"));
        let save = |name: &str, picture: &[u8], width: usize, height: usize| {
            // Over a mid green, as over the map.
            let mut out = image::RgbImage::new(width as u32, height as u32);
            for (at, pixel) in out.pixels_mut().enumerate() {
                let over = &picture[at * 4..at * 4 + 4];
                let seen = over[3] as f32 / 255.;
                for channel in 0..3 {
                    pixel.0[channel] = ([90u8, 140, 70][channel] as f32 * (1. - seen) + over[2 - channel] as f32) as u8;
                }
            }
            out.save(dir.join(name)).expect("written");
        };
        let state = PanelState { on: true, mobs: false, size: 1.2, status: "Сейчас: слежу за картой (77%)".to_owned(), russian: true };
        save("panel_on.png", &panel(&state), PANEL_WIDTH, PANEL_HEIGHT);
        save("panel_off.png", &panel(&PanelState { on: false, ..state }), PANEL_WIDTH, PANEL_HEIGHT);
        let (picture, width, height) = looking(1, true);
        save("looking.png", &picture, width, height);
        let rows = [
            NearRow { icon: None, name: "Химера-бронезверь".to_owned(), count: 3, metres: 42, up: 12, turn: Some(0) },
            NearRow { icon: None, name: "Волкодлак-стрелок с очень длинным именем".to_owned(), count: 1, metres: 118, up: -37, turn: Some(135) },
            NearRow { icon: None, name: "Хиличурл".to_owned(), count: 1, metres: 7, up: 0, turn: Some(-90) },
            NearRow { icon: None, name: "Слайм".to_owned(), count: 1, metres: 64, up: 0, turn: None },
        ];
        let (picture, width, height) = nearby(&rows, true, false);
        save("nearby.png", &picture, width, height);
        let (picture, width, height) = nearby(&[], true, true);
        save("nearby_moving.png", &picture, width, height);
    }

    #[test]
    fn the_sign_of_looking_has_a_size_and_is_seen() {
        let (picture, width, height) = looking(1, true);
        assert_eq!(picture.len(), width * height * 4);
        assert!(width > 150 && picture[3] == 255);
    }
}
