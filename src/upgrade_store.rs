//! The game data in `data/upgrade_data.json.gz`, read once when first needed.

use std::sync::OnceLock;

use flate2::read::GzDecoder;

use crate::upgrade_data::UpgradeData;

static PACKED: &[u8] = include_bytes!("data/upgrade_data.json.gz");

pub fn data() -> &'static UpgradeData {
    static DATA: OnceLock<UpgradeData> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_reader(GzDecoder::new(PACKED)).unwrap_or_else(|e| {
            tracing::error!("unable to read the game data: {e}");
            UpgradeData::default()
        })
    })
}
