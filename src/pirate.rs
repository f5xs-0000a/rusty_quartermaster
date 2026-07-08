use std::{collections::HashMap, fmt, str::FromStr};

use chrono::{DateTime, Utc};
use ratatui::style::{Modifier, Style};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};

use crate::{
    ocean::Ocean,
    ratelimit::{Service, throttled},
};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[repr(u8)]
pub enum Standing {
    Able = 0,
    Proficient = 1,
    Distinguished = 2,
    Respected = 3,
    Master = 4,
    Renowned = 5,
    GrandMaster = 6,
    Legendary = 7,
    Ultimate = 8,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[repr(u8)]
pub enum Experience {
    Novice = 0,
    Neophyte = 1,
    Apprentice = 2,
    Narrow = 3,
    Broad = 4,
    Solid = 5,
    Weighty = 6,
    Expert = 7,
    Paragon = 8,
    Illustrious = 9,
    Sublime = 10,
    Revered = 11,
    Exalted = 12,
    Transcendent = 13,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[repr(u8)]
pub enum Fame {
    Aspiring = 0,
    Obscure = 1,
    Rumored = 2,
    Noted = 3,
    Established = 4,
    Renowned = 5,
    Celebrated = 6,
    Eminent = 7,
    Illustrious = 8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Skill {
    // Piracy
    Sailing,
    Rigging,
    Carpentry,
    Patching,
    Bilging,
    Gunning,
    TreasureHaul,
    Navigating,
    BattleNavigation,
    Swordfighting,
    Rumble,
    // Carousing
    Drinking,
    Spades,
    Hearts,
    TreasureDrop,
    Poker,
    // Crafting
    Distilling,
    Alchemistry,
    Shipwrightery,
    Blacksmithing,
    Foraging,
    Weaving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReputationType {
    Conqueror,
    Explorer,
    Patron,
    Magnate,
}

/// The three families a [`Skill`] belongs to, matching how yoweb groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillCategory {
    Piracy,
    Carousing,
    Crafting,
}

/// A pirate's rank within their crew, ordered most → least senior. `Other`
/// preserves anything yoweb shows that we don't recognise (so styling falls
/// back to plain rather than dropping the text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrewRank {
    Captain,
    SeniorOfficer,
    FleetOfficer,
    Officer,
    JobbingPirate,
    CabinPerson,
    Pirate,
    Other(String),
}

/// A pirate's title within their flag. Gendered pairs (King/Queen, …) collapse
/// to one tier; `Other` keeps anything unrecognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagTitle {
    /// King / Queen.
    Royalty,
    /// Prince / Princess.
    Noble,
    /// Lord / Lady.
    Titled,
    Member,
    Other(String),
}

// ---------------------------------------------------------------------------
// FromStr
// ---------------------------------------------------------------------------

impl FromStr for Standing {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Able" => Ok(Self::Able),
            "Proficient" => Ok(Self::Proficient),
            "Distinguished" => Ok(Self::Distinguished),
            "Respected" => Ok(Self::Respected),
            "Master" => Ok(Self::Master),
            "Renowned" => Ok(Self::Renowned),
            "Grand-Master" => Ok(Self::GrandMaster),
            "Legendary" => Ok(Self::Legendary),
            "Ultimate" => Ok(Self::Ultimate),
            _ => Err(format!("unknown standing: {s:?}")),
        }
    }
}

impl FromStr for Experience {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Novice" => Ok(Self::Novice),
            "Neophyte" => Ok(Self::Neophyte),
            "Apprentice" => Ok(Self::Apprentice),
            "Narrow" => Ok(Self::Narrow),
            "Broad" => Ok(Self::Broad),
            "Solid" => Ok(Self::Solid),
            "Weighty" => Ok(Self::Weighty),
            "Expert" => Ok(Self::Expert),
            "Paragon" => Ok(Self::Paragon),
            "Illustrious" => Ok(Self::Illustrious),
            "Sublime" => Ok(Self::Sublime),
            "Revered" => Ok(Self::Revered),
            "Exalted" => Ok(Self::Exalted),
            "Transcendent" => Ok(Self::Transcendent),
            _ => Err(format!("unknown experience: {s:?}")),
        }
    }
}

impl FromStr for Fame {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Aspiring" => Ok(Self::Aspiring),
            "Obscure" => Ok(Self::Obscure),
            "Rumored" => Ok(Self::Rumored),
            "Noted" => Ok(Self::Noted),
            "Established" => Ok(Self::Established),
            "Renowned" => Ok(Self::Renowned),
            "Celebrated" => Ok(Self::Celebrated),
            "Eminent" => Ok(Self::Eminent),
            "Illustrious" => Ok(Self::Illustrious),
            _ => Err(format!("unknown fame: {s:?}")),
        }
    }
}

