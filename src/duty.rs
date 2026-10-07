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
//! A copied report belongs to the voyage that was under way when it was
//! copied: it joins that run as a [`CopiedReport`] and reaches disk with it.
//! A run whose only record is a duty report is a run all the same, which is
//! what most voyages that never fight a battle look like. The report also
//! folds the pirates it names into the vessel's roster, for which see
//! `AppShell::take_duty_report`.
//!
//! What the reports counted is read back per pirate over a whole run
//! ([`maneuvers`], [`treasure`]), which is what the Jobbers page's Tokens and
//! Chests box ranks. The ratings are read back per pirate over time instead
//! ([`timelapse`]), which is what the Pirate popup's Duty Timelapse draws: a
//! rating is relative to the pirate's own standing, so it ranks nobody
//! against anybody, and reading one pirate's own run of them is the one
//! reading it carries.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    marker::PhantomData,
};

use chrono::{DateTime, Utc};
use serde::{
    Deserialize,
    Deserializer,
    Serialize,
    de::{MapAccess, Visitor},
};
use serde_json::{Map, Value};

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
/// another pirate.
///
/// The game writes a rank we have no word for, which a capture has shown
/// listed below every rated pirate at its station while the same pirate was
/// rated ordinarily at another. Standings are per-puzzle, and a greenie at
/// one puzzle is shown "Learning" in place of both
/// [`Booched`](Self::Booched) and [`Poor`](Self::Poor), which is the reading
/// that fits; until a capture settles it the rank is kept as
/// [`Unknown`](Self::Unknown) rather than named. A rank is a word or a number
/// we hold onto, never a reason to lose a report.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Performance {
    /// A rank outside the words below, kept as the game wrote it. Ordered
    /// under all of them, which is where the game itself has listed it.
    Unknown(u64),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Maneuver tokens earned, indexed by [`TokenShape::slot`].
    Maneuvers([u32; MANEUVER_SLOTS]),
    /// Cannons loaded. The report draws none of these for a mercenary, so a
    /// station's loads are a floor and not a count.
    CannonsLoaded(u32),
    /// Chests hauled, indexed by [`ChestTier::slot`].
    TreasureHauled([u32; TREASURE_SLOTS]),
    /// A figure we have no reading for yet, kept whole so a new encounter's
    /// numbers survive the parse and can be studied. Clipboard text: it is
    /// held for diagnosis and written to the user's own persistence file,
    /// never rendered.
    Unknown {
        key: String,
        value: Value,
    },
}

/// One pirate's line under a station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub performance: Performance,
    /// Empty when the station rated the pirate without counting anything,
    /// which is what an omitted figure means: none of it. Written only when
    /// something was counted, the way the report itself writes it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Metric>,
}

/// One station's lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Station {
    /// The key as the report spelled it, kept so a station we cannot name is
    /// still identifiable.
    pub key: String,
    /// The pirates rated here, in the order the report listed them, which is
    /// by descending performance. That order is finer than [`Performance`]:
    /// the game quantizes the word it shows, so two pirates on the same word
    /// are still ranked against each other by their place in the list.
    pub pirates: Vec<Entry>,
}

/// A top-level field of a report that is not a station: whatever the game put
/// beside the duties, under its own name and whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extra {
    pub key: String,
    pub value: Value,
}

/// One duty report, as the clipboard carried it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DutyReport {
    /// The stations the report listed, in its own order.
    pub stations: Vec<Station>,
    /// Anything the report carried beside its stations. No capture has shown
    /// one yet; a header, a sequence number or a vessel would land here
    /// instead of costing us the report, and would be waiting to be read when
    /// we noticed it.
    #[serde(default)]
    pub extras: Vec<Extra>,
}

/// A duty report and when it was copied: one interval of one voyage, as it
/// sits on the run and as the file keeps it.
///
/// The report names no vessel, encounter or time of its own, so the copy's
/// time is all that places it. The interval a report covers ends when the
/// player takes the copy, which puts the two within a keystroke of each
/// other, and which voyage it belongs to is whichever one was under way then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CopiedReport {
    pub copied_at: DateTime<Utc>,
    #[serde(default)]
    pub stations: Vec<Station>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<Extra>,
}

