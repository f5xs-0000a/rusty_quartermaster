//! An ocean's colonized islands as yoweb lists them: governor, ruling flag,
//! property tax and exports.
//!
//! The list is fetched at run time - the Map page asks for it when it is
//! opened - through the same puzzlepirates throttle as pirate pages, and is
//! cached per ocean in the cache file. An entry goes stale a week after it
//! was fetched and is refreshed the next time the Map page is opened.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ocean::Ocean,
    ratelimit::{Service, throttled},
};

/// Days before a cached island list is fetched again.
pub const TTL_DAYS: i64 = 7;

/// One colonized island as yoweb describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IslandInfo {
    pub name: String,
    pub governor: Option<String>,
    /// The flag the island is ruled by.
    pub flag: Option<String>,
    /// Property tax, in percent.
    pub property_tax: Option<u8>,
    /// Commodities the island exports, as listed.
    pub exports: Vec<String>,
}

/// One ocean's island list and when it was fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedIslands {
    pub fetched_at: DateTime<Utc>,
    pub islands: Vec<IslandInfo>,
}

impl CachedIslands {
    /// Whether the list is due for a refetch at `now`.
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        Duration::days(TTL_DAYS) <= now.signed_duration_since(self.fetched_at)
    }

    /// An island by (case-insensitive) name.
    pub fn get(&self, name: &str) -> Option<&IslandInfo> {
        self.islands
            .iter()
            .find(|i| i.name.eq_ignore_ascii_case(name))
    }
}

/// Fetch and parse the ocean's island list (`island/info.wm?showAll=true`),
/// paced behind every other puzzlepirates request.
pub async fn fetch_island_list(
    client: &reqwest::Client,
    ocean: Ocean,
) -> Result<CachedIslands, String> {
    let url = format!(
        "{}/island/info.wm?showAll=true",
        ocean.yoweb_base()
    );
    let resp = throttled(Service::PuzzlePirates, || {
        client.get(&url).send()
    })
    .await
    .map_err(|e| format!("failed to fetch island list: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "island list returned HTTP {}",
            resp.status()
        ));
    }
    let html = resp
        .text()
        .await
        .map_err(|e| format!("failed to read island list: {e}"))?;
    let islands = parse_island_list(&html);
    if islands.is_empty() {
        return Err("island list named no islands".to_owned());
    }
    Ok(CachedIslands {
        fetched_at: Utc::now(),
        islands,
    })
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse yoweb's island list. Each island is a block headed by its name in
/// a `<font size="+1">` and followed by `Governor: <a>..</a>`,
/// `Property tax: N%`, `Ruled by <a>..</a>` and `Exports: a , b` lines; a
/// line that is missing just leaves its field empty.
pub fn parse_island_list(html: &str) -> Vec<IslandInfo> {
    html.split("<font size=\"+1\">")
        .skip(1)
        .filter_map(parse_island_block)
        .collect()
}

fn parse_island_block(block: &str) -> Option<IslandInfo> {
    let name = decode(block.split("</font>").next()?.trim());
    if name.is_empty() {
        return None;
    }
    let tax = after(block, "Property tax:")
        .map(|s| s.trim_start())
        .and_then(|s| s.split('%').next())
        .and_then(|digits| digits.trim().parse().ok());
    let exports = after(block, "Exports:")
        .and_then(|s| s.split("<br>").next())
        .map(|s| {
            s.split(',')
                .map(|e| decode(e.trim()))
                .filter(|e| !e.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(IslandInfo {
        name,
        governor: after(block, "Governor:").and_then(link_text),
        flag: after(block, "Ruled by").and_then(link_text),
        property_tax: tax,
        exports,
    })
}

/// The rest of `text` after the first `marker`.
fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.split_once(marker).map(|(_, rest)| rest)
}

/// The text of the first `<a ...>` link in `text`.
fn link_text(text: &str) -> Option<String> {
    let rest = after(text, "<a")?;
    let inner = after(rest, ">")?.split("</a>").next()?;
    let inner = decode(inner.trim());
    (!inner.is_empty()).then_some(inner)
}

/// Undo the few entities yoweb uses in names.
fn decode(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<center><img src="/yoweb/images/header.png"><br>
<center><font size="+1">Foo Island</font><br>
Population: 1,234<br>
Located in the Bar archipelago.<br>
Governor: <a href="/yoweb/pirate.wm?target=Someone">Someone</a><br>
Property tax: 15%<br>
</center> Ruled by <a href="/yoweb/flag/info.wm?flagid=1">Some &amp; Flag</a><br>
Exports: Hemp , Sugar cane , Iron <br><br>
<center><font size="+1">Baz Rock</font><br>
Population: 2<br>
Located in the Bar archipelago.<br>
</center>
Exports: <br><br>"#;

    #[test]
    fn parses_every_block_and_tolerates_missing_lines() {
        let islands = parse_island_list(PAGE);
        assert_eq!(
            islands,
            [
                IslandInfo {
                    name: "Foo Island".to_owned(),
                    governor: Some("Someone".to_owned()),
                    flag: Some("Some & Flag".to_owned()),
                    property_tax: Some(15),
                    exports: vec![
                        "Hemp".to_owned(),
                        "Sugar cane".to_owned(),
                        "Iron".to_owned(),
                    ],
                },
                IslandInfo {
                    name: "Baz Rock".to_owned(),
                    governor: None,
                    flag: None,
                    property_tax: None,
                    exports: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn a_page_without_islands_parses_to_nothing() {
        assert!(
            parse_island_list("<html><body>nothing here</body></html>")
                .is_empty()
        );
    }

    #[test]
    fn the_list_goes_stale_after_a_week() {
        let cached = CachedIslands {
            fetched_at: Utc::now() - Duration::days(TTL_DAYS)
                + Duration::hours(1),
            islands: Vec::new(),
        };
        assert!(!cached.is_stale(Utc::now()));
        assert!(cached.is_stale(Utc::now() + Duration::hours(2)));
    }

    #[test]
    fn lookup_ignores_case() {
        let cached = CachedIslands {
            fetched_at: Utc::now(),
            islands: parse_island_list(PAGE),
        };
        assert_eq!(
            cached.get("foo island").map(|i| i.name.as_str()),
            Some("Foo Island")
        );
        assert!(cached.get("Nowhere").is_none());
    }
}