impl FromStr for Skill {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Sailing" => Ok(Self::Sailing),
            "Rigging" => Ok(Self::Rigging),
            "Carpentry" => Ok(Self::Carpentry),
            "Patching" => Ok(Self::Patching),
            "Bilging" => Ok(Self::Bilging),
            "Gunning" => Ok(Self::Gunning),
            "Treasure Haul" => Ok(Self::TreasureHaul),
            "Navigating" => Ok(Self::Navigating),
            "Battle Navigation" => Ok(Self::BattleNavigation),
            "Swordfighting" => Ok(Self::Swordfighting),
            "Rumble" => Ok(Self::Rumble),
            "Drinking" => Ok(Self::Drinking),
            "Spades" => Ok(Self::Spades),
            "Hearts" => Ok(Self::Hearts),
            "Treasure Drop" => Ok(Self::TreasureDrop),
            "Poker" => Ok(Self::Poker),
            "Distilling" => Ok(Self::Distilling),
            "Alchemistry" => Ok(Self::Alchemistry),
            "Shipwrightery" => Ok(Self::Shipwrightery),
            "Blacksmithing" => Ok(Self::Blacksmithing),
            "Foraging" => Ok(Self::Foraging),
            "Weaving" => Ok(Self::Weaving),
            _ => Err(format!("unknown skill: {s:?}")),
        }
    }
}

