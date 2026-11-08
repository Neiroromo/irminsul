//! What the game says of the quests, put on the markers of the world quests of the map.
//!
//! The whole quest list is told when logging in (`StoryEntry`), and a story's steps when
//! it changes.  A story is done when all the steps it has are done; it is in the list and
//! not done when it is offered (taken into the journal) or begun.  The markers of the
//! quests (the kind 52 of the map) that are known to be of a story (`quest_markers.json`,
//! made from the names) are given to the maps as done or not, the way the chests are.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use auto_artifactarium::{StoryEntry, StoryStatus};

use crate::chest_watch::ChestMarks;
use crate::profiles;

/// Which marker of the map is of which story: the markers are named by the map, and the
/// stories by the game (and by Project Amber, which has the names of the newer ones), and
/// where the names are alike they are the same.  A name may be of more than one story: then
/// it is the one of them that the player has in the list.
static MARKERS_JSON: &str = include_str!("data/quest_markers.json");

/// Each marker, with the stories it may be of.
fn markers() -> &'static HashMap<u32, Vec<u32>> {
    static MARKERS: OnceLock<HashMap<u32, Vec<u32>>> = OnceLock::new();
    MARKERS.get_or_init(|| {
        serde_json::from_str::<HashMap<String, Vec<u32>>>(MARKERS_JSON)
            .map(|map| map.into_iter().filter_map(|(marker, stories)| Some((marker.parse().ok()?, stories))).collect())
            .unwrap_or_default()
    })
}

/// 0 offered or begun (not done), 1 done.
const NOT_DONE: u8 = 0;
const DONE: u8 = 1;

fn path(uid: u32) -> Option<PathBuf> {
    Some(profiles::profiles_dir().ok()?.join("quest_status").join(format!("{uid}.json")))
}

pub struct QuestWatch {
    uid: Option<u32>,
    /// Story: done or not.
    stories: HashMap<u32, u8>,
    /// Marker: since when it is done (seconds).
    done_at: HashMap<u32, u64>,
    version: u64,
    dirty: bool,
    saved_at: Instant,
}

impl QuestWatch {
    pub fn new() -> Self {
        Self { uid: None, stories: HashMap::new(), done_at: HashMap::new(), version: 1, dirty: false, saved_at: Instant::now() }
    }

    pub fn use_account(&mut self, uid: Option<u32>) {
        if self.uid == uid {
            return;
        }
        self.save(true);
        self.uid = uid;
        self.stories.clear();
        if let Some(path) = uid.and_then(path)
            && let Ok(text) = std::fs::read_to_string(path)
            && let Ok(book) = serde_json::from_str::<HashMap<u32, u8>>(&text)
        {
            self.stories = book;
        }
        self.version += 1;
    }

    fn save(&mut self, force: bool) {
        if !self.dirty || (!force && self.saved_at.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.dirty = false;
        self.saved_at = Instant::now();
        let Some(path) = self.uid.and_then(path) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(&self.stories) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn set(&mut self, story: u32, state: u8) {
        if self.stories.get(&story) != Some(&state) {
            self.stories.insert(story, state);
            if state == DONE {
                let now = crate::markers::unix_now();
                for (marker, of) in markers() {
                    if of.contains(&story) {
                        self.done_at.entry(*marker).or_insert(now);
                    }
                }
            }
            self.version += 1;
            self.dirty = true;
        }
    }

    /// The whole list, as told when logging in: what is not in it is not offered to the
    /// player (and is left as it was).
    pub fn apply_list(&mut self, list: &[StoryEntry]) {
        for entry in list {
            self.set(entry.story, if entry.status == StoryStatus::Done { DONE } else { NOT_DONE });
        }
        self.save(false);
    }

    /// A story changed: the steps it has now, and their states (3 is done).
    pub fn apply_story(&mut self, story: u32, steps: &[(u32, u32)]) {
        if steps.is_empty() {
            self.set(story, NOT_DONE);
        } else if steps.iter().all(|(_, state)| *state == 3) {
            self.set(story, DONE);
        } else {
            self.set(story, NOT_DONE);
        }
        self.save(false);
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// What the maps are given: the markers of done stories are done, those of stories that
    /// are offered or begun are not.
    pub fn snapshot(&self) -> ChestMarks {
        let mut marks = ChestMarks { version: self.version, uid: self.uid, ..Default::default() };
        for (marker, candidates) in markers() {
            // Of the stories the marker may be of, those the player has.
            let states: Vec<u8> = candidates.iter().filter_map(|story| self.stories.get(story).copied()).collect();
            match states.as_slice() {
                [] => (),
                states if states.iter().all(|state| *state == DONE) => {
                    marks.opened.push((*marker, self.done_at.get(marker).copied().unwrap_or_else(crate::markers::unix_now)));
                }
                _ => marks.closed.push(*marker),
            }
        }
        marks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(story: u32, status: StoryStatus) -> StoryEntry {
        StoryEntry { story, since: 0, status, steps: 1, open_steps: Vec::new() }
    }

    #[test]
    fn the_markers_of_done_stories_are_done_and_of_offered_ones_are_not() {
        let mut watch = QuestWatch::new();
        // 1701 is the marker of the story 20030, 1688 of 469.
        watch.apply_list(&[entry(20030, StoryStatus::Done), entry(469, StoryStatus::Available)]);
        let marks = watch.snapshot();
        assert!(marks.opened.iter().any(|(marker, _)| *marker == 1701));
        assert!(marks.closed.contains(&1688));
        // 469 is done: its steps are all done.
        watch.apply_story(469, &[(46901, 3), (46902, 3), (46904, 3)]);
        let marks = watch.snapshot();
        assert!(marks.opened.iter().any(|(marker, _)| *marker == 1688));
        assert!(!marks.closed.contains(&1688));
    }

    #[test]
    fn what_is_not_in_the_list_is_left_alone() {
        let mut watch = QuestWatch::new();
        watch.apply_list(&[entry(20030, StoryStatus::Done)]);
        let marks = watch.snapshot();
        assert!(!marks.closed.contains(&1688) && !marks.opened.iter().any(|(marker, _)| *marker == 1688));
    }

    #[test]
    fn the_table_is_read() {
        assert_eq!(markers().get(&1688), Some(&vec![469]));
        // "Аромат на ветру" is the story 470.
        assert_eq!(markers().get(&1682), Some(&vec![470]));
        assert!(markers().len() > 50);
    }
}
