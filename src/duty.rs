//! Duty reports read from the clipboard.
//!
//! The game can put one duty report on the clipboard as JSON: a station per
//! key, and under each the pirates rated at it.
//!
//! ```json
//! {"sail":{"Foo":{"performance":4,"maneuver_tokens":[3,2,2,3,0,0,0]},
//!          "Bar":{"performance":2}},
//!  "gunnery":{"Baz":{"performance":4,"cannons_loaded":43}},
//!  "haul":{"Qux":{"performance":3,"m.treasure_hauled":[3,1,1]}}}
//! ```
//!
//! A report covers one interval, the last league or the time since the last
//! break, so a run produces many of them and each carries that interval's
//! figures rather than a running total. How long the interval ran is not in
//! the report and varies from a fast league leg to a ten-minute board
//! session, so counts can be summed across reports but never turned into
//! rates.
//!
//! Which stations appear is itself information: the game omits a station
//! nobody worked, and which stations can be worked depends on the encounter.
//! Only the station keys seen so far are mapped onto a [`Skill`]; an
//! unrecognized key keeps its spelling instead of failing the parse, so a
//! report from an encounter we have yet to capture still reads.
//!
//! Nothing routes a report into the app yet. This is the parsed model.

use std::{collections::BTreeSet, fmt, marker::PhantomData};

use serde::{
    Deserialize,
    Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::Value;

use crate::pirate::Skill;

/// Slots in a `maneuver_tokens` array. Five carry a [`TokenShape`]; the game
/// reserves the rest. Widen this when it implements another shape.
pub const MANEUVER_SLOTS: usize = 7;

/// Slots in a `treasure_hauled` array, one per [`ChestTier`].
pub const TREASURE_SLOTS: usize = 3;

/// A maneuver token's shape. Tokens are earned only where maneuvers are:
/// blockades, flotillas, and sea monster hunts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenShape {
    Circle,
    Diamond,
    Plus,
    Cross,
    /// Earned only while attacking the Cursed Isles.
    Flower,
}

/// A hauled chest's tier, smallest to largest.
///
/// One object per tier throughout, whatever the encounter calls it: 5, 10 and
/// 15 kg, 2, 4 and 6 L, and a 1x1, 2x2 and 3x2 footprint on a foraging board.
/// Only the names change:
///
/// - Atlantis: sunken box, ancient locker, antediluvian chest
/// - Cursed Isles: bone box, fetish jar, cursed chest
/// - Haunted Seas: ghostly box, ethereal locker, spectral chest
/// - buried treasure: strong box, ship's locker, treasure chest
/// - vampirates: blood box, nocturnal locker, immortal chest
///
/// A report carries only the counts, never the names, so this figure cannot
/// say which encounter produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChestTier {
    Box,
    Locker,
    Chest,
}

/// A pirate's rating at a station, worst to best.
///
/// A rating is relative to the pirate's own standing: an Incredible from an
/// Able puzzler is a smaller contribution than an Incredible from a Legendary
/// one, so a rating ranks a pirate against themselves and never against
/// another pirate. The game also shows greenies one "Learning" in place of
/// both [`Booched`](Self::Booched) and [`Poor`](Self::Poor), so the bottom
/// two are not reliably told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Performance {
    Booched,
    Poor,
    Fine,
    Good,
    Excellent,
    Incredible,
}

/// What a station counted for a pirate, beyond the rating.
///
/// A figure is identified by its key, never by its width. A vampirate
/// expedition rates carpentry on a three-wide array of its own (coffin holes
/// closed with no extra piece, with one, and with two), which is exactly as
/// wide as the chest tiers the same report carries under treasure haul.
#[derive(Debug, Clone, PartialEq)]
pub enum Metric {
    /// Maneuver tokens earned, indexed by [`TokenShape::slot`].
    Maneuvers([u32; MANEUVER_SLOTS]),
    /// Cannons loaded. The report draws none of these for a mercenary, so a
    /// station's loads are a floor and not a count.
    CannonsLoaded(u32),
    /// Chests hauled, indexed by [`ChestTier::slot`].
    TreasureHauled([u32; TREASURE_SLOTS]),
    /// A figure we have no reading for yet, kept whole so a new encounter's
    /// numbers survive the parse and can be studied. Clipboard text: hold it
    /// for diagnosis, never render it.
    Unknown {
        key: String,
        value: Value,
    },
}

