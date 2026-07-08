//! The bare (default) cache: hard-coded reference data baked into the binary.
//!
//! Everything the app needs to bootstrap *before* it has ever talked to
//! Market or yoweb lives here, embedded at compile time via
//! [`include_str!`]:
//!
//! - **goods** — the canonical commodity grouping and ordering (our source of
//!   truth for display order; ids come from Market at runtime),
//! - **adjectives** / **swabbie names** — the seed NPC name vocabulary used to
//!   tell a swabbie (`[adjective] [name]`) from a mercenary (`[name]
//!   [epithet]`),
//! - **oceans → archipelagos → islands** — the geography (currently only
//!   Emerald and Meridian are filled in; the rest are placeholders to be
//!   crawled from yppedia later).
//!
//! This is the starting point for anything we persist: a first run with no
//! `cache.json` seeds itself from here (see [`crate::cache::load`]).

use std::sync::LazyLock;

use serde::Deserialize;

/// One commodity group with its members, in canonical (in-game) order.
#[derive(Deserialize)]
pub struct GoodGroup {
    pub group: String,
    pub commodities: Vec<String>,
}

/// Physical island size class (governs building slots, etc.).
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Outpost,
    Small,
    Medium,
    Large,
}

/// Inhabitation status. `Capital` is the archipelago's hub island — always also
/// colonized; at most one per archipelago (some archipelagos have none).
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Uninhabited,
    Colonized,
    Capital,
}

/// One island: its name, size class, and inhabitation status. Status reflects
/// the yppedia snapshot at crawl time (colonization is dynamic in-game); size
/// and capital designation are stable.
#[derive(Deserialize)]
pub struct Island {
    pub name: String,
    pub size: Size,
    pub status: Status,
}

/// One archipelago and the islands within it.
#[derive(Deserialize)]
pub struct Archipelago {
    pub name: String,
    pub islands: Vec<Island>,
}

impl Archipelago {
    /// The archipelago's capital (hub) island, if one is designated.
    pub fn capital(&self) -> Option<&Island> {
        self.islands.iter().find(|i| i.status == Status::Capital)
    }
}

/// One ocean's geography. `archipelagos` is empty for oceans not yet crawled.
#[derive(Deserialize)]
pub struct Ocean {
    pub name: String,
    pub archipelagos: Vec<Archipelago>,
}

/// The whole bare cache, as parsed from the embedded JSON.
#[derive(Deserialize)]
pub struct BareCache {
    pub goods: Vec<GoodGroup>,
    pub adjectives: Vec<String>,
    pub swabbie_names: Vec<String>,
    pub oceans: Vec<Ocean>,
}

impl BareCache {
    /// Look up an ocean's geography by (case-insensitive) name, e.g.
    /// `"Emerald"`.
    pub fn ocean(&self, name: &str) -> Option<&Ocean> {
        self.oceans
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case(name))
    }
}

// The pretty source (`data/bare_cache.json`) is minified at build time by
// `build.rs`; we embed the compact copy it drops in `OUT_DIR`, not the source.
const BARE_JSON: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/bare_cache.min.json"
));

/// The embedded bare cache, parsed once on first access. A malformed
/// `bare_cache.json` is a build-time authoring error, so we panic loudly rather
/// than limp along with empty data.
pub static BARE: LazyLock<BareCache> = LazyLock::new(|| {
    serde_json::from_str(BARE_JSON)
        .expect("embedded bare_cache.json is malformed")
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_cache_parses_and_is_populated() {
        let bare = &*BARE;
        assert!(
            !bare.goods.is_empty(),
            "goods must be seeded"
        );
        assert!(
            !bare.adjectives.is_empty(),
            "adjectives must be seeded"
        );
        assert!(
            !bare.swabbie_names.is_empty(),
            "swabbie names must be seeded"
        );
        // All seven live oceans are present, even if some carry no archipelagos
        // yet.
        assert_eq!(
            bare.oceans.len(),
            crate::ocean::Ocean::LIVE.len()
        );
    }

    #[test]
    fn emerald_geography_is_filled_in() {
        let emerald = BARE
            .oceans
            .iter()
            .find(|o| o.name == "Emerald")
            .expect("Emerald ocean present");
        let gull = emerald
            .archipelagos
            .iter()
            .find(|a| a.name == "Gull")
            .expect("Gull archipelago present");
        // Gull's capital (hub) is Wensleydale, a Large island.
        let capital = gull.capital().expect("Gull has a capital");
        assert_eq!(capital.name, "Wensleydale");
        assert_eq!(capital.size, Size::Large);
        assert!(gull.islands.iter().any(|i| i.name == "Admiral Island"));
    }

    #[test]
    fn at_most_one_capital_per_archipelago() {
        for ocean in &BARE.oceans {
            for arch in &ocean.archipelagos {
                let capitals = arch
                    .islands
                    .iter()
                    .filter(|i| i.status == Status::Capital)
                    .count();
                assert!(
                    capitals <= 1,
                    "{}/{} has {capitals} capitals",
                    ocean.name,
                    arch.name
                );
            }
        }
    }
}