/// A copy that was shaped like a duty report but would not read as one, kept
/// verbatim.
///
/// The parse is all-or-nothing by design, so a report carrying one figure we
/// cannot read is lost whole, and the blob is then the only record of the
/// shape that defeated us - which is the shape worth having. The text is
/// written to the user's own persistence file and goes nowhere else: it is
/// clipboard text, so it is never logged and never drawn.
///
/// Unlike a [`CopiedReport`] this joins no voyage. We cannot read it, so we
/// cannot say what it records; it is kept for the reading, not for the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnreadCopy {
    pub copied_at: DateTime<Utc>,
    pub text: String,
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

    /// The shape itself, for a column that counts it. A token is known by its
    /// shape and has no name the game ever shows, so the shape is the heading.
    ///
    /// Each is one column wide in a Latin context and none has an emoji face,
    /// the lozenge standing in for the diamond because the diamonds of the
    /// Geometric Shapes block are drawn double-width by too many fonts.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Circle => "\u{25cb}",
            Self::Diamond => "\u{25ca}",
            Self::Plus => "+",
            Self::Cross => "\u{00d7}",
            Self::Flower => "\u{2740}",
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

    /// The letter a column counting this tier is headed with. The tiers are
    /// named differently by every encounter (see the type's own docs), so a
    /// heading can only be as specific as the initial they share.
    pub fn initial(self) -> &'static str {
        match self {
            Self::Box => "B",
            Self::Locker => "L",
            Self::Chest => "C",
        }
    }
}

impl Performance {
    /// The word the game shows for this rating.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown(_) => "Unrated",
            Self::Booched => "Booched",
            Self::Poor => "Poor",
            Self::Fine => "Fine",
            Self::Good => "Good",
            Self::Excellent => "Excellent",
            Self::Incredible => "Incredible",
        }
    }

    /// The one character a rating is written as where a run of them is read
    /// side by side, as in the Pirate popup's Duty Timelapse.
    ///
    /// The ladder is `b p F G E I`, the rank's own order, and the case is the
    /// ladder: what falls short of Fine is set in lower case and reads as the
    /// smaller mark it is. A rank we have no word for is a question mark,
    /// which is what we have to say about it.
    pub fn mark(self) -> &'static str {
        match self {
            Self::Unknown(_) => "?",
            Self::Booched => "b",
            Self::Poor => "p",
            Self::Fine => "F",
            Self::Good => "G",
            Self::Excellent => "E",
            Self::Incredible => "I",
        }
    }

    /// The rating a rank stands for. A rank we have no word for is the rank
    /// itself: a report is the only record its interval will ever have, and
    /// one line we cannot read the rating of is no reason to lose the rest.
    fn from_rank(rank: u64) -> Self {
        match rank {
            0 => Self::Booched,
            1 => Self::Poor,
            2 => Self::Fine,
            3 => Self::Good,
            4 => Self::Excellent,
            5 => Self::Incredible,
            other => Self::Unknown(other),
        }
    }
}

impl Station {
    /// The duty this station's key names, where we recognize it. Read off the
    /// key rather than stored beside it, so one mapping answers for every
    /// report however it reached us; see [`station_skill`].
    pub fn skill(&self) -> Option<Skill> {
        station_skill(&self.key)
    }
}

impl UnreadCopy {
    /// Keep `text` verbatim under the moment it was copied.
    pub fn new(text: &str, copied_at: DateTime<Utc>) -> Self {
        Self {
            copied_at,
            text: text.to_owned(),
        }
    }

    /// Whether this is the copy `text` is, the time aside.
    pub fn holds(&self, text: &str) -> bool {
        self.text == text
    }
}

impl CopiedReport {
    /// Keep `report` under the moment it was copied.
    pub fn new(report: &DutyReport, copied_at: DateTime<Utc>) -> Self {
        Self {
            copied_at,
            stations: report.stations.clone(),
            extras: report.extras.clone(),
        }
    }

    /// Whether this is the report `report` is. The time is no part of the
    /// answer: a report that reaches the clipboard twice is one report, and
    /// the copy that first brought it is the one that dates it.
    ///
    /// The stations settle it. Whatever an [`Extra`] turns out to hold, we
    /// cannot read it, so we cannot have it decide that one rated interval is
    /// two.
    pub fn holds(&self, report: &DutyReport) -> bool {
        self.stations == report.stations
    }
}

