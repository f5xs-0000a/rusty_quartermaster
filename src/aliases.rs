use std::collections::HashMap;
use std::sync::LazyLock;

static ALIASES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let entries: &[(&str, &str)] = &[
        ("kb", "kraken's blood"),
        ("scb", "small cannon balls"),
        ("mcb", "medium cannon balls"),
        ("lcb", "large cannon balls"),
        // ("kraken", "kraken ink"),  // example
    ];
    entries.iter().copied().collect()
});

pub fn get() -> &'static HashMap<&'static str, &'static str> {
    &ALIASES
}