impl FromStr for ReputationType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Conqueror" => Ok(Self::Conqueror),
            "Explorer" => Ok(Self::Explorer),
            "Patron" => Ok(Self::Patron),
            "Magnate" => Ok(Self::Magnate),
            _ => {
                Err(format!(
                    "unknown reputation type: {s:?}"
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GrandMaster => f.write_str("Grand-Master"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl fmt::Display for Experience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl fmt::Display for Fame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl fmt::Display for Skill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TreasureHaul => f.write_str("Treasure Haul"),
            Self::Navigating => f.write_str("Duty Navigation"),
            Self::BattleNavigation => f.write_str("Battle Navigation"),
            Self::TreasureDrop => f.write_str("Treasure Drop"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl fmt::Display for ReputationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// Skill / rank / title classification + styling
// ---------------------------------------------------------------------------

impl Skill {
    /// Which family this skill belongs to (Piracy, Carousing, or Crafting),
    /// matching the grouping yoweb renders the skill tables in.
    pub fn category(&self) -> SkillCategory {
        use Skill::*;
        match self {
            Sailing | Rigging | Carpentry | Patching | Bilging | Gunning
            | TreasureHaul | Navigating | BattleNavigation | Swordfighting
            | Rumble => SkillCategory::Piracy,
            Drinking | Spades | Hearts | TreasureDrop | Poker => {
                SkillCategory::Carousing
            }
            Distilling | Alchemistry | Shipwrightery | Blacksmithing
            | Foraging | Weaving => SkillCategory::Crafting,
        }
    }

    /// A compact, column-friendly label for the skill — used as a Top Jobbers
    /// header where the full [`Display`](std::fmt::Display) name is too wide.
    pub fn short_label(&self) -> &'static str {
        use Skill::*;
        match self {
            Sailing => "Sailing",
            Rigging => "Rigging",
            Carpentry => "Carpentry",
            Patching => "Patching",
            Bilging => "Bilging",
            Gunning => "Gunnery",
            TreasureHaul => "T. Haul",
            Navigating => "Navigation",
            BattleNavigation => "B. Navigation",
            Swordfighting => "Swordfight",
            Rumble => "Rumble",
            Drinking => "Drinking",
            Spades => "Spades",
            Hearts => "Hearts",
            TreasureDrop => "T. Drop",
            Poker => "Poker",
            Distilling => "Distilling",
            Alchemistry => "Alchemy",
            Shipwrightery => "Shipwright",
            Blacksmithing => "Smithing",
            Foraging => "Foraging",
            Weaving => "Weaving",
        }
    }

    /// A single-letter marker for the skill, used by merged Top Jobbers columns
    /// to flag which of the column's puzzles a jobber is strongest at (e.g.
    /// `C` for Carpentry, `P` for Patching, `S` for Sailing, `R` for
    /// Rigging).
    pub fn marker(&self) -> char {
        self.short_label().chars().next().unwrap_or('?')
    }
}

impl CrewRank {
    /// Classify a yoweb crew-rank label. Unknown labels are preserved as
    /// [`CrewRank::Other`] rather than discarded.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "Captain" => Self::Captain,
            "Senior Officer" => Self::SeniorOfficer,
            "Fleet Officer" => Self::FleetOfficer,
            "Officer" => Self::Officer,
            "Jobbing Pirate" => Self::JobbingPirate,
            "Cabin Person" => Self::CabinPerson,
            "Pirate" => Self::Pirate,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Emphasis for the rank label, scaling with seniority.
    pub fn style(&self) -> Style {
        match self {
            Self::Captain => {
                Style::default().add_modifier(
                    Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED,
                )
            }
            Self::SeniorOfficer => {
                Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC)
            }
            Self::FleetOfficer => Style::default().add_modifier(Modifier::BOLD),
            Self::Officer => Style::default().add_modifier(Modifier::ITALIC),
            Self::JobbingPirate
            | Self::CabinPerson
            | Self::Pirate
            | Self::Other(_) => Style::default(),
        }
    }
}

impl FlagTitle {
    /// Classify a yoweb flag-title label. Gendered pairs collapse to one tier;
    /// unknown labels are preserved as [`FlagTitle::Other`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "King" | "Queen" => Self::Royalty,
            "Prince" | "Princess" => Self::Noble,
            "Lord" | "Lady" => Self::Titled,
            "Member" => Self::Member,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Emphasis for the title label, scaling with rank.
    pub fn style(&self) -> Style {
        match self {
            Self::Royalty => {
                Style::default().add_modifier(
                    Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED,
                )
            }
            Self::Noble => {
                Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC)
            }
            Self::Titled => Style::default().add_modifier(Modifier::BOLD),
            Self::Member | Self::Other(_) => Style::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRecord {
    pub experience: Experience,
    pub standing: Standing,
    pub archipelago: Option<Standing>,
}

impl SkillRecord {
    pub fn archipelago_standing(&self) -> Standing {
        self.archipelago.unwrap_or(self.standing)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrophySection {
    pub category: String,
    pub trophies: Vec<String>,
}

impl TrophySection {
    pub fn has_trophy(&self, name: &str) -> bool {
        self.trophies.iter().any(|t| t == name)
    }
}

/// A pirate's crew membership, resolved from the raw page fields. A pirate may
/// hold a duty role within the crew (e.g. a fleet officer also titled something
/// in-crew); `role` is `None` when no such middle title is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewAffiliation {
    pub rank: CrewRank,
    pub role: Option<String>,
    pub name: String,
}

/// A pirate's flag membership, resolved from the raw page fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagAffiliation {
    pub title: FlagTitle,
    pub name: String,
}

/// A pirate's basic profile from the `pirate.wm` page: identity, crew/flag
/// affiliation, reputation and skills. Trophies live separately (see
/// [`Trophies`]) because they're on a different page that ages independently.
///
/// The crew/flag are stored as raw strings (as scraped) for cache stability;
/// callers should use [`BasicInfo::crew`]/[`BasicInfo::flag`] for the resolved,
/// optional structured form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicInfo {
    pub name: String,
    pub crew_rank: String,
    /// Optional duty role within the crew, between rank and crew name.
    #[serde(default)]
    pub crew_role: Option<String>,
    pub crew_name: String,
    pub flag_rank: String,
    pub flag_name: String,
    pub reputation: HashMap<ReputationType, Fame>,
    pub skills: HashMap<Skill, SkillRecord>,
}

impl BasicInfo {
    /// The pirate's crew membership, or `None` if they belong to no crew.
    pub fn crew(&self) -> Option<CrewAffiliation> {
        if self.crew_name.trim().is_empty() {
            return None;
        }
        Some(CrewAffiliation {
            rank: CrewRank::from_str(&self.crew_rank),
            role: self
                .crew_role
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned),
            name: self.crew_name.clone(),
        })
    }

    /// The pirate's flag membership, or `None` if they fly no flag.
    pub fn flag(&self) -> Option<FlagAffiliation> {
        if self.flag_name.trim().is_empty() {
            return None;
        }
        Some(FlagAffiliation {
            title: FlagTitle::from_str(&self.flag_rank),
            name: self.flag_name.clone(),
        })
    }
}

/// A pirate's trophies, grouped into the sections yoweb displays them in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trophies {
    pub sections: Vec<TrophySection>,
}

impl Trophies {
    pub fn has_trophy(&self, name: &str) -> bool {
        self.sections.iter().any(|s| s.has_trophy(name))
    }
}

/// A cached pirate plus when each part was last fetched from yoweb. Basic info
/// (the pirate page: crew, flag, skills, reputation) and the trophy list live
/// on separate yoweb pages, so they age independently and carry separate
/// timestamps — letting callers decide how stale each may be before refetching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPirate {
    /// Basic profile (the `pirate.wm` page).
    pub basic: BasicInfo,
    /// Trophies (a separate page).
    #[serde(default)]
    pub trophies: Trophies,
    /// When the basic pirate page was last fetched.
    pub basic_fetched_at: DateTime<Utc>,
    /// When the trophy list was last fetched.
    pub trophies_fetched_at: DateTime<Utc>,
}

/// Which yoweb pages a (re)fetch should pull. The basic page and the trophy
/// page age independently, so a refetch driven by staleness only pulls the
/// part(s) that actually expired; a forced refresh pulls both.
#[derive(Debug, Clone, Copy)]
pub struct FetchPlan {
    pub basic: bool,
    pub trophies: bool,
}

/// The result of a (partial) pirate fetch, ready to be folded into the cache.
pub enum PirateUpdate {
    /// The pages we pulled, each with the instant it was fetched. A part is
    /// `None` when it wasn't in the plan (or, for trophies, when only that part
    /// failed while the basic page succeeded — it stays stale for a later
    /// retry).
    Refreshed {
        basic: Option<(BasicInfo, DateTime<Utc>)>,
        trophies: Option<(Trophies, DateTime<Utc>)>,
    },
    /// The pirate page loaded but named no pirate ("no tell of that pirate"):
    /// the name doesn't exist or is banned, so the entry should be dropped.
    NotFound,
    /// A network or server error — the existing cache should be kept and the
    /// name remains eligible for a later requery.
    Error(String),
}

// ---------------------------------------------------------------------------
// Name normalization
// ---------------------------------------------------------------------------

const SPECIAL_NAMES: &[&str] = &[
    "Mother o' Nyght",
    "Azarbad the Great",
    "Barnabas the Pale",
    "Vargas the Mad",
    "Admiral Finius",
    "Brynhild Skullsplitter",
    "Gretchen Goldfang",
    "Madam Yu Jian",
    "The Widow Queen",
];

/// Whether `name` is a real player-pirate name (as opposed to an NPC such as a
/// swabbie or one of the special characters in [`SPECIAL_NAMES`]).
///
/// Player names match `[a-zA-Z]+(-[a-zA-Z]+)?`: ASCII letters with at most one
/// internal dash, and crucially *no spaces*. NPC names always contain a space —
/// swabbies are either "A swabbie" or named ones like "Tony Ironsides" and
/// "Master Hogan", and special characters look like "Mother o' Nyght" — so the
/// space is the tell, and anything with one is rejected here. Leading/trailing
/// whitespace is ignored; interior whitespace is not.
pub fn is_player_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    // At most one dash, every part non-empty and all ASCII letters. A space (or
    // any non-letter/non-dash byte) lands inside a part and fails the check.
    let lower = trimmed.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('-').collect();
    if parts.len() > 2 {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase())
    })
}