/// One pirate's line under a station.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub performance: Performance,
    /// Empty when the station rated the pirate without counting anything,
    /// which is what an omitted figure means: none of it.
    pub metrics: Vec<Metric>,
}

/// One station's lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    /// The key as the report spelled it, kept so a station we cannot name is
    /// still identifiable.
    pub key: String,
    /// The duty that key names, where we recognize it; see [`station_skill`].
    pub skill: Option<Skill>,
    /// The pirates rated here, in the order the report listed them, which is
    /// by descending performance. That order is finer than [`Performance`]:
    /// the game quantizes the word it shows, so two pirates on the same word
    /// are still ranked against each other by their place in the list.
    pub pirates: Vec<Entry>,
}

/// One duty report, as the clipboard carried it.
#[derive(Debug, Clone, PartialEq)]
pub struct DutyReport {
    /// The stations the report listed, in its own order.
    pub stations: Vec<Station>,
}

impl TokenShape {
    /// Where this shape's count sits in a [`Metric::Maneuvers`] array.
    pub fn slot(self) -> usize {
        match self {
            Self::Circle => 0,
            Self::Diamond => 1,
            Self::Plus => 2,
            Self::Cross => 3,
            Self::Flower => 4,
        }
    }
}

impl ChestTier {
    /// Where this tier's count sits in a [`Metric::TreasureHauled`] array.
    pub fn slot(self) -> usize {
        match self {
            Self::Box => 0,
            Self::Locker => 1,
            Self::Chest => 2,
        }
    }
}

impl Performance {
    /// The word the game shows for this rating.
    pub fn label(self) -> &'static str {
        match self {
            Self::Booched => "Booched",
            Self::Poor => "Poor",
            Self::Fine => "Fine",
            Self::Good => "Good",
            Self::Excellent => "Excellent",
            Self::Incredible => "Incredible",
        }
    }

    fn from_rank(rank: u64) -> Option<Self> {
        match rank {
            0 => Some(Self::Booched),
            1 => Some(Self::Poor),
            2 => Some(Self::Fine),
            3 => Some(Self::Good),
            4 => Some(Self::Excellent),
            5 => Some(Self::Incredible),
            _ => None,
        }
    }
}

impl DutyReport {
    /// The report's line-up for one duty, if it listed that station.
    pub fn station(&self, skill: Skill) -> Option<&Station> {
        self.stations.iter().find(|s| s.skill == Some(skill))
    }

    /// Every pirate the report names, once each however many stations rated
    /// them. This is whoever puzzled long enough to be rated during the
    /// interval, which includes anyone who has since left the vessel, so it
    /// is a superset of who is aboard now and not a roster.
    pub fn names(&self) -> BTreeSet<&str> {
        self.stations
            .iter()
            .flat_map(|station| station.pirates.iter())
            .map(|pirate| pirate.name.as_str())
            .collect()
    }
}

/// The duty a report's station key names. `None` for a key we have not seen
/// on a capture yet: the set grows with the encounter (foraging on a Cursed
/// Isles landing or a forage expedition, navigating at a league point), and a
/// station we cannot name is still worth keeping.
pub fn station_skill(key: &str) -> Option<Skill> {
    match key {
        "sail" => Some(Skill::Sailing),
        "rigging" => Some(Skill::Rigging),
        "gunnery" => Some(Skill::Gunning),
        "carpentry" => Some(Skill::Carpentry),
        "patching" => Some(Skill::Patching),
        "bilge" => Some(Skill::Bilging),
        "haul" => Some(Skill::TreasureHaul),
        _ => None,
    }
}

