//! Authoritative commodity ordering.
//!
//! This is *our* source of truth for how commodities are grouped and ordered —
//! seeded from the in-game canonical order. Market supplies live data (ids,
//! prices); we own the display order. Where our list drifts from the game we
//! nudge it here.
//!
//! Names match Market's exact spelling (including its lowercase quirks like
//! "fine brown cloth"); lookups are case-insensitive. A commodity Market
//! returns that isn't in the list sorts *last* — that's the signal to come add
//! it to the right place.
//!
//! The grouping and ordering themselves live in the embedded bare cache
//! (`data/bare_cache.json`, see [`crate::bare`]); this module just indexes it.

use std::{collections::HashMap, sync::LazyLock};

use crate::bare::BARE;

/// Lowercased name → its flat position in the canonical order.
static INDEX: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    let mut i = 0;
    for group in &BARE.goods {
        for name in &group.commodities {
            map.insert(name.to_lowercase(), i);
            i += 1;
        }
    }
    map
});

/// Lowercased name → its group label.
#[allow(dead_code)] // companion to order_index; not yet used
static GROUP_OF: LazyLock<HashMap<String, &'static str>> =
    LazyLock::new(|| {
        let mut map = HashMap::new();
        for group in &BARE.goods {
            for name in &group.commodities {
                map.insert(
                    name.to_lowercase(),
                    group.group.as_str(),
                );
            }
        }
        map
    });

/// Canonical position of a commodity, or `None` if it isn't in our list.
pub fn order_index(name: &str) -> Option<usize> {
    INDEX.get(&name.to_lowercase()).copied()
}

/// The group a commodity belongs to, or `None` if it isn't in our list.
#[allow(dead_code)] // companion to order_index; not yet used
pub fn group_of(name: &str) -> Option<&'static str> {
    GROUP_OF.get(&name.to_lowercase()).copied()
}

/// Sort key for canonical ordering: known commodities by position, unknown ones
/// last (then alphabetically, for a stable order among unknowns).
pub fn sort_key(name: &str) -> (usize, String) {
    (
        order_index(name).unwrap_or(usize::MAX),
        name.to_lowercase(),
    )
}

/// Potency weight of a rum commodity (the in-game "units of rum" a
/// single item is worth): Swill 2, Grog 3, Fine rum 6; 0 for everything else.
/// A quantity times this weight is comparable across the three rum tiers.
pub fn rum_multiplier(name: &str) -> u64 {
    match () {
        _ if name.eq_ignore_ascii_case("swill") => 2,
        _ if name.eq_ignore_ascii_case("grog") => 3,
        _ if name.eq_ignore_ascii_case("fine rum") => 6,
        _ => 0,
    }
}
