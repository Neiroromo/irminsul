//! Makes `src/data/official_chests.json`: the chests the official interactive
//! map of the game shows, with where they are on it.  Irminsul uses it to show
//! the chests that are not found yet, and to tell which of the official ones a
//! chest it found is.
//!
//! Only the chests of the open world (the main layer of the map) are kept, as
//! `[id, tier, x, y]`: the tier is 1 to 5 (common, exquisite, precious,
//! luxurious, remarkable), x and y are the map's own coordinates, from the
//! middle of the world.  Run it after a game update:
//!
//! ```text
//! cargo run --release --example gen_official_chests
//! ```

use anyhow::{Context, Result};
use serde_json::{Value, json};

const POINTS_URL: &str = "https://sg-public-api-static.hoyolab.com/common/map_user/ys_obc/v3/map/point/list?map_id=2&app_sn=ys_obc&lang=en-us";
const OUTPUT: &str = "src/data/official_chests.json";

/// The labels of the chests of the map, and the tier of each.
const LABELS: [(u64, u64); 5] = [(17, 1), (44, 2), (45, 3), (46, 4), (269, 5)];

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::builder().user_agent("irminsul-gen").build()?;
    println!("downloading the points of the map");
    let body: Value = client
        .get(POINTS_URL)
        .send()
        .await
        .context("request failed")?
        .error_for_status()?
        .json()
        .await?;
    let points = body["data"]["point_list"]
        .as_array()
        .context("no list of points")?;
    println!("{} points", points.len());

    let mut chests: Vec<(u64, u64, f64, f64)> = Vec::new();
    for point in points {
        let Some(label) = point["label_id"].as_u64() else {
            continue;
        };
        let Some(&(_, tier)) = LABELS.iter().find(|(id, _)| *id == label) else {
            continue;
        };
        // Other layers of the map (underground) are not the world's surface.
        if point["z_level"].as_i64().unwrap_or(0) != 0 {
            continue;
        }
        let (Some(id), Some(x), Some(y)) = (
            point["id"].as_u64(),
            point["x_pos"].as_f64(),
            point["y_pos"].as_f64(),
        ) else {
            continue;
        };
        chests.push((id, tier, (x * 100.).round() / 100., (y * 100.).round() / 100.));
    }
    chests.sort_by_key(|chest| chest.0);
    let rows: Vec<Value> = chests
        .iter()
        .map(|(id, tier, x, y)| json!([id, tier, x, y]))
        .collect();
    std::fs::write(OUTPUT, serde_json::to_string(&rows)?)?;
    println!("wrote {OUTPUT} with {} chests", rows.len());
    Ok(())
}