/// Parse the duty report JSON the game copies to the clipboard. `None` for
/// anything that isn't shaped like one.
pub fn parse(text: &str) -> Option<DutyReport> {
    let raw: Ordered<Ordered<Ordered<Value>>> =
        serde_json::from_str(text.trim()).ok()?;
    let mut stations = Vec::with_capacity(raw.0.len());
    let mut rated = 0;
    for (key, listed) in raw.0 {
        let mut pirates = Vec::with_capacity(listed.0.len());
        for (name, fields) in listed.0 {
            pirates.push(entry(name, fields)?);
        }
        rated += pirates.len();
        stations.push(Station {
            skill: station_skill(&key),
            key,
            pirates,
        });
    }
    // An object of empty objects rates nobody, a shape far too much other
    // JSON shares; a report always has somebody to rate.
    (0 < rated).then_some(DutyReport {
        stations,
    })
}

/// One pirate's line. `None` for a line we only half understand, which would
/// be worse to act on than not recognizing the report at all.
fn entry(name: String, fields: Ordered<Value>) -> Option<Entry> {
    let mut performance = None;
    let mut metrics = Vec::new();
    for (key, value) in fields.0 {
        match key.as_str() {
            "performance" => {
                performance = Some(Performance::from_rank(value.as_u64()?)?);
            }
            _ => metrics.push(metric(key, value)?),
        }
    }
    Some(Entry {
        name,
        performance: performance?,
        metrics,
    })
}

/// One counted figure. The exporter prefixes some keys with `m.`, which is no
/// part of the figure's name.
fn metric(key: String, value: Value) -> Option<Metric> {
    let bare = key.strip_prefix("m.").unwrap_or(&key);
    if bare == "maneuver_tokens" {
        return slots(&value).map(Metric::Maneuvers);
    }
    if bare == "treasure_hauled" {
        return slots(&value).map(Metric::TreasureHauled);
    }
    if bare == "cannons_loaded" {
        return count(&value).map(Metric::CannonsLoaded);
    }
    Some(Metric::Unknown {
        key,
        value,
    })
}

/// An array of counts exactly as wide as we expect that figure to be. Any
/// other width means it isn't the figure we take it for, so the report goes
/// unread rather than misread.
fn slots<const N: usize>(value: &Value) -> Option<[u32; N]> {
    let listed = value.as_array()?;
    if listed.len() != N {
        return None;
    }
    let mut counts = [0; N];
    for (slot, raw) in counts.iter_mut().zip(listed) {
        *slot = count(raw)?;
    }
    Some(counts)
}

fn count(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}

/// A JSON object kept in the order it was written. A station ranks its
/// pirates by their place in the list, so that order is data and survives the
/// parse rather than being dropped for a sorted map.
struct Ordered<T>(Vec<(String, T)>);