impl DutyReport {
    /// The report's line-up for one duty, if it listed that station.
    pub fn station(&self, skill: Skill) -> Option<&Station> {
        self.stations.iter().find(|s| s.skill() == Some(skill))
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

/// What a run's reports counted for one pirate, of one kind of figure.
///
/// The slots are the figure's own, so a maneuver tally is read with
/// [`TokenShape::slot`] and a haul with [`ChestTier::slot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counted<const N: usize> {
    pub name: String,
    pub counts: [u32; N],
}

impl<const N: usize> Counted<N> {
    /// Every slot together. Each slot counts the same kind of thing, so the
    /// total is what the pirate produced.
    pub fn sum(&self) -> u32 {
        self.counts.iter().sum()
    }
}

/// The maneuver tokens each pirate generated over `reports`, in name order.
///
/// A pirate who generated none is not listed: the figure is what they made,
/// and a row of noughts says only that they were rated at something else.
pub fn maneuvers(reports: &[CopiedReport]) -> Vec<Counted<MANEUVER_SLOTS>> {
    tally(reports, |metric| {
        match metric {
            Metric::Maneuvers(counts) => Some(counts),
            _ => None,
        }
    })
}

/// The chests each pirate hauled over `reports`, in name order. Listed on the
/// same terms as [`maneuvers`]: a pirate who hauled nothing is not.
pub fn treasure(reports: &[CopiedReport]) -> Vec<Counted<TREASURE_SLOTS>> {
    tally(reports, |metric| {
        match metric {
            Metric::TreasureHauled(counts) => Some(counts),
            _ => None,
        }
    })
}

/// Add up one kind of figure across a run's reports, a pirate to an entry.
///
/// One report rates a pirate at as many stations as they worked, and a run
/// hands us a report an interval, so the figures of one pirate arrive spread
/// across both and are summed over the lot. Summing is all that may be done
/// with them: how long an interval ran is nowhere in the report, so no rate
/// can be had from these.
fn tally<const N: usize>(
    reports: &[CopiedReport],
    pick: impl Fn(&Metric) -> Option<&[u32; N]>,
) -> Vec<Counted<N>> {
    let mut totals: BTreeMap<&str, [u32; N]> = BTreeMap::new();
    for station in reports.iter().flat_map(|r| r.stations.iter()) {
        for pirate in &station.pirates {
            for counts in pirate.metrics.iter().filter_map(&pick) {
                let total =
                    totals.entry(pirate.name.as_str()).or_insert([0; N]);
                for (slot, count) in total.iter_mut().zip(counts) {
                    *slot = slot.saturating_add(*count);
                }
            }
        }
    }
    totals
        .into_iter()
        .filter(|(_, counts)| counts.iter().any(|count| 0 < *count))
        .map(|(name, counts)| {
            Counted {
                name: name.to_owned(),
                counts,
            }
        })
        .collect()
}

/// One duty's ratings for one pirate across a run, a column to the report.
///
/// `marks` is as long as the run's reports, `None` where that report did not
/// rate the pirate at this duty - they worked another, or they worked none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DutyRun {
    /// The duty as it is read: its [`duty_tag`] where we know the key, else
    /// the key the report spelled, so a station we cannot name still shows.
    pub label: String,
    pub marks: Vec<Option<Performance>>,
}

/// What a run's reports said about one pirate, duty by duty and report by
/// report: the shape the Duty Timelapse is drawn from.
///
/// A rating is standing-relative, so it ranks a pirate against themselves and
/// no one else. Read over time it does exactly that, which is the one reading
/// it carries: a pirate who booches once against their own bar had a bad
/// interval, and a pirate who booches every interval is telling us something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timelapse {
    /// When each report was copied, in the order they were.
    pub copied: Vec<DateTime<Utc>>,
    /// The duties the pirate was rated at, in the order the reports first
    /// listed them, which is the game's own duty order.
    pub duties: Vec<DutyRun>,
}

