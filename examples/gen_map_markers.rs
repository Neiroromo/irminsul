//! Makes `src/data/map_labels.json`, `src/data/map_points.json.gz` and
//! `src/data/map_atlas.json`: the kinds of marker of the official interactive
//! map of the game (resources, oculi, waypoints and the rest), where each marker
//! is, and the maps with their regions, underground places and floors.  Irminsul
//! shows them on its map.  The program also fetches them by itself now and then
//! (see `markers.rs`); this is for what it is built with.
//!
//! ```text
//! cargo run --release --example gen_map_markers
//! ```

#[path = "../src/atlas_fetch.rs"]
mod atlas_fetch;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("irminsul-gen")
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let fetched = atlas_fetch::fetch_all(&client).await?;
    std::fs::write("src/data/map_labels.json", &fetched.labels)?;
    std::fs::write("src/data/map_atlas.json", &fetched.atlas)?;
    std::fs::write("src/data/map_points.json.gz", &fetched.points_gz)?;
    println!(
        "wrote {} kinds, {} markers ({} bytes), the atlas ({} bytes)",
        fetched.kinds,
        fetched.markers,
        fetched.points_gz.len(),
        fetched.atlas.len()
    );
    Ok(())
}