/// Whether `name` is one of the special characters (Brigand Kings, "Mother o'
/// Nyght", ...) in [`SPECIAL_NAMES`], matched case-insensitively against the
/// *whole* name. Callers use this to exclude specials as atomic units rather
/// than breaking them into name segments (their words overlap ordinary ones).
pub fn is_special_name(name: &str) -> bool {
    let trimmed = name.trim();
    SPECIAL_NAMES
        .iter()
        .any(|s| trimmed.eq_ignore_ascii_case(s))
}

/// Normalize a pirate name for use in yoweb URLs.
///
/// Special NPC names are matched case-insensitively and returned as-is.
/// Regular names must be a valid player name ([`is_player_name`]) and are
/// normalized to capitalize the first letter (and the letter after a dash) with
/// everything else lowercased.
pub fn normalize_name(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty pirate name".to_string());
    }

    // Check special names (case-insensitive).
    for &special in SPECIAL_NAMES {
        if trimmed.eq_ignore_ascii_case(special) {
            return Ok(special.to_string());
        }
    }

    // Validate against the player-name pattern.
    if !is_player_name(trimmed) {
        return Err(format!(
            "invalid pirate name: {trimmed:?}"
        ));
    }

    let lower = trimmed.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('-').collect();

    // Capitalize first letter of each part.
    let normalized = parts
        .iter()
        .map(|part| {
            let mut chars = part.chars();
            let first = chars.next().unwrap().to_ascii_uppercase();
            let rest: String = chars.collect();
            format!("{first}{rest}")
        })
        .collect::<Vec<_>>()
        .join("-");

    Ok(normalized)
}

/// URL-encode a pirate name for yoweb (spaces become `+`).
fn url_encode_name(name: &str) -> String {
    name.replace(' ', "+")
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

/// Fetch the parts of a pirate named by `plan` and report what to do with the
/// cache. Each fetched part is timestamped at the moment it lands, so the basic
/// profile and trophies carry independent ages. A found part is returned whole,
/// ready to replace the cached object in one swap.
pub async fn fetch_pirate_update(
    client: &reqwest::Client,
    name: &str,
    ocean: Ocean,
    plan: FetchPlan,
) -> PirateUpdate {
    let normalized = match normalize_name(name) {
        Ok(n) => n,
        Err(e) => return PirateUpdate::Error(e),
    };
    let encoded = url_encode_name(&normalized);
    let yoweb_base = ocean.yoweb_base();

    let mut basic = None;
    if plan.basic {
        match fetch_basic_page(client, &yoweb_base, &encoded).await {
            BasicOutcome::Found(info) => basic = Some((info, Utc::now())),
            BasicOutcome::NotFound => return PirateUpdate::NotFound,
            BasicOutcome::Error(e) => return PirateUpdate::Error(e),
        }
    }

    let mut trophies = None;
    if plan.trophies {
        match fetch_trophy_page(client, &yoweb_base, &encoded).await {
            Ok(t) => trophies = Some((t, Utc::now())),
            // If the basic page already came back fresh, keep it and let the
            // trophies stay stale for a later retry; otherwise it's a plain
            // error.
            Err(e) if basic.is_none() => return PirateUpdate::Error(e),
            Err(_) => {}
        }
    }

    PirateUpdate::Refreshed {
        basic,
        trophies,
    }
}

/// Outcome of fetching just the basic pirate page.
enum BasicOutcome {
    Found(BasicInfo),
    /// HTTP 200 but no pirate named on the page — doesn't exist or is banned.
    NotFound,
    Error(String),
}

/// Fetch and parse the basic pirate page (`pirate.wm`). A successful HTTP
/// response that names no pirate is [`BasicOutcome::NotFound`]; transport or
/// non-2xx responses are [`BasicOutcome::Error`].
async fn fetch_basic_page(
    client: &reqwest::Client,
    yoweb_base: &str,
    encoded: &str,
) -> BasicOutcome {
    let url = format!("{yoweb_base}/pirate.wm?target={encoded}");
    let resp = match throttled(Service::PuzzlePirates, || {
        client.get(&url).send()
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return BasicOutcome::Error(format!(
                "failed to fetch pirate page: {e}"
            ));
        }
    };
    if !resp.status().is_success() {
        return BasicOutcome::Error(format!(
            "pirate page returned HTTP {}",
            resp.status()
        ));
    }
    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            return BasicOutcome::Error(format!(
                "failed to read pirate page: {e}"
            ));
        }
    };
    let info = parse_pirate_page(&html);
    if info.name.is_empty() {
        BasicOutcome::NotFound
    } else {
        BasicOutcome::Found(info)
    }
}

