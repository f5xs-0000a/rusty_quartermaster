//! The single on-disk cache file.
//!
//! Everything we persist between runs lives in one JSON file (see [`SavedCache`]):
//! the inventory and commodity list are global, while market prices and the
//! playerbase are kept per-ocean, since each ocean has its own economy and its
//! own pirates.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::api::{CachedOffers, SavedCommodity};
use crate::pirate::CachedPirate;
use crate::profits::persistence::SavedInventory;

/// Per-ocean cached data. Prices (`market`) and the playerbase are both
/// specific to one ocean, so each ocean gets its own bucket.
#[derive(Serialize, Deserialize, Default)]
pub struct OceanCache {
    /// Market prices (offers) keyed by commodity name.
    #[serde(default)]
    pub market: HashMap<String, CachedOffers>,
    /// Yoweb pirate stats keyed by normalized name; includes our own pirate so
    /// we don't re-query it next run. Each entry carries fetch timestamps.
    #[serde(default)]
    pub players: HashMap<String, CachedPirate>,
}

/// Everything we persist between runs, in a single JSON file.
///
/// New global categories get a new top-level field; new per-ocean categories
/// get a field on [`OceanCache`]. All fields use `#[serde(default)]` so older
/// files keep loading.
#[derive(Serialize, Deserialize, Default)]
pub struct SavedCache {
    #[serde(default)]
    pub inventory: SavedInventory,
    /// Commodity id<->name list. Market's commodity list is ocean-independent,
    /// so it lives once at the top level rather than under each ocean.
    #[serde(default)]
    pub commodities: Vec<SavedCommodity>,
    /// Per-ocean data, keyed by ocean name (e.g. `"Emerald"`).
    #[serde(default)]
    pub oceans: HashMap<String, OceanCache>,
}

/// Load the cache from `path`. A missing or unparseable file yields an empty
/// cache rather than an error, so a first run just starts fresh.
pub fn load(path: &Path) -> SavedCache {
    let Ok(data) = std::fs::read_to_string(path) else {
        return SavedCache::default();
    };
    match serde_json::from_str(&data) {
        Ok(cache) => {
            eprintln!("Loaded cache from {}", path.display());
            cache
        }
        Err(e) => {
            eprintln!("warning: failed to parse cache: {}", e);
            SavedCache::default()
        }
    }
}

/// Write `cache` to `path` as pretty JSON.
pub fn save(path: &Path, cache: &SavedCache) {
    let json = match serde_json::to_string_pretty(cache) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("error: failed to serialize cache: {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("error: failed to write cache to {}: {}", path.display(), e);
    } else {
        eprintln!("Saved cache to {}", path.display());
    }
}
