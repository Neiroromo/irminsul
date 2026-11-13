//! Where on the map a player has been, as far as the game would have told them
//! of what is there.
//!
//! The game sends what is around a player, and only that: an object comes when
//! the player is near enough (for a chest, about a hundred metres), and not
//! otherwise.  So a place that was walked by has been told of: what was not
//! sent from there is not there (for that player: an opened chest is not sent).
//! This keeps the places, as a grid of cells over the map, with a cell marked
//! when the player came close enough to it for everything in it to be sent.
//!
//! The coordinates are the map's own, from the middle of the world, which do not
//! change when the map does.

/// How big a cell is, in map units (about a metre of the game).
pub const CELL: f32 = 32.;
/// Where the grid begins, and how big it is: the whole of the map.
pub const X0: f32 = -24_576.;
pub const Y0: f32 = -16_640.;
pub const COLUMNS: usize = 1168;
pub const ROWS: usize = 1032;
/// A cell counts as looked at when its middle is this near the walk.  A chest
/// is sent from about 110 m (measured), and a cell is up to 23 m from its
/// middle to its corner, so what is in a cell is within about 95 m of the walk.
pub const SEEN_RADIUS: f32 = 70.;
/// How far from the walk a cell counts as looked at, for the radar the user chose (metres):
/// the usual radar (90 m) gives `SEEN_RADIUS`, a wider one looks farther (what is in a cell
/// is then within about the radar of the walk), with more wrong guesses.
pub fn seen_radius(radar: f32) -> f32 {
    (radar - 20.).max(20.)
}

/// A walk is followed between two places this far apart, and no further: more
/// is not a walk but a jump (a teleport), which goes nowhere between.
pub const MAX_STEP: f32 = 400.;
/// How far apart the marks on a walk are.
const STEP: f32 = 24.;

const MAGIC: &[u8; 4] = b"ICOV";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    bits: Vec<u64>,
}

impl Default for Coverage {
    fn default() -> Self {
        Self::new()
    }
}

impl Coverage {
    pub fn new() -> Self {
        Self {
            bits: vec![0; (COLUMNS * ROWS).div_ceil(64)],
        }
    }

    /// The cell of a place of the map, if it is on the grid.
    fn cell(x: f32, y: f32) -> Option<(usize, usize)> {
        let (column, row) = ((x - X0) / CELL, (y - Y0) / CELL);
        if column < 0. || row < 0. || column >= COLUMNS as f32 || row >= ROWS as f32 {
            return None;
        }
        Some((column as usize, row as usize))
    }

    fn bit(column: usize, row: usize) -> (usize, u64) {
        let index = row * COLUMNS + column;
        (index / 64, 1u64 << (index % 64))
    }

    /// Whether a cell (by its column and row) is looked at.
    pub fn cell_covered(&self, column: usize, row: usize) -> bool {
        let (word, mask) = Self::bit(column, row);
        self.bits[word] & mask != 0
    }

    /// Whether the place has been looked at.
    pub fn is_covered(&self, x: f32, y: f32) -> bool {
        Self::cell(x, y).is_some_and(|(column, row)| self.cell_covered(column, row))
    }

    /// Marks what is within reach of a place.  Returns whether anything was new.
    pub fn mark_disc(&mut self, x: f32, y: f32) -> bool {
        self.mark_disc_within(x, y, SEEN_RADIUS)
    }

