//! The maps of the game as the official interactive map has them: the world
//! (Teyvat), Enkanomiya, the Chasm underground, the Sea of Bygone Eras and the
//! rest; for each, its regions, and its underground places, which have floors.
//!
//! All places are in pixels of the full size map counted from the map's own
//! origin, the way the markers are.

use serde::Deserialize;

use crate::map_tiles::MapInfo;

/// The world: the map the chests, the walked places and the player are on.
pub const WORLD: u32 = 2;

#[derive(Clone, Debug, Deserialize)]
pub struct Anchor {
    pub id: u32,
    pub parent: u32,
    /// The region of the world it is in (1 is Mondstadt, and so on).
    pub area: u32,
    pub en: String,
    pub ru: String,
    /// Left, top, right, bottom.
    pub rect: [f32; 4],
}

impl Anchor {
    pub fn name(&self, russian: bool) -> &str {
        if russian && !self.ru.is_empty() { &self.ru } else { &self.en }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Floor {
    pub id: u32,
    pub en: String,
    pub ru: String,
    pub short: String,
    /// The picture of the floor, laid on the map.
    pub url: String,
    pub rect: [f32; 4],
}

impl Floor {
    pub fn name(&self, russian: bool) -> &str {
        if russian && !self.ru.is_empty() { &self.ru } else { &self.en }
    }

    /// What the floor is called on its button: `B1`, `L2`, or its number.
    pub fn tag(&self, index: usize) -> String {
        if !self.short.is_empty() {
            return self.short.clone();
        }
        let last = self.en.rsplit(['_', ' ']).next().unwrap_or("");
        let mut chars = last.chars();
        let looks_like_one = matches!(chars.next(), Some('B' | 'L' | 'F'))
            && chars.clone().count() > 0
            && chars.all(|c| c.is_ascii_digit());
        if looks_like_one { last.to_owned() } else { (index + 1).to_string() }
    }
}

/// An underground place: a cave, a ruin, a dungeon, with one floor or more.
#[derive(Clone, Debug, Deserialize)]
pub struct Site {
    pub id: u32,
    /// The floor that is shown first.
    pub default: usize,
    /// Where the way down is.
    pub entrance: [f32; 2],
    pub floors: Vec<Floor>,
    /// The region of the world it is in, told when the atlas is read.
    #[serde(skip)]
    pub area: u32,
}

impl Site {
    /// Its name is that of its first floor, without the floor.
    pub fn name(&self, russian: bool) -> String {
        let floor = &self.floors[self.default.min(self.floors.len() - 1)];
        let name = floor.name(russian);
        let cut = name.rsplit_once(['_', ' ']).filter(|(_, last)| {
            let mut chars = last.chars();
            matches!(chars.next(), Some('B' | 'L' | 'F')) && chars.clone().count() > 0 && chars.all(|c| c.is_ascii_digit())
        });
        cut.map_or(name, |(head, _)| head).trim_end_matches(['_', ' ', ':']).to_owned()
    }

    pub fn floor(&self, id: u32) -> Option<&Floor> {
        self.floors.iter().find(|floor| floor.id == id)
    }

    /// Where all its floors are (left, top, right, bottom), if they have a place.
    pub fn extent(&self) -> Option<[f32; 4]> {
        union(self.floors.iter().filter(|floor| floor.rect[2] > floor.rect[0]).map(|floor| floor.rect))
    }

    pub fn first_floor(&self) -> &Floor {
        &self.floors[self.default.min(self.floors.len() - 1)]
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct MapDef {
    pub id: u32,
    pub en: String,
    pub ru: String,
    pub version: String,
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub max_level: u32,
    pub anchors: Vec<Anchor>,
    pub sites: Vec<Site>,
}

impl MapDef {
    pub fn name(&self, russian: bool) -> &str {
        if self.id == WORLD {
            // The map calls it by what it is, an interactive map.
            return if russian { "Тейват" } else { "Teyvat" };
        }
        if russian && !self.ru.is_empty() { &self.ru } else { &self.en }
    }

    /// What is needed to draw its tiles.
    pub fn info(&self) -> MapInfo {
        MapInfo {
            map: self.id,
            version: self.version.clone(),
            origin: self.origin,
            size: self.size,
            max_level: self.max_level,
        }
    }

    /// The regions that are not in another one.
    pub fn top_anchors(&self) -> impl Iterator<Item = &Anchor> {
        self.anchors.iter().filter(|anchor| anchor.parent == 0)
    }

    /// Where all the regions are, or the whole map if it has none.
    pub fn bounds(&self) -> [f32; 4] {
        union(self.top_anchors().map(|anchor| anchor.rect))
            .unwrap_or([-self.origin[0], -self.origin[1], self.size[0] - self.origin[0], self.size[1] - self.origin[1]])
    }

    pub fn site(&self, id: u32) -> Option<&Site> {
        self.sites.iter().find(|site| site.id == id)
    }

    /// The site a floor is of.
    pub fn site_of_floor(&self, floor: u32) -> Option<&Site> {
        self.sites.iter().find(|site| site.floor(floor).is_some())
    }
}

/// The smallest rectangle that has all of them.
pub fn union(rects: impl Iterator<Item = [f32; 4]>) -> Option<[f32; 4]> {
    rects.reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// The name of a region of the world, by its number.
pub fn region_name(area: u32, russian: bool) -> Option<&'static str> {
    let (en, ru) = match area {
        1 => ("Mondstadt", "Мондштадт"),
        2 => ("Liyue", "Ли Юэ"),
        3 => ("Inazuma", "Инадзума"),
        4 => ("Sumeru", "Сумеру"),
        8 => ("Fontaine", "Фонтейн"),
        11 => ("Natlan", "Натлан"),
        13 => ("Nod-Krai", "Нод-Край"),
        16 => ("Snezhnaya", "Снежная"),
        _ => return None,
    };
    Some(if russian { ru } else { en })
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Atlas {
    pub maps: Vec<MapDef>,
}

impl Atlas {
    pub fn parse(text: &str) -> Option<Self> {
        let mut atlas: Atlas = serde_json::from_str(text).ok()?;
        for map in &mut atlas.maps {
            let anchors: Vec<Anchor> = map.anchors.iter().filter(|anchor| anchor.parent == 0).cloned().collect();
            for site in &mut map.sites {
                if site.floors.is_empty() {
                    continue;
                }
                // The region it is in: by where the way down is, or where it is.
                let first = site.first_floor().rect;
                let middle = [(first[0] + first[2]) / 2., (first[1] + first[3]) / 2.];
                let inside = |at: [f32; 2]| {
                    anchors
                        .iter()
                        .filter(|anchor| {
                            at[0] >= anchor.rect[0] && at[0] <= anchor.rect[2] && at[1] >= anchor.rect[1] && at[1] <= anchor.rect[3]
                        })
                        .map(|anchor| anchor.area)
                        .next()
                };
                site.area = inside(site.entrance).or_else(|| inside(middle)).unwrap_or(0);
            }
            map.sites.retain(|site| !site.floors.is_empty());
        }
        (!atlas.maps.is_empty()).then_some(atlas)
    }

    pub fn map(&self, id: u32) -> Option<&MapDef> {
        self.maps.iter().find(|map| map.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built_in() -> Atlas {
        Atlas::parse(include_str!("data/map_atlas.json")).expect("the atlas the program has can be read")
    }

    #[test]
    fn the_maps_the_program_has_are_there() {
        let atlas = built_in();
        let world = atlas.map(WORLD).unwrap();
        assert!(world.en.contains("Teyvat"));
        assert_eq!(world.info().size, [36864., 32768.]);
        assert!(atlas.map(7).is_some_and(|map| map.en == "Enkanomiya"));
        assert!(atlas.maps.len() >= 5);
        assert!(world.top_anchors().count() > 40);
        assert!(world.sites.len() > 100);
        // Every site has a region of its own to be listed under, or none.
        assert!(world.sites.iter().filter(|site| site.area != 0).count() > 100);
    }

    #[test]
    fn floors_are_named_as_the_map_names_them() {
        let floor = |en: &str, short: &str| Floor {
            id: 1,
            en: en.into(),
            ru: String::new(),
            short: short.into(),
            url: String::new(),
            rect: [0.; 4],
        };
        assert_eq!(floor("Abandoned Station_B1", "").tag(0), "B1");
        assert_eq!(floor("Abandoned Station_B2", "B2").tag(1), "B2");
        assert_eq!(floor("Somewhere", "").tag(2), "3");
        let site = Site {
            id: 1,
            default: 0,
            entrance: [0.; 2],
            floors: vec![floor("Hollow of the Ancient Scaled One_L2", "L1")],
            area: 0,
        };
        assert_eq!(site.name(false), "Hollow of the Ancient Scaled One");
    }
}
