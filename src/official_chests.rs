//! The chests of the official interactive map of the game, and which of them
//! the ones found in the game are.
//!
//! The map lists every chest of the open world, by where it is, but has no
//! number in common with what the game says about a chest in its packets (the
//! group and the id in it).  The two are told by where they are: the game's
//! coordinates are put on the map's, and a chest found is the chest of the
//! same kind that is nearest, if that is near enough and clearly nearer than
//! the next.  Of 32 chests found in the game, 29 were told so, all of them
//! of the same kind as the chest of the map they were told to be; the other
//! three are not on the map where they are (they are not shown there).
//!
//! The list is made by `cargo run --release --example gen_official_chests`.

use std::sync::OnceLock;

/// A chest of the map: its number there, its kind (1 common, 2 exquisite,
/// 3 precious, 4 luxurious, 5 remarkable) and where it is, in the map's
/// coordinates from the middle of the world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Official {
    pub id: u32,
    pub tier: u8,
    pub x: f32,
    pub y: f32,
}

pub fn all() -> &'static [Official] {
    static ALL: OnceLock<Vec<Official>> = OnceLock::new();
    ALL.get_or_init(|| {
        let rows: Vec<(u32, u8, f32, f32)> =
            serde_json::from_str(include_str!("data/official_chests.json")).unwrap_or_default();
        rows.into_iter()
            .map(|(id, tier, x, y)| Official { id, tier, x, y })
            .collect()
    })
}

/// Where a place of the game is on the map, from the middle of the world: the
/// game's x and z go to the map's as `map_x = -z - 750` and `map_y = -x + 2321`.
/// Found by laying the chests seen in the game over the chests of the map: 29
/// of them fell within a few units of each other.
pub fn game_to_map(position: [f32; 3]) -> (f32, f32) {
    (-position[2] - 750., -position[0] + 2321.)
}

/// How far from a chest of the map a chest found can be, to be that one.
const NEAR: f32 = 14.;
/// Nearer than this, it is that one whatever else is near.
const SURE: f32 = 3.;
/// The next chest must be this many times as far as the one told to be it.
const CLEARLY: f32 = 1.5;

/// For each chest found (its kind and where it is), the chest of the map it
/// is, if it can be told.  No chest of the map is told to be two.
pub fn match_found(found: &[(u8, Option<[f32; 3]>)]) -> Vec<Option<usize>> {
    let official = all();
    // The candidates of each chest: the official one it is, and how far.
    let mut chosen: Vec<Option<(usize, f32)>> = Vec::with_capacity(found.len());
    for (tier, position) in found {
        let Some(position) = position else {
            chosen.push(None);
            continue;
        };
        let (x, y) = game_to_map(*position);
        let (mut first, mut second) = ((usize::MAX, f32::MAX), f32::MAX);
        for (index, chest) in official.iter().enumerate() {
            if chest.tier != *tier {
                continue;
            }
            let distance = (chest.x - x).hypot(chest.y - y);
            if distance < first.1 {
                second = first.1;
                first = (index, distance);
            } else if distance < second {
                second = distance;
            }
        }
        let near = first.1 <= NEAR;
        let clear = first.1 <= SURE || second >= first.1 * CLEARLY;
        chosen.push((near && clear).then_some(first));
    }
    // The nearest of the chests found that choose the same one has it.
    let mut taken: std::collections::HashMap<usize, (usize, f32)> = std::collections::HashMap::new();
    for (index, choice) in chosen.iter().enumerate() {
        if let Some((official, distance)) = choice {
            match taken.get(official) {
                Some((_, other)) if other <= distance => (),
                _ => {
                    taken.insert(*official, (index, *distance));
                }
            }
        }
    }
    chosen
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            choice.and_then(|(official, _)| {
                (taken.get(&official).map(|(winner, _)| *winner) == Some(index)).then_some(official)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A place of the game that is where the map has chest `index`, a little off.
    fn near(index: usize, dx: f32, dy: f32) -> [f32; 3] {
        let chest = all()[index];
        // The inverse of `game_to_map`.
        [-(chest.y + dy - 2321.), 200., -(chest.x + dx + 750.)]
    }

    #[test]
    fn the_chests_of_the_map_are_there() {
        let chests = all();
        assert!(chests.len() > 3000, "{} chests", chests.len());
        assert!(chests.iter().all(|chest| (1..=5).contains(&chest.tier)));
        // 1,448 common chests, more than any other kind.
        assert!(chests.iter().filter(|chest| chest.tier == 1).count() > 1000);
    }

    #[test]
    fn a_place_goes_to_the_map_and_back() {
        let (x, y) = game_to_map([2468.0, 206.3, -1234.3]);
        assert!((x - 484.3).abs() < 0.01 && (y - -147.0).abs() < 0.01);
        let back = near(10, 0., 0.);
        let (bx, by) = game_to_map(back);
        assert!((bx - all()[10].x).abs() < 0.01 && (by - all()[10].y).abs() < 0.01);
    }

    #[test]
    fn a_chest_found_is_the_chest_of_the_map_it_is_by() {
        let index = 20;
        let tier = all()[index].tier;
        let found = [(tier, Some(near(index, 3., -4.)))];
        assert_eq!(match_found(&found), vec![Some(index)]);
    }

    #[test]
    fn a_chest_of_another_kind_or_far_off_is_not_told() {
        let index = 20;
        let tier = all()[index].tier;
        let other = if tier == 1 { 2 } else { 1 };
        assert_eq!(match_found(&[(other, Some(near(index, 0., 0.)))]), vec![None]);
        assert_eq!(match_found(&[(tier, Some(near(index, 60., 60.)))]), vec![None]);
        assert_eq!(match_found(&[(tier, None)]), vec![None]);
    }

    #[test]
    fn two_chests_found_are_not_the_same_chest_of_the_map() {
        let index = 20;
        let tier = all()[index].tier;
        let found = [
            (tier, Some(near(index, 6., 0.))),
            (tier, Some(near(index, 1., 0.))),
        ];
        // The nearer one has it.
        assert_eq!(match_found(&found), vec![None, Some(index)]);
    }
}
