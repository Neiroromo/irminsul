use std::fs::File;
use std::path::Path;
use std::{env, io};

use flate2::Compression;
use flate2::write::GzEncoder;
use winresource::WindowsResource;

const COMMITS_API_URL: &str = "https://gitlab.com/api/v4/projects/83871005/repository/commits";
const REPO_BASE_URL: &str = "https://gitlab.com/Dimbreath/animegamedata2/-/raw";

/// Writes a JSON object mapping achievement id to its category (`goalId`).
/// Achievements without a `goalId` belong to category 0.
async fn fetch_achievement_goals(path: &Path) {
    let client = reqwest::Client::new();
    let commits: serde_json::Value = serde_json::from_slice(
        &client
            .get(COMMITS_API_URL)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let git_ref = commits[0]["id"].as_str().unwrap();

    let url = format!("{REPO_BASE_URL}/{git_ref}/ExcelBinOutput/AchievementExcelConfigData.json");
    let entries: Vec<serde_json::Value> = serde_json::from_slice(
        &client
            .get(&url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();

    let goals: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_u64()?;
            let goal = entry["goalId"].as_u64().unwrap_or(0);
            Some((id.to_string(), goal.into()))
        })
        .collect();
    std::fs::write(path, serde_json::to_vec(&goals).unwrap()).unwrap();
}

#[tokio::main]
async fn main() -> io::Result<()> {
    // Download new game data and save it in a location to be included by the source.
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let cache_path = Path::new(&out_dir).join("game_data.json");

    let mut db = anime_game_data::AnimeGameData::new_with_cache(&cache_path);
    let db_updated = db.needs_update().await.unwrap();
    if db_updated {
        db.update().await.unwrap();
        let out_path = Path::new(&out_dir).join("game_data.gz");
        let f = File::create(out_path).unwrap();
        let writer = GzEncoder::new(f, Compression::best());
        db.save_to_writer(writer).unwrap();
    }

    // anime-game-data has no achievement tables, so fetch the achievement ->
    // category (goal) mapping separately.
    let goals_path = Path::new(&out_dir).join("achievement_goals.json");
    if db_updated || !goals_path.exists() {
        fetch_achievement_goals(&goals_path).await;
    }

    // Add icon to windows binary.
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()?;
    }

    #[cfg(all(unix, feature = "static-libpcap"))]
    {
        println!("cargo:rustc-link-lib=static=pcap");
    }
    Ok(())
}