    /// Marks what is within `radius` of a place.  Returns whether anything was new.
    pub fn mark_disc_within(&mut self, x: f32, y: f32, radius: f32) -> bool {
        let reach = (radius / CELL).ceil() as i32 + 1;
        let Some((column, row)) = Self::cell(x, y) else {
            return false;
        };
        let mut changed = false;
        for dr in -reach..=reach {
            for dc in -reach..=reach {
                let (c, r) = (column as i32 + dc, row as i32 + dr);
                if c < 0 || r < 0 || c >= COLUMNS as i32 || r >= ROWS as i32 {
                    continue;
                }
                let middle = (
                    X0 + (c as f32 + 0.5) * CELL,
                    Y0 + (r as f32 + 0.5) * CELL,
                );
                if (middle.0 - x).hypot(middle.1 - y) > radius {
                    continue;
                }
                let (word, mask) = Self::bit(c as usize, r as usize);
                if self.bits[word] & mask == 0 {
                    self.bits[word] |= mask;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Marks what is within reach of a walk from one place to another.
    pub fn mark_walk(&mut self, from: (f32, f32), to: (f32, f32)) -> bool {
        self.mark_walk_within(from, to, SEEN_RADIUS)
    }

    /// Marks what is within `radius` of a walk from one place to another.
    pub fn mark_walk_within(&mut self, from: (f32, f32), to: (f32, f32), radius: f32) -> bool {
        let distance = (to.0 - from.0).hypot(to.1 - from.1);
        let steps = (distance / STEP).ceil().max(1.) as usize;
        let mut changed = false;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            changed |= self.mark_disc_within(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, radius);
        }
        changed
    }

    /// Adds what the other has.  Returns whether anything was new.
    pub fn merge(&mut self, other: &Coverage) -> bool {
        let mut changed = false;
        for (mine, theirs) in self.bits.iter_mut().zip(&other.bits) {
            if *mine | *theirs != *mine {
                *mine |= *theirs;
                changed = true;
            }
        }
        changed
    }

    /// How many cells are looked at.
    pub fn count(&self) -> usize {
        self.bits.iter().map(|word| word.count_ones() as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|word| *word == 0)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.bits.len() * 8);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(COLUMNS as u32).to_le_bytes());
        out.extend_from_slice(&(ROWS as u32).to_le_bytes());
        for word in &self.bits {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    /// The places from what `to_bytes` made, if it is that and of this grid.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let words = (COLUMNS * ROWS).div_ceil(64);
        if bytes.len() != 12 + words * 8 || &bytes[..4] != MAGIC {
            return None;
        }
        let columns = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let rows = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        if columns != COLUMNS || rows != ROWS {
            return None;
        }
        let bits = bytes[12..]
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])))
            .collect();
        Some(Self { bits })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wider_radar_looks_farther() {
        assert_eq!(seen_radius(90.), SEEN_RADIUS);
        let mut usual = Coverage::new();
        usual.mark_disc_within(500., 500., seen_radius(90.));
        let mut wide = Coverage::new();
        wide.mark_disc_within(500., 500., seen_radius(130.));
        assert!(!usual.is_covered(600., 500.));
        assert!(wide.is_covered(600., 500.));
        assert!(!wide.is_covered(650., 500.));
    }

    #[test]
    fn a_place_looked_at_is_marked_around_it() {
        let mut coverage = Coverage::new();
        assert!(!coverage.is_covered(500., 500.));
        assert!(coverage.mark_disc(500., 500.));
        assert!(coverage.is_covered(500., 500.));
        assert!(coverage.is_covered(540., 500.));
        // Far enough is not covered.
        assert!(!coverage.is_covered(700., 500.));
        // Again is nothing new.
        assert!(!coverage.mark_disc(500., 500.));
    }

    #[test]
    fn a_walk_is_marked_all_along() {
        let mut coverage = Coverage::new();
        assert!(coverage.mark_walk((0., 0.), (300., 0.)));
        for x in [0., 75., 150., 225., 300.] {
            assert!(coverage.is_covered(x, 0.), "x {x}");
            assert!(coverage.is_covered(x, 40.), "x {x}");
        }
        assert!(!coverage.is_covered(150., 200.));
    }

    #[test]
    fn nothing_off_the_grid_is_marked() {
        let mut coverage = Coverage::new();
        assert!(!coverage.mark_disc(-100_000., 0.));
        assert!(!coverage.is_covered(100_000., 100_000.));
        assert!(coverage.is_empty());
        // At the edge, only what is in is marked.
        assert!(coverage.mark_disc(X0 + 5., Y0 + 5.));
        assert!(coverage.count() > 0);
    }

    #[test]
    fn two_walks_are_put_together() {
        let mut a = Coverage::new();
        let mut b = Coverage::new();
        a.mark_disc(0., 0.);
        b.mark_disc(1000., 1000.);
        assert!(a.merge(&b));
        assert!(a.is_covered(0., 0.) && a.is_covered(1000., 1000.));
        assert!(!a.merge(&b));
    }

    #[test]
    fn the_places_survive_being_written() {
        let mut coverage = Coverage::new();
        coverage.mark_walk((100., 100.), (500., 300.));
        let bytes = coverage.to_bytes();
        assert_eq!(Coverage::from_bytes(&bytes), Some(coverage));
        assert!(Coverage::from_bytes(&bytes[1..]).is_none());
        assert!(Coverage::from_bytes(b"nope").is_none());
    }
}