impl<'de, T> Deserialize<'de> for Ordered<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AsListed<T>(PhantomData<T>);

        impl<'de, T> Visitor<'de> for AsListed<T>
        where
            T: Deserialize<'de>,
        {
            type Value = Ordered<T>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Ordered<T>, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut kept = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(entry) = map.next_entry()? {
                    kept.push(entry);
                }
                Ok(Ordered(kept))
            }
        }

        deserializer.deserialize_map(AsListed(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board session's report: tokens at the sailing stations, loads at the
    /// guns, treasure below. "Test Example" stands in for an NPC, who is
    /// rated like anyone else but has nothing counted. Each station lists its
    /// pirates by descending rating, which here is not their alphabetical
    /// order, so a sorted map would be caught reordering them.
    const REPORT: &str = r#"{
        "sail":{"Foo":{"performance":4,"maneuver_tokens":[3,2,2,3,0,0,0]},
                "Test Example":{"performance":3},
                "Bar":{"performance":1,"maneuver_tokens":[1,1,0,0,0,0,0]}},
        "gunnery":{"Baz":{"performance":4,"cannons_loaded":43}},
        "haul":{"Qux":{"performance":3,"m.treasure_hauled":[3,1,1]},
                "Foo":{"performance":2,"m.treasure_hauled":[5,0,0]}}
    }"#;

    #[test]
    fn reads_every_station_the_report_listed() {
        let report = parse(REPORT).expect("report");
        let listed: Vec<_> = report
            .stations
            .iter()
            .map(|s| (s.key.as_str(), s.skill))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("sail", Some(Skill::Sailing)),
                ("gunnery", Some(Skill::Gunning)),
                ("haul", Some(Skill::TreasureHaul)),
            ]
        );

        let guns = report.station(Skill::Gunning).expect("gunnery");
        assert_eq!(guns.pirates[0].name, "Baz");
        assert_eq!(
            guns.pirates[0].performance,
            Performance::Excellent
        );
        assert_eq!(
            guns.pirates[0].metrics,
            vec![Metric::CannonsLoaded(43)]
        );
    }

    #[test]
    fn a_stations_ranking_survives_the_parse() {
        let report = parse(REPORT).expect("report");
        let sail = report.station(Skill::Sailing).expect("sail");
        let ranked: Vec<_> = sail
            .pirates
            .iter()
            .map(|p| (p.name.as_str(), p.performance))
            .collect();
        assert_eq!(
            ranked,
            vec![
                ("Foo", Performance::Excellent),
                ("Test Example", Performance::Good),
                ("Bar", Performance::Poor),
            ]
        );
    }

    #[test]
    fn a_rated_pirate_who_earned_nothing_counts_nothing() {
        let report = parse(REPORT).expect("report");
        let sail = report.station(Skill::Sailing).expect("sail");
        assert!(sail.pirates[1].metrics.is_empty());
        assert_eq!(
            sail.pirates[0].metrics,
            vec![Metric::Maneuvers([3, 2, 2, 3, 0, 0, 0])]
        );
        let tokens = match &sail.pirates[0].metrics[0] {
            Metric::Maneuvers(tokens) => tokens,
            other => panic!("maneuvers, got {other:?}"),
        };
        assert_eq!(tokens[TokenShape::Cross.slot()], 3);
        assert_eq!(tokens[TokenShape::Flower.slot()], 0);
    }

    #[test]
    fn the_metric_prefix_is_no_part_of_its_name() {
        let prefixed =
            r#"{"haul":{"Foo":{"performance":3,"m.treasure_hauled":[1,2,3]}}}"#;
        let bare =
            r#"{"haul":{"Foo":{"performance":3,"treasure_hauled":[1,2,3]}}}"#;
        let hauled = vec![Metric::TreasureHauled([1, 2, 3])];
        for text in [prefixed, bare] {
            let report = parse(text).expect("report");
            let haul = report.station(Skill::TreasureHaul).expect("haul");
            assert_eq!(haul.pirates[0].metrics, hauled);
            let chests = match &haul.pirates[0].metrics[0] {
                Metric::TreasureHauled(chests) => chests,
                other => panic!("treasure, got {other:?}"),
            };
            assert_eq!(chests[ChestTier::Box.slot()], 1);
            assert_eq!(chests[ChestTier::Chest.slot()], 3);
        }
    }

    #[test]
    fn a_station_we_cannot_name_is_kept_whole() {
        let text = r#"{"forage":{"Foo":{"performance":3,"m.gathered":7}}}"#;
        let report = parse(text).expect("report");
        let station = &report.stations[0];
        assert_eq!(station.key, "forage");
        assert_eq!(station.skill, None);
        assert_eq!(
            station.pirates[0].metrics,
            vec![Metric::Unknown {
                key: "m.gathered".to_owned(),
                value: Value::from(7),
            }]
        );
    }

    #[test]
    fn names_the_roster_once_each() {
        let report = parse(REPORT).expect("report");
        assert_eq!(
            report.names(),
            BTreeSet::from(["Bar", "Baz", "Foo", "Qux", "Test Example"])
        );
    }

    #[test]
    fn rejects_other_text() {
        // a hold, the other thing the clipboard carries
        assert!(parse(r#"{"contents":[{"Foo":84}],"coffers":0}"#).is_none());
        assert!(parse("hello").is_none());
        assert!(parse("{}").is_none());
        assert!(parse(r#"{"sail":{}}"#).is_none());
        // a line with no rating, or a rating off the end of the scale
        assert!(parse(r#"{"sail":{"Foo":{}}}"#).is_none());
        assert!(parse(r#"{"sail":{"Foo":{"performance":6}}}"#).is_none());
        // a figure of a width we don't take it for
        assert!(
            parse(
                r#"{"sail":{"Foo":{"performance":3,
                "maneuver_tokens":[1,1,1,1,1,1]}}}"#
            )
            .is_none()
        );
    }
}