/// Fetch and parse the trophy page. A non-2xx or transport failure is an error;
/// the trophy page has no "no such pirate" state of its own (existence is
/// decided by the basic page).
async fn fetch_trophy_page(
    client: &reqwest::Client,
    yoweb_base: &str,
    encoded: &str,
) -> Result<Trophies, String> {
    let url = format!("{yoweb_base}/trophy/?pirate={encoded}&classic=$classic");
    let resp = throttled(Service::PuzzlePirates, || {
        client.get(&url).send()
    })
    .await
    .map_err(|e| format!("failed to fetch trophy page: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "trophy page returned HTTP {}",
            resp.status()
        ));
    }
    let html = resp
        .text()
        .await
        .map_err(|e| format!("failed to read trophy page: {e}"))?;
    Ok(Trophies {
        sections: parse_trophy_page(&html),
    })
}

// ---------------------------------------------------------------------------
// Pirate page parsing
// ---------------------------------------------------------------------------

fn parse_pirate_page(html: &str) -> BasicInfo {
    let document = Html::parse_document(html);

    let name = parse_name(&document);
    let (crew_rank, crew_role, crew_name) =
        parse_affiliation(&document, "crew-");
    // A flag has only a title + name (no middle role); ignore any stray role.
    let (flag_rank, _flag_role, flag_name) =
        parse_affiliation(&document, "flag-");
    let reputation = parse_reputation(&document);
    let skills = parse_skills(&document);

    BasicInfo {
        name,
        crew_rank,
        crew_role,
        crew_name,
        flag_rank,
        flag_name,
        reputation,
        skills,
    }
}

