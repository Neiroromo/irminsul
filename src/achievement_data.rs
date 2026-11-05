//! Static achievement database used by the achievement tracker tab.
//!
//! The data comes from paimon-moe (see `THIRD_PARTY_NOTICES.md`).  The
//! category key of each group is the game's achievement `goalId`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::APP_ID;

static DATA_RU: &str = include_str!("data/achievements_ru.json");
static DATA_EN: &str = include_str!("data/achievements_en.json");
/// paimon-moe commit the bundled data was taken from.
static DATA_VERSION: &str = include_str!("data/achievements_version.txt");

const COMMITS_URL: &str =
    "https://api.github.com/repos/MadeBaruna/paimon-moe/commits?path=src/data/achievement&per_page=1";
const RAW_URL: &str = "https://raw.githubusercontent.com/MadeBaruna/paimon-moe";
const VERSION_FILE: &str = "version.txt";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum Language {
    #[default]
    Ru,
    En,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Achievement {
    pub id: u32,
    pub name: String,
    pub desc: String,
    pub reward: u32,
    pub ver: String,
    /// Region the achievement belongs to when it is a commission achievement.
    #[serde(default, rename = "commissions")]
    pub region: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawEntry {
    Chain(Vec<Achievement>),
    Single(Achievement),
}

#[derive(Deserialize)]
struct RawCategory {
    name: String,
    order: u32,
    achievements: Vec<RawEntry>,
}

pub struct Category {
    pub name: String,
    /// Achievements in display order.  Multi-stage achievements are chains
    /// with more than one element; everything else has exactly one.
    pub entries: Vec<Vec<Achievement>>,
}

pub struct AchievementDb {
    pub categories: Vec<Category>,
    /// Every game version that added an achievement, oldest first.
    pub versions: Vec<String>,
}

fn version_key(version: &str) -> (u32, u32) {
    let (major, minor) = version.split_once('.').unwrap_or((version, "0"));
    (major.parse().unwrap_or(0), minor.parse().unwrap_or(0))
}

impl Language {
    fn file_name(self) -> &'static str {
        match self {
            Language::Ru => "ru.json",
            Language::En => "en.json",
        }
    }

    fn bundled(self) -> &'static str {
        match self {
            Language::Ru => DATA_RU,
            Language::En => DATA_EN,
        }
    }
}

/// Directory holding achievement data downloaded by an update.
fn cache_dir() -> Option<PathBuf> {
    let mut dir = eframe::storage_dir(APP_ID)?;
    dir.push("achievement_data");
    Some(dir)
}

/// Version of the data currently in use: the downloaded copy if there is one,
/// the bundled copy otherwise.
fn current_version() -> String {
    cache_dir()
        .and_then(|dir| fs::read_to_string(dir.join(VERSION_FILE)).ok())
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| DATA_VERSION.trim().to_owned())
}

#[derive(Debug)]
pub enum UpdateOutcome {
    UpToDate,
    Updated,
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("request to {url} failed"))?;
    Ok(response.bytes().await?.to_vec())
}

async fn update_async() -> Result<UpdateOutcome> {
    let client = reqwest::Client::builder().user_agent("irminsul").build()?;

    let commits: Vec<serde_json::Value> = serde_json::from_slice(&fetch(&client, COMMITS_URL).await?)?;
    let latest = commits
        .first()
        .and_then(|commit| commit["sha"].as_str())
        .ok_or_else(|| anyhow!("no commits found for the achievement data"))?
        .to_owned();
    if latest == current_version() {
        return Ok(UpdateOutcome::UpToDate);
    }

    let dir = cache_dir().context("storage directory not found")?;
    let mut downloads = Vec::new();
    for language in [Language::Ru, Language::En] {
        let url = format!(
            "{RAW_URL}/{latest}/src/data/achievement/{}",
            language.file_name()
        );
        let data = String::from_utf8(fetch(&client, &url).await?)?;
        // Don't replace working data with something that doesn't parse.
        AchievementDb::from_str(&data).with_context(|| format!("invalid data in {url}"))?;
        downloads.push((language, data));
    }

    fs::create_dir_all(&dir)?;
    for (language, data) in downloads {
        fs::write(dir.join(language.file_name()), data)?;
    }
    // Written last so a partial update is retried.
    fs::write(dir.join(VERSION_FILE), &latest)?;
    Ok(UpdateOutcome::Updated)
}

/// Checks for newer achievement data and downloads it.  Blocks, so run it
/// off the UI thread.
pub fn check_for_update() -> Result<UpdateOutcome> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(update_async())
}

impl AchievementDb {
    /// Loads the downloaded data if there is any and it is valid, falling back
    /// to the bundled data.
    pub fn load(language: Language) -> Result<Self> {
        if let Some(path) = cache_dir().map(|dir| dir.join(language.file_name()))
            && let Ok(data) = fs::read_to_string(&path)
        {
            match Self::from_str(&data) {
                Ok(db) => return Ok(db),
                Err(e) => tracing::warn!("ignoring downloaded achievement data {path:?}: {e}"),
            }
        }
        Self::from_str(language.bundled())
    }

    fn from_str(data: &str) -> Result<Self> {
        let raw: BTreeMap<u32, RawCategory> = serde_json::from_str(data)?;

        let mut raw: Vec<_> = raw.into_iter().collect();
        raw.sort_by_key(|(_, category)| category.order);

        let mut versions = Vec::new();
        let categories = raw
            .into_iter()
            .map(|(_, category)| {
                let entries: Vec<Vec<Achievement>> = category
                    .achievements
                    .into_iter()
                    .map(|entry| match entry {
                        RawEntry::Chain(chain) => chain,
                        RawEntry::Single(achievement) => vec![achievement],
                    })
                    .collect();
                for achievement in entries.iter().flatten() {
                    if !versions.contains(&achievement.ver) {
                        versions.push(achievement.ver.clone());
                    }
                }
                Category {
                    name: category.name,
                    entries,
                }
            })
            .collect();
        versions.sort_by_key(|version| version_key(version));

        Ok(Self {
            categories,
            versions,
        })
    }
}
