use std::{collections::HashMap, sync::LazyLock};

static ALIASES: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        let entries: &[(&str, &str)] = &[
            ("kb", "kraken's blood"),
            ("scb", "small cannon balls"),
            ("mcb", "medium cannon balls"),
            ("lcb", "large cannon balls"),
            // ("kraken", "kraken ink"),  // example
        ];
        entries.iter().copied().collect()
    });

static ISLAND_ALIASES: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        let entries: &[(&str, &str)] = &[
            ("addy", "admiral island"),
            ("aim", "aimuari island"),
            ("scrim", "scrimshaw island"),
        ];
        entries.iter().copied().collect()
    });

pub fn get() -> &'static HashMap<&'static str, &'static str> {
    &ALIASES
}

pub fn get_islands() -> &'static HashMap<&'static str, &'static str> {
    &ISLAND_ALIASES
}
