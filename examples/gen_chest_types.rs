//! Makes `src/data/chest_types.json`, which Irminsul reads to tell which of the
//! things in the world are chests, and what kind.
//!
//! The kinds are taken from the game's gadget table: those of the type "Chest"
//! whose names say they are the chests found in the world.  Run it after a game
//! update:
//!
//! ```text
//! cargo run --release --example gen_chest_types
//! ```

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

const COMMITS_API_URL: &str =
    "https://api.github.com/repos/DimbreathBot/AnimeGameData/commits?per_page=1";
const REPO_URL: &str = "https://raw.githubusercontent.com/DimbreathBot/AnimeGameData";
const OUTPUT: &str = "src/data/chest_types.json";

async fn download(client: &reqwest::Client, git_ref: &str, path: &str) -> Result<Vec<u8>> {
    let url = format!("{REPO_URL}/{git_ref}/{path}");
    println!("downloading {path}");
    Ok(client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("request to {url} failed"))?
        .bytes()
        .await?
        .to_vec())
}

/// Whether the gadget is a chest that is found in the world.
fn is_world_chest(json_name: &str) -> bool {
    let chest = json_name.starts_with("SceneObj_Chest_")
        || json_name.starts_with("SceneObj_EssenceChest_")
        || json_name.starts_with("SceneObj_Ticket_Chest_");
    let not_one = ["Test", "DropTest", "Drop_Chest", "Empty", "FollowMove_Chest"]
        .iter()
        .any(|word| json_name.contains(word));
    chest && !not_one
}

/// How good the chest is, by what the game calls it; 0 for the ones that are
/// not among the usual five.
fn tier(name: &str) -> u64 {
    match name {
        "Common Chest" => 1,
        "Exquisite Chest" => 2,
        "Precious Chest" => 3,
        "Luxurious Chest" => 4,
        "Remarkable Chest" => 5,
        _ => 0,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::builder().user_agent("irminsul-gen").build()?;
    let commits: Vec<Value> = serde_json::from_slice(
        &client
            .get(COMMITS_API_URL)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?,
    )?;
    let git_ref = commits
        .first()
        .and_then(|commit| commit["sha"].as_str())
        .context("no commits found")?
        .to_owned();
    println!("game data {git_ref}");

    let gadgets: Vec<Value> = match serde_json::from_slice(
        &download(&client, &git_ref, "ExcelBinOutput/GadgetExcelConfigData.json").await?,
    )? {
        Value::Array(entries) => entries,
        _ => return Err(anyhow!("the gadget table is not a list")),
    };
    let chests: Vec<&Value> = gadgets
        .iter()
        .filter(|row| row["type"] == "Chest")
        .filter(|row| is_world_chest(row["jsonName"].as_str().unwrap_or("")))
        .collect();
    println!("{} chests", chests.len());

    let text_map = |bytes: Vec<u8>| -> Result<HashMap<String, String>> {
        Ok(serde_json::from_slice(&bytes)?)
    };
    let en = text_map(download(&client, &git_ref, "TextMap/TextMap_MediumEN.json").await?)?;
    let mut ru = text_map(download(&client, &git_ref, "TextMap/TextMap_MediumRU_0.json").await?)?;
    ru.extend(text_map(download(&client, &git_ref, "TextMap/TextMap_MediumRU_1.json").await?)?);

    let mut out = BTreeMap::new();
    for row in chests {
        let id = row["id"].as_u64().context("a chest without an id")?;
        let json_name = row["jsonName"].as_str().unwrap_or("");
        // The name of a chest is in what the game says when it can be opened.
        let hash = row["interactNameTextMapHash"].as_u64().unwrap_or(0).to_string();
        let name_en = en.get(&hash).cloned().unwrap_or_default();
        let name_ru = ru.get(&hash).cloned().unwrap_or_default();
        let kind = json_name
            .trim_start_matches("SceneObj_")
            .trim_start_matches("Ticket_")
            .split('_')
            .nth(1)
            .unwrap_or("")
            .to_owned();
        out.insert(
            id.to_string(),
            json!({
                "en": name_en,
                "ru": name_ru,
                "tier": tier(&name_en),
                "locked": json_name.contains("Locked"),
                "kind": kind,
                "water": json_name.contains("Water") || json_name.contains("Amphibious"),
                "json": json_name,
            }),
        );
    }
    std::fs::write(OUTPUT, serde_json::to_string_pretty(&out)?)?;
    println!("wrote {OUTPUT} with {} kinds of chest", out.len());
    Ok(())
}