/// Read `reports` back as one pirate's run of ratings, or `None` where no
/// report of the run rated them at all.
///
/// Every report is a column, the ones that rated the pirate at nothing
/// included: a column of nothing is the run saying they were not at a station
/// it rated, which is the whole of what an idler looks like from here. What
/// the run never had a report for is no column, so a gap means one thing.
pub fn timelapse(reports: &[CopiedReport], name: &str) -> Option<Timelapse> {
    let mut copied = Vec::with_capacity(reports.len());
    let mut duties: Vec<DutyRun> = Vec::new();
    for report in reports {
        let col = copied.len();
        copied.push(report.copied_at);
        for station in &report.stations {
            let Some(entry) = station
                .pirates
                .iter()
                .find(|pirate| pirate.name.eq_ignore_ascii_case(name))
            else {
                continue;
            };
            let label = station
                .skill()
                .and_then(duty_tag)
                .map_or_else(|| station.key.clone(), str::to_owned);
            let at = match duties.iter().position(|duty| duty.label == label) {
                Some(at) => at,
                None => {
                    duties.push(DutyRun {
                        label,
                        marks: Vec::new(),
                    });
                    duties.len() - 1
                }
            };
            // the first line a station gives a pirate is the one kept, a
            // station listing them twice being a report we cannot read as two
            // ratings of one interval
            if col < duties[at].marks.len() {
                continue;
            }
            duties[at].marks.resize(col, None);
            duties[at].marks.push(Some(entry.performance));
        }
    }
    if duties.is_empty() {
        return None;
    }
    for duty in &mut duties {
        duty.marks.resize(copied.len(), None);
    }
    Some(Timelapse {
        copied,
        duties,
    })
}

/// The duty a report's station key names. `None` for a key we have not seen
/// on a capture yet: the set grows with the encounter (foraging on a Cursed
/// Isles landing or a forage expedition, navigating at a league point), and a
/// station we cannot name is still worth keeping.
/// The short name a duty is read under where its column of them stands beside
/// a run of marks, as in the Duty Timelapse. Navigating is told from battle
/// navigation, the game reporting the two at stations of their own.
///
/// `None` for a skill that is no duty station, which no report can name.
pub fn duty_tag(skill: Skill) -> Option<&'static str> {
    Some(match skill {
        Skill::Sailing => "Sail",
        Skill::Carpentry => "Carp",
        Skill::Rigging => "Rig",
        Skill::Bilging => "Bilge",
        Skill::Patching => "Patch",
        Skill::Gunning => "Gun",
        Skill::TreasureHaul => "THaul",
        Skill::Foraging => "Forage",
        Skill::Navigating => "DNav",
        Skill::BattleNavigation => "BNav",
        _ => return None,
    })
}

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
    let raw: Ordered<TopLevel> = serde_json::from_str(text.trim()).ok()?;
    let mut stations = Vec::with_capacity(raw.0.len());
    let mut extras = Vec::new();
    let mut rated = 0;
    for (key, value) in raw.0 {
        let listed = match value {
            TopLevel::Station(listed) => listed,
            TopLevel::Other(value) => {
                extras.push(Extra {
                    key,
                    value,
                });
                continue;
            }
        };
        let mut pirates = Vec::with_capacity(listed.0.len());
        for (name, fields) in listed.0 {
            pirates.push(entry(name, fields)?);
        }
        rated += pirates.len();
        stations.push(Station {
            key,
            pirates,
        });
    }
    // An object of empty objects rates nobody, a shape far too much other
    // JSON shares; a report always has somebody to rate.
    (0 < rated).then_some(DutyReport {
        stations,
        extras,
    })
}

/// A top-level value of a report: the pirates a station rated, or anything
/// else the game wrote beside the duties.
///
/// A station's line-up is read into [`Ordered`] maps, which keep the order
/// the report wrote them in, because a station ranks its pirates by their
/// place in the list. Anything that is not a map of maps is no station, and
/// is kept as it came rather than taking the report down with it.
#[derive(Deserialize)]
#[serde(untagged)]
enum TopLevel {
    Station(Ordered<Ordered<Value>>),
    Other(Value),
}

/// The pirate names a blob lists, if the blob is shaped like a duty report we
/// failed to read. `None` for anything we have no business keeping.
///
/// This is the net under [`parse`], which rejects a whole report over one
/// figure it cannot read rather than half-understand it. Two of the three
/// tests a blob has to pass are here:
///
/// - **is it JSON, and an object?** Nothing else can be a report.
/// - **are most of its keys stations we know?** Most of its *keys*, not most of
///   the stations there are: a report lists only the stations that were worked,
///   so a run spent bilging is one key long. The test is the one the roster
///   uses for the same kind of question, `2 * known < listed`.
///
/// The third is the caller's, because only the caller knows who we have
/// sailed with: do these names include somebody of ours? Hence the names
/// rather than a yes or no. They are the keys under each station, so a blob
/// whose stations hold something other than a list of pirates yields none and
/// is kept for nothing.
pub fn suspect(text: &str) -> Option<BTreeSet<String>> {
    let listed: Map<String, Value> = serde_json::from_str(text.trim()).ok()?;
    let known = listed
        .keys()
        .filter(|key| station_skill(key).is_some())
        .count();
    if known == 0 || 2 * known < listed.len() {
        return None;
    }
    let names: BTreeSet<String> = listed
        .values()
        .filter_map(Value::as_object)
        .flat_map(Map::keys)
        .cloned()
        .collect();
    (!names.is_empty()).then_some(names)
}