/// Pirate name: `<font size="+1"><b>NAME</b></font>`
fn parse_name(document: &Html) -> String {
    let sel = Selector::parse(r#"font[size="+1"] > b"#).unwrap();
    document
        .select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .unwrap_or_default()
}

/// Crew/flag: find `<img src="...{prefix}...">`, walk up to the parent `<tr>`,
/// then read the bold runs in order. The first `<b>` is the rank/title and the
/// `<a>` link (inside the last `<b>`) is the crew/flag name. A `<b>` *between*
/// them is a duty role: three bolds → the middle one is the role; two bolds →
/// no role. Returns `(rank, role, name)`; role is `None` when absent.
fn parse_affiliation(
    document: &Html,
    prefix: &str,
) -> (String, Option<String>, String) {
    let img_sel = Selector::parse("img").unwrap();

    for img in document.select(&img_sel) {
        let src = img.value().attr("src").unwrap_or("");
        if !src.contains(prefix) {
            continue;
        }

        let tr = match find_ancestor_tag(&img, "tr") {
            Some(t) => t,
            None => continue,
        };

        let tr_html = tr.inner_html();
        let bolds = extract_bold_texts(&tr_html);
        let rank = bolds.first().cloned().unwrap_or_default();
        let name = extract_link_in_bold(&tr_html);
        // The role is a bold between the rank and the crew/flag name. The name
        // link may or may not itself be bold, so the run count alone is
        // ambiguous: pick the first bold after the rank that isn't the
        // name. That captures both `[rank, role, name]` and `[rank,
        // role]` (name not bold), while `[rank, name]` (no role)
        // correctly yields none.
        let role = bolds
            .iter()
            .skip(1)
            .find(|b| **b != name && !b.is_empty())
            .cloned();

        return (rank, role, name);
    }
    (String::new(), None, String::new())
}

/// Reputation section: `<font color="#0052b5"><b>Reputation</b></font>`,
/// then rows with `<img alt="TYPE" src="...repute-...">` and
/// `<font size="-1">VALUE</font>`.
fn parse_reputation(document: &Html) -> HashMap<ReputationType, Fame> {
    let font_sel = Selector::parse(r##"font[color="#0052b5"]"##).unwrap();
    let table_sel = Selector::parse("table").unwrap();
    let tr_sel = Selector::parse("tr").unwrap();
    let img_sel = Selector::parse("img").unwrap();
    let td_sel = Selector::parse("td").unwrap();

    for font_el in document.select(&font_sel) {
        let text = font_el.text().collect::<String>();
        if text.trim() != "Reputation" {
            continue;
        }

        let parent_td = match find_ancestor_td(&font_el) {
            Some(td) => td,
            None => continue,
        };

        let table = match parent_td.select(&table_sel).next() {
            Some(t) => t,
            None => continue,
        };

        let mut reps = HashMap::new();
        for row in table.select(&tr_sel) {
            let img = match row.select(&img_sel).next() {
                Some(i) => i,
                None => continue,
            };
            let src = img.value().attr("src").unwrap_or("");
            if !src.contains("repute-") {
                continue;
            }
            let alt = match img.value().attr("alt") {
                Some(a) => a,
                None => continue,
            };

            let Ok(rep_type) = alt.parse::<ReputationType>() else {
                continue;
            };

            let tds: Vec<_> = row.select(&td_sel).collect();
            if tds.len() < 2 {
                continue;
            }
            let value_text = tds[1].text().collect::<String>();
            let Ok(fame) = value_text.trim().parse::<Fame>() else {
                continue;
            };

            reps.insert(rep_type, fame);
        }
        return reps;
    }
    HashMap::new()
}

/// Skill sections: `<font color="#0052b5"><b>* Skills</b></font>`,
/// each containing a `<table cellspacing="2">` with skill rows.
fn parse_skills(document: &Html) -> HashMap<Skill, SkillRecord> {
    let font_sel = Selector::parse(r##"font[color="#0052b5"]"##).unwrap();
    let table_sel = Selector::parse("table").unwrap();
    let tr_sel = Selector::parse("tr").unwrap();
    let img_sel = Selector::parse("img").unwrap();
    let td_sel = Selector::parse("td").unwrap();

    let mut skills = HashMap::new();

    for font_el in document.select(&font_sel) {
        let header_text = font_el.text().collect::<String>();
        if !header_text.trim().ends_with("Skills") {
            continue;
        }

        let parent_td = match find_ancestor_td(&font_el) {
            Some(td) => td,
            None => continue,
        };

        let skill_table = parent_td
            .select(&table_sel)
            .find(|t| t.value().attr("cellspacing") == Some("2"));

        let skill_table = match skill_table {
            Some(t) => t,
            None => continue,
        };

        for row in skill_table.select(&tr_sel) {
            let img = match row.select(&img_sel).next() {
                Some(i) => i,
                None => continue,
            };

            let src = img.value().attr("src").unwrap_or("");
            if !src.contains("stat-") {
                continue;
            }

            let alt = match img.value().attr("alt") {
                Some(a) => a,
                None => continue,
            };

            let Ok(skill) = alt.parse::<Skill>() else {
                continue;
            };

            let tds: Vec<_> = row.select(&td_sel).collect();
            if tds.len() < 2 {
                continue;
            }

            let td_html = tds[1].inner_html();
            let record = match parse_skill_record(&td_html) {
                Some(r) => r,
                None => continue,
            };

            skills.insert(skill, record);
        }
    }

    skills
}

/// Parse a `<td>` inner HTML into a `SkillRecord`.
/// Expected: `<font size="-1">EXP/STANDING</font>` and optionally
/// `<font size="-2">(archipelago: STANDING)</font>`.
fn parse_skill_record(td_html: &str) -> Option<SkillRecord> {
    let font_marker = r#"<font size="-1">"#;
    let start = td_html.find(font_marker)? + font_marker.len();
    let end = start + td_html[start ..].find("</font>")?;

    let raw = &td_html[start .. end];
    let text = strip_tags(raw);
    let text = text.trim();

    let (exp_str, stand_str) = text.split_once('/')?;
    let experience = exp_str.trim().parse::<Experience>().ok()?;
    let standing = stand_str.trim().parse::<Standing>().ok()?;

    let archipelago = parse_archipelago_standing(td_html);

    Some(SkillRecord {
        experience,
        standing,
        archipelago,
    })
}

/// Extract the archipelago standing from `<font size="-2">`.
fn parse_archipelago_standing(td_html: &str) -> Option<Standing> {
    let marker = r#"<font size="-2">"#;
    let start = td_html.find(marker)? + marker.len();
    let end = start + td_html[start ..].find("</font>")?;

    let raw = &td_html[start .. end];
    let text = strip_tags(raw).trim().to_string();

    let text = text
        .replace("&#58;", ":")
        .replace("&nbsp;", " ")
        .replace('\u{a0}', " ");

    let text = text.trim_start_matches('(').trim_end_matches(')');
    let (_, value) = text.split_once(':')?;
    value.trim().parse::<Standing>().ok()
}

// ---------------------------------------------------------------------------
// Trophy page parsing
// ---------------------------------------------------------------------------

/// Parse the trophy page into categorised sections.
///
/// Each section is an outer `<table cellspacing="0">` wrapper containing an
/// inner `<table cellspacing="10">` trophy grid.  Named sections also have a
/// `<font size="+1" color="#0052b5">` header; unnamed ones don't.
///
/// We anchor on the trophy grids and look upward for an optional header.
fn parse_trophy_page(html: &str) -> Vec<TrophySection> {
    let document = Html::parse_document(html);
    let grid_sel = Selector::parse(r#"table[cellspacing="10"]"#).unwrap();
    let font_sel =
        Selector::parse(r##"font[size="+1"][color="#0052b5"]"##).unwrap();
    let td_sel =
        Selector::parse(r#"td[align="center"][valign="top"]"#).unwrap();
    let b_sel = Selector::parse("b").unwrap();

    let mut sections = Vec::new();

    for grid in document.select(&grid_sel) {
        // Walk up to the outer wrapper table (cellspacing="0").
        let wrapper = match find_ancestor_tag(&grid, "table") {
            Some(t) => t,
            None => continue,
        };

        // Check if the wrapper has a category header.
        let category = wrapper
            .select(&font_sel)
            .next()
            .map(|f| f.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        // Collect trophy names from the grid.
        let mut trophies = Vec::new();
        for td in grid.select(&td_sel) {
            if let Some(b) = td.select(&b_sel).next() {
                let name = b.text().collect::<String>().trim().to_string();
                if !name.is_empty() {
                    trophies.push(name);
                }
            }
        }

        if !trophies.is_empty() {
            sections.push(TrophySection {
                category,
                trophies,
            });
        }
    }

    sections
}

// ---------------------------------------------------------------------------
// HTML helpers
// ---------------------------------------------------------------------------

fn find_ancestor_td<'a>(el: &ElementRef<'a>) -> Option<ElementRef<'a>> {
    find_ancestor_tag(el, "td")
}

fn find_ancestor_tag<'a>(
    el: &ElementRef<'a>,
    tag: &str,
) -> Option<ElementRef<'a>> {
    let mut node = el.parent()?;
    loop {
        if let Some(element) = ElementRef::wrap(node) {
            if element.value().name() == tag {
                return Some(element);
            }
        }
        node = node.parent()?;
    }
}

/// Collect the text of each `<b>…</b>` run in order, tags stripped. Used to
/// read an affiliation row's rank / role / name bolds positionally.
fn extract_bold_texts(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = html[from ..].find("<b>") {
        let start = from + rel + 3;
        let Some(rel_end) = html[start ..].find("</b>") else {
            break;
        };
        let end = start + rel_end;
        out.push(strip_tags(&html[start .. end]).trim().to_string());
        from = end + 4;
    }
    out
}

fn extract_link_in_bold(html: &str) -> String {
    let mut result = String::new();
    let mut search_from = 0;
    while let Some(a_start) = html[search_from ..].find("<a ") {
        let abs_start = search_from + a_start;
        if let Some(tag_end) = html[abs_start ..].find('>') {
            let text_start = abs_start + tag_end + 1;
            if let Some(a_end) = html[text_start ..].find("</a>") {
                result =
                    html[text_start .. text_start + a_end].trim().to_string();
                search_from = text_start + a_end + 4;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    strip_tags(&result).trim().to_string()
}

fn strip_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_names_are_accepted() {
        assert!(is_player_name("Playerone"));
        assert!(is_player_name("playerone")); // case-insensitive
        assert!(is_player_name("Mary-jane")); // single internal dash
        assert!(is_player_name("  Playerone  ")); // surrounding whitespace ok
    }

    #[test]
    fn npc_names_are_rejected() {
        // Swabbies and special characters all carry a space.
        assert!(!is_player_name("A swabbie"));
        assert!(!is_player_name("Tony Ironsides"));
        assert!(!is_player_name("Master Hogan"));
        assert!(!is_player_name("Mother o' Nyght"));
    }

    #[test]
    fn malformed_names_are_rejected() {
        assert!(!is_player_name("")); // empty
        assert!(!is_player_name("   ")); // whitespace only
        assert!(!is_player_name("-jane")); // empty part
        assert!(!is_player_name("a-b-c")); // too many dashes
        assert!(!is_player_name("Bob123")); // digits
    }

    #[test]
    fn normalize_capitalizes_and_validates() {
        assert_eq!(
            normalize_name("playerONE").unwrap(),
            "Playerone"
        );
        assert_eq!(
            normalize_name("mary-jane").unwrap(),
            "Mary-Jane"
        );
        assert_eq!(
            normalize_name("Mother o' Nyght").unwrap(),
            "Mother o' Nyght"
        );
        assert!(normalize_name("Tony Ironsides").is_err());
    }

    #[test]
    fn crew_rank_classifies_known_and_unknown() {
        assert_eq!(
            CrewRank::from_str("Captain"),
            CrewRank::Captain
        );
        assert_eq!(
            CrewRank::from_str("Senior Officer"),
            CrewRank::SeniorOfficer
        );
        assert_eq!(
            CrewRank::from_str(" Pirate "),
            CrewRank::Pirate
        );
        assert_eq!(
            CrewRank::from_str("Deckhand"),
            CrewRank::Other("Deckhand".to_owned())
        );
    }

    #[test]
    fn flag_title_collapses_gendered_pairs() {
        assert_eq!(
            FlagTitle::from_str("King"),
            FlagTitle::Royalty
        );
        assert_eq!(
            FlagTitle::from_str("Queen"),
            FlagTitle::Royalty
        );
        assert_eq!(
            FlagTitle::from_str("Prince"),
            FlagTitle::Noble
        );
        assert_eq!(
            FlagTitle::from_str("Lady"),
            FlagTitle::Titled
        );
        assert_eq!(
            FlagTitle::from_str("Member"),
            FlagTitle::Member
        );
        assert_eq!(
            FlagTitle::from_str("Founder"),
            FlagTitle::Other("Founder".to_owned())
        );
    }

    #[test]
    fn skill_category_groups_all_skills() {
        assert_eq!(
            Skill::Gunning.category(),
            SkillCategory::Piracy
        );
        assert_eq!(
            Skill::Poker.category(),
            SkillCategory::Carousing
        );
        assert_eq!(
            Skill::Blacksmithing.category(),
            SkillCategory::Crafting
        );
    }

    #[test]
    fn basic_info_crew_and_flag_are_optional() {
        let mut info = BasicInfo {
            name: "Playerone".to_owned(),
            crew_rank: "Captain".to_owned(),
            crew_role: Some("Fleet Officer".to_owned()),
            crew_name: "Some Crew".to_owned(),
            flag_rank: String::new(),
            flag_name: String::new(),
            reputation: HashMap::new(),
            skills: HashMap::new(),
        };

        let crew = info.crew().expect("has a crew");
        assert_eq!(crew.rank, CrewRank::Captain);
        assert_eq!(
            crew.role.as_deref(),
            Some("Fleet Officer")
        );
        assert_eq!(crew.name, "Some Crew");
        // No flag name → no flag.
        assert!(info.flag().is_none());

        // Empty crew name → no crew, even with a stale rank string.
        info.crew_name = String::new();
        assert!(info.crew().is_none());
    }

    // The affiliation rows below mirror the real `pirate.wm` markup (verified
    // against saved sample pages): rank, optional duty role, and the crew/flag
    // name as a link, all inside `<b>` runs within the row's `<font
    // size="-1">`.

    #[test]
    fn parse_affiliation_extracts_rank_role_and_crew() {
        // "Fleet Officer and Strategist of the crew The Example Crew"
        let html = r#"<table><tr valign="middle">
            <td><img src="files/crew-fleet_officer.png"></td>
            <td><font size="-1"><b>Fleet Officer</b> and <b>Strategist</b> of the crew
                <b><a href="https://x/crew?id=1">The Example Crew</a></b></font></td>
        </tr></table>"#;
        let doc = Html::parse_document(html);
        let (rank, role, name) = parse_affiliation(&doc, "crew-");
        assert_eq!(rank, "Fleet Officer");
        assert_eq!(role.as_deref(), Some("Strategist"));
        assert_eq!(name, "The Example Crew");
    }

    #[test]
    fn parse_affiliation_handles_rank_and_crew_without_role() {
        // "Officer of the crew The Other Crew"
        let html = r#"<table><tr valign="middle">
            <td><img src="files/crew-officer.png"></td>
            <td><font size="-1"><b>Officer</b> of the crew
                <b><a href="https://x/crew?id=2">The Other Crew</a></b></font></td>
        </tr></table>"#;
        let doc = Html::parse_document(html);
        let (rank, role, name) = parse_affiliation(&doc, "crew-");
        assert_eq!(rank, "Officer");
        assert_eq!(role, None);
        assert_eq!(name, "The Other Crew");
    }

    #[test]
    fn parse_affiliation_role_when_crew_name_link_not_bold() {
        // Same as the role case but the crew-name link is NOT wrapped in <b>,
        // so the row has only two bold runs — the run count alone can't
        // tell role from name. The non-rank, non-name bold is still the
        // role.
        let html = r#"<table><tr valign="middle">
            <td><img src="files/crew-fleet_officer.png"></td>
            <td><font size="-1"><b>Fleet Officer</b> and <b>Strategist</b> of the crew
                <a href="https://x/crew?id=1">The Example Crew</a></font></td>
        </tr></table>"#;
        let doc = Html::parse_document(html);
        let (rank, role, name) = parse_affiliation(&doc, "crew-");
        assert_eq!(rank, "Fleet Officer");
        assert_eq!(role.as_deref(), Some("Strategist"));
        assert_eq!(name, "The Example Crew");
    }

    #[test]
    fn parse_affiliation_absent_when_no_crew() {
        let html = r#"<table><tr><td>no affiliations here</td></tr></table>"#;
        let doc = Html::parse_document(html);
        let (rank, role, name) = parse_affiliation(&doc, "crew-");
        assert_eq!(rank, "");
        assert_eq!(role, None);
        assert_eq!(name, "");
    }
}
