//! What changed in the player's belongings while playing: mora and items gained
//! or spent.  The monitor says what changed; the popup shows it.
//!
//! Changes that come close together are added up, so picking up three of the
//! same plant is one line, "+3 plant", and not three.

use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Mora,
    Primogems,
    /// A material, by its id in the game.
    Item(u32),
    /// A character, by its id in the game, and the level it has now: the number
    /// of a change of this kind is the level, which replaces the one before.
    Level(u32),
    /// A talent of a character (its id, and the id of the talent), and the
    /// level it has now, which replaces the one before.
    Talent(u32, u32),
    /// A weapon, by its id in the game, and the level it has now.
    Weapon(u32),
    /// A weapon, and the refinement it has now, from 1.
    Refinement(u32),
    /// A line of text, for what has no number.
    Note(String),
    /// Chests of this tier (1 to 4; 0 for the others) came into view, closed:
    /// the number of a change of this kind is how many.
    ChestSeen(u8),
    /// A chest of this gadget was opened.
    ChestOpened(u32),
    /// How many chests are opened, of how many known: a later one replaces the
    /// one before.  The number of a change of this kind is how many are known.
    ChestTotals { opened: u32, known: u32 },
}

/// Whether two kinds are one line of the feed.
fn same_line(a: &ChangeKind, b: &ChangeKind) -> bool {
    match (a, b) {
        (ChangeKind::ChestTotals { .. }, ChangeKind::ChestTotals { .. }) => true,
        _ => a == b,
    }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub kind: ChangeKind,
    /// How much more (or less, if negative) there is now.
    pub delta: i64,
}

/// Changes that arrived together.
#[derive(Clone, Debug)]
pub struct ChangeBatch {
    /// Grows with each batch, so what was seen already can be told.
    pub id: u64,
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub kind: ChangeKind,
    pub delta: i64,
    pub updated: Instant,
}

/// The changes being shown, added up by what they are about.
#[derive(Default)]
pub struct Feed {
    pub lines: Vec<Line>,
}

impl Feed {
    pub fn add(&mut self, change: &Change, now: Instant) {
        match self
            .lines
            .iter_mut()
            .find(|line| same_line(&line.kind, &change.kind))
        {
            Some(line) => {
                if matches!(line.kind, ChangeKind::ChestTotals { .. }) {
                    line.kind = change.kind.clone();
                    line.delta = change.delta;
                } else if matches!(
                    line.kind,
                    ChangeKind::Level(_)
                        | ChangeKind::Talent(..)
                        | ChangeKind::Weapon(_)
                        | ChangeKind::Refinement(_)
                ) {
                    line.delta = change.delta;
                } else {
                    line.delta += change.delta;
                }
                line.updated = now;
            }
            None => self.lines.push(Line {
                kind: change.kind.clone(),
                delta: change.delta,
                updated: now,
            }),
        }
        // What was gained and spent again is no change.
        self.lines
            .retain(|line| line.delta != 0 || matches!(line.kind, ChangeKind::Note(_)));
    }

    /// Takes away what has been shown for `hold` without a change.
    pub fn expire(&mut self, now: Instant, hold: Duration) {
        self.lines.retain(|line| now - line.updated < hold);
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, delta: i64) -> Change {
        Change {
            kind: ChangeKind::Item(id),
            delta,
        }
    }

    #[test]
    fn changes_of_the_same_thing_are_added_up() {
        let now = Instant::now();
        let mut feed = Feed::default();
        feed.add(&item(1, 1), now);
        feed.add(&item(2, 4), now);
        feed.add(&item(1, 2), now);
        assert_eq!(feed.lines.len(), 2);
        assert_eq!(feed.lines[0].delta, 3);
        assert_eq!(feed.lines[1].delta, 4);
    }

    #[test]
    fn a_level_replaces_the_one_before() {
        let now = Instant::now();
        let mut feed = Feed::default();
        let level = |level| Change {
            kind: ChangeKind::Level(10000002),
            delta: level,
        };
        feed.add(&level(80), now);
        feed.add(&level(81), now);
        assert_eq!(feed.lines.len(), 1);
        assert_eq!(feed.lines[0].delta, 81);
    }

    #[test]
    fn the_totals_of_chests_replace_the_ones_before() {
        let now = Instant::now();
        let mut feed = Feed::default();
        let totals = |opened, known| Change {
            kind: ChangeKind::ChestTotals { opened, known },
            delta: known as i64,
        };
        feed.add(&totals(3, 40), now);
        feed.add(&totals(4, 41), now);
        assert_eq!(feed.lines.len(), 1);
        assert_eq!(
            feed.lines[0].kind,
            ChangeKind::ChestTotals {
                opened: 4,
                known: 41
            }
        );
    }

    #[test]
    fn chests_seen_add_up() {
        let now = Instant::now();
        let mut feed = Feed::default();
        let seen = |count| Change {
            kind: ChangeKind::ChestSeen(1),
            delta: count,
        };
        feed.add(&seen(2), now);
        feed.add(&seen(3), now);
        assert_eq!(feed.lines.len(), 1);
        assert_eq!(feed.lines[0].delta, 5);
    }

    #[test]
    fn what_was_gained_and_spent_is_no_change() {
        let now = Instant::now();
        let mut feed = Feed::default();
        feed.add(&item(1, 3), now);
        feed.add(&item(1, -3), now);
        assert!(feed.is_empty());
    }

    #[test]
    fn lines_go_when_they_have_been_shown_long_enough() {
        let start = Instant::now();
        let mut feed = Feed::default();
        feed.add(&item(1, 1), start);
        feed.add(&item(2, 1), start + Duration::from_secs(4));
        feed.expire(start + Duration::from_secs(6), Duration::from_secs(5));
        assert_eq!(feed.lines.len(), 1);
        assert_eq!(feed.lines[0].kind, ChangeKind::Item(2));
    }
}