/// One pirate's line. `None` for a line we only half understand, which would
/// be worse to act on than not recognizing the report at all.
fn entry(name: String, fields: Ordered<Value>) -> Option<Entry> {
    let mut performance = None;
    let mut metrics = Vec::new();
    for (key, value) in fields.0 {
        match key.as_str() {
            "performance" => {
                performance = Some(Performance::from_rank(value.as_u64()?));
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
            .map(|s| (s.key.as_str(), s.skill()))
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
        assert_eq!(station.skill(), None);
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

    /// A kept report reads back as the one that was kept. The file is where a
    /// report lives until something reads it, so every figure has to survive
    /// the trip, and the time it was copied has to come back with it.
    #[test]
    fn a_kept_report_reads_back_whole() {
        let report = parse(REPORT).expect("report");
        let copied_at = Utc::now();
        let filed = CopiedReport::new(&report, copied_at);
        let text = serde_json::to_string(&filed).expect("written");
        let read: CopiedReport =
            serde_json::from_str(&text).expect("read back");
        assert_eq!(read, filed);
        assert_eq!(read.copied_at, copied_at);
        assert!(read.holds(&report));
        // a rating is filed as the word it stands for, and a line that
        // counted nothing says nothing about figures
        assert!(text.contains(r#""performance":"excellent""#));
        assert!(!text.contains(r#""metrics":[]"#));
    }

    /// Whatever the report carries that we have no reading for is kept where
    /// it was found: a figure under a pirate, a station we cannot name, and a
    /// field beside the stations that is no station at all. A header of some
    /// kind would cost us the whole report if it were not kept.
    #[test]
    fn what_we_cannot_read_is_kept_where_it_was_found() {
        let text = r#"{"vessel":"Test Vessel","sequence":7,
            "forage":{"Foo":{"performance":3,"m.gathered":[1,2]}}}"#;
        let report = parse(text).expect("report");
        assert_eq!(
            report.extras,
            vec![
                Extra {
                    key: "vessel".to_owned(),
                    value: Value::from("Test Vessel"),
                },
                Extra {
                    key: "sequence".to_owned(),
                    value: Value::from(7),
                },
            ]
        );
        let station = &report.stations[0];
        assert_eq!(station.key, "forage");
        assert_eq!(station.skill(), None);
        assert_eq!(
            station.pirates[0].metrics,
            vec![Metric::Unknown {
                key: "m.gathered".to_owned(),
                value: Value::from(vec![1, 2]),
            }]
        );

        // and all of it survives being kept
        let kept = CopiedReport::new(&report, Utc::now());
        let text = serde_json::to_string(&kept).expect("written");
        let read: CopiedReport =
            serde_json::from_str(&text).expect("read back");
        assert_eq!(read, kept);
    }

    /// A report carrying one figure we cannot read is rejected whole, and
    /// that is the blob worth keeping: it is the only evidence of the shape
    /// that defeated the parse. It passes its names up so the caller can ask
    /// whether the crew is one of ours.
    #[test]
    fn a_report_we_cannot_read_is_still_recognized_as_one() {
        // a maneuver figure of a width we don't take it for
        let text = r#"{"sail":{"Foo":{"performance":3,
            "maneuver_tokens":[1,1,1,1,1,1]},
            "Bar":{"performance":2}},
            "bilge":{"Baz":{"performance":5}}}"#;
        assert!(parse(text).is_none());
        assert_eq!(
            suspect(text),
            Some(BTreeSet::from([
                "Bar".to_owned(),
                "Baz".to_owned(),
                "Foo".to_owned(),
            ]))
        );

        // a figure of a width we don't take it for, and a station we cannot
        // name alongside two we can
        let text = r#"{"sail":{"Foo":{"performance":3,
            "maneuver_tokens":[1,1,1,1,1,1]}},
            "bilge":{"Bar":{"performance":2}},
            "forage":{"Baz":{"performance":2}}}"#;
        assert!(parse(text).is_none());
        assert_eq!(
            suspect(text),
            Some(BTreeSet::from([
                "Bar".to_owned(),
                "Baz".to_owned(),
                "Foo".to_owned(),
            ]))
        );
    }

    /// The net is narrow on purpose: a blob kept for study is clipboard text,
    /// so anything that isn't plainly about duties stays out of the file.
    #[test]
    fn other_json_is_not_kept_for_a_report() {
        // the hold, the other thing the clipboard carries: JSON, an object,
        // and not one key of it a station
        assert_eq!(
            suspect(r#"{"contents":[{"Foo":84}],"coffers":0}"#),
            None
        );
        // one station key is not most of them
        assert_eq!(
            suspect(
                r#"{"sail":{"Foo":{"performance":3}},"width":80,
                "height":24,"theme":"dark"}"#
            ),
            None
        );
        // shaped right, but it rates nobody
        assert_eq!(
            suspect(r#"{"sail":{},"bilge":{}}"#),
            None
        );
        assert_eq!(suspect(r#"{"sail":[1,2,3]}"#), None);
        // not an object, or not JSON at all
        assert_eq!(suspect(r#"["sail","bilge"]"#), None);
        assert_eq!(suspect("hello"), None);
        assert_eq!(suspect(""), None);
    }

    /// A run's figures are what a pirate did over the whole of it, so they
    /// are summed across the reports and across the stations within each —
    /// Foo earns tokens at the sails and hauls below in the same report.
    #[test]
    fn a_runs_figures_are_summed_per_pirate() {
        let report = parse(REPORT).expect("report");
        let at = DateTime::UNIX_EPOCH;
        let run = vec![
            CopiedReport::new(&report, at),
            CopiedReport::new(&report, at),
        ];

        let tokens = maneuvers(&run);
        let listed: Vec<_> = tokens
            .iter()
            .map(|c| (c.name.as_str(), c.counts, c.sum()))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("Bar", [2, 2, 0, 0, 0, 0, 0], 4),
                ("Foo", [6, 4, 4, 6, 0, 0, 0], 20),
            ]
        );

        let hauled = treasure(&run);
        let listed: Vec<_> = hauled
            .iter()
            .map(|c| (c.name.as_str(), c.counts, c.sum()))
            .collect();
        assert_eq!(
            listed,
            vec![("Foo", [10, 0, 0], 10), ("Qux", [6, 2, 2], 10)]
        );
    }

    /// A figure lists whoever produced some of it and nobody else: a pirate
    /// rated at a station that counts something else, or rated with nothing
    /// counted at all, is no part of that figure's reckoning.
    #[test]
    fn only_those_who_made_some_are_counted() {
        let report = parse(REPORT).expect("report");
        let run = vec![CopiedReport::new(&report, DateTime::UNIX_EPOCH)];

        let named = |counted: Vec<Counted<{ MANEUVER_SLOTS }>>| {
            counted.into_iter().map(|c| c.name).collect::<Vec<_>>()
        };
        assert_eq!(
            named(maneuvers(&run)),
            vec!["Bar", "Foo"]
        );
        assert_eq!(
            treasure(&run)
                .into_iter()
                .map(|c| c.name)
                .collect::<Vec<_>>(),
            vec!["Foo", "Qux"]
        );
    }

    /// A rank we have no word for costs us the line's rating and nothing
    /// else: the rest of the station reads, the figures are counted, and the
    /// rank is kept as the game wrote it. A report is the only record its
    /// interval will ever have, so one unreadable rating must not take the
    /// other seventy with it.
    #[test]
    fn a_rank_we_have_no_word_for_keeps_the_report() {
        let report = parse(
            r#"{"carpentry":{"Foo":{"performance":5,
                "maneuver_tokens":[2,0,0,0,0,0,0]},
                "Bar":{"performance":2},
                "Baz":{"performance":6,
                "maneuver_tokens":[0,1,0,0,0,0,0]}}}"#,
        )
        .expect("report");
        let carpentry = report.station(Skill::Carpentry).expect("carpentry");
        let rated: Vec<_> = carpentry
            .pirates
            .iter()
            .map(|p| (p.name.as_str(), p.performance))
            .collect();
        assert_eq!(
            rated,
            vec![
                ("Foo", Performance::Incredible),
                ("Bar", Performance::Fine),
                ("Baz", Performance::Unknown(6)),
            ]
        );
        // The game lists such a line under every rated one, which is where
        // this rating sorts.
        assert!(Performance::Unknown(6) < Performance::Booched);
        // And the figure beside it counts like any other.
        let run = vec![CopiedReport::new(&report, DateTime::UNIX_EPOCH)];
        let tokens = maneuvers(&run);
        assert_eq!(
            tokens
                .iter()
                .map(|c| (c.name.as_str(), c.sum()))
                .collect::<Vec<_>>(),
            vec![("Baz", 1), ("Foo", 2)]
        );
    }

    #[test]
    fn rejects_other_text() {
        // a hold, the other thing the clipboard carries
        assert!(parse(r#"{"contents":[{"Foo":84}],"coffers":0}"#).is_none());
        assert!(parse("hello").is_none());
        assert!(parse("{}").is_none());
        assert!(parse(r#"{"sail":{}}"#).is_none());
        // a line with no rating at all, or one that is no number. A rank we
        // have no word for is another matter: see
        // `a_rank_we_have_no_word_for_keeps_the_report`.
        assert!(parse(r#"{"sail":{"Foo":{}}}"#).is_none());
        assert!(parse(r#"{"sail":{"Foo":{"performance":"Good"}}}"#).is_none());
        // a figure of a width we don't take it for
        assert!(
            parse(
                r#"{"sail":{"Foo":{"performance":3,
                "maneuver_tokens":[1,1,1,1,1,1]}}}"#
            )
            .is_none()
        );
    }

    /// A pirate's run reads a duty to the row and a report to the column, the
    /// rows in the order the reports first listed the stations. Foo sails and
    /// hauls, and an interval spent at only one of the two leaves the other
    /// row saying nothing for it rather than shifting what follows.
    #[test]
    fn a_pirates_run_is_read_duty_by_duty() {
        let at = DateTime::UNIX_EPOCH;
        let sailed_only =
            parse(r#"{"sail":{"Foo":{"performance":0}}}"#).expect("report");
        let run = vec![
            CopiedReport::new(&parse(REPORT).expect("report"), at),
            CopiedReport::new(&sailed_only, at),
        ];

        let read = timelapse(&run, "Foo").expect("timelapse");
        let rows: Vec<_> = read
            .duties
            .iter()
            .map(|duty| {
                (
                    duty.label.as_str(),
                    duty.marks
                        .iter()
                        .map(|mark| mark.map_or(".", Performance::mark))
                        .collect::<String>(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![("Sail", "Eb".to_owned()), ("THaul", "F.".to_owned())]
        );
        assert_eq!(read.copied.len(), 2);
    }

    /// Every report of the run is a column, one that rated the pirate at
    /// nothing included. That column is what an idler looks like from here,
    /// and it reads across every row at once.
    #[test]
    fn a_report_that_rated_them_at_nothing_is_still_a_column() {
        let at = DateTime::UNIX_EPOCH;
        let others =
            parse(r#"{"bilge":{"Bar":{"performance":3}}}"#).expect("report");
        let run = vec![
            CopiedReport::new(&parse(REPORT).expect("report"), at),
            CopiedReport::new(&others, at),
            CopiedReport::new(&parse(REPORT).expect("report"), at),
        ];

        let read = timelapse(&run, "Foo").expect("timelapse");
        let sailing = &read.duties[0];
        assert_eq!(sailing.label, "Sail");
        assert_eq!(
            sailing.marks,
            vec![
                Some(Performance::Excellent),
                None,
                Some(Performance::Excellent),
            ]
        );
        // And a pirate no report of the run rated has no run to read.
        assert_eq!(timelapse(&run, "Nobody"), None);
    }

    /// The marks are the rank's own order, the case carrying the ladder: what
    /// falls short of Fine is set lower.
    #[test]
    fn the_marks_ladder_follows_the_ranks() {
        let ladder = [
            Performance::Booched,
            Performance::Poor,
            Performance::Fine,
            Performance::Good,
            Performance::Excellent,
            Performance::Incredible,
        ];
        assert!(ladder.is_sorted());
        assert_eq!(
            ladder.map(Performance::mark).concat(),
            "bpFGEI"
        );
        assert_eq!(Performance::Unknown(6).mark(), "?");
    }
}
