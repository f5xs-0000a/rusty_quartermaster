//! Authoritative commodity ordering.
//!
//! This is *our* source of truth for how commodities are grouped and ordered —
//! seeded from the in-game canonical order. Market supplies live data (ids,
//! prices); we own the display order. Where our list drifts from the game we
//! nudge it here.
//!
//! Names match Market's exact spelling (including its lowercase quirks like
//! "fine brown cloth"); lookups are case-insensitive. A commodity Market
//! returns that isn't listed here sorts *last* — that's the signal to come add
//! it to the right place below.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Each group, in canonical order, with its commodities in canonical order.
pub const ORDER: &[(&str, &[&str])] = &[
    (
        "Basic commodities",
        &[
            "Sugar cane",
            "Hemp",
            "Iron",
            "Wood",
            "Stone",
            "Hemp oil",
            "Varnish",
            "Lacquer",
            "Kraken's ink",
        ],
    ),
    (
        "Ship supplies",
        &[
            "Swill",
            "Grog",
            "Fine rum",
            "Rum spice",
            "Small cannon balls",
            "Medium cannon balls",
            "Large cannon balls",
            "Lifeboats",
        ],
    ),
    (
        "Herbs",
        &[
            "Madder",
            "Old man's beard",
            "Yarrow",
            "Sassafras",
            "Iris root",
            "Weld",
            "Broom flower",
            "Lobelia",
            "Pokeweed berries",
            "Indigo",
            "Elderberries",
            "Cowslip",
            "Lily of the valley",
            "Nettle",
            "Butterfly weed",
            "Allspice",
        ],
    ),
    (
        "Minerals",
        &[
            "Lorandite",
            "Leushite",
            "Tellurium",
            "Thorianite",
            "Chalcocite",
            "Cubanite",
            "Serandite",
            "Papagoite",
            "Sincosite",
            "Masuyite",
            "Gold nuggets",
        ],
    ),
    (
        "Cloth",
        &[
            "Red cloth",
            "Fine red cloth",
            "Tan cloth",
            "Fine tan cloth",
            "White cloth",
            "Fine white cloth",
            "Black cloth",
            "Fine black cloth",
            "Grey cloth",
            "Fine grey cloth",
            "Yellow cloth",
            "Fine yellow cloth",
            "Pink cloth",
            "Fine pink cloth",
            "Violet cloth",
            "Fine violet cloth",
            "Purple cloth",
            "Fine purple cloth",
            "Navy cloth",
            "Fine navy cloth",
            "Blue cloth",
            "Fine blue cloth",
            "Aqua cloth",
            "Fine aqua cloth",
            "Lime cloth",
            "Fine lime cloth",
            "Green cloth",
            "Fine green cloth",
            "Orange cloth",
            "Fine orange cloth",
            "Maroon cloth",
            "Fine maroon cloth",
            "Brown cloth",
            "Fine brown cloth",
            "Gold cloth",
            "Fine gold cloth",
            "Rose cloth",
            "Fine rose cloth",
            "Lavender cloth",
            "Fine lavender cloth",
            "Mint cloth",
            "Fine mint cloth",
            "Light green cloth",
            "Fine light green cloth",
            "Sail cloth",
            "Magenta cloth",
            "Fine magenta cloth",
            "Lemon cloth",
            "Fine lemon cloth",
            "Peach cloth",
            "Fine peach cloth",
            "Light blue cloth",
            "Fine light blue cloth",
            "Persimmon cloth",
            "Fine persimmon cloth",
        ],
    ),
    (
        // Lime/Navy dye lead the group (game order); they were absent from the
        // canonical list we were given, nudged to their in-game position.
        "Dye",
        &[
            "Lime dye",
            "Navy dye",
            "Red dye",
            "Kraken's blood",
            "Yellow dye",
            "Blue dye",
            "Green dye",
        ],
    ),
    (
        // The last 12 (Ice blue … Pumpkin) aren't in Market's data yet; kept
        // here so they land correctly if/when Market adds them.
        "Paint",
        &[
            "Red paint",
            "Tan paint",
            "White paint",
            "Black paint",
            "Grey paint",
            "Yellow paint",
            "Pink paint",
            "Violet paint",
            "Purple paint",
            "Navy paint",
            "Blue paint",
            "Aqua paint",
            "Lime paint",
            "Green paint",
            "Orange paint",
            "Maroon paint",
            "Brown paint",
            "Gold paint",
            "Rose paint",
            "Lavender paint",
            "Mint paint",
            "Light green paint",
            "Magenta paint",
            "Lemon paint",
            "Peach paint",
            "Light blue paint",
            "Persimmon paint",
            "Ice blue paint",
            "Spring green paint",
            "Banana paint",
            "Wine paint",
            "Plum paint",
            "Chocolate paint",
            "Sea green paint",
            "Emerald paint",
            "Hot pink paint",
            "Cranberry paint",
            "Periwinkle paint",
            "Pumpkin paint",
        ],
    ),
    (
        "Enamel",
        &[
            "Red enamel",
            "Orange enamel",
            "Yellow enamel",
            "Green enamel",
            "Blue enamel",
            "Purple enamel",
            "White enamel",
            "Black enamel",
            "Tan enamel",
            "Grey enamel",
            "Pink enamel",
            "Violet enamel",
            "Navy enamel",
            "Aqua enamel",
            "Lime enamel",
            "Maroon enamel",
            "Brown enamel",
            "Gold enamel",
            "Rose enamel",
            "Lavender enamel",
            "Mint enamel",
            "Light green enamel",
            "Magenta enamel",
            "Lemon enamel",
            "Peach enamel",
            "Light blue enamel",
            "Persimmon enamel",
        ],
    ),
    (
        // Gems precede fruits (game order). "Topaz gems" was stranded after the
        // fruits in Market's ids; moved up to finish the alphabetical run.
        "Forageables",
        &[
            "Diamonds",
            "Emeralds",
            "Moonstones",
            "Opals",
            "Pearls",
            "Rubies",
            "Sapphires",
            "Topazes",
            "Amber gems",
            "Amethyst gems",
            "Beryl gems",
            "Coral gems",
            "Jade gems",
            "Jasper gems",
            "Jet gems",
            "Lapis lazuli gems",
            "Quartz gems",
            "Tigereye gems",
            "Topaz gems",
            "Bananas",
            "Carambolas",
            "Coconuts",
            "Durians",
            "Limes",
            "Mangos",
            "Passion fruit",
            "Pineapples",
            "Pomegranates",
            "Rambutan",
        ],
    ),
];

/// Lowercased name → its flat position in the canonical order.
static INDEX: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    let mut i = 0;
    for (_group, names) in ORDER {
        for name in *names {
            map.insert(name.to_lowercase(), i);
            i += 1;
        }
    }
    map
});

/// Lowercased name → its group label.
static GROUP_OF: LazyLock<HashMap<String, &'static str>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for (group, names) in ORDER {
        for name in *names {
            map.insert(name.to_lowercase(), *group);
        }
    }
    map
});

/// Canonical position of a commodity, or `None` if it isn't in our list.
pub fn order_index(name: &str) -> Option<usize> {
    INDEX.get(&name.to_lowercase()).copied()
}

/// The group a commodity belongs to, or `None` if it isn't in our list.
pub fn group_of(name: &str) -> Option<&'static str> {
    GROUP_OF.get(&name.to_lowercase()).copied()
}

/// Sort key for canonical ordering: known commodities by position, unknown ones
/// last (then alphabetically, for a stable order among unknowns).
pub fn sort_key(name: &str) -> (usize, String) {
    (order_index(name).unwrap_or(usize::MAX), name.to_lowercase())
}
