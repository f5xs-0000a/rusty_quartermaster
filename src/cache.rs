//! The single on-disk cache file.
//!
//! Everything we persist between runs lives in one JSON file (see [`SavedCache`]):
//! the inventory and commodity list are global, while market prices and the
//! playerbase are kept per-ocean, since each ocean has its own economy and its
//! own pirates.

use std::collections::{BTreeSet, HashMap};
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

/// Learned NPC name-segment vocabulary, used to tell a **swabbie** from a
/// **mercenary** among the NPCs aboard a ship.
///
/// Puzzle Pirates names NPCs two ways:
/// - mercenary = `[name] [epithet]`  (e.g. "Luka Merciless")
/// - swabbie / brigand / vampirate = `[adjective] [name]`  (e.g. "Gentle Gayle")
///
/// The two families share a *given-name* pool, so only the *position* of the
/// adjective distinguishes them. We bootstrap the vocabulary from **brigand
/// victories** (fights we lost to NPC brigands — no enemy players, no Brigand
/// King): those rosters are uniformly `[adjective] [name]`, so the left word is
/// always an adjective and the right always a name. Our own wins and PvP losses
/// are skipped, since those crews mix mercenaries and swabbies.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct NameSegments {
    /// Given names — the right word of a brigand `[adjective] [name]`.
    #[serde(default)]
    pub name: BTreeSet<String>,
    /// Adjectives / epithets — the left word of a brigand `[adjective] [name]`.
    #[serde(default)]
    pub adjectives: BTreeSet<String>,
}

/// The kind of NPC aboard, per [`NameSegments::classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpcKind {
    /// `[adjective] [name]` — takes only a pre-divvy skim, not a counted share.
    Swabbie,
    /// `[name] [epithet]` — divvied like a pirate.
    Mercenary,
}

impl NameSegments {
    /// Learn one brigand's name (`[adjective] [name]`): the left word is an
    /// adjective, the right a given name. No-op unless the name is exactly two
    /// whitespace-separated words and not a special character. Specials (Brigand
    /// Kings, "Mother o' Nyght") are matched and excluded *whole* — never split
    /// into segments, since their words overlap real ones (e.g. "Mad" in "Vargas
    /// the Mad" is also a legitimate adjective).
    pub fn learn_brigand(&mut self, npc: &str) {
        if crate::pirate::is_special_name(npc) {
            return;
        }
        let mut words = npc.split_whitespace();
        if let (Some(adj), Some(name), None) = (words.next(), words.next(), words.next()) {
            self.adjectives.insert(adj.to_string());
            self.name.insert(name.to_string());
        }
    }

    /// Classify a two-word NPC as a swabbie or mercenary. Returns `None` for a
    /// special character or anything that isn't a two-word NPC (e.g. a single-word
    /// player name, or a three-word special we don't recognize).
    ///
    /// It's a **swabbie** when the split is unambiguously `[adjective] [name]`:
    /// - the left word is a known adjective that is *not* also a known name, or
    /// - the right word is a known name.
    ///
    /// Otherwise it's a **mercenary**. The left-side `!name` guard is essential —
    /// it keeps "Red Ear-biter" a mercenary even though "Red" is also a name. The
    /// right-side test needs no such guard, because a mercenary's epithet is never
    /// a name; that's what lets "Red Red" correctly resolve to a swabbie.
    pub fn classify(&self, npc: &str) -> Option<NpcKind> {
        if crate::pirate::is_special_name(npc) {
            return None;
        }
        let mut words = npc.split_whitespace();
        let (left, right) = (words.next()?, words.next()?);
        if words.next().is_some() {
            return None; // more than two words — not a plain NPC name
        }
        let left_is_adjective = self.adjectives.contains(left) && !self.name.contains(left);
        let right_is_name = self.name.contains(right);
        Some(if left_is_adjective || right_is_name {
            NpcKind::Swabbie
        } else {
            NpcKind::Mercenary
        })
    }
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
    /// Learned NPC name-segment vocabulary (swabbie vs mercenary classification).
    #[serde(default)]
    pub name_segments: NameSegments,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Vocabulary learned from a handful of brigand victories (each entry is
    /// `[adjective] [name]`). "Red" deliberately lands on *both* sides.
    fn learned() -> NameSegments {
        let mut ns = NameSegments::default();
        for brigand in [
            "Gentle Gayle",
            "Furious William",
            "Callous Aster",
            "Red Alan",
            "Barmy Red",
            "Red Red",
        ] {
            ns.learn_brigand(brigand);
        }
        ns
    }

    #[test]
    fn learn_splits_adjective_and_name() {
        let ns = learned();
        assert!(ns.adjectives.contains("Gentle") && ns.name.contains("Gayle"));
        assert!(ns.adjectives.contains("Furious") && ns.name.contains("William"));
        // "Red" appears in both positions across the rosters -> in both sets.
        assert!(ns.adjectives.contains("Red") && ns.name.contains("Red"));
    }

    #[test]
    fn learn_skips_specials_and_non_two_word() {
        let mut ns = NameSegments::default();
        ns.learn_brigand("Vargas the Mad"); // special -> excluded whole
        ns.learn_brigand("Mother o' Nyght"); // special
        ns.learn_brigand("Playerone"); // single word (a player)
        assert!(ns.adjectives.is_empty() && ns.name.is_empty());
        // "Mad" is a real adjective and must stay learnable from a normal brigand,
        // even though it also appears inside the special "Vargas the Mad".
        ns.learn_brigand("Mad Carter");
        assert!(ns.adjectives.contains("Mad") && ns.name.contains("Carter"));
    }

    #[test]
    fn classifies_plain_swabbie_and_mercenary() {
        let ns = learned();
        assert_eq!(ns.classify("Gentle Gayle"), Some(NpcKind::Swabbie));
        assert_eq!(ns.classify("Callous Aster"), Some(NpcKind::Swabbie));
        // Mercenary = `[name] [epithet]`; the epithet is in neither set.
        assert_eq!(ns.classify("Elias Callous"), Some(NpcKind::Mercenary));
        assert_eq!(ns.classify("Bree Steeljaw"), Some(NpcKind::Mercenary));
    }

    #[test]
    fn resolves_the_red_overlap() {
        let ns = learned();
        // "Red" is both a name and an adjective; each pair still resolves.
        assert_eq!(ns.classify("Red Alan"), Some(NpcKind::Swabbie)); // right is a name
        assert_eq!(ns.classify("Barmy Red"), Some(NpcKind::Swabbie)); // left is a pure adjective
        assert_eq!(ns.classify("Red Ear-biter"), Some(NpcKind::Mercenary)); // left=name, epithet right
        assert_eq!(ns.classify("Red Red"), Some(NpcKind::Swabbie)); // both-dual edge case
    }

    #[test]
    fn non_npc_inputs_are_none() {
        let ns = learned();
        assert_eq!(ns.classify("Playerone"), None); // single-word player
        assert_eq!(ns.classify("Vargas the Mad"), None); // special (three words)
        assert_eq!(ns.classify("Mother o' Nyght"), None); // special
    }

    #[test]
    fn name_segments_round_trip_through_cache() {
        let mut cache = SavedCache::default();
        cache.name_segments = learned();
        let json = serde_json::to_string(&cache).unwrap();
        let back: SavedCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name_segments.name, cache.name_segments.name);
        assert_eq!(back.name_segments.adjectives, cache.name_segments.adjectives);
    }
}
