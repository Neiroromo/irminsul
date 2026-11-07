//! What was last captured from each game account, one file per account.
//!
//! The file of an account is replaced whenever data is captured from it, so
//! there is always exactly one, holding the freshest of everything.  Captured
//! lists are kept as the protobuf messages they came in, so the exports can be
//! made from a stored account as well as from a live capture.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::prelude::*;
use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::APP_ID;

/// Where the profiles are kept.  `IRMINSUL_PROFILES_DIR` replaces it, for
/// trying things out without touching the real ones.
pub fn profiles_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("IRMINSUL_PROFILES_DIR") {
        return Ok(dir.into());
    }
    let mut dir = eframe::storage_dir(APP_ID).context("Storage dir not found")?;
    dir.push("profiles");
    Ok(dir)
}

/// The current time, as stored with the data.
pub fn now() -> String {
    Local::now().to_rfc3339()
}

mod byte_list {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    use super::*;

    pub fn serialize<S: Serializer>(list: &[Vec<u8>], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(list.iter().map(|bytes| BASE64_STANDARD.encode(bytes)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Vec<u8>>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .iter()
            .map(|text| BASE64_STANDARD.decode(text).map_err(D::Error::custom))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct StoredAchievement {
    pub id: u32,
    pub status: u32,
    pub finished_at: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Profile {
    pub uid: u32,
    #[serde(default)]
    pub nickname: Option<String>,

    /// The mora.  It is not among the items.
    #[serde(default)]
    pub mora: Option<u64>,

    /// The inventory, as protobuf `Item` messages.
    #[serde(default, with = "byte_list")]
    pub items: Vec<Vec<u8>>,
    #[serde(default)]
    pub items_at: Option<String>,

    /// The characters, as protobuf `AvatarInfo` messages.
    #[serde(default, with = "byte_list")]
    pub characters: Vec<Vec<u8>>,
    #[serde(default)]
    pub characters_at: Option<String>,

    #[serde(default)]
    pub achievements: Vec<StoredAchievement>,
    #[serde(default)]
    pub achievements_at: Option<String>,
}

/// What a profile holds, for listing them.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSummary {
    pub uid: u32,
    pub nickname: Option<String>,
    pub items: usize,
    pub characters: usize,
    pub achievements: usize,
    /// When something was last captured.
    pub updated_at: Option<String>,
}

fn path_in(dir: &Path, uid: u32) -> PathBuf {
    dir.join(format!("{uid}.json"))
}

impl Profile {
    pub fn summary(&self) -> ProfileSummary {
        // RFC 3339 times with the same offset sort like the times they are.
        let updated_at = [&self.items_at, &self.characters_at, &self.achievements_at]
            .into_iter()
            .flatten()
            .max()
            .cloned();
        ProfileSummary {
            uid: self.uid,
            nickname: self.nickname.clone(),
            items: self.items.len(),
            characters: self.characters.len(),
            achievements: self.achievements.len(),
            updated_at,
        }
    }

    pub fn load_from(dir: &Path, uid: u32) -> Result<Option<Profile>> {
        let path = path_in(dir, uid);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("unable to read {path:?}")),
        };
        let profile: Profile =
            serde_json::from_str(&text).with_context(|| format!("unable to parse {path:?}"))?;
        Ok(Some(profile))
    }

    /// Replaces the file of the account.  Written next to it and renamed, so
    /// an interrupted write can't leave half a file behind.
    pub fn save_in(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let path = path_in(dir, self.uid);
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_vec(self)?)
            .with_context(|| format!("unable to write {temp:?}"))?;
        fs::rename(&temp, &path).with_context(|| format!("unable to replace {path:?}"))?;
        Ok(())
    }

    pub fn load(uid: u32) -> Result<Option<Profile>> {
        Self::load_from(&profiles_dir()?, uid)
    }

    pub fn save(&self) -> Result<()> {
        self.save_in(&profiles_dir()?)
    }
}

/// The stored profiles, the most recently updated first.
pub fn list_in(dir: &Path) -> Vec<ProfileSummary> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut profiles: Vec<ProfileSummary> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            let uid: u32 = path.file_stem()?.to_str()?.parse().ok()?;
            match Profile::load_from(dir, uid) {
                Ok(profile) => profile.map(|profile| profile.summary()),
                Err(e) => {
                    tracing::warn!("ignoring the profile {uid}: {e:#}");
                    None
                }
            }
        })
        .collect();
    profiles.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.uid.cmp(&b.uid)));
    profiles
}

pub fn list() -> Vec<ProfileSummary> {
    profiles_dir()
        .map(|dir| list_in(&dir))
        .unwrap_or_default()
}

pub fn delete_in(dir: &Path, uid: u32) -> Result<()> {
    match fs::remove_file(path_in(dir, uid)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn delete(uid: u32) -> Result<()> {
    delete_in(&profiles_dir()?, uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(uid: u32, nickname: &str, items: usize, at: &str) -> Profile {
        Profile {
            uid,
            nickname: Some(nickname.to_owned()),
            items: (0..items).map(|i| vec![i as u8, 1, 2, 255]).collect(),
            items_at: Some(at.to_owned()),
            achievements: vec![StoredAchievement {
                id: 80001,
                status: 3,
                finished_at: Some(1_700_000_000),
            }],
            achievements_at: Some(at.to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn a_profile_survives_storing() {
        let dir = tempfile::tempdir().unwrap();
        let stored = profile(700_000_001, "yawn", 5, "2026-11-07T10:00:00+03:00");
        stored.save_in(dir.path()).unwrap();

        let loaded = Profile::load_from(dir.path(), 700_000_001).unwrap().unwrap();
        assert_eq!(loaded.nickname.as_deref(), Some("yawn"));
        assert_eq!(loaded.items, stored.items);
        assert_eq!(loaded.achievements, stored.achievements);
        assert!(Profile::load_from(dir.path(), 1).unwrap().is_none());
    }

    #[test]
    fn saving_again_replaces_the_one_file() {
        let dir = tempfile::tempdir().unwrap();
        profile(700_000_001, "yawn", 5, "2026-11-07T10:00:00+03:00")
            .save_in(dir.path())
            .unwrap();
        profile(700_000_001, "yawn", 9, "2026-11-07T12:00:00+03:00")
            .save_in(dir.path())
            .unwrap();

        let files: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(files.len(), 1, "one file per account, no leftovers");
        let loaded = Profile::load_from(dir.path(), 700_000_001).unwrap().unwrap();
        assert_eq!(loaded.items.len(), 9);
    }

    #[test]
    fn profiles_are_listed_newest_first_and_can_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        profile(1, "old", 1, "2026-11-01T10:00:00+03:00").save_in(dir.path()).unwrap();
        profile(2, "new", 2, "2026-11-07T10:00:00+03:00").save_in(dir.path()).unwrap();
        fs::write(dir.path().join("notes.txt"), "not a profile").unwrap();
        fs::write(dir.path().join("3.json"), "{ not json").unwrap();

        let listed = list_in(dir.path());
        assert_eq!(listed.iter().map(|p| p.uid).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(listed[0].items, 2);
        assert_eq!(listed[0].nickname.as_deref(), Some("new"));

        delete_in(dir.path(), 2).unwrap();
        delete_in(dir.path(), 2).unwrap();
        assert_eq!(list_in(dir.path()).len(), 1);
    }
}
