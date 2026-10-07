//! The "Vessel's Jobbers" page: pick a vessel on the left, see its jobbers'
//! skills and activity on the right.
//!
//! Two pieces of state feed this page:
//!   * [`crate::chatlog::GameState`] — per-vessel crew/greedy/plank tallies
//!     from the chat log (wiped on relog).
//!   * [`PirateCache`] — yoweb stats fetched per pirate name. This is *global*:
//!     a pirate's stats are intrinsic to them, not to any vessel, and they
//!     don't change on relog, so the cache is never wiped.
//!
//! [`JobbersUi`] holds the view state (selected vessel + per-list scroll).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Duration, Utc};
use ratatui::{
    prelude::*,
    widgets::{
        Block,
        Borders,
        Clear,
        HighlightSpacing,
        List,
        ListItem,
        ListState,
        Padding,
        Paragraph,
        Wrap,
    },
};

use crate::{
    chatlog::{
        GameState,
        LAIR_WAVE_GROWTH,
        LAIR_WAVE_HI,
        LAIR_WAVE_LO,
        Vessel,
        WaveKind,
        WaveRecord,
        island_wave_band,
        vargas_in_wave,
        wave_kind_for,
    },
    clickmap::{ClickMap, ClickRegion, ClickTarget},
    pirate::{
        self,
        BasicInfo,
        CachedPirate,
        CrewRank,
        Experience,
        FetchPlan,
        PirateUpdate,
        Skill,
        SkillRecord,
        Standing,
        TrophySection,
    },
    ships::{SHIPS, Ship},
    utils::{offset_title, offset_title_width, text_similarity, wrap_words},
    voyage::{AxisMode, ui::fight_chart_lines},
};

/// Width of the `EEE/SSS` experience/standing code.
const CODE_LEN: usize = 7;
/// Spaces between a jobber's name and their code in the Top Jobbers panel.
const NAME_CODE_GAP: usize = 2;
/// Spaces between Top Jobbers skill columns.
const COLUMN_GAP: u16 = 3;
/// Indent on each pirate name under the Aboard pane's "Pirates (n):" header.
const ABOARD_INDENT: usize = 2;

/// Label on the Vampirates "View Skill Distribution" button.
const SKILL_DIST_BUTTON_LABEL: &str = "View Skill Distribution";

/// Label on the Cursed Isles "Show Per-Fight Statistics" button (currently
/// inert — a placeholder for a future per-fight breakdown popup).
const PER_FIGHT_BUTTON_LABEL: &str = "[ Show Per-Fight Statistics ]";

/// Warning shown at the bottom of the app while the Unpoison button is focused.
pub const UNPOISON_TOOLTIP: [&str; 2] = [
    "Ye left the ship, so we might have missed something important.",
    "Press Enter to pay it no mind.",
];

/// A Top Jobbers column: the skill(s) it ranks aboard jobbers by, plus an
/// optional explicit header. A single-skill column is the common case; a column
/// with several skills (e.g. Sail+Rig) ranks each jobber by their *best* of
/// those skills and flags which one with a marker letter — see
/// [`rank_columns`].
pub struct JobberColumn {
    /// Header text; `None` derives it from the lone skill's short label.
    label: Option<&'static str>,
    /// The skills this column considers, in tie-irrelevant order; never empty.
    skills: &'static [Skill],
}

impl JobberColumn {
    /// The column header: the explicit label, else the single skill's short
    /// label.
    fn header(&self) -> &'static str {
        match self.label {
            Some(l) => l,
            None => self.skills[0].short_label(),
        }
    }

    /// Whether this column merges several skills (so its rows carry a marker).
    fn merged(&self) -> bool {
        self.skills.len() > 1
    }
}

/// Top Jobbers columns for a Pillage: the merged Sail+Rig station
/// (Sailing/Rigging), then gunners, navigators, and battle navigators. Other
/// voyage types prioritise different skills — see [`VoyageType::top_jobbers`].
const PILLAGE_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn {
        label: Some("Sail+Rig"),
        skills: &[Skill::Sailing, Skill::Rigging],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Gunning],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Navigating],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::BattleNavigation],
    },
];

/// Top Jobbers columns for an Atlantis run: treasure haulers, gunners, battle
/// navigators, and carpenters. The Haunted Seas and a flotilla are crewed from
/// the same stations.
const ATLANTIS_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn {
        label: None,
        skills: &[Skill::TreasureHaul],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Gunning],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::BattleNavigation],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Carpentry],
    },
];

/// Top Jobbers columns for a blockade: an Atlantis run's without the treasure
/// haulers, there being no booty to carry off a blockade.
const BLOCKADE_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn {
        label: None,
        skills: &[Skill::Gunning],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::BattleNavigation],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Carpentry],
    },
];

/// Top Jobbers columns for a Cursed Isles run: foragers and battle navigators,
/// the two merged station columns Sail+Rig (Sailing/Rigging) and Carp+Patch
/// (Carpentry/Patching), and bilgers.
const CURSED_ISLES_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn {
        label: None,
        skills: &[Skill::Foraging],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::BattleNavigation],
    },
    JobberColumn {
        label: Some("Sail+Rig"),
        skills: &[Skill::Sailing, Skill::Rigging],
    },
    JobberColumn {
        label: Some("Carp+Patch"),
        skills: &[Skill::Carpentry, Skill::Patching],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Bilging],
    },
];

/// Top Jobbers columns for a Vampirates run: treasure haulers, carpenters, and
/// swordfighters. Unlike the other voyage types this list is the headline (it
/// grows to fill the page — see [`VoyageType::top_jobbers_fills`]).
const VAMPIRATES_TOP_JOBBERS: &[JobberColumn] = &[
    JobberColumn {
        label: None,
        skills: &[Skill::TreasureHaul],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Carpentry],
    },
    JobberColumn {
        label: None,
        skills: &[Skill::Swordfighting],
    },
];

/// Top Jobbers columns for a Vikings run: gunners only. It sits beside the
/// Planked pane rather than above it — see
/// [`VoyageType::panes_beside_top_jobbers`].
const VIKINGS_TOP_JOBBERS: &[JobberColumn] = &[JobberColumn {
    label: None,
    skills: &[Skill::Gunning],
}];

/// Skills in each family, in yoweb display order. The pirate-stats popup
/// renders one table per family using these.
const PIRACY_SKILLS: &[Skill] = &[
    Skill::Sailing,
    Skill::Rigging,
    Skill::Carpentry,
    Skill::Patching,
    Skill::Bilging,
    Skill::Gunning,
    Skill::TreasureHaul,
    Skill::Navigating,
    Skill::BattleNavigation,
    Skill::Swordfighting,
    Skill::Rumble,
];
const CRAFTING_SKILLS: &[Skill] = &[
    Skill::Distilling,
    Skill::Alchemistry,
    Skill::Shipwrightery,
    Skill::Blacksmithing,
    Skill::Foraging,
    Skill::Weaving,
];
const CAROUSING_SKILLS: &[Skill] = &[
    Skill::Drinking,
    Skill::Spades,
    Skill::Hearts,
    Skill::TreasureDrop,
    Skill::Poker,
];

/// The kind of voyage being crewed. Each type drives its own Top Jobbers
/// columns ([`VoyageType::top_jobbers`]) and bottom panes
/// ([`VoyageType::panes`]), so the enum can grow without disturbing the
/// existing data model; a type with neither of its own falls back to a "coming
/// soon" placeholder ([`VoyageType::implemented`]).
///
/// Which of them the chat log can tell apart is a separate matter: a dragoon
/// boarding says Atlantis, a lair says Vampirates, and the rest are the
/// quartermaster's own word, chosen in the picker.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoyageType {
    #[default]
    Pillage,
    Vampirates,
    Vikings,
    Flotilla,
    Blockade,
    Atlantis,
    HauntedSeas,
    CursedIsles,
}

/// Every voyage type, in picker order.
pub const VOYAGE_TYPES: &[VoyageType] = &[
    VoyageType::Pillage,
    VoyageType::Vampirates,
    VoyageType::Vikings,
    VoyageType::Flotilla,
    VoyageType::Blockade,
    VoyageType::Atlantis,
    VoyageType::HauntedSeas,
    VoyageType::CursedIsles,
];

impl VoyageType {
    /// Display name shown in the Voyage box and its picker.
    pub fn name(self) -> &'static str {
        match self {
            VoyageType::Pillage => "Pillage",
            VoyageType::Vampirates => "Vampirates",
            VoyageType::Vikings => "Vikings",
            VoyageType::Flotilla => "Flotilla",
            VoyageType::Blockade => "Blockade",
            VoyageType::Atlantis => "Atlantis",
            VoyageType::HauntedSeas => "Haunted Seas",
            VoyageType::CursedIsles => "Cursed Isles",
        }
    }

    /// Whether the full jobbers layout (Top Jobbers + the panes) is wired up
    /// for this voyage type. Every one of them is, so far; a type added
    /// without columns and panes of its own draws the placeholder instead, and
    /// is said to be coming in the picker.
    pub fn implemented(self) -> bool {
        matches!(
            self,
            VoyageType::Pillage
                | VoyageType::Vampirates
                | VoyageType::Vikings
                | VoyageType::Flotilla
                | VoyageType::Blockade
                | VoyageType::Atlantis
                | VoyageType::HauntedSeas
                | VoyageType::CursedIsles
        )
    }

    /// The Top Jobbers columns this voyage type ranks, in display order. Each
    /// is one or more skills (merged columns rank by a jobber's best of
    /// them); new voyage types override this with the columns they need.
    pub fn top_jobbers(self) -> &'static [JobberColumn] {
        match self {
            VoyageType::Pillage => PILLAGE_TOP_JOBBERS,
            VoyageType::Vampirates => VAMPIRATES_TOP_JOBBERS,
            VoyageType::Vikings => VIKINGS_TOP_JOBBERS,
            VoyageType::Blockade => BLOCKADE_TOP_JOBBERS,
            VoyageType::Flotilla
            | VoyageType::Atlantis
            | VoyageType::HauntedSeas => ATLANTIS_TOP_JOBBERS,
            VoyageType::CursedIsles => CURSED_ISLES_TOP_JOBBERS,
        }
    }

    /// The bottom panes this voyage type shows, in left-to-right order. Pillage
    /// gets all three; Atlantis and Cursed Isles drop Greedy. Unimplemented
    /// types get none (they render the "coming soon" placeholder instead).
    pub fn panes(self) -> &'static [JobberPane] {
        match self {
            VoyageType::Pillage => PILLAGE_PANES,
            VoyageType::Vampirates => VAMPIRATES_PANES,
            VoyageType::Vikings => VIKINGS_PANES,
            VoyageType::Flotilla
            | VoyageType::Blockade
            | VoyageType::Atlantis
            | VoyageType::HauntedSeas => ATLANTIS_PANES,
            VoyageType::CursedIsles => CURSED_ISLES_PANES,
        }
    }

    /// Whether the panes sit *beside* the Top Jobbers list (a horizontal split)
    /// rather than below it. Vikings does this: its single Gunnery column
    /// shares the row with the Planked pane.
    pub fn panes_beside_top_jobbers(self) -> bool {
        matches!(self, VoyageType::Vikings)
    }

    /// Whether this voyage type shows the Vikings Statistics box (a
    /// Gunnery-standing breakdown of the crew aboard). Only Vikings does.
    pub fn tracks_vikings(self) -> bool {
        matches!(self, VoyageType::Vikings)
    }

    /// Whether this voyage type fights vampirates (so the Vampirates Stats box
    /// shows). Only Vampirates does.
    pub fn tracks_vampirates(self) -> bool {
        matches!(self, VoyageType::Vampirates)
    }

    /// Whether this voyage type is the Atlantis one, whose layout stands its
    /// panes one over the other to make room for the Tokens and Chests box.
    /// Only Atlantis does; see [`boards`] for the rest of what the box waits
    /// on.
    pub fn tracks_atlantis(self) -> bool {
        matches!(self, VoyageType::Atlantis)
    }

    /// Whether this voyage type runs the Cursed Isles tracking (the Enthralled
    /// leaderboard in place of Aboard, plus the Fight Statistics box). Only
    /// Cursed Isles does.
    pub fn tracks_cursed_isles(self) -> bool {
        matches!(self, VoyageType::CursedIsles)
    }

    /// Whether to show the "View Skill Distribution" button (the Treasure Haul
    /// × Carpentry scatterplot). Only Vampirates, whose axes those skills
    /// are.
    pub fn has_skill_distribution(self) -> bool {
        matches!(self, VoyageType::Vampirates)
    }
}

/// Bottom panes for a Pillage: who's aboard, who's been greedy, who we planked.
const PILLAGE_PANES: &[JobberPane] =
    &[JobberPane::Aboard, JobberPane::Greedy, JobberPane::Planked];

/// Bottom panes for an Atlantis run: aboard and planked, no greedy tally. The
/// Haunted Seas, a blockade and a flotilla are crewed the same way, none of
/// them having greedy brigands to bash.
const ATLANTIS_PANES: &[JobberPane] =
    &[JobberPane::Aboard, JobberPane::Planked];

/// Bottom panes for a Cursed Isles run: the Enthralled leaderboard (replacing
/// the usual Aboard list) beside Planked.
const CURSED_ISLES_PANES: &[JobberPane] =
    &[JobberPane::Enthralled, JobberPane::Planked];

/// Bottom panes for a Vampirates run: aboard and planked, pinned short below
/// the headline Top Jobbers list (see [`VoyageType::top_jobbers_fills`]).
const VAMPIRATES_PANES: &[JobberPane] =
    &[JobberPane::Aboard, JobberPane::Planked];

/// Bottom panes for a Vikings run: just Planked, sat beside the Gunnery Top
/// Jobbers list (see [`VoyageType::panes_beside_top_jobbers`]).
const VIKINGS_PANES: &[JobberPane] = &[JobberPane::Planked];

/// A pirate pane along the bottom of the layout. Which panes show is
/// voyage-type dependent ([`VoyageType::panes`]); Pillage shows all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobberPane {
    Aboard,
    Greedy,
    Planked,
    /// Cursed Isles only: a leaderboard of who enthralled the most zombies,
    /// shown in place of the Aboard list. Rows are `name  alive/total`,
    /// ranked by total.
    Enthralled,
}

// ---------------------------------------------------------------------------
// Global pirate stat cache
// ---------------------------------------------------------------------------

/// One yoweb page for a pirate. The background scheduler fetches at most one
/// page at a time, basic before trophies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiratePage {
    Basic,
    Trophies,
}

/// The single most important page to fetch right now, with the priority tier it
/// came from (lower = more urgent: 0 on-demand, 1 aboard, 2 planked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOrder {
    pub norm: String,
    pub page: PiratePage,
    pub tier: u8,
}

/// Yoweb stats fetched per pirate, keyed by normalized name, plus bookkeeping
/// so the background fetcher knows what's stale and what's been requested.
#[derive(Default)]
pub struct PirateCache {
    /// Successfully fetched pirates, keyed by normalized name. Each entry
    /// carries its basic profile, trophies, and per-part fetch timestamps.
    pub fetched: HashMap<String, CachedPirate>,
    /// Normalized names confirmed not to exist on yoweb this session (the page
    /// loaded but named no pirate). Never auto-fetched again; a force-requery
    /// clears the name so it can be retried.
    pub gone: HashSet<String>,
    /// Normalized names the user explicitly requested (clicked) — fetched at
    /// top priority and in full (both pages), ignoring staleness. Cleared
    /// once the trophies page (the last in the sequence) lands.
    pub forced: HashSet<String>,
}

impl PirateCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a pirate's basic profile by (un-normalized) name.
    pub fn get(&self, name: &str) -> Option<&BasicInfo> {
        self.get_cached(name).map(|c| &c.basic)
    }

    /// Look up a cached pirate, including trophies and fetch timestamps.
    pub fn get_cached(&self, name: &str) -> Option<&CachedPirate> {
        pirate::normalize_name(name)
            .ok()
            .and_then(|n| self.fetched.get(&n))
    }

    /// Queue `name` for a full, top-priority re-query, ignoring staleness and
    /// any prior not-found result. Used by the on-demand path (clicking a
    /// pirate). Marks both pages stale so the (TTL-based) scheduler
    /// refetches them now, and raises the pirate to tier 0 until its
    /// trophies page lands.
    pub fn force_requery(&mut self, name: &str) {
        if let Ok(norm) = pirate::normalize_name(name) {
            self.gone.remove(&norm);
            if let Some(c) = self.fetched.get_mut(&norm) {
                c.basic_fetched_at = DateTime::<Utc>::MIN_UTC;
                c.trophies_fetched_at = DateTime::<Utc>::MIN_UTC;
            }
            self.forced.insert(norm);
        }
    }

    /// Decide which yoweb pages are due for an already-normalized `norm`, or
    /// `None` if nothing is. A never-seen pirate needs both pages; otherwise
    /// each page is due only once its TTL has elapsed. A forced re-query
    /// expresses itself by stale timestamps (see [`Self::force_requery`]),
    /// so it flows through this same TTL logic rather than a special case.
    pub fn fetch_plan(
        &self,
        norm: &str,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<FetchPlan> {
        match self.fetched.get(norm) {
            None => {
                Some(FetchPlan {
                    basic: true,
                    trophies: true,
                })
            }
            Some(c) => {
                let basic =
                    now.signed_duration_since(c.basic_fetched_at) >= basic_ttl;
                let trophies = now.signed_duration_since(c.trophies_fetched_at)
                    >= trophy_ttl;
                (basic || trophies).then_some(FetchPlan {
                    basic,
                    trophies,
                })
            }
        }
    }

    /// Fold a completed single-page fetch for `norm` into the cache, swapping
    /// in whichever part came back fresh. An on-demand (`forced`) request
    /// is cleared only once its trophies page — the last in the
    /// basic→trophies sequence — lands, so a forced pirate fetches both
    /// pages before dropping off tier 0.
    pub fn apply_update(&mut self, norm: String, update: PirateUpdate) {
        match update {
            PirateUpdate::Refreshed {
                basic,
                trophies,
            } => {
                // The trophies page is the tail of the sequence: once it lands
                // an on-demand request is satisfied.
                if trophies.is_some() {
                    self.forced.remove(&norm);
                }
                match self.fetched.get_mut(&norm) {
                    Some(entry) => {
                        if let Some((info, at)) = basic {
                            entry.basic = *info;
                            entry.basic_fetched_at = at;
                        }
                        if let Some((t, at)) = trophies {
                            entry.trophies = t;
                            entry.trophies_fetched_at = at;
                        }
                    }
                    // A pirate's basic page is always fetched before its
                    // trophies, so for a brand-new entry
                    // `basic` is present; trophies lag
                    // behind, left stale (MIN_UTC) so the next pass fetches
                    // them.
                    None => {
                        if let Some((info, basic_at)) = basic {
                            let (trophies, trophies_at) = match trophies {
                                Some((t, at)) => (t, at),
                                None => {
                                    (
                                        Default::default(),
                                        DateTime::<Utc>::MIN_UTC,
                                    )
                                }
                            };
                            self.fetched.insert(
                                norm,
                                CachedPirate {
                                    basic: *info,
                                    trophies,
                                    basic_fetched_at: basic_at,
                                    trophies_fetched_at: trophies_at,
                                },
                            );
                        }
                    }
                }
            }
            PirateUpdate::NotFound => {
                self.fetched.remove(&norm);
                self.forced.remove(&norm);
                self.gone.insert(norm);
            }
            // Transport/server error: keep whatever we have and give up the
            // forced request (don't hammer a failing page); natural
            // staleness may retry.
            PirateUpdate::Error(_) => {
                self.forced.remove(&norm);
            }
        }
    }

    /// Which page (if any) is due for an already-normalized `norm`: basic
    /// before trophies. `None` means nothing is due.
    fn due_page(
        &self,
        norm: &str,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<PiratePage> {
        let plan = self.fetch_plan(norm, basic_ttl, trophy_ttl, now)?;
        if plan.basic {
            Some(PiratePage::Basic)
        } else if plan.trophies {
            Some(PiratePage::Trophies)
        } else {
            None
        }
    }

    /// The priority tier of an already-normalized `norm` against the current
    /// sets, or `None` if it's no longer relevant (so an in-flight fetch for it
    /// can be cancelled). 0 = on-demand, 1 = aboard/self, 2 = planked.
    pub fn tier_of(
        &self,
        norm: &str,
        aboard: &HashSet<String>,
        planked: &HashSet<String>,
    ) -> Option<u8> {
        if self.forced.contains(norm) {
            Some(0)
        } else if aboard.contains(norm) {
            Some(1)
        } else if planked.contains(norm) {
            Some(2)
        } else {
            None
        }
    }

    /// The single most important page to fetch right now, scanning tiers in
    /// priority order (on-demand ▸ aboard/self ▸ planked) and, within a tier,
    /// pirates in a stable order — basic before trophies per pirate. All name
    /// sets are already normalized. `None` means nothing is due.
    pub fn next_order(
        &self,
        aboard: &HashSet<String>,
        planked: &HashSet<String>,
        basic_ttl: Duration,
        trophy_ttl: Duration,
        now: DateTime<Utc>,
    ) -> Option<FetchOrder> {
        let due = |norm: &str| self.due_page(norm, basic_ttl, trophy_ttl, now);

        // Tier 0: on-demand. Stable order for determinism.
        let mut forced: Vec<&String> = self.forced.iter().collect();
        forced.sort_unstable();
        for norm in forced {
            if let Some(page) = due(norm) {
                return Some(FetchOrder {
                    norm: norm.clone(),
                    page,
                    tier: 0,
                });
            }
        }

        // Tier 1: aboard + self (skipping gone / already-forced).
        let mut aboard_v: Vec<&String> = aboard.iter().collect();
        aboard_v.sort_unstable();
        for norm in aboard_v {
            if self.gone.contains(norm) || self.forced.contains(norm) {
                continue;
            }
            if let Some(page) = due(norm) {
                return Some(FetchOrder {
                    norm: norm.clone(),
                    page,
                    tier: 1,
                });
            }
        }

        // Tier 2: planked (skipping gone / forced / already covered as aboard).
        let mut planked_v: Vec<&String> = planked.iter().collect();
        planked_v.sort_unstable();
        for norm in planked_v {
            if self.gone.contains(norm)
                || self.forced.contains(norm)
                || aboard.contains(norm)
            {
                continue;
            }
            if let Some(page) = due(norm) {
                return Some(FetchOrder {
                    norm: norm.clone(),
                    page,
                    tier: 2,
                });
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// View state
// ---------------------------------------------------------------------------

/// Which widget on the page has keyboard focus. The Unpoison button is only
/// reachable when the selected vessel is actually poisoned; the three panes
/// only exist on the Pillage layout.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobberFocus {
    #[default]
    Vessels,
    ShipType,
    VoyageType,
    Unpoison,
    /// The Skill Leaderboard panel (the ranked per-skill columns). Selectable:
    /// a 2D cursor (`top_col`/`top_sel`) walks its columns, Enter opens the
    /// pirate.
    Leaderboard,
    /// The "View Skill Distribution" button (Vampirates only), between the
    /// Skill Leaderboard and the panes.
    SkillDist,
    Aboard,
    Greedy,
    Planked,
    /// The Enthralled leaderboard pane (Cursed Isles only).
    Enthralled,
    /// The Tokens and Chests box (Atlantis only, and only once there is
    /// something in it — see [`boards`]). Its list scrolls and its tabs and
    /// column heads answer to keys of their own.
    Board,
}

#[derive(Default)]
pub struct JobbersUi {
    /// Vessel currently shown; resolved against the live vessel set each
    /// frame.
    pub selected: Option<Arc<str>>,
    pub focus: JobberFocus,
    /// Scroll offset of each pane, recomputed each frame to keep the pane's
    /// selection visible (the panes auto-scroll rather than scroll manually).
    pub aboard_offset: usize,
    pub greedy_offset: usize,
    pub planked_offset: usize,
    pub enthralled_offset: usize,
    /// Selected pirate index within each pane (into that pane's pirate list).
    pub aboard_sel: usize,
    pub greedy_sel: usize,
    pub planked_sel: usize,
    pub enthralled_sel: usize,
    /// Skill Leaderboard cursor: which column (`top_col`) and which rank
    /// within it (`top_sel`), plus the shared vertical scroll offset
    /// (`top_offset`) — all columns share one window so their ranks stay
    /// aligned row-for-row.
    pub top_col: usize,
    pub top_sel: usize,
    pub top_offset: usize,
    /// The voyage type being crewed; gates the Pillage-only layout.
    pub voyage_type: VoyageType,
    /// Which of the Tokens and Chests box's leaderboards is up.
    pub board_tab: BoardTab,
    /// How each board is ranked, and how far down each is scrolled. Kept per
    /// board: a ranking is about the figures it ranks, so one tab's has no
    /// business moving the other's.
    pub tokens_ranking: Ranking,
    pub treasures_ranking: Ranking,
    pub tokens_offset: usize,
    pub treasures_offset: usize,
    /// How many entries each Top Jobbers column shows. `None` (the default)
    /// shows the whole ranked list with no cap.
    pub leaderboard_size: Option<usize>,
    /// Ship type chosen per vessel (index into [`SHIPS`]), keyed by vessel
    /// name. Lives here, not on the `Vessel`, so picks survive a relog
    /// state wipe.
    pub ship_types: HashMap<Arc<str>, usize>,
    /// When `Some`, the ship-type popup is open with this highlighted index;
    /// it applies to the currently-selected vessel.
    pub ship_popup: Option<usize>,
    /// When `Some`, the vessel picker popup is open with this highlighted
    /// index (into the latest-first vessel ordering).
    pub vessel_popup: Option<usize>,
    /// When `Some`, the voyage-type picker popup is open with this highlighted
    /// index (into [`VOYAGE_TYPES`]).
    pub voyage_popup: Option<usize>,
    /// When `Some`, the pirate-stats popup is open for this pirate.
    pub pirate_popup: Option<PiratePopup>,
    /// When `Some`, the trophies popup is open (layered over the stats popup).
    pub trophy_popup: Option<TrophyPopup>,
    /// When `Some`, the note editor is open (layered over the stats popup, as
    /// the trophies are, and reached from the same button row).
    pub note_popup: Option<NotePopup>,
    /// When `Some`, the Vampirates skill-distribution scatterplot is open,
    /// with the cursor parked on a grid cell.
    pub skill_dist_popup: Option<SkillDistPopup>,
    /// When `Some`, the per-fight advantage-over-time graph is open (Cursed
    /// Isles / Vampirate waves), on the selected fight with the chosen
    /// X-axis.
    pub per_fight_popup: Option<PerFightPopup>,
    /// When `Some`, a copied duty report is asking before its names join the
    /// roster.
    pub roster_popup: Option<RosterPrompt>,
    /// A roster prompt waiting for the popup slot to free up; see
    /// [`Self::raise_pending_roster`].
    pub pending_roster: Option<RosterPrompt>,
}

/// A copied duty report that named pirates the current vessel has no record
/// of, held for the user's answer.
///
/// A report names whoever puzzled long enough to be rated, so it can only ever
/// *add*: a pirate it leaves out may simply not have puzzled, which is no
/// evidence they left. It is raised only when the report looks too unlike the
/// vessel we think we're on to fold in unasked, which is what
/// [`known`](Self::known) against [`rated`](Self::rated) measures.
#[derive(Clone, Default)]
pub struct RosterPrompt {
    /// The pirates to add, in name order.
    pub missing: Vec<String>,
    /// How many of the report's players we already had aboard.
    pub known: usize,
    /// How many players the report rated in all.
    pub rated: usize,
    pub yes_focused: bool,
}

impl JobbersUi {
    /// How `tab`'s board is ranked.
    pub fn board_ranking(&self, tab: BoardTab) -> Ranking {
        match tab {
            BoardTab::Tokens => self.tokens_ranking,
            BoardTab::Treasures => self.treasures_ranking,
        }
    }

    /// Rank `tab`'s board as a click on the column `key` names asks.
    pub fn rank_board(&mut self, tab: BoardTab, key: BoardKey) {
        let ranking = match tab {
            BoardTab::Tokens => &mut self.tokens_ranking,
            BoardTab::Treasures => &mut self.treasures_ranking,
        };
        *ranking = ranking.clicked(key);
    }

    /// How far down the board on show is scrolled.
    pub fn board_offset(&self) -> usize {
        match self.board_tab {
            BoardTab::Tokens => self.tokens_offset,
            BoardTab::Treasures => self.treasures_offset,
        }
    }

    /// How far down `tab`'s board is scrolled.
    fn board_offset_mut(&mut self, tab: BoardTab) -> &mut usize {
        match tab {
            BoardTab::Tokens => &mut self.tokens_offset,
            BoardTab::Treasures => &mut self.treasures_offset,
        }
    }

    /// Scroll the board on show by `delta` rows. What it may not pass is the
    /// render's to say, the window being known there and nowhere earlier.
    pub fn scroll_board(&mut self, delta: isize) {
        let tab = self.board_tab;
        let offset = self.board_offset_mut(tab);
        *offset = offset.saturating_add_signed(delta);
    }

    /// Park the board on show at `offset` rows down, for a drag of its bar.
    pub fn seek_board(&mut self, offset: usize) {
        let tab = self.board_tab;
        *self.board_offset_mut(tab) = offset;
    }

    /// Whether any of the page's modal popups holds the slot.
    fn popup_open(&self) -> bool {
        self.ship_popup.is_some()
            || self.vessel_popup.is_some()
            || self.voyage_popup.is_some()
            || self.pirate_popup.is_some()
            || self.trophy_popup.is_some()
            || self.skill_dist_popup.is_some()
            || self.per_fight_popup.is_some()
            || self.roster_popup.is_some()
    }

    /// Hold a roster prompt for the user's answer, opening it only once the
    /// popup slot is free (see [`Self::raise_pending_roster`]). A newer report
    /// replaces an unanswered older one, since the clipboard only ever holds
    /// the latest copy.
    pub fn queue_roster(&mut self, prompt: RosterPrompt) {
        match self.roster_popup {
            Some(ref mut open) => *open = prompt,
            None => self.pending_roster = Some(prompt),
        }
    }

    /// Open the queued roster prompt if the popup slot is free. Returns
    /// whether one opened.
    pub fn raise_pending_roster(&mut self) -> bool {
        if self.popup_open() || self.pending_roster.is_none() {
            return false;
        }
        self.roster_popup = self.pending_roster.take();
        true
    }
}

/// State of the open per-fight statistics popup: which fight (wave) is shown
/// and the graph's X-axis mode.
#[derive(Clone, Copy, Default)]
pub struct PerFightPopup {
    /// Index into the current vessel's fight list (oldest first; the
    /// in-progress wave, if any, is last).
    pub idx: usize,
    /// Wall-clock time vs KO-event sequence on the X-axis.
    pub axis: AxisMode,
}

/// State of the open pirate-stats popup: the pirate being viewed, which of its
/// two buttons ([See Trophies] / [Close]) is focused, and — for the skill
/// tables, which are taller than a short window can hold — the vertical scroll
/// offset (in rendered lines) and the last-rendered view height (so Page
/// Up/Down can scroll by half a page).
#[derive(Clone)]
pub struct PiratePopup {
    pub name: String,
    /// Which of [`pirate_popup_buttons`] is marked, left to right.
    pub button: usize,
    pub offset: usize,
    pub view_h: usize,
    /// What the user has written down about this pirate, as the persistence
    /// file had it when the popup opened. `None` where no note can be kept
    /// at all: notes are filed under an ocean, and a run told of none has
    /// nowhere to file them, so the popup then offers nothing and shows
    /// nothing.
    pub note: Option<String>,
}

/// What the note editor's keys are going to: the text itself, or one of the
/// buttons under it. ↓ off the last line of the text hands them to Save, and ↑
/// from a button hands them back.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoteFocus {
    Text,
    Cancel,
    Save,
}

/// State of the open note editor: whose note, the text as it stands, which part
/// of the box has the keys, and what the last render made of it — the width it
/// wrapped to and the rows it had, which is what ↑/↓ and the scroll are
/// measured in.
pub struct NotePopup {
    pub name: String,
    pub field: crate::utils::PromptField,
    pub focus: NoteFocus,
    /// First wrapped line on show, for a note too long for the box.
    pub offset: usize,
    pub wrap_w: usize,
    pub view_h: usize,
}

/// State of the open trophies popup: whose trophies, the live search filter,
/// the vertical scroll offset (in rendered lines), and the last-rendered view
/// height (so Page Up/Down can scroll by half a page).
#[derive(Clone)]
pub struct TrophyPopup {
    pub name: String,
    /// The open filter, if any. Summoned by `/` the way the Map's island
    /// search is, so the keys mean what they mean elsewhere in the popup until
    /// one is asked for.
    pub search: Option<crate::utils::PromptField>,
    pub offset: usize,
    pub view_h: usize,
}

/// State of the open Vampirates skill-distribution popup: a scatterplot of
/// aboard jobbers' Treasure Haul standing (x) against Carpentry standing (y).
/// The cursor is the highlighted cell — moved by mouse hover or the arrow keys
/// — whose pirates are listed in the right-hand panel.
#[derive(Clone, Copy)]
pub struct SkillDistPopup {
    /// Cursor cell as `(treasure_haul_idx, carpentry_idx)`, each a
    /// [`Standing`] `as u8` in `0..=8`.
    pub cursor: (u8, u8),
}

/// Every standing, low → high, for iterating the scatterplot axes.
const STANDINGS: [Standing; 9] = [
    Standing::Able,
    Standing::Proficient,
    Standing::Distinguished,
    Standing::Respected,
    Standing::Master,
    Standing::Renowned,
    Standing::GrandMaster,
    Standing::Legendary,
    Standing::Ultimate,
];

/// The scatterplot axes: Treasure Haul along x (columns), Carpentry along y
/// (rows).
const SKILL_DIST_X: Skill = Skill::TreasureHaul;
const SKILL_DIST_Y: Skill = Skill::Carpentry;

/// One aboard jobber placed on the skill-distribution grid, with the two
/// plotted skill records (for the right-hand detail panel).
struct SkillDistEntry {
    name: String,
    x: SkillRecord,
    y: SkillRecord,
}

/// Aboard jobbers bucketed for the skill-distribution plot. `entries` are the
/// plottable pirates (both skills known); `unplotted` counts those aboard whose
/// Treasure Haul or Carpentry stats aren't fetched yet (shown as a footer
/// note).
struct SkillDistData {
    entries: Vec<SkillDistEntry>,
    unplotted: usize,
}

impl SkillDistData {
    /// Pirates sitting on cell `(th_idx, carp_idx)`, alphabetical.
    fn at(&self, cell: (u8, u8)) -> Vec<&SkillDistEntry> {
        let mut v: Vec<&SkillDistEntry> = self
            .entries
            .iter()
            .filter(|e| (e.x.standing as u8, e.y.standing as u8) == cell)
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// How many pirates sit on each cell, indexed `[th_idx][carp_idx]`.
    fn counts(&self) -> [[u16; 9]; 9] {
        let mut grid = [[0u16; 9]; 9];
        for e in &self.entries {
            grid[e.x.standing as usize][e.y.standing as usize] += 1;
        }
        grid
    }

    /// The most populated cell (tie-break: higher Treasure Haul, then
    /// Carpentry) — a sensible place to park the cursor when the popup
    /// opens. `(0, 0)` if empty.
    fn densest_cell(&self) -> (u8, u8) {
        let grid = self.counts();
        let mut best = (0u8, 0u8);
        let mut best_n = 0u16;
        for x in 0 .. 9u8 {
            for y in 0 .. 9u8 {
                let n = grid[x as usize][y as usize];
                if n > best_n {
                    best_n = n;
                    best = (x, y);
                }
            }
        }
        best
    }
}

/// The cell to park the cursor on when the skill-distribution popup opens: the
/// most populated one (so the detail panel isn't empty), or `(0, 0)` if no
/// aboard jobber has both skills fetched.
pub fn default_skill_dist_cursor(
    aboard: &HashSet<String>,
    cache: &PirateCache,
) -> (u8, u8) {
    skill_dist_data(aboard, cache).densest_cell()
}

/// Bucket aboard jobbers into the skill-distribution grid by their Treasure
/// Haul and Carpentry standings. Pirates missing either skill's fetched stats
/// are tallied into `unplotted` instead.
fn skill_dist_data(
    aboard: &HashSet<String>,
    cache: &PirateCache,
) -> SkillDistData {
    let mut entries = Vec::new();
    let mut unplotted = 0;
    for name in aboard {
        let plotted = cache.get(name).and_then(|info| {
            let x = info.skills.get(&SKILL_DIST_X)?;
            let y = info.skills.get(&SKILL_DIST_Y)?;
            Some((x.clone(), y.clone()))
        });
        match plotted {
            Some((x, y)) => {
                entries.push(SkillDistEntry {
                    name: name.clone(),
                    x,
                    y,
                })
            }
            None => unplotted += 1,
        }
    }
    SkillDistData {
        entries,
        unplotted,
    }
}

/// Bottom-bar tooltip lines for the current focus (empty when nothing to say).
pub fn tooltip(state: &GameState, ui: &JobbersUi) -> Vec<&'static str> {
    match ui.focus {
        JobberFocus::Vessels => vec!["Press Enter to pick a vessel."],
        JobberFocus::ShipType => {
            vec!["Press Enter to pick this vessel's ship type."]
        }
        JobberFocus::VoyageType => vec!["Press Enter to pick the voyage type."],
        JobberFocus::Unpoison => {
            let poisoned = ui
                .selected
                .as_ref()
                .and_then(|k| state.vessels.get(k))
                .is_some_and(|v| v.poisoned);
            if poisoned {
                UNPOISON_TOOLTIP.to_vec()
            } else {
                Vec::new()
            }
        }
        JobberFocus::Leaderboard => {
            vec![
                "Enter: pirate stats \u{00b7} \u{2190}/\u{2192} columns \
                 \u{00b7} \u{2191}/\u{2193} scroll",
            ]
        }
        JobberFocus::Aboard
        | JobberFocus::Greedy
        | JobberFocus::Planked
        | JobberFocus::Enthralled => {
            vec![
                "Enter: pirate stats \u{00b7} \u{2190}/\u{2192} panes \
                 \u{00b7} \u{2191}/\u{2193} select",
            ]
        }
        JobberFocus::SkillDist => {
            vec!["Press Enter to view the skill distribution plot."]
        }
        JobberFocus::Board => {
            vec![
                "Tab: boards \u{00b7} s: ranking \u{00b7} \u{2190}/\u{2192} \
                 panes \u{00b7} \u{2191}/\u{2193} scroll",
            ]
        }
    }
}

// ---------------------------------------------------------------------------
// Styling helpers
// ---------------------------------------------------------------------------

/// Three-letter standing code (e.g. `Mas` for Master).
fn standing_abbr(s: Standing) -> &'static str {
    match s {
        Standing::Able => "Abl",
        Standing::Proficient => "Pro",
        Standing::Distinguished => "Dis",
        Standing::Respected => "Res",
        Standing::Master => "Mas",
        Standing::Renowned => "Ren",
        Standing::GrandMaster => "Gra",
        Standing::Legendary => "Leg",
        Standing::Ultimate => "Ult",
    }
}

/// Three-letter experience code (e.g. `Bro` for Broad).
fn experience_abbr(e: Experience) -> &'static str {
    match e {
        Experience::Novice => "Nov",
        Experience::Neophyte => "Neo",
        Experience::Apprentice => "App",
        Experience::Narrow => "Nar",
        Experience::Broad => "Bro",
        Experience::Solid => "Sol",
        Experience::Weighty => "Wei",
        Experience::Expert => "Exp",
        Experience::Paragon => "Par",
        Experience::Illustrious => "Ill",
        Experience::Sublime => "Sub",
        Experience::Revered => "Rev",
        Experience::Exalted => "Exa",
        Experience::Transcendent => "Tra",
    }
}

/// Standing emphasis: bold from Proficient, bold+italic from Renowned.
fn standing_style(s: Standing) -> Style {
    if s >= Standing::Renowned {
        Style::default().bold().italic()
    } else if s >= Standing::Proficient {
        Style::default().bold()
    } else {
        Style::default()
    }
}

/// The colour a pirate's name is drawn in: green for a greenie, plain for
/// everyone else. A name's *emphasis* answers a different question — bold for a
/// crewmate, bold+italic for the player — so callers add that on top of this.
fn greenie_colour(greenie: bool) -> Style {
    if greenie {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    }
}

/// [`greenie_colour`] for a name the cache can answer for. A pirate the cache
/// has nothing on is drawn plain: nothing is known of them yet, which is not
/// the same as knowing them to be green.
fn name_colour(cache: &PirateCache, name: &str) -> Style {
    greenie_colour(
        cache
            .get_cached(name)
            .is_some_and(crate::pirate::CachedPirate::is_greenie),
    )
}

/// Columns a tag spends, the blank between two of them, and the blank between
/// the strip of them and the longest name beside it.
const TAG_W: usize = 2;
const TAG_SEP: usize = 1;
const TAG_GAP: usize = 2;

/// The tags a row ends in, left to right, each of [`TAG_W`] columns. Only the
/// tags a row has earned are in it: the strip is laid against the row's right
/// edge, so a row with fewer of them has the rest of the line instead of a
/// column held empty.
type Tags = Vec<(&'static str, Style)>;

/// Columns a strip of `tags` spends, separators and all.
fn strip_w(tags: &Tags) -> usize {
    tags.len() * TAG_W + tags.len().saturating_sub(1) * TAG_SEP
}

/// Columns `tags` add to the width a row asks for: the strip, and the blank
/// that holds it off the name. A row with no tags asks for neither.
fn tags_w(tags: &Tags) -> usize {
    if tags.is_empty() {
        0
    } else {
        TAG_GAP + strip_w(tags)
    }
}

/// The crew our own pirate sails with, if the cache has them and they sail with
/// one. This is what a roster measures another pirate's crew against.
fn my_crew_name(state: &GameState, cache: &PirateCache) -> Option<String> {
    state
        .player_name
        .as_deref()
        .and_then(|me| cache.get(me))
        .map(|p| p.crew_name.clone())
        .filter(|crew| !crew.is_empty())
}

/// The tag marking a pirate's rank in our own crew, and the emphasis it
/// carries: bold from a fleet officer up, and the captain underlined on top of
/// it.
///
/// `None` for anyone the tag has nothing to say about — a pirate of another
/// crew, one of no crew, one the cache has yet to fetch, and one jobbing with
/// ours rather than sailing in it, a jobber being no rank.
fn crew_tag(
    cache: &PirateCache,
    my_crew: Option<&str>,
    name: &str,
) -> Option<(&'static str, Style)> {
    let mine = my_crew?;
    let theirs = cache.get(name)?;
    if !theirs.crew_name.eq_ignore_ascii_case(mine) {
        return None;
    }
    let plain = Style::default();
    let bold = plain.bold();
    match CrewRank::from_str(&theirs.crew_rank) {
        CrewRank::Captain => Some(("Ca", bold.underlined())),
        CrewRank::SeniorOfficer => Some(("SO", bold)),
        CrewRank::FleetOfficer => Some(("FO", bold)),
        CrewRank::Officer => Some(("Of", plain)),
        CrewRank::Pirate => Some(("Pi", plain)),
        CrewRank::CabinPerson => Some(("CP", plain)),
        CrewRank::JobbingPirate | CrewRank::Other(_) => None,
    }
}

/// The tags an Aboard row ends in, left to right: the mark for a pirate we have
/// planked off this vessel before, then their rank in our crew. A pirate who
/// has earned one of them and not the other wears only that one, at the row's
/// right edge, no column being held for the tag they lack.
fn aboard_tags(
    cache: &PirateCache,
    my_crew: Option<&str>,
    vessel: Option<&Vessel>,
    name: &str,
) -> Tags {
    let mut tags = Tags::new();
    if planked_before(vessel, name) {
        tags.push(("!P", Style::default()));
    }
    tags.extend(crew_tag(cache, my_crew, name));
    tags
}

/// Whether we have planked `name` off this vessel already. They are aboard
/// again if a roster is asking, the plank having taken them off it.
fn planked_before(vessel: Option<&Vessel>, name: &str) -> bool {
    vessel.is_some_and(|v| v.planked_by_us.contains(name))
}

/// Experience emphasis: bold from Broad, bold+italic from Sublime.
fn experience_style(e: Experience) -> Style {
    if e >= Experience::Sublime {
        Style::default().bold().italic()
    } else if e >= Experience::Broad {
        Style::default().bold()
    } else {
        Style::default()
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Border for a focusable widget: bright only when the page is focused *and*
/// this widget is the active one.
fn box_border(page_focused: bool, active: bool) -> Style {
    if page_focused && active {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Truncate to `max` columns, marking elision with `…`.
fn truncate(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// Ship staffing
// ---------------------------------------------------------------------------

/// Staffing verdict for the selected vessel against the chosen ship.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Staffing {
    /// Room for more crew *and* mercenaries still available — hire some.
    Understaffed,
    /// More swabbies or pirates aboard than the ship can hold: wrong ship.
    Invalid,
}

impl Staffing {
    fn message(self) -> &'static str {
        match self {
            Staffing::Understaffed => "Understaffed. Hire jobbers.",
            Staffing::Invalid => "Arr, too many aboard for that ship.",
        }
    }

    fn style(self) -> Style {
        match self {
            Staffing::Understaffed => Style::default().fg(Color::Yellow),
            Staffing::Invalid => Style::default().fg(Color::Red),
        }
    }
}

/// Compare the crew aboard against the chosen ship's capacity.
///
/// * `players` — named pirates aboard (the Aboard list).
/// * `swabbies` — the anonymous swabbie/mercenary tally aboard.
///
/// Overstaffed takes priority: exceeding the mercenary cap *or* the total
/// pirate cap means the wrong ship was probably picked. Otherwise it's
/// understaffed only while both there's room for more crew and the mercenary
/// cap hasn't been hit.
fn staffing(ship: &Ship, players: usize, swabbies: u32) -> Option<Staffing> {
    let total = players as u64 + swabbies as u64;
    if swabbies > ship.max_mercenaries as u32 || total > ship.max_pirates as u64
    {
        Some(Staffing::Invalid)
    } else if swabbies < ship.max_mercenaries as u32
        && total < ship.max_pirates as u64
    {
        Some(Staffing::Understaffed)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Render entry point
// ---------------------------------------------------------------------------

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GameState,
    cache: &PirateCache,
    ui: &mut JobbersUi,
    focused: bool,
    regions: &mut ClickMap,
) {
    // Nothing to frame without a log to read, so the notice saying so stands in
    // for the whole page.
    if !state.attached {
        crate::utils::render_page_notice(
            frame,
            area,
            &[(
                "No chat log attached. Pass --chat-log <PATH> (and --user \
                 <NAME>) to monitor a game log.",
                Style::default(),
            )],
        );
        return;
    }

    // Resolve the selected vessel against the current (latest-first) ordering.
    let ordered = state.vessels_by_recency();
    let selected: Option<Arc<str>> = ui
        .selected
        .as_ref()
        .filter(|k| state.vessels.contains_key(*k))
        .cloned()
        .or_else(|| ordered.first().cloned());
    ui.selected = selected.clone();

    let implemented = ui.voyage_type.implemented();
    let panes = ui.voyage_type.panes();
    // The Tokens and Chests box, where the run has earned one. It stands
    // beside the panes, which stack one over the other to make the column for
    // it, so its presence is the layout's and not just a widget's.
    let boards = boards(state, selected.as_ref(), ui.voyage_type);
    let stacked = boards.is_some();

    // The Unpoison button is only focusable while the vessel is poisoned, and a
    // pane is only focusable when this voyage type actually shows it — bounce
    // focus out of either when it no longer applies.
    let sel_poisoned = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.poisoned);
    let sel_provisional = selected
        .as_ref()
        .and_then(|k| state.vessels.get(k))
        .is_some_and(|v| v.provisional);
    if !sel_poisoned && ui.focus == JobberFocus::Unpoison {
        ui.focus = JobberFocus::Vessels;
    }
    let focused_pane = match ui.focus {
        JobberFocus::Aboard => Some(JobberPane::Aboard),
        JobberFocus::Greedy => Some(JobberPane::Greedy),
        JobberFocus::Planked => Some(JobberPane::Planked),
        JobberFocus::Enthralled => Some(JobberPane::Enthralled),
        _ => None,
    };
    if focused_pane.is_some_and(|p| !panes.contains(&p)) {
        ui.focus = JobberFocus::VoyageType;
    }
    // The Tokens and Chests box is only focusable while it is drawn.
    if ui.focus == JobberFocus::Board && !stacked {
        ui.focus = JobberFocus::VoyageType;
    }
    // The Skill Distribution button is only focusable on voyage types that show
    // it.
    if ui.focus == JobberFocus::SkillDist
        && !ui.voyage_type.has_skill_distribution()
    {
        ui.focus = JobberFocus::VoyageType;
    }

    // ---- Voyage box sizing ----
    // Longest ship name, computed at compile time — the Ship Type row's value
    // must fit it.
    const MAX_SHIP_NAME: usize = {
        let mut max = 0usize;
        let mut i = 0;
        while i < SHIPS.len() {
            let len = SHIPS[i].name.len();
            if len > max {
                max = len;
            }
            i += 1;
        }
        max
    };
    let label_w = ["Vessels", "Ship Type", "Voyage Type"]
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(0) as u16;
    let vessel_name_w = selected.as_ref().map_or(0, |k| k.chars().count());
    let voyage_name_w = VOYAGE_TYPES
        .iter()
        .map(|v| v.name().len())
        .max()
        .unwrap_or(0);
    let value_w = vessel_name_w
        .max(MAX_SHIP_NAME)
        .max(voyage_name_w)
        .max("Select ship".len())
        .max("No vessel".len()) as u16;
    // label + 2-space gap + value, plus borders(2) + padding(2).
    let voyage_w =
        (label_w + 2 + value_w + 4).max(offset_title_width("Voyage"));

    // Staffing data + the warning the chosen ship implies.
    let vessel = selected.as_ref().and_then(|k| state.vessels.get(k));
    let aboard_set = selected
        .as_ref()
        .map(|k| state.aboard(k))
        .unwrap_or_default();
    let ship_idx = selected
        .as_ref()
        .and_then(|k| ui.ship_types.get(k).copied());
    let swabbies = vessel.map_or(0, |v| v.swabbies);
    let warn =
        ship_idx.and_then(|i| staffing(&SHIPS[i], aboard_set.len(), swabbies));

    // ---- Skill Leaderboard + panes sizing (only used when implemented) ----
    // The leaderboard ranks *everyone* aboard per skill (no truncation) so it
    // can be scrolled through; what varies is the viewport height.
    let top_columns = rank_columns(
        ui.voyage_type.top_jobbers(),
        &aboard_set,
        cache,
        None,
    );
    let total_rows =
        top_columns.iter().map(|c| c.rows.len()).max().unwrap_or(0);
    // Viewport rows: a leaderboard shows its top few, so the window is capped
    // whatever the voyage type and the rest of the ranking scrolls. At least
    // one row so the box never collapses flat.
    let view_cap = ui.leaderboard_size.unwrap_or(5).max(1);
    let view_rows = total_rows.min(view_cap).max(1);
    let top_h = view_rows as u16 + 3;
    let top_panel_w = top_panel_width(&top_columns);

    // Per-pane natural widths: content + borders(2) + padding(2), floored at
    // title. The Aboard pane leads with a "Pirates (n):" header and indents
    // each name two spaces. A row may also end in tags, whose columns the width
    // counts so the longest name among them keeps the two blanks before its
    // strip.
    let my_crew = my_crew_name(state, cache);
    let aboard_cw = aboard_set
        .iter()
        .map(|n| {
            let tags = aboard_tags(cache, my_crew.as_deref(), vessel, n);
            n.chars().count() + ABOARD_INDENT + tags_w(&tags)
        })
        .max()
        .unwrap_or(0)
        .max(aboard_header(aboard_set.len()).len())
        .max(
            if swabbies > 0 {
                swabbie_footer(swabbies).len()
            } else {
                0
            },
        );
    let greedy: Vec<(&String, u32, u32)> = vessel
        .map(|v| {
            v.greedy_by_pirate
                .iter()
                .map(|(n, total)| {
                    let current = v.greedy_current.get(n).copied().unwrap_or(0);
                    (n, *total, current)
                })
                .collect()
        })
        .unwrap_or_default();
    let greedy_cw = name_col_plus_value(&greedy);
    let planked_n = vessel.map_or(0, |v| v.planked_by_us.len());
    // Planked carries the same rank tags as Aboard, so it reserves their
    // columns the same way.
    let planked_cw = vessel
        .map(|v| {
            v.planked_by_us
                .iter()
                .map(|n| {
                    let tags: Tags = crew_tag(cache, my_crew.as_deref(), n)
                        .into_iter()
                        .collect();
                    n.chars().count() + tags_w(&tags)
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    // The Enthralled leaderboard (Cursed Isles) replaces the Aboard pane: `name
    // alive/total`, ranked by lifetime enthralled.
    let enthralled: Vec<(String, u32, u32)> = selected
        .as_ref()
        .map(|k| enthralled_ranked(state, k))
        .unwrap_or_default();
    let enthralled_cw = enthralled_col_width(&enthralled);
    // Every pane's list scrolls, so each reserves the scrollbar's columns on
    // top of its frame — the pane is then one width whether the roster is
    // long enough for a bar or not.
    let pane_w = |cw: usize, title: &'static str| {
        (cw as u16 + crate::utils::BOX_MARGIN + crate::utils::SCROLLBAR_W)
            .max(offset_title_width(title))
    };
    let aboard_w = pane_w(aboard_cw, "Aboard");
    let greedy_w = pane_w(greedy_cw, "Greedy");
    let planked_w = pane_w(planked_cw, "Planked");
    let enthralled_w = pane_w(enthralled_cw, "Enthralled");
    // Only the panes this voyage type shows contribute to the block width.
    let pane_widths: Vec<u16> = panes
        .iter()
        .map(|p| {
            match p {
                JobberPane::Aboard => aboard_w,
                JobberPane::Greedy => greedy_w,
                JobberPane::Planked => planked_w,
                JobberPane::Enthralled => enthralled_w,
            }
        })
        .collect();
    // Stacked, the panes share one column as wide as the widest of them, and
    // the box stands beside it; side by side they each take their own width.
    let pane_col_w = pane_widths.iter().copied().max().unwrap_or(0);
    let board_w = boards.as_ref().map_or(0, board_box_width);
    let panes_w: u16 = if stacked {
        pane_col_w + board_w
    } else {
        pane_widths.iter().sum()
    };

    // ---- Stats box sizing (Vampirates waves) ----
    // A small non-selectable `label | value` table between Voyage and Top
    // Jobbers, tracking the lair wave model.
    let stats: Option<StatsBox> = if ui.voyage_type.tracks_vampirates() {
        let active = vessel.is_some_and(|v| v.lair_active);
        let wave = vessel.map_or(0, |v| v.lair_wave);
        let defeated = vessel.map_or(0, |v| v.vampires_defeated);
        // Next wave's vampires: a [low, high] range projected from the current
        // wave's anchor while in a lair; before a lair it's just wave 1 = the
        // pirates aboard (an exact single number).
        let next = if active {
            let base = vessel.map_or(0.0, |v| v.lair_pirates as f64)
                * LAIR_WAVE_GROWTH.powi((wave.max(1) - 1) as i32);
            let lo = (base * LAIR_WAVE_LO).round() as u32;
            let hi = (base * LAIR_WAVE_HI).round() as u32;
            if lo == hi {
                lo.to_string()
            } else {
                format!("{lo} to {hi}")
            }
        } else {
            aboard_set.len().to_string()
        };
        let mut notes: Vec<Line<'static>> = Vec::new();
        // From wave 5 on, Mother o' Nyght herself enters the fight.
        if active && wave >= 5 {
            notes.push(Line::from(Span::styled(
                "Mother o' Nyght has joined the fray!",
                Style::default().fg(Color::Red).bold(),
            )));
        }
        // A wave whose kill count missed its projection means we left the
        // fight.
        if vessel.is_some_and(|v| v.lair_warn) {
            notes.push(Line::from(Span::styled(
                "Don't leave the fray, even if ye lose.",
                Style::default().fg(Color::Red).italic(),
            )));
        }
        Some(StatsBox {
            title: "Vampirates Stats",
            rows: vec![
                StatRow::new("Wave", wave.to_string()),
                StatRow::new(
                    "Vampirates Defeated",
                    defeated.to_string(),
                ),
                StatRow::new("Next Wave's Vampirates", next),
            ],
            notes,
        })
    } else if ui.voyage_type.tracks_vikings() {
        // A breakdown of the crew's Gunnery standing: a "Gunnery Standing"
        // header followed by one indented row per standing (highest
        // first) with the count aboard, then a final "Not queried yet"
        // row for pirates whose Gunnery stat hasn't been fetched. A standing
        // nobody aboard holds is left out: a count of nought says the same as
        // the row's absence and the rows are worth more to the panes below.
        let mut counts = [0u32; 9];
        let mut unqueried = 0u32;
        for name in &aboard_set {
            match cache.get(name).and_then(|i| i.skills.get(&Skill::Gunning)) {
                Some(rec) => counts[rec.standing as usize] += 1,
                None => unqueried += 1,
            }
        }
        let mut rows = vec![StatRow::new("Gunnery Standing", "")];
        rows.extend(
            STANDINGS
                .iter()
                .rev()
                .filter(|s| 0 < counts[**s as usize])
                .map(|s| {
                    StatRow::new(
                        format!("  {s}"),
                        counts[*s as usize].to_string(),
                    )
                }),
        );
        if 0 < unqueried {
            rows.push(StatRow::styled(
                "  Not looked up yet",
                unqueried.to_string(),
                Style::default().fg(Color::DarkGray).italic(),
            ));
        }
        Some(StatsBox {
            title: "Vikings Statistics",
            rows,
            notes: Vec::new(),
        })
    } else {
        None
    };
    // Rows + (blank separator + notes, when present) + borders(2).
    let stats_h = stats.as_ref().map_or(0, |s| {
        let notes = if s.notes.is_empty() {
            0
        } else {
            s.notes.len() as u16 + 1
        };
        s.rows.len() as u16 + notes + 2
    });
    let stats_w = stats.as_ref().map_or(0, stats_box_width);

    // ---- "View Skill Distribution" button sizing (Vampirates only) ----
    // A focusable bordered button between Top Jobbers and the panes; opens the
    // Treasure Haul × Carpentry scatterplot. Collapses to 0 height elsewhere.
    let show_skill_dist = ui.voyage_type.has_skill_distribution();
    let button_h: u16 = if show_skill_dist { 1 } else { 0 };
    let button_w: u16 = if show_skill_dist {
        SKILL_DIST_BUTTON_LABEL.len() as u16
    } else {
        0
    };

    // ---- Fight Statistics box sizing (Cursed Isles only) ----
    // A non-selectable box sat between the Skill Leaderboard and the panes: the
    // live island-wave model (current/next wave, our manpower, projected
    // advantage), plus boss / left-the-fight warnings and an inert "Show
    // Per-Fight Statistics" button.
    let fight_stats: Option<StatsBox> = if ui.voyage_type.tracks_cursed_isles()
    {
        let island_active = vessel.is_some_and(|v| v.island_active);
        let wave = vessel.map_or(0, |v| v.island_wave);
        let observed = vessel.map_or(0, |v| v.wave_enemies_observed);
        let kind = vessel.map_or(WaveKind::Unknown, |v| v.wave_kind);
        let zombies = vessel.map_or(0, |v| v.zombies_aboard);
        let thralls_alive: u32 =
            vessel.map_or(0, |v| v.thralls_alive.values().sum());
        // Our island melee strength: real pirates aboard (incl. us) + live
        // thralls.
        let manpower = aboard_set.len() as u32 + thralls_alive;
        // Forecast anchor: pirates aboard at landing once known, else the live
        // count.
        let anchor = vessel
            .map(|v| {
                if v.island_pirates > 0 {
                    v.island_pirates
                } else {
                    aboard_set.len() as u32
                }
            })
            .unwrap_or(aboard_set.len() as u32);
        let next_wave = if island_active {
            wave.saturating_add(1)
        } else {
            wave.max(1)
        };
        let (lo, hi) = island_wave_band(anchor, next_wave);
        let mid = (lo + hi) / 2;

        let mut rows: Vec<StatRow> = Vec::new();
        // One "Phase" row: "Sailing" until we land, then "Wave N
        // (Rumble/Swordfight)".
        if wave == 0 {
            rows.push(StatRow::new(
                "Phase",
                "Sailing".to_string(),
            ));
            rows.push(StatRow::new(
                "Zombies Aboard",
                zombies.to_string(),
            ));
        } else {
            rows.push(StatRow::new(
                "Phase",
                format!(
                    "Wave {wave} ({})",
                    wave_kind_label(kind)
                ),
            ));
            rows.push(StatRow::new(
                "Enemies This Wave",
                observed.to_string(),
            ));
        }
        // The next wave's kind is deterministic (waves alternate from a Rumble
        // start).
        let next_label = if wave == 0 {
            "First Wave (est.)"
        } else {
            "Next Wave (est.)"
        };
        let next_count = if lo == hi {
            lo.to_string()
        } else {
            format!("{lo} to {hi}")
        };
        let next_val = format!(
            "{next_count} ({})",
            wave_kind_label(wave_kind_for(next_wave))
        );
        rows.push(StatRow::new(next_label, next_val));
        rows.push(StatRow::new(
            "Manpower",
            manpower.to_string(),
        ));
        let advantage = if mid > 0 {
            format!("{:.1}x", manpower as f64 / mid as f64)
        } else {
            "\u{2014}".to_string()
        };
        rows.push(StatRow::new(
            "Projected Advantage",
            advantage,
        ));

        let mut notes: Vec<Line<'static>> = Vec::new();
        // Vargas is guaranteed on Rumble waves from wave 5 on — derived, not
        // detected.
        if island_active && vargas_in_wave(wave) {
            notes.push(Line::from(Span::styled(
                "Ye be tremored by the presence of Vargas the Mad! Man at \
                 arms!",
                Style::default().fg(Color::Red).bold(),
            )));
        }
        if vessel.is_some_and(|v| v.island_left_warn) {
            notes.push(Line::from(Span::styled(
                "Counts may be off \u{2014} ye left a fight early.",
                Style::default().fg(Color::Red).italic(),
            )));
        }
        // The "Show Per-Fight Statistics" button — opens the
        // advantage-over-time graph popup. A click region is registered
        // over its row after render (see the `fight_stats` render
        // branch). Greyed out when there's no fight to show.
        let has_fights = vessel
            .is_some_and(|v| !v.island_waves.is_empty() || v.island_active);
        let btn_style = if has_fights {
            Style::default().bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        notes.push(Line::from(Span::styled(
            PER_FIGHT_BUTTON_LABEL,
            btn_style,
        )));
        Some(StatsBox {
            title: "Fight Statistics",
            rows,
            notes,
        })
    } else {
        None
    };
    let fight_h = fight_stats.as_ref().map_or(0, |s| {
        let notes = if s.notes.is_empty() {
            0
        } else {
            s.notes.len() as u16 + 1
        };
        s.rows.len() as u16 + notes + 2
    });
    let fight_w = fight_stats.as_ref().map_or(0, stats_box_width);

    // Vikings lays Top Jobbers and its pane(s) side by side in one row instead
    // of stacking them; the block must be wide enough for both together.
    let side_by_side = implemented && ui.voyage_type.panes_beside_top_jobbers();

    // ---- Block geometry: centered horizontally, full content height so the
    // panes      can run the whole way down. ----
    let block_w = if side_by_side {
        voyage_w.max(stats_w).max(top_panel_w + panes_w)
    } else if implemented {
        voyage_w
            .max(top_panel_w)
            .max(panes_w)
            .max(stats_w)
            .max(button_w)
            .max(fight_w)
    } else {
        voyage_w.max(offset_title_width("Coming Soon"))
    };
    // The boxes are sized to their contents, so squeezing the block into a
    // narrower window would clip names and tallies rather than tighten them.
    if crate::utils::too_narrow(frame, area, block_w) {
        return;
    }
    let block = Rect::new(
        area.x + area.width.saturating_sub(block_w) / 2,
        area.y,
        block_w,
        area.height,
    );

    // Voyage box height: 3 rows + (blank + Unpoison) when poisoned + warning
    // lines
    // + borders(2). Warnings wrap to the block's inner width.
    let warn_lines: Vec<String> = warn
        .map(|w| {
            wrap_words(
                w.message(),
                block_w.saturating_sub(4) as usize,
            )
        })
        .unwrap_or_default();
    let unpoison_h: u16 = if sel_poisoned { 2 } else { 0 };
    let voyage_h = 3 + unpoison_h + warn_lines.len() as u16 + 2;

    // The pirate-stats / trophies popups own the screen, so hide the page
    // tooltip underneath them.
    let tooltip_lines = if ui.pirate_popup.is_some()
        || ui.trophy_popup.is_some()
        || ui.skill_dist_popup.is_some()
        || ui.per_fight_popup.is_some()
    {
        Vec::new()
    } else {
        tooltip(state, ui)
    };
    let tip_h = tooltip_lines.len() as u16;

    // Rows each pane pins around its scrolling list: the Aboard pane holds a
    // "Pirates (n):" header and its swabbie footer still while the names
    // between them scroll. Mirrors the rows `render_panes` pins.
    let pinned = |pane: &JobberPane| -> u16 {
        match pane {
            JobberPane::Aboard => 1 + u16::from(0 < swabbies),
            JobberPane::Greedy
            | JobberPane::Planked
            | JobberPane::Enthralled => 0,
        }
    };
    // The least each pane can be given: what it pins, a scrollable view's
    // worth of list under that whatever the rosters hold just now — so the
    // panes show the room the names they do not hold yet would be read in —
    // and its borders.
    let pane_floors: Vec<u16> = panes
        .iter()
        .map(|pane| pinned(pane) + crate::utils::SCROLL_MIN_ROWS + 2)
        .collect();
    // What each pane would fill: every row of its list, pins and borders.
    let pane_wants: Vec<u16> = panes
        .iter()
        .map(|pane| {
            let rows = match pane {
                JobberPane::Aboard => aboard_set.len(),
                JobberPane::Greedy => greedy.len(),
                JobberPane::Planked => planked_n,
                JobberPane::Enthralled => enthralled.len(),
            };
            (rows as u16).saturating_add(pinned(pane) + 2)
        })
        .collect();
    // Side by side the panes share one height, so it must suit whichever of
    // them pins the most; stacked they each take their own, so the column
    // must hold every floor at once.
    let pane_min = if stacked {
        pane_floors.iter().sum::<u16>().max(board_box_min_height())
    } else {
        pane_floors.iter().copied().max().unwrap_or(0)
    };
    // The same floor for the Skill Leaderboard, whose header and borders are
    // the 3 rows `top_h` adds to its ranking. A window capped below the floor
    // can never show four rows, so there the cap is the floor.
    let top_min = crate::utils::SCROLL_MIN_ROWS.min(view_cap as u16) + 3;

    // Room the page must have: the rows the boxes that cannot scroll come to,
    // and a scrollable view's worth for each that can, whatever those views
    // hold at the moment. The tooltip's rows are counted whether or not one is
    // up, so what the page needs does not move as focus does.
    const TIP_RESERVE: u16 = 2;
    // Rows the boxes that cannot give way take between them.
    let pinned_h = if side_by_side {
        voyage_h + stats_h
    } else if implemented {
        voyage_h + stats_h + fight_h + button_h
    } else {
        voyage_h
    };
    let stacked_h = if side_by_side {
        // Vikings stands the leaderboard beside the panes, so the row they
        // share suits whichever of them is taller.
        pinned_h + top_min.max(pane_min)
    } else if implemented {
        pinned_h + top_min + pane_min
    } else {
        // Nothing to scroll on a voyage type we have not built yet: the
        // "Coming Soon" box is one line in a border.
        pinned_h + PLACEHOLDER_H
    };
    if crate::utils::too_short(frame, area, stacked_h + TIP_RESERVE) {
        return;
    }

    // The boxes above the panes take the rows their contents come to — the
    // leaderboard its ranking, capped, and never less than a scrollable
    // view's worth whatever it ranks — and the panes take everything left
    // over, holding the rosters that grow and so the lists worth the room. A
    // window too short for all of that is given out the other way round: the
    // boxes whose lists scroll give way, the panes first, then the
    // leaderboard, rather than the page dropping one of them.
    let mut room = block.height.saturating_sub(pinned_h + tip_h);
    let top_given = top_h.max(top_min).min(room.saturating_sub(pane_min));
    room -= top_given;
    let panes_given = room;

    let rows = if side_by_side {
        // Vikings: Voyage, stats, then one row holding Top Jobbers beside the
        // pane(s) (split horizontally at render time), then the tip. The row
        // takes the rest of the page, the panes being what fills it.
        Layout::vertical([
            Constraint::Length(voyage_h),
            Constraint::Length(stats_h),
            Constraint::Length(room + top_given),
            Constraint::Length(tip_h),
        ])
        .flex(ratatui::layout::Flex::Start)
        .split(block)
    } else if implemented {
        // The stats row sits between Voyage and Top Jobbers; it collapses to
        // zero height (rendering nothing) on voyage types without a stats box.
        Layout::vertical([
            Constraint::Length(voyage_h),
            Constraint::Length(stats_h),
            Constraint::Length(top_given),
            Constraint::Length(fight_h),
            Constraint::Length(button_h),
            Constraint::Length(panes_given),
            Constraint::Length(tip_h),
        ])
        .flex(ratatui::layout::Flex::Start)
        .split(block)
    } else {
        Layout::vertical([
            Constraint::Length(voyage_h),
            Constraint::Length(PLACEHOLDER_H),
            Constraint::Length(tip_h),
        ])
        .flex(ratatui::layout::Flex::Start)
        .split(block)
    };

    render_voyage_box(
        frame,
        rows[0],
        ui,
        &selected,
        ship_idx,
        sel_poisoned,
        sel_provisional,
        warn,
        &warn_lines,
        label_w,
        focused,
        regions,
    );

    let tip_area = if side_by_side {
        if let Some(s) = &stats {
            render_stats_box(frame, rows[1], s, focused);
        }
        // Skill Leaderboard (its natural width) on the left, the pane(s)
        // filling the rest. Both take the whole row: the leaderboard stands
        // beside the panes here rather than above them, and a box that stopped
        // short of the ones it is shoulder to shoulder with would read as a
        // hole in the page rather than as a box that had said its piece. The
        // rows it gains are a longer ranking on show.
        let main = Layout::horizontal([
            Constraint::Length(top_panel_w),
            Constraint::Min(0),
        ])
        .split(rows[2]);
        render_top_panel(
            frame,
            main[0],
            &top_columns,
            ui,
            focused,
            regions,
        );
        render_panes(
            frame,
            state,
            cache,
            selected.as_ref(),
            &aboard_set,
            &greedy,
            ui,
            focused,
            panes,
            &pane_areas(main[1], &pane_widths),
            regions,
        );
        rows[3]
    } else if implemented {
        if let Some(s) = &stats {
            render_stats_box(frame, rows[1], s, focused);
        }
        render_top_panel(
            frame,
            rows[2],
            &top_columns,
            ui,
            focused,
            regions,
        );
        // Fight Statistics (Cursed Isles) sits between the leaderboard and the
        // panes; collapses to zero height (rendering nothing) on other
        // voyage types.
        if let Some(s) = &fight_stats {
            render_stats_box(frame, rows[3], s, focused);
            // The "Show Per-Fight Statistics" button is the last note line;
            // register a click region over its row so it opens the
            // per-fight graph popup.
            let btn_y =
                rows[3].y + s.rows.len() as u16 + s.notes.len() as u16 + 1;
            if btn_y < rows[3].y + rows[3].height {
                regions.push(ClickRegion {
                    rect: Rect::new(
                        rows[3].x + 2,
                        btn_y,
                        rows[3].width.saturating_sub(4),
                        1,
                    ),
                    target: ClickTarget::JobberPerFightButton,
                });
            }
        }
        if show_skill_dist {
            render_skill_dist_button(
                frame,
                rows[4],
                focused,
                ui.focus == JobberFocus::SkillDist,
                regions,
            );
        }
        // Stacked, the panes take a column of their own and the Tokens and
        // Chests box the rest of the row: it runs the whole way down beside
        // them, a box that stopped level with the upper pane reading as a
        // hole in the page rather than as one that had said its piece.
        let pane_cols = if stacked {
            let split = Layout::horizontal([
                Constraint::Length(pane_col_w),
                Constraint::Min(0),
            ])
            .split(rows[5]);
            if let Some(boards) = &boards {
                render_board_box(
                    frame, split[1], boards, ui, focused, regions,
                );
            }
            stacked_pane_areas(split[0], &pane_wants, &pane_floors)
        } else {
            pane_areas(rows[5], &pane_widths)
        };
        render_panes(
            frame,
            state,
            cache,
            selected.as_ref(),
            &aboard_set,
            &greedy,
            ui,
            focused,
            panes,
            &pane_cols,
            regions,
        );
        rows[6]
    } else {
        render_placeholder(frame, rows[1], ui.voyage_type, focused);
        rows[2]
    };

    if !tooltip_lines.is_empty() {
        let text: Vec<Line> =
            tooltip_lines.iter().map(|l| Line::from(*l)).collect();
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            tip_area,
        );
    }

    // Modal popups, drawn last so they sit atop the page, each opening a layer
    // of the click map so the page beneath it answers to nothing.
    if let Some(sel) = ui.ship_popup {
        regions.layer();
        render_ship_popup(frame, sel, regions);
    } else if let Some(sel) = ui.vessel_popup {
        regions.layer();
        render_vessel_popup(frame, sel, &ordered, state, regions);
    } else if let Some(sel) = ui.voyage_popup {
        regions.layer();
        render_voyage_popup(frame, sel, regions);
    }

    // The pirate-stats popup, and the two popups layered over it: the trophies
    // and the note editor. Either is drawn over it rather than instead of it,
    // so the pirate it is about is still named behind it — and each opens a
    // layer of its own, so only the topmost of them answers to the mouse.
    if let Some(pp) = ui.pirate_popup.as_mut() {
        regions.layer();
        render_pirate_popup(frame, pp, cache, focused, regions);
    }
    if let Some(tp) = ui.trophy_popup.as_mut() {
        regions.layer();
        render_trophy_popup(frame, tp, cache, regions);
    }
    if let Some(np) = ui.note_popup.as_mut() {
        regions.layer();
        render_note_popup(frame, np, cache, regions);
    }

    // The Vampirates skill-distribution scatterplot (its own modal).
    if let Some(sd) = ui.skill_dist_popup {
        regions.layer();
        render_skill_dist_popup(frame, sd, &aboard_set, cache, regions);
    }

    // The per-fight advantage-over-time graph (its own modal).
    if let Some(pf) = ui.per_fight_popup {
        let fights = selected
            .as_ref()
            .map(|k| fight_timelines(state, k))
            .unwrap_or_default();
        regions.layer();
        render_per_fight_popup(frame, pf, &fights, regions);
    }

    // The roster prompt (its own modal), raised only when no other holds the
    // slot, so it is drawn last and nothing lands on top of it.
    if let Some(rp) = ui.roster_popup.as_ref() {
        regions.layer();
        render_roster_prompt(frame, rp, regions);
    }
}

/// The prompt asking whether a copied duty report's pirates should join the
/// roster. It lists the names it would add and says how much of the report we
/// already recognized, which is the whole reason it is asking.
fn render_roster_prompt(
    frame: &mut Frame,
    prompt: &RosterPrompt,
    regions: &mut ClickMap,
) {
    const CAP: usize = 8; // names listed before "...and N more"
    let area = frame.area();

    let shown = prompt.missing.len().min(CAP);
    let extra = prompt.missing.len().saturating_sub(CAP);
    let list_lines = shown + usize::from(0 < extra);
    let w: u16 = 52;
    let inner_w = w as usize - 4; // borders + horizontal padding

    let heading = match prompt.missing.len() {
        1 => {
            "The duty report names this pirate we don't have aboard.".to_owned()
        }
        n => {
            format!(
                "The duty report names these {n} pirates we don't have aboard."
            )
        }
    };
    let note = format!(
        "It shares only {} of its {} pirates with who we think is aboard. Are \
         ye sure that this be from the same vessel?",
        prompt.known, prompt.rated,
    );
    let heading_h = crate::utils::wrapped_line_count(&heading, inner_w);
    let note_h = crate::utils::wrapped_line_count(&note, inner_w);

    // borders, heading, blank, list, blank, note, blank, buttons
    let h = 2 + heading_h + 1 + list_lines as u16 + 1 + note_h + 1 + 1;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let (block, _) =
        crate::utils::titled_block("Update Roster?", inner_w as u16);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut constraints =
        vec![Constraint::Length(heading_h), Constraint::Length(1)];
    constraints.extend((0 .. list_lines).map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(note_h));
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // buttons
    let rows = Layout::vertical(constraints).split(inner);

    frame.render_widget(
        Paragraph::new(heading).wrap(Wrap {
            trim: true,
        }),
        rows[0],
    );
    for (i, name) in prompt.missing.iter().take(CAP).enumerate() {
        frame.render_widget(
            Paragraph::new(format!("  - {name}")),
            rows[2 + i],
        );
    }
    if 0 < extra {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("  ...and {extra} more"),
                Style::default().fg(Color::DarkGray),
            )),
            rows[2 + shown],
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            note,
            Style::default().fg(Color::Yellow),
        ))
        .wrap(Wrap {
            trim: true,
        }),
        rows[rows.len() - 3],
    );

    let buttons = crate::utils::render_buttons(
        frame,
        rows[rows.len() - 1],
        &["No", "Yes"],
        Some(usize::from(prompt.yes_focused)),
    );
    for (rect, target) in buttons
        .into_iter()
        .zip([ClickTarget::JobberRosterNo, ClickTarget::JobberRosterYes])
    {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
}

/// The current vessel's per-fight timelines for the graph popup, oldest first:
/// the completed Cursed Isles / Vampirate waves, then the in-progress wave (if
/// a fight is underway). Each is `(label, timeline)`.
fn fight_timelines(
    state: &GameState,
    key: &Arc<str>,
) -> Vec<(String, crate::voyage::FightTimeline)> {
    let Some(v) = state.vessels.get(key) else {
        return Vec::new();
    };
    // Cursed Isles uses island waves; Vampirates uses lair waves. Only one is
    // ever populated for a given run, so chain whichever has data.
    let (completed, active, cur_wave, cur_kind): (
        &[WaveRecord],
        bool,
        u32,
        WaveKind,
    ) = if !v.island_waves.is_empty() || v.island_active {
        (
            &v.island_waves,
            v.island_active,
            v.island_wave,
            v.wave_kind,
        )
    } else {
        (
            &v.lair_waves,
            v.lair_active,
            v.lair_wave,
            WaveKind::Swordfight,
        )
    };
    let mut out: Vec<(String, crate::voyage::FightTimeline)> = completed
        .iter()
        .map(|w| {
            (
                wave_label(w.wave, w.kind),
                w.timeline.clone(),
            )
        })
        .collect();
    if active {
        out.push((
            format!(
                "{} (current)",
                wave_label(cur_wave, cur_kind)
            ),
            v.wave_timeline.clone(),
        ));
    }
    out
}

/// Number of per-fight timelines available for a vessel (completed waves + the
/// in-progress one). Used by the app to bound the popup's fight index.
pub fn fight_count(state: &GameState, key: &Arc<str>) -> usize {
    fight_timelines(state, key).len()
}

/// "Wave N" or "Wave N (Rumble)" depending on whether the kind is known.
fn wave_label(wave: u32, kind: WaveKind) -> String {
    match kind {
        WaveKind::Unknown => format!("Wave {wave}"),
        k => format!("Wave {wave} ({})", wave_kind_label(k)),
    }
}

/// The per-fight statistics popup: a signed advantage-over-time line graph for
/// one fight (wave), with prev/next paging, an X-axis toggle (time ↔ KO
/// sequence), and a close button. Backdrop click closes. Modeled on
/// [`render_skill_dist_popup`].
fn render_per_fight_popup(
    frame: &mut Frame,
    popup: PerFightPopup,
    fights: &[(String, crate::voyage::FightTimeline)],
    regions: &mut ClickMap,
) {
    let area = frame.area();
    // Backdrop closes; pushed first so inner controls win the reverse hit test.
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::JobberPerFightClose,
    });

    const PLOT_H: usize = 9;
    let popup_w = 72u16.min(area.width.max(1));
    let popup_h = (
        PLOT_H as u16
            + 2 /*axis*/
            + 1 /*header*/
            + 2 /*the two button rows*/
            + 2
        // borders
    )
    .min(area.height.max(1));
    let popup_area = Rect::new(
        area.x + area.width.saturating_sub(popup_w) / 2,
        area.y + area.height.saturating_sub(popup_h) / 2,
        popup_w,
        popup_h,
    );
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(true))
        .padding(Padding::horizontal(1))
        .title(offset_title("Per-Fight Statistics").0);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if fights.is_empty() {
        // Nothing to graph until a fight has been had: the notice stands in
        // for the whole of the popup, so it is centered in all of it.
        crate::utils::render_notice(
            frame,
            inner,
            &[(
                "No fights recorded yet this run.",
                Style::default().fg(Color::DarkGray),
            )],
        );
        return;
    }
    let idx = popup.idx.min(fights.len() - 1);
    let (label, timeline) = &fights[idx];

    // Header: which fight, and the final advantage / start headcounts.
    let series = timeline.advantage_series(popup.axis);
    let final_adv = series.last().map(|&(_, v)| v).unwrap_or(0);
    let result = match timeline.their_start {
        Some(theirs) => {
            format!(
                "{label}   {} v {}   (final {}{})",
                timeline.our_start,
                theirs,
                if final_adv >= 0 { "+" } else { "" },
                final_adv,
            )
        }
        None => format!("{label}   (in progress)"),
    };
    let rows = Layout::vertical([
        Constraint::Length(1),                 // header
        Constraint::Length(PLOT_H as u16 + 2), // chart + axis
        Constraint::Length(1),                 // the fight's own controls
        Constraint::Length(1),                 // Close
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(Span::styled(
            result,
            Style::default().bold(),
        )),
        rows[0],
    );

    // Wave charts have no ship morale, so plot the raw headcount as floats (the
    // shared renderer is morale-weighted for sea battles).
    let fseries: Vec<(f64, f64)> =
        series.iter().map(|&(x, v)| (x, v as f64)).collect();
    let chart = fight_chart_lines(
        &fseries,
        rows[1].width as usize,
        PLOT_H,
        popup.axis,
    );
    frame.render_widget(Paragraph::new(chart), rows[1]);

    // What this fight's graph shows — stepping between fights, and what the
    // x-axis measures — then Close on its own row below, as every popup has it.
    let axis = match popup.axis {
        AxisMode::Time => "Axis: Time",
        AxisMode::Event => "Axis: # KOs",
    };
    for (rect, target) in crate::utils::render_buttons(
        frame,
        rows[2],
        &["← Prev", axis, "Next →"],
        None,
    )
    .into_iter()
    .zip([
        ClickTarget::JobberPerFightPrev,
        ClickTarget::JobberPerFightAxisToggle,
        ClickTarget::JobberPerFightNext,
    ]) {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
    regions.push(ClickRegion {
        rect: crate::utils::render_close_button(frame, rows[3]),
        target: ClickTarget::JobberPerFightClose,
    });
}

// ---------------------------------------------------------------------------
// Voyage box: vessel / ship-type / voyage-type buttons + Unpoison
// ---------------------------------------------------------------------------

/// The Voyage box at the top of the page: a 2-column `label | value` table
/// whose three value cells are buttons (each opens its picker popup), plus a
/// conditional Unpoison button and any staffing warning, all framed in one
/// bordered box.
#[allow(clippy::too_many_arguments)]
fn render_voyage_box(
    frame: &mut Frame,
    area: Rect,
    ui: &JobbersUi,
    selected: &Option<Arc<str>>,
    ship_idx: Option<usize>,
    poisoned: bool,
    provisional: bool,
    warn: Option<Staffing>,
    warn_lines: &[String],
    label_w: u16,
    page_focused: bool,
    regions: &mut ClickMap,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(page_focused))
        .padding(Padding::horizontal(1))
        .title(offset_title("Voyage").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Three label/value rows, then (when poisoned) a blank + Unpoison line,
    // then any warning lines, then slack.
    let mut constraints: Vec<Constraint> = vec![Constraint::Length(1); 3];
    if poisoned {
        constraints.push(Constraint::Length(1)); // blank
        constraints.push(Constraint::Length(1)); // unpoison
    }
    for _ in warn_lines {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let rows = Layout::vertical(constraints).split(inner);

    let vessel_value = selected
        .as_ref()
        .map(|k| k.to_string())
        .unwrap_or_else(|| "No vessel".to_string());
    let ship_value = ship_idx
        .map(|i| SHIPS[i].name.to_string())
        .unwrap_or_else(|| "Select ship".to_string());
    let voyage_value = ui.voyage_type.name().to_string();

    // The fourth field flags a placeholder value (nothing picked yet): it
    // renders greyed + italic when unfocused, matching the profits "Query
    // Market first" affordance.
    let entries: [(
        &str,
        String,
        bool,
        JobberFocus,
        ClickTarget,
    ); 3] = [
        (
            "Vessels",
            vessel_value,
            false,
            JobberFocus::Vessels,
            ClickTarget::JobberVesselButton,
        ),
        (
            "Ship Type",
            ship_value,
            ship_idx.is_none(),
            JobberFocus::ShipType,
            ClickTarget::JobberShipType,
        ),
        (
            "Voyage Type",
            voyage_value,
            false,
            JobberFocus::VoyageType,
            ClickTarget::JobberVoyageType,
        ),
    ];

    for (i, (label, value, placeholder, focus, target)) in
        entries.into_iter().enumerate()
    {
        let cols = Layout::horizontal([
            Constraint::Length(label_w),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(rows[i]);
        frame.render_widget(
            Paragraph::new(Span::styled(
                label,
                Style::default().bold(),
            )),
            cols[0],
        );
        let is_focused = page_focused && ui.focus == focus;
        // A jobbed vessel we can't name yet shows its `Ship of <crew>`
        // placeholder in italics, matching the selector popup.
        let provisional_row = provisional && focus == JobberFocus::Vessels;
        let value_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else if placeholder {
            Style::default().fg(Color::DarkGray).italic()
        } else if provisional_row {
            Style::default().italic()
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::raw(value)).right_aligned())
                .style(value_style),
            cols[2],
        );
        regions.push(ClickRegion {
            rect: rows[i],
            target,
        });
    }

    if poisoned {
        let btn_area = rows[4]; // rows: 0..2 table, 3 blank, 4 unpoison
        let btn_style = if page_focused && ui.focus == JobberFocus::Unpoison {
            Style::default().bg(Color::White).fg(Color::Black).bold()
        } else {
            Style::default().fg(Color::Red).bold()
        };
        frame.render_widget(
            Paragraph::new(
                Line::from(Span::styled("Unpoison", btn_style)).centered(),
            ),
            btn_area,
        );
        regions.push(ClickRegion {
            rect: btn_area,
            target: ClickTarget::JobberUnpoison,
        });
    }

    let warn_start = if poisoned { 5 } else { 3 };
    let warn_style = warn.map(Staffing::style).unwrap_or_default();
    for (j, w) in warn_lines.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(
                Line::from(Span::styled(w.clone(), warn_style)).centered(),
            ),
            rows[warn_start + j],
        );
    }
}

/// Short label for a Cursed Isles island wave's kind, shown beside the wave
/// number.
fn wave_kind_label(kind: WaveKind) -> &'static str {
    match kind {
        WaveKind::Unknown => "?",
        WaveKind::Swordfight => "Swordfight",
        WaveKind::Rumble => "Rumble",
    }
}

/// A small non-selectable `label | value` table shown between the Voyage box
/// and Top Jobbers: the Vampirates wave counts, the Vikings gunnery
/// breakdown, or the Cursed Isles fight statistics. One box, one row per
/// stat, plus optional centered note lines below (e.g. the Vampirates
/// "Mother o' Nyght has joined the fray!" / leave-the-fight reminder).
struct StatsBox {
    title: &'static str,
    rows: Vec<StatRow>,
    notes: Vec<Line<'static>>,
}

/// One `label | value` line in a [`StatsBox`]. By default the label is bold and
/// the value plain; a row may carry a `style` override applied to the whole row
/// instead (e.g. the Vikings "Not queried yet" row, dimmed and italic).
struct StatRow {
    label: String,
    value: String,
    style: Option<Style>,
}

impl StatRow {
    /// A normal row: bold label, plain value.
    fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            style: None,
        }
    }

    /// A row whose label and value both use `style` instead of the default.
    fn styled(
        label: impl Into<String>,
        value: impl Into<String>,
        style: Style,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            style: Some(style),
        }
    }
}

/// Natural outer width of a stats box: the widest of its `label  value` rows
/// (with a 2-space gap) and its centered note lines, plus borders + padding,
/// floored so the title stays readable.
fn stats_box_width(stats: &StatsBox) -> u16 {
    let rows = stats
        .rows
        .iter()
        .map(|r| r.label.chars().count() + 2 + r.value.chars().count())
        .max()
        .unwrap_or(0);
    let notes = stats.notes.iter().map(|l| l.width()).max().unwrap_or(0);
    (rows.max(notes) as u16 + 4).max(offset_title_width(stats.title))
}

/// Render a [`StatsBox`]: a bordered box of non-selectable `label | value`
/// rows, label bold on the left, value right-aligned on the right, each
/// spanning the inner width.
fn render_stats_box(
    frame: &mut Frame,
    area: Rect,
    stats: &StatsBox,
    focused: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .padding(Padding::horizontal(1))
        .title(offset_title(stats.title).0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    for (i, r) in stats.rows.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let row = Rect::new(
            inner.x,
            inner.y + i as u16,
            inner.width,
            1,
        );
        let cols =
            Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
                .split(row);
        // A styled row uses its style for both cells; otherwise the label is
        // bold and the value plain.
        let label_style = r.style.unwrap_or_else(|| Style::default().bold());
        let value_style = r.style.unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Span::styled(
                r.label.clone(),
                label_style,
            )),
            cols[0],
        );
        frame.render_widget(
            Paragraph::new(
                Line::from(Span::styled(
                    r.value.clone(),
                    value_style,
                ))
                .right_aligned(),
            ),
            cols[1],
        );
    }

    // Centered note lines below the rows, separated by one blank line.
    for (y, note) in (stats.rows.len() as u16 + 1 ..).zip(stats.notes.iter()) {
        if y >= inner.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(note.clone()).centered(),
            Rect::new(inner.x, inner.y + y, inner.width, 1),
        );
    }
}

/// The "View Skill Distribution" button (Vampirates): a single unboxed centered
/// line between Top Jobbers and the panes (styled like the Unpoison button).
/// Its row is a click target that opens the scatterplot popup.
fn render_skill_dist_button(
    frame: &mut Frame,
    area: Rect,
    page_focused: bool,
    active: bool,
    regions: &mut ClickMap,
) {
    let label_style = if page_focused && active {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().bold()
    };
    frame.render_widget(
        Paragraph::new(
            Line::from(Span::styled(
                SKILL_DIST_BUTTON_LABEL,
                label_style,
            ))
            .centered(),
        ),
        area,
    );
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::JobberSkillDistButton,
    });
}

/// The Vampirates skill-distribution popup: a Treasure Haul (x) × Carpentry (y)
/// scatterplot of aboard jobbers by standing, with a right-hand panel listing
/// the jobbers on the cursor cell. The cursor is moved by mouse hover (each
/// cell is a click region) or the arrow keys.
fn render_skill_dist_popup(
    frame: &mut Frame,
    popup: SkillDistPopup,
    aboard: &HashSet<String>,
    cache: &PirateCache,
    regions: &mut ClickMap,
) {
    // Plot geometry. The left margin holds the vertical "Carpentry" axis title
    // (its 9 letters line up with the 9 standing rows) and the per-row standing
    // label; the columns hold the Treasure Haul standings.
    const CELL_W: u16 = 5;
    const VAXIS_W: u16 = 2; // vertical "Carpentry" letter + a space
    const GUT: u16 = 4; // row standing abbr + "│", e.g. "Ult│"

    let data = skill_dist_data(aboard, cache);
    let counts = data.counts();
    let cursor = (
        popup.cursor.0.min(8),
        popup.cursor.1.min(8),
    );
    let here = data.at(cursor);
    let th_standing = STANDINGS[cursor.0 as usize];
    let carp_standing = STANDINGS[cursor.1 as usize];

    let area = frame.area();

    // ---- geometry ----
    let plot_w = VAXIS_W + GUT + 9 * CELL_W;
    let plot_h: u16 = 1 /*x-axis title*/ + 1 /*column header*/ + 9 /*standing rows*/;

    // Detail panel: a centered 3-line header naming the cursor cell's
    // standings, then the jobbers there (names only — their standings are
    // the cell itself).
    let count_line = format!(
        "{} jobber{} here with",
        here.len(),
        if here.len() == 1 { "" } else { "s" }
    );
    /// The count, then a line naming each of the cell's two standings.
    const HEADER_H: u16 = 3;
    let name_w = here
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(0);
    // Width accounting uses the widest possible standing line — "<longest
    // standing> Carpentry and" / "… Treasure Haul" — not the current
    // cursor's, so the panel doesn't resize as the cursor moves between
    // cells. (The longest standing name, "Distinguished", is even longer
    // than "Grand-Master", so every cell fits.)
    let widest_standing = STANDINGS
        .iter()
        .map(|s| s.to_string().chars().count())
        .max()
        .unwrap_or(0);
    let standing_line_w =
        widest_standing + " Carpentry and".len().max(" Treasure Haul".len());
    let detail_w = standing_line_w.max(name_w) as u16;

    // The "not plotted" footer wraps to the detail width.
    let note_lines: Vec<String> = if data.unplotted > 0 {
        wrap_words(
            &format!(
                "({} aboard not plotted — stats pending)",
                data.unplotted
            ),
            detail_w as usize,
        )
    } else {
        Vec::new()
    };

    let inner_w = plot_w + 2 + detail_w; // 2-col gap between plot and detail
    // detail = 3 header lines + blank + names + (blank + wrapped note).
    let mut detail_h = HEADER_H + 1 + here.len() as u16;
    if !note_lines.is_empty() {
        detail_h += 1 + note_lines.len() as u16;
    }
    let inner_h = plot_h.max(detail_h);

    let popup_w = (inner_w + 4).min(area.width.max(1)); // +2 borders +2 padding
    let popup_h = (inner_h + 2).min(area.height.max(1)); // +2 borders
    let popup_area = Rect::new(
        area.x + area.width.saturating_sub(popup_w) / 2,
        area.y + area.height.saturating_sub(popup_h) / 2,
        popup_w,
        popup_h,
    );

    // Backdrop: a click anywhere outside the cells closes the popup. Pushed
    // first so the per-cell regions below win the reverse-iterating hit
    // test.
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::JobberSkillDistClose,
    });

    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(true))
        .padding(Padding::horizontal(1))
        .title(offset_title("Skill Distribution").0);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let cols = Layout::horizontal([
        Constraint::Length(plot_w),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .split(inner);
    let plot = cols[0];
    let detail = cols[2];

    // ---- plot: centered "Treasure Haul" x-axis title, the column header, then
    // the      9 standing rows top (Ultimate) → bottom (Able), with the
    // vertical      "Carpentry" y-axis title down the left margin. ----
    let grid_x = plot.x + VAXIS_W + GUT;
    let cols_w = 9 * CELL_W;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Treasure Haul",
            Style::default().fg(Color::DarkGray),
        )))
        .centered(),
        Rect::new(grid_x, plot.y, cols_w, 1),
    );

    // Every standing on the axes is emphasised but Able, which is the floor
    // every pirate starts on and so says nothing about them.
    let axis_style = |s: Standing| {
        if s == Standing::Able {
            Style::default()
        } else {
            Style::default().bold()
        }
    };

    let header_y = plot.y + 1;
    for c in 0 .. 9u16 {
        let standing = STANDINGS[c as usize];
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                standing_abbr(standing),
                axis_style(standing),
            )))
            .centered(),
            Rect::new(grid_x + c * CELL_W, header_y, CELL_W, 1),
        );
    }

    let grid_y = header_y + 1;
    // The vertical "Carpentry" axis title: its 9 letters align with the 9 rows.
    let carp_axis: Vec<char> = "Carpentry".chars().collect();
    for r in 0 .. 9u16 {
        // Rows run high → low, so the top row is the highest standing.
        let carp = 8 - r;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                carp_axis[r as usize].to_string(),
                Style::default().fg(Color::DarkGray),
            ))),
            Rect::new(plot.x, grid_y + r, 1, 1),
        );
        // Row label (Carpentry standing), the gutter rule left unemphasised.
        let standing = STANDINGS[carp as usize];
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{:>3}", standing_abbr(standing)),
                    axis_style(standing),
                ),
                Span::raw("│"),
            ])),
            Rect::new(plot.x + VAXIS_W, grid_y + r, GUT, 1),
        );
        for c in 0 .. 9u16 {
            let cell = (c as u8, carp as u8);
            let n = counts[c as usize][carp as usize];
            let is_cursor = cell == cursor;
            let text = if n == 0 {
                "·".to_string()
            } else {
                n.to_string()
            };
            let style = if is_cursor {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else if n == 0 {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().bold()
            };
            let rect = Rect::new(
                grid_x + c * CELL_W,
                grid_y + r,
                CELL_W,
                1,
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(text, style)))
                    .centered(),
                rect,
            );
            regions.push(ClickRegion {
                rect,
                target: ClickTarget::JobberSkillDistCell {
                    th: c as u8,
                    carp: carp as u8,
                },
            });
        }
    }

    // ---- detail panel: centered header naming the cell's standings, then the
    //      jobbers there (names only — their standings are the cell itself).
    // ----
    // Line 0 is the count; lines 1-2 name the cell's two standings. Each reads
    // as one phrase — the skill in italics with its standing emphasised inside
    // it — and the "and" joining them belongs to neither, so it stays plain.
    let phrase = |standing: Standing, skill: &'static str| {
        let italic = Style::default().italic();
        vec![
            Span::styled(
                standing.to_string(),
                if standing == Standing::Able {
                    italic
                } else {
                    italic.bold()
                },
            ),
            Span::styled(format!(" {skill}"), italic),
        ]
    };
    let mut carp_line = phrase(carp_standing, "Carpentry");
    carp_line.push(Span::raw(" and"));
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            count_line,
            Style::default().bold(),
        ))
        .centered(),
        Line::from(carp_line).centered(),
        Line::from(phrase(th_standing, "Treasure Haul")).centered(),
        Line::from(""),
    ];
    for e in &here {
        lines.push(
            Line::from(Span::styled(
                e.name.clone(),
                name_colour(cache, &e.name),
            ))
            .centered(),
        );
    }
    if !note_lines.is_empty() {
        lines.push(Line::from(""));
        for note in &note_lines {
            lines.push(
                Line::from(Span::styled(
                    note.clone(),
                    Style::default().fg(Color::DarkGray).italic(),
                ))
                .centered(),
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), detail);
}

/// Placeholder shown in place of the Pillage-only Top Jobbers + panes when the
/// selected voyage type isn't wired up yet.
/// Rows [`render_placeholder`] draws in: its one line, and its border.
const PLACEHOLDER_H: u16 = 3;

fn render_placeholder(
    frame: &mut Frame,
    area: Rect,
    voyage_type: VoyageType,
    focused: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(offset_title("Coming Soon").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(format!(
            "{} voyages aren't supported yet.",
            voyage_type.name()
        ))
        .centered(),
        inner,
    );
}

// ---------------------------------------------------------------------------
// Bottom panes: Aboard | Greedy | Planked, side by side, with per-pane
// selection
// ---------------------------------------------------------------------------

/// Clamp a stored pane selection to the live pirate count.
fn clamp_sel(sel: usize, n: usize) -> usize {
    if n == 0 { 0 } else { sel.min(n - 1) }
}

/// The Aboard pane's "Pirates (n):" header, where `n` is the number aboard.
fn aboard_header(n: usize) -> String {
    format!("Pirates ({n}):")
}

/// The Aboard pane's swabbie footer, pluralised: "and 1 swabbie" / "and N
/// swabbies".
fn swabbie_footer(n: u32) -> String {
    if n == 1 {
        "and 1 swabbie".to_string()
    } else {
        format!("and {n} swabbies")
    }
}

fn pane_focus_target(pane: JobberPane) -> ClickTarget {
    match pane {
        JobberPane::Aboard => ClickTarget::JobberAboardList,
        JobberPane::Greedy => ClickTarget::JobberGreedyList,
        JobberPane::Planked => ClickTarget::JobberPlankedList,
        JobberPane::Enthralled => ClickTarget::JobberEnthralledList,
    }
}

/// Selectable pirate names in a pane, in the same display order `render_panes`
/// uses — Aboard & Planked alphabetical, Greedy by current-fight strikes desc,
/// then run-total desc, then name.
/// The single source of truth for mapping a pane selection index to a pirate.
pub fn pane_pirates(
    state: &GameState,
    key: &Arc<str>,
    pane: JobberPane,
) -> Vec<String> {
    match pane {
        JobberPane::Aboard => {
            let mut v: Vec<String> = state.aboard(key).into_iter().collect();
            v.sort_unstable();
            v
        }
        JobberPane::Greedy => {
            let mut g: Vec<(String, u32, u32)> = state
                .vessels
                .get(key)
                .map(|v| {
                    v.greedy_by_pirate
                        .iter()
                        .map(|(n, t)| {
                            let current =
                                v.greedy_current.get(n).copied().unwrap_or(0);
                            (n.clone(), *t, current)
                        })
                        .collect()
                })
                .unwrap_or_default();
            g.sort_by(|a, b| {
                b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0))
            });
            g.into_iter().map(|(n, ..)| n).collect()
        }
        JobberPane::Planked => {
            // BTreeSet already iterates alphabetically.
            state
                .vessels
                .get(key)
                .map(|v| v.planked_by_us.iter().cloned().collect())
                .unwrap_or_default()
        }
        JobberPane::Enthralled => {
            enthralled_ranked(state, key)
                .into_iter()
                .map(|(n, ..)| n)
                .collect()
        }
    }
}

/// The Enthralled leaderboard rows for a vessel: `(pirate, live thralls,
/// lifetime enthralled)`, ranked by lifetime total descending, then name. Every
/// pirate who has ever enthralled appears (even with zero alive now). The
/// single source of truth for both the rendered order and the pane's
/// index→pirate mapping.
fn enthralled_ranked(
    state: &GameState,
    key: &Arc<str>,
) -> Vec<(String, u32, u32)> {
    let Some(v) = state.vessels.get(key) else {
        return Vec::new();
    };
    let mut rows: Vec<(String, u32, u32)> = v
        .thralls_total
        .iter()
        .map(|(n, total)| {
            (
                n.clone(),
                v.thralls_alive.get(n).copied().unwrap_or(0),
                *total,
            )
        })
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    rows
}

/// The pirate names in each Skill Leaderboard column, ranked exactly as
/// `render_top_panel` shows them (best standing, then experience, then name)
/// with no truncation — column-major. Used to resolve a `(top_col, top_sel)`
/// cursor to a pirate and to size leaderboard navigation. Mirrors
/// [`pane_pirates`].
pub fn leaderboard_columns(
    state: &GameState,
    cache: &PirateCache,
    key: &Arc<str>,
    voyage_type: VoyageType,
) -> Vec<Vec<String>> {
    let aboard = state.aboard(key);
    rank_columns(
        voyage_type.top_jobbers(),
        &aboard,
        cache,
        None,
    )
    .into_iter()
    .map(|c| c.rows.into_iter().map(|r| r.name).collect())
    .collect()
}

#[allow(clippy::too_many_arguments)]
/// Render each pane into the area `cols` gives it, in `panes` order. Where
/// they sit is the caller's: side by side ([`pane_areas`]) or one over the
/// other ([`stacked_pane_areas`]).
fn render_panes(
    frame: &mut Frame,
    state: &GameState,
    cache: &PirateCache,
    selected: Option<&Arc<str>>,
    aboard_set: &HashSet<String>,
    greedy: &[(&String, u32, u32)],
    ui: &mut JobbersUi,
    focused: bool,
    panes: &[JobberPane],
    cols: &[Rect],
    regions: &mut ClickMap,
) {
    if panes.is_empty() {
        return;
    }

    let my_crew = my_crew_name(state, cache);
    let is_player = |name: &str| {
        state
            .player_name
            .as_deref()
            .is_some_and(|me| name.eq_ignore_ascii_case(me))
    };
    // Green says what the pirate is and emphasis says who they are to us, so
    // the two stack rather than compete for the name. Crew membership is
    // not among them: a tag naming the rank says it where there is room for
    // one, and bold could say neither which crew nor what rank.
    let style_for = |name: &str| -> Style {
        let colour = name_colour(cache, name);
        if is_player(name) {
            colour.bold().italic()
        } else {
            colour
        }
    };

    let vessel = selected.and_then(|k| state.vessels.get(k));

    // The Enthralled leaderboard rows: (pirate, live thralls, lifetime
    // enthralled), ranked by total. Built once here for both clamping and
    // rendering.
    let enthralled: Vec<(String, u32, u32)> = selected
        .map(|k| enthralled_ranked(state, k))
        .unwrap_or_default();

    // Clamp every pane's selection up front, whether or not it's shown.
    ui.aboard_sel = clamp_sel(ui.aboard_sel, aboard_set.len());
    ui.greedy_sel = clamp_sel(ui.greedy_sel, greedy.len());
    let planked_n = vessel.map(|v| v.planked_by_us.len()).unwrap_or(0);
    ui.planked_sel = clamp_sel(ui.planked_sel, planked_n);
    ui.enthralled_sel = clamp_sel(ui.enthralled_sel, enthralled.len());

    // Render only the panes this voyage type asks for, in order.
    for (i, pane) in panes.iter().enumerate() {
        let Some(col) = cols.get(i).copied() else {
            break;
        };
        match pane {
            // -- Aboard: a pinned "Pirates (n):" header, an indented scrollable
            // name    list, then a pinned swabbie footer. --
            JobberPane::Aboard => {
                let mut aboard: Vec<&String> = aboard_set.iter().collect();
                aboard.sort_unstable();
                let header = Line::from(Span::raw(aboard_header(aboard.len())));
                let names: Vec<(Line, Tags)> = aboard
                    .iter()
                    .map(|n| {
                        (
                            Line::from(vec![
                                Span::raw(" ".repeat(ABOARD_INDENT)),
                                Span::styled((*n).clone(), style_for(n)),
                            ]),
                            aboard_tags(cache, my_crew.as_deref(), vessel, n),
                        )
                    })
                    .collect();
                let mut footers: Vec<Line> = Vec::new();
                let swabbies = vessel.map_or(0, |v| v.swabbies);
                if swabbies > 0 {
                    footers.push(Line::from(Span::styled(
                        swabbie_footer(swabbies),
                        Style::default().italic(),
                    )));
                }
                render_aboard_pane(
                    frame,
                    col,
                    header,
                    names,
                    footers,
                    ui.aboard_sel,
                    &mut ui.aboard_offset,
                    focused,
                    ui.focus == JobberFocus::Aboard,
                    regions,
                );
            }
            // -- Greedy (current-fight desc, then run-total desc, then
            // alphabetical) --
            JobberPane::Greedy => {
                let mut greedy_sorted: Vec<(&String, u32, u32)> =
                    greedy.to_vec();
                greedy_sorted.sort_by(|a, b| {
                    b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(b.0))
                });
                let inner_w = (col.width.saturating_sub(4) as usize)
                    .max(name_col_plus_value(&greedy_sorted));
                let rows: Vec<PaneRow> = greedy_sorted
                    .iter()
                    .enumerate()
                    .map(|(i, (name, total, current))| {
                        PaneRow {
                            line: greedy_line(
                                name,
                                *total,
                                *current,
                                inner_w,
                                style_for(name),
                            ),
                            pirate: Some(i),
                            tags: Vec::new(),
                        }
                    })
                    .collect();
                render_pane(
                    frame,
                    col,
                    "Greedy",
                    rows,
                    ui.greedy_sel,
                    &mut ui.greedy_offset,
                    focused,
                    ui.focus == JobberFocus::Greedy,
                    JobberPane::Greedy,
                    regions,
                );
            }
            // -- Planked (alphabetical; BTreeSet already iterates in order) --
            JobberPane::Planked => {
                let planked: Vec<String> = vessel
                    .map(|v| v.planked_by_us.iter().cloned().collect())
                    .unwrap_or_default();
                let rows: Vec<PaneRow> = planked
                    .iter()
                    .enumerate()
                    .map(|(i, n)| {
                        PaneRow {
                            line: Line::from(Span::styled(
                                n.clone(),
                                style_for(n),
                            )),
                            pirate: Some(i),
                            tags: crew_tag(cache, my_crew.as_deref(), n)
                                .into_iter()
                                .collect(),
                        }
                    })
                    .collect();
                render_pane(
                    frame,
                    col,
                    "Planked",
                    rows,
                    ui.planked_sel,
                    &mut ui.planked_offset,
                    focused,
                    ui.focus == JobberFocus::Planked,
                    JobberPane::Planked,
                    regions,
                );
            }
            // -- Enthralled (Cursed Isles): "name  alive/total", ranked by
            // total. --
            JobberPane::Enthralled => {
                let inner_w = (col.width.saturating_sub(4) as usize)
                    .max(enthralled_col_width(&enthralled));
                let rows: Vec<PaneRow> = enthralled
                    .iter()
                    .enumerate()
                    .map(|(i, (name, alive, total))| {
                        PaneRow {
                            line: enthralled_line(
                                name,
                                *alive,
                                *total,
                                inner_w,
                                style_for(name),
                            ),
                            pirate: Some(i),
                            tags: Vec::new(),
                        }
                    })
                    .collect();
                render_pane(
                    frame,
                    col,
                    "Enthralled",
                    rows,
                    ui.enthralled_sel,
                    &mut ui.enthralled_offset,
                    focused,
                    ui.focus == JobberFocus::Enthralled,
                    JobberPane::Enthralled,
                    regions,
                );
            }
        }
    }
}

/// Where each pane goes in a row they share side by side.
///
/// Each starts from its natural (content) width, and any slack — the block may
/// be wider than the panes combined when Top Jobbers or the Voyage box is the
/// widest piece — is spread evenly rather than all dumped into the last pane.
fn pane_areas(area: Rect, widths: &[u16]) -> Vec<Rect> {
    let n = widths.len();
    if n == 0 {
        return Vec::new();
    }
    let slack = area.width.saturating_sub(widths.iter().sum());
    let add = slack / n as u16;
    let rem = slack % n as u16;
    let constraints: Vec<Constraint> = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if i + 1 == n {
                // The last pane absorbs the remainder so rounding leaves no
                // gap.
                Constraint::Min(0)
            } else {
                Constraint::Length(w + add + u16::from((i as u16) < rem))
            }
        })
        .collect();
    Layout::horizontal(constraints).split(area).to_vec()
}

/// Where each pane goes in a column they share one over the other.
///
/// The panes fight each other for the rows: each is floored at what it cannot
/// read without (`floors`), and what is left over is shared in proportion to
/// the rows each would fill (`wants`), so the longer roster draws the larger
/// share. Neither is given more than it can fill while the other still has a
/// list to scroll, and rows nobody can fill go to the first pane, the page
/// reading better with its slack at the top than between the two.
fn stacked_pane_areas(area: Rect, wants: &[u16], floors: &[u16]) -> Vec<Rect> {
    let mut given: Vec<u16> = floors.to_vec();
    let mut spare = area.height.saturating_sub(floors.iter().sum::<u16>());
    let needs: Vec<u16> = wants
        .iter()
        .zip(floors)
        .map(|(want, floor)| want.saturating_sub(*floor))
        .collect();
    let total: u16 = needs.iter().sum();
    if total <= spare {
        // Room for every pane's whole list: the rest goes to the first.
        for (give, need) in given.iter_mut().zip(&needs) {
            *give += need;
        }
        spare -= total;
        if let Some(first) = given.first_mut() {
            *first += spare;
        }
    } else if 0 < total {
        // Short of that, each takes a share of the spare rows in proportion
        // to the list it has waiting, the odd rows going to the hungriest.
        let mut shared = 0;
        for (give, need) in given.iter_mut().zip(&needs) {
            let share = (spare as u32 * *need as u32 / total as u32) as u16;
            *give += share;
            shared += share;
        }
        let mut left = spare - shared;
        let mut order: Vec<usize> = (0 .. needs.len()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(needs[*i]));
        for i in order {
            if left == 0 {
                break;
            }
            given[i] += 1;
            left -= 1;
        }
    }
    Layout::vertical(given.into_iter().map(Constraint::Length))
        .split(area)
        .to_vec()
}

// ---------------------------------------------------------------------------
// Tokens and Chests: what a run's duty reports counted
// ---------------------------------------------------------------------------

/// The token shapes an Atlantis board columns, in slot order. The flower is
/// earned only while attacking the Cursed Isles, so it is no column of this
/// one; the encounter that pays it would board it itself.
const ATLANTIS_TOKENS: &[crate::duty::TokenShape] = &[
    crate::duty::TokenShape::Circle,
    crate::duty::TokenShape::Diamond,
    crate::duty::TokenShape::Plus,
    crate::duty::TokenShape::Cross,
];

/// The chest tiers a haul board columns, smallest first.
const CHEST_TIERS: &[crate::duty::ChestTier] = &[
    crate::duty::ChestTier::Box,
    crate::duty::ChestTier::Locker,
    crate::duty::ChestTier::Chest,
];

/// The head over a board's name column.
const BOARD_NAME_HEAD: &str = "Pirate";

/// Head of the column holding a row's figures added together.
const BOARD_SUM_HEAD: &str = "\u{03a3}";

/// Columns the ranking mark takes ahead of a column's head: the arrow and the
/// blank between. Every figure cell keeps them, so the mark can move from
/// column to column without a cell moving with it.
const BOARD_MARK_W: usize = 2;

/// Blank columns between a board's columns.
const BOARD_GAP: usize = 2;

/// Title of the box both boards are shown in.
const BOARD_BOX_TITLE: &str = "Tokens and Chests";

/// Blank columns either side of a tab's label within its slot.
const BOARD_TAB_PADDING: u16 = 1;

/// Which leaderboard the Tokens and Chests box is showing.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoardTab {
    #[default]
    Tokens,
    Treasures,
}

impl BoardTab {
    /// The tab's label on the strip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tokens => "Tokens",
            Self::Treasures => "Treasures",
        }
    }
}

/// What a board is ranked on: one of its figure columns, or their sum.
///
/// The sum is a key and not a column index so that it means the same thing on
/// a board of four figures as on one of three.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoardKey {
    #[default]
    Sum,
    /// Index into the board's figure columns, the sum aside.
    Figure(usize),
}

/// How a board is ranked: on what, and which way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ranking {
    pub key: BoardKey,
    pub desc: bool,
}

impl Default for Ranking {
    /// Most of whatever it counts first — what a leaderboard is for.
    fn default() -> Self {
        Self {
            key: BoardKey::Sum,
            desc: true,
        }
    }
}

impl Ranking {
    /// This ranking after a click on the column `key` names: the same column
    /// turns around, a different one starts from the top.
    fn clicked(self, key: BoardKey) -> Self {
        Self {
            key,
            desc: self.key != key || !self.desc,
        }
    }
}

/// One pirate's standing on a board: their figures in column order and the
/// sum of them.
pub struct BoardRow {
    pub name: String,
    pub figures: Vec<u32>,
    pub sum: u32,
}

/// One leaderboard of the Tokens and Chests box: a column per figure the
/// encounter counts, the sum last, and a row per pirate who produced some of
/// it.
pub struct Board {
    pub tab: BoardTab,
    /// Heads over the figure columns, left to right. The sum's own head is
    /// [`BOARD_SUM_HEAD`] and no part of this.
    pub heads: Vec<&'static str>,
    pub rows: Vec<BoardRow>,
}

/// Both of the box's leaderboards, each present only where the run has that
/// figure to show. Never both absent: the box is not drawn then.
pub struct Boards {
    pub tokens: Option<Board>,
    pub treasures: Option<Board>,
}

impl Board {
    /// The rows ranked as `ranking` asks, ties by name so the order is the
    /// same board every frame.
    fn ranked(&self, ranking: Ranking) -> Vec<&BoardRow> {
        let figure = |row: &BoardRow| {
            match ranking.key {
                BoardKey::Sum => row.sum,
                BoardKey::Figure(i) => row.figures.get(i).copied().unwrap_or(0),
            }
        };
        let mut rows: Vec<&BoardRow> = self.rows.iter().collect();
        rows.sort_by(|a, b| {
            let (x, y) = (figure(a), figure(b));
            if ranking.desc {
                y.cmp(&x).then_with(|| a.name.cmp(&b.name))
            } else {
                x.cmp(&y).then_with(|| a.name.cmp(&b.name))
            }
        });
        rows
    }

    /// The key a click on column `col` ranks by, counting the sum's own
    /// column at the end.
    pub fn key_at(&self, col: usize) -> BoardKey {
        if col < self.heads.len() {
            BoardKey::Figure(col)
        } else {
            BoardKey::Sum
        }
    }

    /// The key after `key`, walking the figure columns in turn and then the
    /// sum, round again. What the keyboard ranks by, a column at a time.
    pub fn next_key(&self, key: BoardKey) -> BoardKey {
        let col = match key {
            BoardKey::Figure(i) if i + 1 < self.heads.len() => {
                BoardKey::Figure(i + 1)
            }
            BoardKey::Figure(_) => BoardKey::Sum,
            BoardKey::Sum => BoardKey::Figure(0),
        };
        // A board of no figure columns can only ever rank by its sum.
        if self.heads.is_empty() {
            BoardKey::Sum
        } else {
            col
        }
    }
}

impl Boards {
    /// The tabs on show, in strip order. A figure the run has nothing of has
    /// no tab: there is nothing it would say.
    pub fn tabs(&self) -> Vec<BoardTab> {
        [&self.tokens, &self.treasures]
            .into_iter()
            .flatten()
            .map(|board| board.tab)
            .collect()
    }

    /// The board `tab` shows, if that tab is on show at all.
    pub fn tab_board(&self, tab: BoardTab) -> Option<&Board> {
        match tab {
            BoardTab::Tokens => self.tokens.as_ref(),
            BoardTab::Treasures => self.treasures.as_ref(),
        }
    }

    /// The tab after the one on show, round the strip. The same tab back
    /// where it is the only one: there is nowhere else to be.
    pub fn next_tab(&self, showing: BoardTab) -> BoardTab {
        let tabs = self.tabs();
        let next = tabs
            .iter()
            .position(|tab| *tab == showing)
            .map_or(0, |i| (i + 1) % tabs.len());
        tabs.get(next).copied().unwrap_or(showing)
    }

    /// The tab the box draws: the one asked for, or the first on the strip
    /// where that one has nothing to show.
    pub fn showing(&self, asked: BoardTab) -> BoardTab {
        if self.tab_board(asked).is_some() {
            return asked;
        }
        self.tabs().first().copied().unwrap_or(asked)
    }
}

/// The Tokens and Chests boards for the selected vessel, or `None` where the
/// box is not drawn at all.
///
/// Three things have to hold. The page must be the Atlantis one, since this
/// is the layout the box belongs to. The run must be an Atlantis one *by its
/// own tells* — a dragoon boarding says so and the voyage-type picker is only
/// the quartermaster's word, and the tells re-arm per run. And a report of the
/// run must have carried maneuver tokens or a haul, because a board of nobody
/// says nothing worth the room.
///
/// Until all three hold the figures are still kept: every copied report joins
/// the run and reaches disk whatever the page is drawing, so the board that
/// finally appears has everything set aside before it in its sums.
pub fn boards(
    state: &GameState,
    selected: Option<&Arc<str>>,
    voyage_type: VoyageType,
) -> Option<Boards> {
    if !voyage_type.tracks_atlantis() {
        return None;
    }
    let vessel = selected.and_then(|key| state.vessels.get(key))?;
    if vessel.encounter != crate::chatlog::EncounterKind::Atlantis {
        return None;
    }
    let reports = &latest_run(vessel)?.duty_reports;

    let tokens = board(
        BoardTab::Tokens,
        ATLANTIS_TOKENS
            .iter()
            .map(|shape| (shape.glyph(), shape.slot()))
            .collect(),
        crate::duty::maneuvers(reports),
    );
    let treasures = board(
        BoardTab::Treasures,
        CHEST_TIERS
            .iter()
            .map(|tier| (tier.initial(), tier.slot()))
            .collect(),
        crate::duty::treasure(reports),
    );
    (tokens.is_some() || treasures.is_some()).then_some(Boards {
        tokens,
        treasures,
    })
}

/// A board of `counted`, or `None` where nobody counted any.
///
/// `columns` names each column and the slot of the figure it shows, so a
/// board shows the slots its encounter pays and leaves the game's spare ones
/// out without the sums losing anything: the sum is of the columns shown,
/// which is what the rows are ranked on.
fn board<const N: usize>(
    tab: BoardTab,
    columns: Vec<(&'static str, usize)>,
    counted: Vec<crate::duty::Counted<N>>,
) -> Option<Board> {
    let rows: Vec<BoardRow> = counted
        .into_iter()
        .map(|counted| {
            let figures: Vec<u32> = columns
                .iter()
                .map(|(_, slot)| {
                    counted.counts.get(*slot).copied().unwrap_or(0)
                })
                .collect();
            BoardRow {
                sum: figures.iter().sum(),
                figures,
                name: counted.name,
            }
        })
        .filter(|row| 0 < row.sum)
        .collect();
    (!rows.is_empty()).then(|| {
        Board {
            tab,
            heads: columns.into_iter().map(|(head, _)| head).collect(),
            rows,
        }
    })
}

/// The run a vessel's figures are read from: the one under way, or the last it
/// finished where none is.
///
/// A run that has put into port is the run whose rewards are being handed out,
/// so it stays the one on show until the next begins.
fn latest_run(vessel: &Vessel) -> Option<&crate::voyage::Voyage> {
    vessel
        .current_voyage
        .as_ref()
        .or_else(|| vessel.voyages.last())
}

/// Columns each of a board's figure cells takes, in column order with the
/// sum's last: its head and whatever its widest figure needs.
fn board_cells(board: &Board) -> Vec<usize> {
    let digits = |n: u32| n.to_string().len();
    let mut cells: Vec<usize> = board
        .heads
        .iter()
        .enumerate()
        .map(|(i, head)| {
            let widest = board
                .rows
                .iter()
                .map(|row| digits(row.figures.get(i).copied().unwrap_or(0)))
                .max()
                .unwrap_or(0);
            widest.max(head.chars().count() + BOARD_MARK_W)
        })
        .collect();
    let sums = board
        .rows
        .iter()
        .map(|row| digits(row.sum))
        .max()
        .unwrap_or(0);
    cells.push(sums.max(BOARD_SUM_HEAD.chars().count() + BOARD_MARK_W));
    cells
}

/// Columns the name column takes: the longest name either board holds, and
/// never less than its own head.
///
/// Measured over both boards at once so that the figures stand in the same
/// columns on each, and a flip of the tab moves nothing but the figures.
fn board_name_width(boards: &Boards) -> usize {
    [&boards.tokens, &boards.treasures]
        .into_iter()
        .flatten()
        .flat_map(|board| board.rows.iter())
        .map(|row| row.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(BOARD_NAME_HEAD.len())
}

/// Columns the box's contents occupy: the name column, then every figure cell
/// behind its gap, the wider board deciding.
///
/// The box takes the width of its wider tab, so flipping tabs never reflows
/// the page.
fn board_content_width(boards: &Boards) -> u16 {
    let cells = [&boards.tokens, &boards.treasures]
        .into_iter()
        .flatten()
        .map(|board| {
            board_cells(board)
                .iter()
                .map(|w| w + BOARD_GAP)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    (board_name_width(boards) + cells) as u16
}

/// The width the Tokens and Chests box must have: its contents inside their
/// margins, the room its list keeps for a scrollbar, and never less than what
/// its title or its tab strip needs.
fn board_box_width(boards: &Boards) -> u16 {
    let labels: Vec<u16> = boards
        .tabs()
        .iter()
        .map(|tab| tab.label().len() as u16)
        .collect();
    let strip: u16 = labels.iter().map(|w| w + 2 * BOARD_TAB_PADDING).sum();
    (board_content_width(boards)
        + crate::utils::BOX_MARGIN
        + crate::utils::SCROLLBAR_W)
        .max(strip + crate::utils::BOX_MARGIN)
        .max(offset_title_width(BOARD_BOX_TITLE))
}

/// Rows the box cannot do without: its tab strip and the blank under it, the
/// column heads, a scrollable view's worth of ranking, and its borders.
fn board_box_min_height() -> u16 {
    3 + crate::utils::SCROLL_MIN_ROWS + 2
}

/// Render the Tokens and Chests box: a tab strip over a ranked table of what
/// the run's duty reports counted.
fn render_board_box(
    frame: &mut Frame,
    area: Rect,
    boards: &Boards,
    ui: &mut JobbersUi,
    page_focused: bool,
    regions: &mut ClickMap,
) {
    let active = ui.focus == JobberFocus::Board;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(box_border(page_focused, active))
        .padding(Padding::horizontal(
            crate::utils::PADDING,
        ))
        .title(offset_title(BOARD_BOX_TITLE).0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole-box focus region first, so the strip's and the heads' regions
    // pushed below win the reverse-iterating hit test where they overlap.
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::JobberBoard,
    });
    if inner.height == 0 {
        return;
    }

    // A tab the run has nothing for is no tab, so the box falls back to the
    // one it has rather than drawing a strip nothing is under.
    ui.board_tab = boards.showing(ui.board_tab);
    let tabs = boards.tabs();
    render_board_tabs(
        frame,
        inner,
        &tabs,
        ui.board_tab,
        regions,
    );
    let Some(board) = boards.tab_board(ui.board_tab) else {
        return;
    };
    // Tab strip, a blank under it, then the heads: the ranking is read under
    // its own columns rather than under the tabs.
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);

    let ranking = ui.board_ranking(board.tab);
    let cells = board_cells(board);
    // One name column over both boards, so the figures of either stand in the
    // same place and a flip of the tab moves nothing but the figures.
    let names = board_name_width(boards);
    let indent = board_indent(inner, &cells, names);
    frame.render_widget(
        Paragraph::new(board_head_line(
            board, &cells, names, indent, ranking,
        )),
        rows[2],
    );
    for (col, rect) in board_head_regions(rows[2], &cells, names, indent) {
        regions.push(ClickRegion {
            rect,
            target: ClickTarget::JobberBoardColumn(col),
        });
    }

    let ranked = board.ranked(ranking);
    let offset = ui.board_offset_mut(board.tab);
    let height = rows[3].height as usize;
    let max_off = ranked.len().saturating_sub(height);
    if max_off < *offset {
        *offset = max_off;
    }
    let offset = *offset;
    let body = crate::utils::render_scrollbar(
        frame,
        regions,
        rows[3],
        crate::clickmap::ScrollView::JobberBoard,
        offset,
        ranked.len(),
    );
    for (vis, row) in ranked.iter().enumerate().skip(offset).take(height) {
        frame.render_widget(
            Paragraph::new(board_row_line(
                row, &cells, names, indent,
            )),
            Rect::new(
                body.x,
                body.y + (vis - offset) as u16,
                body.width,
                1,
            ),
        );
    }
}

/// Render the tab strip: a slot per tab across the box, each label centered in
/// its own and the whole slot the click region for it. The strip the top bar
/// is, one box down.
fn render_board_tabs(
    frame: &mut Frame,
    inner: Rect,
    tabs: &[BoardTab],
    showing: BoardTab,
    regions: &mut ClickMap,
) {
    let labels: Vec<u16> =
        tabs.iter().map(|tab| tab.label().len() as u16).collect();
    let slots =
        crate::utils::bar_slots(inner.width, &labels, BOARD_TAB_PADDING);
    let areas =
        Layout::horizontal(slots.into_iter().map(Constraint::Length)).split(
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    for ((i, tab), slot) in tabs.iter().enumerate().zip(areas.iter()) {
        let style = if *tab == showing {
            Style::default().bg(Color::White).fg(Color::Black).bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(
            Paragraph::new(tab.label()).style(style).centered(),
            *slot,
        );
        regions.push(ClickRegion {
            rect: *slot,
            target: ClickTarget::JobberBoardTab(i),
        });
    }
}

/// Columns the table itself occupies: the name column and every figure cell
/// behind its gap.
fn board_table_width(cells: &[usize], names: usize) -> usize {
    names + cells.iter().map(|width| width + BOARD_GAP).sum::<usize>()
}

/// Blank columns before the table so it stands centered in the box.
///
/// Measured against the room the list has less the columns it keeps for a
/// scrollbar, so the table stands where it stands whether or not the ranking
/// has outgrown its window — a board that shifted sideways as it filled would
/// read as two different tables.
fn board_indent(inner: Rect, cells: &[usize], names: usize) -> usize {
    let room = inner.width.saturating_sub(crate::utils::SCROLLBAR_W) as usize;
    room.saturating_sub(board_table_width(cells, names)) / 2
}

/// A board's head row: the name column, then a head to each figure cell with
/// the ranking's arrow ahead of whichever it ranks on.
///
/// Each head is underlined over its own text and no further, the way the
/// Skill Leaderboard heads its columns.
fn board_head_line(
    board: &Board,
    cells: &[usize],
    names: usize,
    indent: usize,
    ranking: Ranking,
) -> Line<'static> {
    let style = Style::default().bold().underlined();
    let mut spans = vec![Span::raw(" ".repeat(indent))];

    // The name's head stands centered over its column, where the names
    // themselves are read down the left.
    let pad = names.saturating_sub(BOARD_NAME_HEAD.len());
    spans.push(Span::raw(" ".repeat(pad / 2)));
    spans.push(Span::styled(BOARD_NAME_HEAD, style));
    spans.push(Span::raw(" ".repeat(pad - pad / 2)));

    for (col, width) in cells.iter().enumerate() {
        let head = board.heads.get(col).copied().unwrap_or(BOARD_SUM_HEAD);
        let marked = if board.key_at(col) == ranking.key {
            let arrow = if ranking.desc { "\u{2193}" } else { "\u{2191}" };
            format!("{arrow} {head}")
        } else {
            head.to_owned()
        };
        // The cell's own columns count glyphs, not bytes: a token's shape is
        // one column and more than one byte.
        let pad = width.saturating_sub(marked.chars().count()) + BOARD_GAP;
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(marked, style));
    }
    Line::from(spans)
}

/// One ranked row: the pirate, their figures under the heads, their sum last.
fn board_row_line(
    row: &BoardRow,
    cells: &[usize],
    names: usize,
    indent: usize,
) -> Line<'static> {
    let mut line = format!("{:indent$}{:<names$}", "", row.name);
    for (col, width) in cells.iter().enumerate() {
        let figure = row.figures.get(col).copied().unwrap_or(row.sum);
        line.push_str(&format!(
            "{:>w$}",
            figure,
            w = width + BOARD_GAP
        ));
    }
    Line::from(line)
}

/// Where each of a board's heads was drawn, so a click on one can rank by it.
/// The name column is no part of this: the board ranks on figures.
fn board_head_regions(
    row: Rect,
    cells: &[usize],
    names: usize,
    indent: usize,
) -> Vec<(usize, Rect)> {
    let mut x = row.x + (indent + names) as u16;
    let mut regions = Vec::with_capacity(cells.len());
    for (col, width) in cells.iter().enumerate() {
        let span = (width + BOARD_GAP) as u16;
        if row.x + row.width <= x {
            break;
        }
        regions.push((
            col,
            Rect::new(
                x,
                row.y,
                span.min(row.x + row.width - x),
                1,
            ),
        ));
        x += span;
    }
    regions
}

/// Minimum width for the Enthralled pane: widest name + 2-space gap + widest
/// `alive/total` value.
fn enthralled_col_width(rows: &[(String, u32, u32)]) -> usize {
    let name_col = rows
        .iter()
        .map(|(n, ..)| n.chars().count())
        .max()
        .unwrap_or(0);
    let val_col = rows
        .iter()
        .map(|(_, a, t)| format!("{a}/{t}").len())
        .max()
        .unwrap_or(0);
    name_col + 2 + val_col
}

/// Build an Enthralled row: name left, `alive/total` thralls right-aligned.
fn enthralled_line(
    name: &str,
    alive: u32,
    total: u32,
    width: usize,
    style: Style,
) -> Line<'static> {
    let value = format!("{alive}/{total}");
    let name_max = width.saturating_sub(value.len() + 1);
    let nm = truncate(name, name_max);
    let pad = width.saturating_sub(nm.chars().count() + value.len());
    Line::from(vec![
        Span::styled(nm, style),
        Span::raw(" ".repeat(pad)),
        Span::raw(value),
    ])
}

/// `line` with its `tags` laid against the right edge of a row `width` columns
/// wide: the line is padded out to the strip, and each slot is drawn in the
/// columns it owns whether it holds a tag or not. A row with no slots is its
/// line unchanged.
fn with_tags(line: &Line<'static>, tags: &Tags, width: u16) -> Line<'static> {
    let mut line = line.clone();
    if tags.is_empty() {
        return line;
    }
    let written: usize =
        line.spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (width as usize).saturating_sub(written + strip_w(tags));
    line.spans.push(Span::raw(" ".repeat(pad)));
    for (i, (tag, style)) in tags.iter().enumerate() {
        if 0 < i {
            line.spans.push(Span::raw(" ".repeat(TAG_SEP)));
        }
        line.spans.push(Span::styled(*tag, *style));
    }
    line
}

/// One row of a pane.
struct PaneRow {
    line: Line<'static>,
    /// The pirate the row names, or `None` for a row that is no pirate's and
    /// so cannot be selected or clicked.
    pirate: Option<usize>,
    /// Drawn at the row's right edge (see [`with_tags`]).
    tags: Tags,
}

/// Render a single pane: a bordered, auto-scrolling list of pirate rows. The
/// selected pirate is highlighted and the offset is nudged to keep it visible.
#[allow(clippy::too_many_arguments)]
fn render_pane(
    frame: &mut Frame,
    area: Rect,
    title: &'static str,
    rows: Vec<PaneRow>,
    sel: usize,
    offset: &mut usize,
    page_focused: bool,
    active: bool,
    pane: JobberPane,
    regions: &mut ClickMap,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(box_border(page_focused, active))
        .padding(Padding::horizontal(1))
        .title(offset_title(title).0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole-pane focus region first, so the per-row regions pushed below win
    // the reverse-iterating hit test on overlap.
    regions.push(ClickRegion {
        rect: area,
        target: pane_focus_target(pane),
    });

    let height = inner.height as usize;
    if height == 0 {
        return;
    }

    // Auto-scroll: keep the selected pirate's line within the visible window.
    if let Some(sel_line) = rows.iter().position(|r| r.pirate == Some(sel)) {
        if sel_line < *offset {
            *offset = sel_line;
        } else if sel_line >= *offset + height {
            *offset = sel_line + 1 - height;
        }
    }
    let max_off = rows.len().saturating_sub(height);
    if *offset > max_off {
        *offset = max_off;
    }

    let body = crate::utils::render_scrollbar(
        frame,
        regions,
        inner,
        crate::clickmap::ScrollView::JobberPane(pane),
        *offset,
        rows.len(),
    );

    // A tag stands in the column the pane reserved for it, which is inside the
    // bar's: the pane's width counts the bar whether one is up or not, so a
    // roster growing past its window must not shift the tags.
    let tag_edge = inner.width.saturating_sub(crate::utils::SCROLLBAR_W);

    for (vis, row) in rows.iter().enumerate().skip(*offset).take(height) {
        let row_area = Rect::new(
            body.x,
            body.y + (vis - *offset) as u16,
            body.width,
            1,
        );
        let is_sel = page_focused && active && row.pirate == Some(sel);
        let line = with_tags(&row.line, &row.tags, tag_edge);
        let para = if is_sel {
            Paragraph::new(line)
                .style(Style::default().bg(Color::White).fg(Color::Black))
        } else {
            Paragraph::new(line)
        };
        frame.render_widget(para, row_area);
        if let Some(idx) = &row.pirate {
            regions.push(ClickRegion {
                rect: row_area,
                target: ClickTarget::JobberPirate {
                    pane,
                    idx: *idx,
                },
            });
        }
    }
}

/// Render the Aboard pane: a pinned `header` row at the top, a scrollable list
/// of `names` (each its own selectable pirate row) in the middle, and pinned
/// `footers` (the swabbie tally) at the bottom. Only the name list scrolls;
/// the header and footers stay put. `sel` is the selected name index; `offset`
/// the name window.
///
/// A name paired with a tag has it drawn at the right edge of its row, the
/// row's width being known here and nowhere earlier.
#[allow(clippy::too_many_arguments)]
fn render_aboard_pane(
    frame: &mut Frame,
    area: Rect,
    header: Line<'static>,
    names: Vec<(Line<'static>, Tags)>,
    footers: Vec<Line<'static>>,
    sel: usize,
    offset: &mut usize,
    page_focused: bool,
    active: bool,
    regions: &mut ClickMap,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(box_border(page_focused, active))
        .padding(Padding::horizontal(1))
        .title(offset_title("Aboard").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole-pane focus region first, so per-row regions pushed below win the
    // hit test.
    regions.push(ClickRegion {
        rect: area,
        target: pane_focus_target(JobberPane::Aboard),
    });

    let h = inner.height as usize;
    if h == 0 {
        return;
    }

    // Carve the inner height into pinned header, pinned footers, and a
    // scrollable body for the names — the header wins the first row,
    // footers the last rows.
    let header_h = 1.min(h);
    let footer_h = footers.len().min(h - header_h);
    let body_h = h - header_h - footer_h;

    // Header (pinned top).
    frame.render_widget(
        Paragraph::new(header),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    // Auto-scroll the name window to keep the selection visible.
    if body_h > 0 && !names.is_empty() {
        if sel < *offset {
            *offset = sel;
        } else if sel >= *offset + body_h {
            *offset = sel + 1 - body_h;
        }
        let max_off = names.len().saturating_sub(body_h);
        if *offset > max_off {
            *offset = max_off;
        }
    } else {
        *offset = 0;
    }

    // Only the name list scrolls, so the bar spans that window alone — neither
    // the pinned header above it nor the footers below.
    let body = crate::utils::render_scrollbar(
        frame,
        regions,
        Rect::new(
            inner.x,
            inner.y + header_h as u16,
            inner.width,
            body_h as u16,
        ),
        crate::clickmap::ScrollView::JobberPane(JobberPane::Aboard),
        *offset,
        names.len(),
    );

    // As in [`render_pane`]: the tags stand in the column the pane reserved,
    // bar or no bar.
    let tag_edge = inner.width.saturating_sub(crate::utils::SCROLLBAR_W);

    for (vis, (line, tags)) in
        names.iter().enumerate().skip(*offset).take(body_h)
    {
        let row_area = Rect::new(
            body.x,
            body.y + (vis - *offset) as u16,
            body.width,
            1,
        );
        let is_sel = page_focused && active && vis == sel;
        let line = with_tags(line, tags, tag_edge);
        let para = if is_sel {
            Paragraph::new(line)
                .style(Style::default().bg(Color::White).fg(Color::Black))
        } else {
            Paragraph::new(line)
        };
        frame.render_widget(para, row_area);
        regions.push(ClickRegion {
            rect: row_area,
            target: ClickTarget::JobberPirate {
                pane: JobberPane::Aboard,
                idx: vis,
            },
        });
    }

    // Footers (pinned bottom).
    let footer_y = inner.y + (header_h + body_h) as u16;
    for (i, line) in footers.iter().take(footer_h).enumerate() {
        frame.render_widget(
            Paragraph::new(line.clone()),
            Rect::new(
                inner.x,
                footer_y + i as u16,
                inner.width,
                1,
            ),
        );
    }
}

/// Minimum width that keeps every greedy row's name and value from colliding:
/// widest name + 2-space gap + widest `before + current` value.
fn name_col_plus_value(greedy: &[(&String, u32, u32)]) -> usize {
    let name_col = greedy
        .iter()
        .map(|(n, ..)| n.chars().count())
        .max()
        .unwrap_or(0);
    let val_col = greedy
        .iter()
        .map(|(_, t, c)| format!("{} + {}", t.saturating_sub(*c), c).len())
        .max()
        .unwrap_or(0);
    name_col + 2 + val_col
}

/// Build a greedy row: name left, `before + current` strikes right-aligned.
fn greedy_line(
    name: &str,
    total: u32,
    current: u32,
    width: usize,
    style: Style,
) -> Line<'static> {
    let before = total.saturating_sub(current);
    let value = format!("{before} + {current}");
    let name_max = width.saturating_sub(value.len() + 1);
    let nm = truncate(name, name_max);
    let pad = width.saturating_sub(nm.chars().count() + value.len());
    Line::from(vec![
        Span::styled(nm, style),
        Span::raw(" ".repeat(pad)),
        Span::raw(value),
    ])
}

/// One jobber's standing within a column: their best skill among the column's
/// set (by standing, then experience), plus that skill's marker for merged
/// columns.
struct RankedJobber {
    name: String,
    experience: Experience,
    standing: Standing,
    /// The winning skill's marker (e.g. `C`/`P`), set only for merged columns.
    marker: Option<char>,
    /// Whether the name is drawn green (see [`greenie_colour`]). Decided where
    /// the row is built, the cache being at hand there and not at the panel.
    greenie: bool,
}

/// A Top Jobbers column paired with its ranked jobbers — all the sizing and
/// render helpers need, so they take one slice instead of parallel column/row
/// vecs.
struct RankedColumn {
    header: &'static str,
    /// Whether rows carry a marker letter (true for merged columns).
    marked: bool,
    rows: Vec<RankedJobber>,
}

/// Width of the per-row detail that trails a jobber's name, and whether
/// anything trails it at all. With codes shown it's `EEE/SSS` (+` X` marker on
/// merged columns); when the panel is too narrow we drop the code first,
/// leaving only the merged-column marker (or nothing). `0` means name-only — no
/// trailing gap.
fn detail_width(marked: bool, show_codes: bool) -> usize {
    match (show_codes, marked) {
        (true, true) => CODE_LEN + 2, // EEE/SSS + " X"
        (true, false) => CODE_LEN,    // EEE/SSS
        (false, true) => 1,           // marker letter only
        (false, false) => 0,          // name only
    }
}

/// Rank the aboard jobbers for each column. Within a column a jobber is scored
/// by their *best* of the column's skills (by standing, then experience); a
/// merged column also records which skill won, via its marker. Pirates without
/// any of a column's skills fetched are skipped. `limit` caps each column;
/// `None` is uncapped.
fn rank_columns(
    columns: &[JobberColumn],
    aboard: &HashSet<String>,
    cache: &PirateCache,
    limit: Option<usize>,
) -> Vec<RankedColumn> {
    columns
        .iter()
        .map(|col| {
            let mut rows: Vec<RankedJobber> = aboard
                .iter()
                .filter_map(|n| {
                    let entry = cache.get_cached(n)?;
                    let info = &entry.basic;
                    // Best of the column's skills for this pirate: highest
                    // standing, then experience.
                    let (skill, rec) = col
                        .skills
                        .iter()
                        .filter_map(|s| info.skills.get(s).map(|r| (s, r)))
                        .max_by(|a, b| {
                            a.1.standing
                                .cmp(&b.1.standing)
                                .then(a.1.experience.cmp(&b.1.experience))
                        })?;
                    Some(RankedJobber {
                        name: n.clone(),
                        experience: rec.experience,
                        standing: rec.standing,
                        marker: col.merged().then(|| skill.marker()),
                        greenie: entry.is_greenie(),
                    })
                })
                .collect();
            rows.sort_by(|a, b| {
                b.standing
                    .cmp(&a.standing)
                    .then(b.experience.cmp(&a.experience))
                    .then_with(|| a.name.cmp(&b.name))
            });
            if let Some(n) = limit {
                rows.truncate(n);
            }
            RankedColumn {
                header: col.header(),
                marked: col.merged(),
                rows,
            }
        })
        .collect()
}

/// Per-column outer widths for the Top Jobbers panel: each is the wider of its
/// header and its widest `name (+ gap + detail)` row, where the detail depends
/// on `show_codes` (see [`detail_width`]).
fn top_panel_col_widths(
    columns: &[RankedColumn],
    show_codes: bool,
) -> Vec<u16> {
    columns
        .iter()
        .map(|c| {
            let name_w = c
                .rows
                .iter()
                .map(|j| j.name.chars().count())
                .max()
                .unwrap_or(0);
            let detail = detail_width(c.marked, show_codes);
            let row_w = if detail > 0 {
                name_w + NAME_CODE_GAP + detail
            } else {
                name_w
            };
            row_w.max(c.header.chars().count()) as u16
        })
        .collect()
}

/// Total inner width (columns + gaps) the panel needs in a given mode.
fn top_panel_inner_width(columns: &[RankedColumn], show_codes: bool) -> u16 {
    let gaps = columns.len().saturating_sub(1) as u16 * COLUMN_GAP;
    top_panel_col_widths(columns, show_codes)
        .iter()
        .sum::<u16>()
        + gaps
}

/// The Skill Leaderboard panel's natural outer width (codes shown): columns +
/// gaps + padding + borders, with a floor so the title stays readable. This
/// drives the block sizing, so codes are dropped only when the terminal can't
/// fit this.
fn top_panel_width(columns: &[RankedColumn]) -> u16 {
    // Floor so the title stays readable when no jobbers have fetched stats yet.
    const FLOOR: u16 = offset_title_width("Skill Leaderboard");
    // The ranking scrolls, so the scrollbar's columns are part of what the
    // panel asks for whether the window is showing a bar or not.
    (top_panel_inner_width(columns, true)
        + crate::utils::BOX_MARGIN
        + crate::utils::SCROLLBAR_W)
        .max(FLOOR)
}

/// Build one Skill Leaderboard body row's spans (name + EEE/SSS codes /
/// marker).
fn leaderboard_row_spans(
    j: &RankedJobber,
    name_w: usize,
    show_codes: bool,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!("{:<name_w$}", truncate(&j.name, name_w)),
        greenie_colour(j.greenie),
    )];
    if show_codes {
        spans.push(Span::raw(" ".repeat(NAME_CODE_GAP)));
        spans.push(Span::styled(
            experience_abbr(j.experience),
            experience_style(j.experience),
        ));
        spans.push(Span::raw("/"));
        spans.push(Span::styled(
            standing_abbr(j.standing),
            standing_style(j.standing),
        ));
        // Merged columns flag which puzzle the jobber is strongest at.
        if let Some(m) = j.marker {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                m.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    } else if let Some(m) = j.marker {
        // Codes dropped for width, but the marker is "which puzzle", not a
        // standing/experience, so keep it.
        spans.push(Span::raw(" ".repeat(NAME_CODE_GAP)));
        spans.push(Span::styled(
            m.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

/// The Skill Leaderboard: ranked per-skill columns sharing one scroll window so
/// their ranks stay aligned row-for-row. The header pins to the top; the body
/// scrolls. The cursor (`ui.top_col`/`ui.top_sel`) is highlighted when the
/// panel is the active widget, and every body row is a click target opening
/// that pirate.
fn render_top_panel(
    frame: &mut Frame,
    region: Rect,
    columns: &[RankedColumn],
    ui: &mut JobbersUi,
    page_focused: bool,
    regions: &mut ClickMap,
) {
    // The panel fills its region: the region's width was derived from this
    // panel's natural width back in `render`, so it already hugs the content.
    let area = region;
    let active = ui.focus == JobberFocus::Leaderboard;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(box_border(page_focused, active))
        .padding(Padding::horizontal(1))
        .title(offset_title("Skill Leaderboard").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Whole-panel focus region first, so per-row regions pushed below win the
    // reverse-iterating hit test on overlap.
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::JobberLeaderboard,
    });

    if inner.height == 0 || columns.is_empty() {
        return;
    }

    // Responsive degradation: show the EEE/SSS codes only while the full layout
    // fits the available width; when it doesn't, drop the codes first (names —
    // and the merged-column marker — stay). The block width comes from the
    // natural (codes-shown) width, so this only triggers when the terminal
    // is too narrow.
    // Header pins to the top row; the rest is the scrollable body window. The
    // bar's columns come off the grid's width before anything is laid out in
    // it, since the columns cannot be placed twice.
    let body_h = (inner.height as usize).saturating_sub(1);
    let max_rows = columns.iter().map(|c| c.rows.len()).max().unwrap_or(0);
    let grid = Rect {
        width: inner.width.saturating_sub(
            if crate::utils::scrolls(body_h as u16, max_rows) {
                crate::utils::SCROLLBAR_W
            } else {
                0
            },
        ),
        ..inner
    };

    let show_codes = top_panel_inner_width(columns, true) <= grid.width;
    let col_w: Vec<u16> = top_panel_col_widths(columns, show_codes);

    // Interleave a gap between each pair of columns, with equal slack on both
    // sides so the column group sits centered when the panel is wider than its
    // content (e.g. when the panes below dictate the block width).
    let mut constraints: Vec<Constraint> =
        Vec::with_capacity(columns.len() * 2 + 1);
    constraints.push(Constraint::Fill(1));
    for (i, w) in col_w.iter().enumerate() {
        if i > 0 {
            constraints.push(Constraint::Length(COLUMN_GAP));
        }
        constraints.push(Constraint::Length(*w));
    }
    constraints.push(Constraint::Fill(1));
    let cols = Layout::horizontal(constraints).split(grid);

    // Clamp the cursor to the live shape, then nudge the shared window to keep
    // the selected rank visible (offset capped to the longest column so no
    // over-scroll).
    if ui.top_col >= columns.len() {
        ui.top_col = columns.len() - 1;
    }
    let cur_len = columns[ui.top_col].rows.len();
    if ui.top_sel >= cur_len {
        ui.top_sel = cur_len.saturating_sub(1);
    }
    if body_h > 0 {
        if ui.top_sel < ui.top_offset {
            ui.top_offset = ui.top_sel;
        } else if ui.top_sel >= ui.top_offset + body_h {
            ui.top_offset = ui.top_sel + 1 - body_h;
        }
        let max_off = max_rows.saturating_sub(body_h);
        if ui.top_offset > max_off {
            ui.top_offset = max_off;
        }
    } else {
        ui.top_offset = 0;
    }
    let offset = ui.top_offset;

    // One bar for the whole ranking: the columns share a window, so a single
    // position describes all of them. It spans the body, not the pinned header.
    let _ = crate::utils::render_scrollbar(
        frame,
        regions,
        Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            body_h as u16,
        ),
        crate::clickmap::ScrollView::JobberLeaderboard,
        offset,
        max_rows,
    );

    for (ci, column) in columns.iter().enumerate() {
        // Columns are laid out as [Fill, col, gap, col, gap, …, Fill] — the
        // real column areas start after the leading spacer at odd
        // indices.
        let col_area = cols[1 + ci * 2];
        let detail = detail_width(column.marked, show_codes);
        let name_w = (col_w[ci] as usize).saturating_sub(
            if detail > 0 {
                NAME_CODE_GAP + detail
            } else {
                0
            },
        );

        // Header row (pinned).
        frame.render_widget(
            Paragraph::new(
                Line::from(Span::styled(
                    column.header,
                    Style::default().bold().underlined(),
                ))
                .centered(),
            ),
            Rect::new(col_area.x, inner.y, col_area.width, 1),
        );

        // Body rows in the shared window.
        for (vis, j) in column.rows.iter().enumerate().skip(offset).take(body_h)
        {
            let row_area = Rect::new(
                col_area.x,
                inner.y + 1 + (vis - offset) as u16,
                col_area.width,
                1,
            );
            let spans = leaderboard_row_spans(j, name_w, show_codes);
            let is_sel =
                page_focused && active && ci == ui.top_col && vis == ui.top_sel;
            let para = if is_sel {
                Paragraph::new(Line::from(spans))
                    .style(Style::default().bg(Color::White).fg(Color::Black))
            } else {
                Paragraph::new(Line::from(spans))
            };
            frame.render_widget(para, row_area);
            regions.push(ClickRegion {
                rect: row_area,
                target: ClickTarget::JobberLeaderboardPirate {
                    col: ci,
                    row: vis,
                },
            });
        }
    }
}

/// The ship-type select popup: same list as the Damage calculator's, minus the
/// "View" affordance. `selected` is the highlighted ship index.
fn render_ship_popup(
    frame: &mut Frame,
    selected: usize,
    regions: &mut ClickMap,
) {
    let area = frame.area();

    let max_name = SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let (block, w) = crate::utils::choice_block("Select Ship", max_name as u16);
    let h = SHIPS.len() as u16 + 2; // +2 borders
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let items: Vec<ListItem> =
        SHIPS.iter().map(|s| ListItem::new(s.name)).collect();
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol(" ")
        .highlight_spacing(HighlightSpacing::Always);

    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        list,
        crate::utils::choice_rows(inner, max_name as u16),
        &mut state,
    );

    let inner_x = popup_area.x + 1;
    let inner_y = popup_area.y + 1;
    let inner_w = popup_area.width.saturating_sub(2);
    for i in 0 .. SHIPS.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberShipItem(i),
        });
    }
}

/// The vessel picker popup (replaces the old inline vessel list). Poisoned
/// vessels are listed in red, matching the old list styling.
fn render_vessel_popup(
    frame: &mut Frame,
    selected: usize,
    ordered: &[Arc<str>],
    state: &GameState,
    regions: &mut ClickMap,
) {
    let area = frame.area();

    let max_name = ordered
        .iter()
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(0)
        .max("No vessels".len());
    let (block, w) = crate::utils::choice_block("Vessels", max_name as u16);
    let h = (ordered.len() as u16).max(1) + 2;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let items: Vec<ListItem> = if ordered.is_empty() {
        vec![
            ListItem::new("No vessels")
                .style(Style::default().fg(Color::DarkGray)),
        ]
    } else {
        ordered
            .iter()
            .map(|k| {
                let vessel = state.vessels.get(k);
                let poisoned = vessel.is_some_and(|v| v.poisoned);
                // A jobbed vessel whose name we don't know yet shows its
                // `Ship of <crew>` placeholder in italics.
                let provisional = vessel.is_some_and(|v| v.provisional);
                let mut style = Style::default();
                if poisoned {
                    style = style.fg(Color::Red);
                }
                if provisional {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                ListItem::new(k.to_string()).style(style)
            })
            .collect()
    };
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol(" ")
        .highlight_spacing(HighlightSpacing::Always);

    let mut st = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        list,
        crate::utils::choice_rows(inner, max_name as u16),
        &mut st,
    );

    let inner_x = popup_area.x + 1;
    let inner_y = popup_area.y + 1;
    let inner_w = popup_area.width.saturating_sub(2);
    for i in 0 .. ordered.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberVesselItem(i),
        });
    }
}

/// The voyage-type picker popup. Unimplemented types are tagged "(soon)" and
/// muted, but can still be selected (they show the "coming soon" placeholder).
fn render_voyage_popup(
    frame: &mut Frame,
    selected: usize,
    regions: &mut ClickMap,
) {
    let area = frame.area();

    let labels: Vec<String> = VOYAGE_TYPES
        .iter()
        .map(|v| {
            if v.implemented() {
                v.name().to_string()
            } else {
                format!("{} (soon)", v.name())
            }
        })
        .collect();
    let max_name = labels.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let (block, w) = crate::utils::choice_block("Voyage Type", max_name as u16);
    let h = VOYAGE_TYPES.len() as u16 + 2;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let items: Vec<ListItem> = labels
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let item = ListItem::new(s.clone());
            if VOYAGE_TYPES[i].implemented() {
                item
            } else {
                item.style(Style::default().fg(Color::DarkGray))
            }
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol(" ")
        .highlight_spacing(HighlightSpacing::Always);

    let mut st = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        list,
        crate::utils::choice_rows(inner, max_name as u16),
        &mut st,
    );

    let inner_x = popup_area.x + 1;
    let inner_y = popup_area.y + 1;
    let inner_w = popup_area.width.saturating_sub(2);
    for i in 0 .. VOYAGE_TYPES.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::JobberVoyageItem(i),
        });
    }
}

// ---------------------------------------------------------------------------
// Pirate-stats popup
// ---------------------------------------------------------------------------

/// One skill table row: skill name, then Experience and Standing spelled out in
/// their own columns, each emphasised like the Top Jobbers codes.
fn skill_row(
    skill_w: usize,
    exp_w: usize,
    sta_w: usize,
    name: &str,
    exp: Experience,
    standing: Standing,
) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{name:<skill_w$}")),
        Span::raw("  "),
        Span::styled(
            format!("{:<exp_w$}", exp.to_string()),
            experience_style(exp),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:<sta_w$}", standing.to_string()),
            standing_style(standing),
        ),
    ])
}

/// The pirate-stats popup: name, crew/flag boxes, three skill tables, and the
/// [See Trophies] / [Close] buttons. Sized to its content, with the skill
/// tables scrolling when the window cannot hold all of them.
/// The Pirate popup's buttons, left to right, each with what clicking it does.
/// The note button says which of the two things it will do, and is absent
/// altogether where no note can be kept (see [`PiratePopup::note`]) — a button
/// that cannot do anything is not drawn.
pub fn pirate_popup_buttons(
    note: Option<&str>,
) -> Vec<(&'static str, ClickTarget)> {
    let mut buttons = Vec::with_capacity(3);
    if let Some(note) = note {
        let label = if note.trim().is_empty() {
            "Add Note"
        } else {
            "Edit Note"
        };
        buttons.push((label, ClickTarget::JobberPirateNote));
    }
    buttons.push((
        "See Trophies",
        ClickTarget::JobberPirateSeeTrophies,
    ));
    buttons.push(("Close", ClickTarget::JobberPirateClose));
    buttons
}

/// Which button a freshly-opened Pirate popup marks: Close, the last of them.
/// Opening a popup to read it should not leave Enter poised to do anything but
/// undo the opening.
pub fn pirate_popup_default_button(note: Option<&str>) -> usize {
    pirate_popup_buttons(note).len().saturating_sub(1)
}

fn render_pirate_popup(
    frame: &mut Frame,
    pp: &mut PiratePopup,
    cache: &PirateCache,
    page_focused: bool,
    regions: &mut ClickMap,
) {
    let screen = frame.area();
    let cached = cache.get_cached(&pp.name);

    // Buttons line (always present); compute its width up front. Each is as
    // wide as the longest label, with a gap between them and at each end.
    let buttons = pirate_popup_buttons(pp.note.as_deref());
    let labels: Vec<&str> = buttons.iter().map(|(l, _)| *l).collect();
    let buttons_w = crate::utils::buttons_width(&labels) as usize;

    // --- Build the unboxed crew/flag columns (no header label). Each column's
    //     width is its widest line. ---
    let muted = Style::default().fg(Color::DarkGray);
    let (crew_lines, crew_w) =
        match cached.and_then(|c| c.basic.crew().map(|cr| (c, cr))) {
            Some((c, cr)) => {
                // Rank styled via CrewRank; the duty role (if any) renders
                // plain. With a role the affiliation spans
                // three lines:   [Rank] and / [Role] of /
                // [Crew] without one, two:  [Rank] of / [Crew].
                let rank_label = c.basic.crew_rank.trim().to_string();
                let name_w = cr.name.chars().count();
                match &cr.role {
                    Some(role) => {
                        let l1_w = rank_label.chars().count() + " and".len();
                        let l1 = Line::from(vec![
                            Span::styled(rank_label, cr.rank.style()),
                            Span::raw(" and"),
                        ])
                        .centered();
                        let l2_text = format!("{role} of");
                        let l2_w = l2_text.chars().count();
                        let l2 = Line::from(l2_text).centered();
                        let l3 = Line::from(cr.name.clone()).centered();
                        (
                            vec![l1, l2, l3],
                            l1_w.max(l2_w).max(name_w) as u16,
                        )
                    }
                    None => {
                        let l1_w = rank_label.chars().count() + " of".len();
                        let l1 = Line::from(vec![
                            Span::styled(rank_label, cr.rank.style()),
                            Span::raw(" of"),
                        ])
                        .centered();
                        let l2 = Line::from(cr.name.clone()).centered();
                        (vec![l1, l2], l1_w.max(name_w) as u16)
                    }
                }
            }
            None => {
                (
                    vec![Line::from(Span::styled("No crew", muted)).centered()],
                    "No crew".len() as u16,
                )
            }
        };
    let (flag_lines, flag_w) =
        match cached.and_then(|c| c.basic.flag().map(|fl| (c, fl))) {
            Some((c, fl)) => {
                let title_label = c.basic.flag_rank.trim().to_string();
                let tail = " of".to_string();
                let l1_w = title_label.chars().count() + tail.chars().count();
                let l1 = Line::from(vec![
                    Span::styled(title_label, fl.title.style()),
                    Span::raw(tail),
                ])
                .centered();
                let l2 = Line::from(fl.name.clone()).centered();
                (
                    vec![l1, l2],
                    l1_w.max(fl.name.chars().count()) as u16,
                )
            }
            None => {
                (
                    vec![Line::from(Span::styled("No flag", muted)).centered()],
                    "No flag".len() as u16,
                )
            }
        };
    // The two columns split the row into equal halves with a 2-space gap. Each
    // side reserves the wider column's content width so they stay symmetric;
    // when the popup is wider they expand to fill, content centered within
    // each half.
    const AFFIL_GAP: u16 = 2;
    let affil_w = 2 * crew_w.max(flag_w) + AFFIL_GAP;
    // The crew column may be 3 lines tall (with a role); the row fits the
    // taller.
    let affil_h = crew_lines.len().max(flag_lines.len()) as u16;

    // --- Build the skill tables (one per family, in popup order). ---
    let sections: [(&str, &[Skill]); 3] = [
        ("Piracy Skills", PIRACY_SKILLS),
        ("Crafting Skills", CRAFTING_SKILLS),
        ("Carousing Skills", CAROUSING_SKILLS),
    ];
    // Column widths for the skill tables: skill name, then the spelled-out
    // Experience and Standing words.
    let (skill_w, exp_w, sta_w) = cached
        .map(|c| {
            c.basic
                .skills
                .iter()
                .fold((0, 0, 0), |(sw, ew, stw), (s, r)| {
                    (
                        sw.max(s.to_string().chars().count()),
                        ew.max(r.experience.to_string().chars().count()),
                        stw.max(r.standing.to_string().chars().count()),
                    )
                })
        })
        .unwrap_or((0, 0, 0));

    let mut skill_lines: Vec<Line> = Vec::new();
    let mut table_title_w = 0usize;
    match cached {
        None => {
            skill_lines.push(
                Line::from(Span::styled(
                    "Stats not loaded yet.",
                    muted,
                ))
                .centered(),
            )
        }
        Some(c) => {
            let mut first = true;
            for (title, skills) in sections {
                let present: Vec<Skill> = skills
                    .iter()
                    .copied()
                    .filter(|s| c.basic.skills.contains_key(s))
                    .collect();
                if present.is_empty() {
                    continue;
                }
                if !first {
                    skill_lines.push(Line::from(""));
                }
                first = false;
                table_title_w = table_title_w.max(title.len());
                skill_lines.push(
                    Line::from(Span::styled(
                        title,
                        Style::default().bold().underlined(),
                    ))
                    .centered(),
                );
                for s in present {
                    let rec = &c.basic.skills[&s];
                    skill_lines.push(skill_row(
                        skill_w,
                        exp_w,
                        sta_w,
                        &s.to_string(),
                        rec.experience,
                        rec.standing,
                    ));
                }
            }
            if skill_lines.is_empty() {
                skill_lines.push(
                    Line::from(Span::styled(
                        "No skills recorded.",
                        muted,
                    ))
                    .centered(),
                );
            }
        }
    }
    let skill_row_w = if skill_w > 0 {
        skill_w + 2 + exp_w + 2 + sta_w
    } else {
        0
    };
    // Width of the skills section as a block (widest row / title / fallback
    // line), so the whole section can be centered within the popup.
    let skills_block_w = skill_row_w
        .max(table_title_w)
        .max("Stats not loaded yet.".len())
        .max("No skills recorded.".len());

    // --- Geometry ---
    // The skill tables scroll, so their section carries the scrollbar's column
    // whether or not the window is short enough to need the bar: the popup is
    // then the same width however tall the terminal is.
    let content_w = (pp.name.chars().count())
        .max(affil_w as usize)
        .max(skills_block_w + crate::utils::SCROLLBAR_W as usize)
        .max(buttons_w) as u16;
    let box_w = (content_w + 4).min(screen.width.max(1));
    // The note is read above the standings, under a heading of its own. It
    // wraps to the width the rest of the popup asked for rather than
    // setting one: a sentence about a pirate is longer than anything else
    // in the box, and a box as wide as a sentence would dwarf what it is
    // about. Nothing written means no heading and no rows.
    // The note is read above the standings and scrolls with them: what is known
    // about a pirate is one body of text, and a long note must not be able to
    // push the standings or the buttons out of the box. It wraps to the body's
    // full width rather than to the standings' column - a sentence about a
    // pirate is longer than anything else here, and the popup is no wider for
    // it. Nothing written means no heading and no rows.
    let body_w = content_w.saturating_sub(crate::utils::SCROLLBAR_W) as usize;
    let note_lines: Vec<Line> = pp
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(|note| {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Note",
                    Style::default().bold().underlined(),
                ))
                .centered(),
            ];
            lines.extend(
                wrap_offsets(note, body_w)
                    .into_iter()
                    .map(|(_, text)| Line::from(text.to_owned())),
            );
            // A blank under it, so the note and the first table are not read as
            // one block.
            lines.push(Line::from(""));
            lines
        })
        .unwrap_or_default();
    // The standings keep the column they are centered in; the note spans the
    // body, so the two are laid out in one block as wide as the body and the
    // tables are indented into their place within it.
    let indent = body_w.saturating_sub(skills_block_w) / 2;
    let scroll_lines: Vec<Line> = note_lines
        .into_iter()
        .chain(skill_lines.into_iter().map(|line| {
            if indent == 0 {
                return line;
            }
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(line.spans);
            Line::from(spans)
                .alignment(line.alignment.unwrap_or(Alignment::Left))
        }))
        .collect();
    // name + gap + affil + gap + body + gap + buttons, plus borders(2).
    let box_h = (1 + 1 + affil_h + 1 + scroll_lines.len() as u16 + 1 + 1 + 2)
        .min(screen.height.max(1));
    let x = screen.x + screen.width.saturating_sub(box_w) / 2;
    let y = screen.y + screen.height.saturating_sub(box_h) / 2;
    let popup = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Pirate").0);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1),       // name
        Constraint::Length(1),       // gap
        Constraint::Length(affil_h), // crew | flag
        Constraint::Length(1),       // gap
        Constraint::Min(0),          // the note and the standings, scrolling
        Constraint::Length(1),       // gap
        Constraint::Length(1),       // buttons
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(
            Line::from(Span::styled(
                pp.name.clone(),
                greenie_colour(
                    cached.is_some_and(crate::pirate::CachedPirate::is_greenie),
                )
                .bold(),
            ))
            .centered(),
        ),
        rows[0],
    );

    // Crew | Flag (unboxed): two equal halves split by a 2-space gap, each
    // filling its side with the content centered.
    let affil_cols = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(AFFIL_GAP),
        Constraint::Fill(1),
    ])
    .split(rows[2]);
    frame.render_widget(
        Paragraph::new(crew_lines),
        affil_cols[0],
    );
    frame.render_widget(
        Paragraph::new(flag_lines),
        affil_cols[2],
    );

    // The note and the standings are the one part of the popup that scrolls, so
    // a window too short for them shows a bar beside them rather than cutting
    // them off. Clamp the offset to what is left to show.
    pp.view_h = rows[4].height as usize;
    pp.offset = pp.offset.min(scroll_lines.len().saturating_sub(pp.view_h));
    let body = crate::utils::render_scrollbar(
        frame,
        regions,
        rows[4],
        crate::clickmap::ScrollView::JobberPirateSkills,
        pp.offset,
        scroll_lines.len(),
    );
    frame.render_widget(
        Paragraph::new(
            scroll_lines
                .into_iter()
                .skip(pp.offset)
                .take(pp.view_h)
                .collect::<Vec<Line>>(),
        ),
        body,
    );

    // Buttons; each gets a click region. The page's own focus decides whether
    // either is marked, so a popup behind an unfocused page shows neither.
    pp.button = pp.button.min(buttons.len().saturating_sub(1));
    for (rect, (_, target)) in crate::utils::render_buttons(
        frame,
        rows[6],
        &labels,
        page_focused.then_some(pp.button),
    )
    .into_iter()
    .zip(buttons)
    {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
}

// ---------------------------------------------------------------------------
// Trophies popup
// ---------------------------------------------------------------------------

/// Whether a (lowercased) trophy name matches the (lowercased) search needle:
/// a plain substring hit, or a fuzzy Jaro-Winkler match against the whole name
/// or any single word in it (so typos and partial words still surface
/// trophies).
fn trophy_matches(name_lower: &str, needle: &str) -> bool {
    const FUZZY: f64 = 0.82;
    name_lower.contains(needle)
        || text_similarity(needle, name_lower) >= FUZZY
        || name_lower
            .split_whitespace()
            .any(|w| text_similarity(needle, w) >= FUZZY)
}

/// Center `s` within `width` columns (truncating if it somehow overflows).
fn center_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return truncate(s, width);
    }
    let left = (width - len) / 2;
    let right = width - len - left;
    format!(
        "{}{}{}",
        " ".repeat(left),
        s,
        " ".repeat(right)
    )
}

/// Build a category's lines for the trophies popup: a centered name, then the
/// matching trophies laid out in 3 centered, word-wrapped columns. Returns an
/// empty vec when nothing in the category matches `search`.
fn trophy_section_lines(
    section: &TrophySection,
    search: &str,
    inner_w: usize,
) -> Vec<Line<'static>> {
    let needle = search.trim().to_lowercase();
    let mut names: Vec<&String> = section
        .trophies
        .iter()
        .filter(|t| {
            needle.is_empty() || trophy_matches(&t.to_lowercase(), &needle)
        })
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    names.sort_unstable();

    const GAP: usize = 2;
    let col_w = inner_w.saturating_sub(2 * GAP) / 3;
    let col_w = col_w.max(1);

    // Uncategorised trophies render under an italic "Ungrouped" heading.
    let (title_text, title_style) = if section.category.trim().is_empty() {
        (
            "Ungrouped".to_string(),
            Style::default().bold().underlined().italic(),
        )
    } else {
        (
            section.category.clone(),
            Style::default().bold().underlined(),
        )
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(title_text, title_style)).centered());

    // Lay out row-major in threes. The final short row centers oddly per spec:
    // two left → left & right columns; one left → the middle column. Each grid
    // row is followed by a blank line so trophies are visually separated.
    let mut i = 0;
    while i < names.len() {
        let remaining = names.len() - i;
        // Which column each of this row's cells lands in.
        let slots: [Option<&String>; 3] = if remaining == 1 {
            [None, Some(names[i]), None]
        } else if remaining == 2 {
            [Some(names[i]), None, Some(names[i + 1])]
        } else {
            [Some(names[i]), Some(names[i + 1]), Some(names[i + 2])]
        };
        let consumed = remaining.min(3);

        // Wrap each cell, then stack the columns to the tallest cell.
        let wrapped: Vec<Vec<String>> = slots
            .iter()
            .map(|s| s.map(|t| wrap_words(t, col_w)).unwrap_or_default())
            .collect();
        let rows = wrapped.iter().map(Vec::len).max().unwrap_or(0);
        for r in 0 .. rows {
            let mut cells: Vec<String> = Vec::with_capacity(3);
            for col in &wrapped {
                let text = col.get(r).map(String::as_str).unwrap_or("");
                cells.push(center_to(text, col_w));
            }
            lines.push(Line::from(cells.join(&" ".repeat(GAP))));
        }
        lines.push(Line::from("")); // blank under each row
        i += consumed;
    }
    lines
}

/// Wrap `text` into lines of at most `width` columns, each paired with the byte
/// offset in `text` where it begins. It breaks at a blank where it can and
/// inside a word that is longer than a line, and the blank it breaks on belongs
/// to the line it ended — so the offsets run forward over the whole text and a
/// cursor counted in bytes is found in exactly one line.
///
/// This is [`crate::utils::wrap_words`] with the offsets kept, which is what an
/// editable paragraph needs to put its caret where the typing is.
fn wrap_offsets(text: &str, width: usize) -> Vec<(usize, &str)> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    loop {
        let rest = &text[start ..];
        // Where the line must end: the newline that closes it, or the byte past
        // the last column it has room for.
        let newline = rest.find('\n');
        let over = rest.char_indices().nth(width).map(|(i, _)| i);
        // `skip` is the newline the break swallows, which no line draws.
        let (end, skip) = match (newline, over) {
            (Some(nl), None) => (nl, 1),
            (Some(nl), Some(limit)) if nl <= limit => (nl, 1),
            (_, None) => (rest.len(), 0),
            // The line fills its columns exactly and a blank follows, so that
            // blank is the break.
            (_, Some(limit)) if rest[limit ..].starts_with(' ') => (limit, 0),
            (_, Some(limit)) => {
                match rest[.. limit].rfind(' ') {
                    // Break at the last blank that leaves something on the
                    // line.
                    Some(blank) if 0 < blank => (blank, 0),
                    // A word longer than the line is broken at the margin.
                    _ => (limit, 0),
                }
            }
        };
        lines.push((start, rest[.. end].trim_end()));
        let mut next = start + end + skip;
        // The blanks a line broke at belong to it; blanks after a newline are
        // the next line's own.
        if skip == 0 {
            while text[next ..].starts_with(' ') {
                next += 1;
            }
        }
        if text.len() <= next {
            // A text ending in a newline earns the empty line under it, which
            // is where the caret sits once one is typed.
            if 0 < skip {
                lines.push((text.len(), ""));
            }
            break;
        }
        start = next;
    }
    lines
}

/// Where a cursor at byte `at` falls in wrapped `lines`: which line holds it
/// and how many columns into that line it sits. A cursor on a blank a line
/// broke at rests at the end of that line, there being no column of its own for
/// it.
fn caret_in(lines: &[(usize, &str)], at: usize) -> (usize, usize) {
    let row = lines
        .iter()
        .rposition(|(start, _)| *start <= at)
        .unwrap_or(0);
    let (start, text) = lines[row];
    let col = match at.checked_sub(start).filter(|off| *off <= text.len()) {
        Some(off) => text[.. off].chars().count(),
        None => text.chars().count(),
    };
    (row, col)
}

/// Step the note's caret one wrapped line up (`down` false) or down, keeping
/// the column it was in as far as the new line reaches. Answers whether it
/// moved: at the top or bottom line there is nowhere to step, which is what
/// tells the editor to hand the keys to its buttons instead.
///
/// The lines are the ones the last render drew, [`NotePopup::wrap_w`] being the
/// width it wrapped them to: what ↑ and ↓ mean in a wrapping box is a question
/// about what is on the screen.
pub fn note_caret_step(np: &mut NotePopup, down: bool) -> bool {
    let lines = wrap_offsets(&np.field.value, np.wrap_w.max(1));
    let (row, col) = caret_in(&lines, np.field.cursor);
    let target = if down { row + 1 } else { row.wrapping_sub(1) };
    let Some((start, text)) = lines.get(target).copied() else {
        return false;
    };
    let within = text
        .char_indices()
        .nth(col)
        .map_or(text.len(), |(offset, _)| offset);
    np.field.cursor = start + within;
    true
}

/// The note editor: a wrapping text box over the pirate whose note it is, as
/// wide as the Trophies popup it shares a button row with and at least four
/// lines tall, with Cancel and Save beneath.
fn render_note_popup(
    frame: &mut Frame,
    np: &mut NotePopup,
    cache: &PirateCache,
    regions: &mut ClickMap,
) {
    const BUTTONS: [&str; 2] = ["Cancel", "Save"];
    /// Lines the box keeps for the text however little of it there is.
    const MIN_LINES: u16 = 4;

    let screen = frame.area();
    let title = format!("Notes on {}", np.name);
    let box_w = trophy_popup_width(screen, cache.get_cached(&np.name))
        .max(offset_title_width(&title))
        .max(crate::utils::buttons_width(&BUTTONS) + crate::utils::BOX_MARGIN)
        .min(screen.width.max(1));

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title(&title).0);
    // The text wraps to the room inside the frame less the scrollbar's column,
    // which it keeps whether a bar is up or not: a note growing past the box
    // must not re-wrap what is already written.
    let wrap_w = box_w
        .saturating_sub(crate::utils::BOX_MARGIN + crate::utils::SCROLLBAR_W)
        .max(1) as usize;
    np.wrap_w = wrap_w;
    let lines = wrap_offsets(&np.field.value, wrap_w);

    // The box grows with the note, down to four lines and up to what the screen
    // can hold; past that the text scrolls within it.
    let room = screen
        .height
        .saturating_sub(
            1 /*blank*/ + 1 /*buttons*/ + 2, // borders
        )
        .max(1);
    let text_h = (lines.len() as u16).clamp(MIN_LINES.min(room), room);
    let box_h = (text_h + 1 + 1 + 2).min(screen.height.max(1));

    let x = screen.x + screen.width.saturating_sub(box_w) / 2;
    let y = screen.y + screen.height.saturating_sub(box_h) / 2;
    let popup = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, popup);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Min(0),    // the text
        Constraint::Length(1), // blank
        Constraint::Length(1), // buttons
    ])
    .split(inner);

    // Keep the caret in view: typing at the foot of a long note scrolls to it
    // rather than leaving the user writing off the bottom of the box.
    let (row, col) = caret_in(&lines, np.field.cursor);
    np.view_h = rows[0].height as usize;
    if np.view_h <= row {
        np.offset = row + 1 - np.view_h;
    } else if row < np.offset {
        np.offset = row;
    }
    np.offset = np.offset.min(lines.len().saturating_sub(np.view_h));

    let text = crate::utils::render_scrollbar(
        frame,
        regions,
        rows[0],
        crate::clickmap::ScrollView::JobberNoteText,
        np.offset,
        lines.len(),
    );

    // An empty note says what the box is for rather than sitting blank.
    if np.field.value.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Write what ye know of this pirate.",
                Style::default().fg(Color::DarkGray),
            ))),
            text,
        );
    } else {
        frame.render_widget(
            Paragraph::new(
                lines
                    .iter()
                    .skip(np.offset)
                    .take(np.view_h)
                    .map(|(_, line)| Line::from((*line).to_owned()))
                    .collect::<Vec<Line>>(),
            ),
            text,
        );
    }

    // The caret is the only thing marking the text as editable, the box being
    // nothing but text: it sits where the typing will land, and only while the
    // text is what the keys are going to.
    if np.focus == NoteFocus::Text && np.offset <= row {
        let line = (row - np.offset) as u16;
        if line < text.height {
            frame.set_cursor_position((
                text.x + col.min(wrap_w.saturating_sub(1)) as u16,
                text.y + line,
            ));
        }
    }

    // Neither button is marked while the text has the keys: Enter is a newline
    // there, and marking Save would say otherwise.
    let marked = match np.focus {
        NoteFocus::Text => None,
        NoteFocus::Cancel => Some(0),
        NoteFocus::Save => Some(1),
    };
    for (rect, target) in
        crate::utils::render_buttons(frame, rows[2], &BUTTONS, marked)
            .into_iter()
            .zip([ClickTarget::JobberNoteCancel, ClickTarget::JobberNoteSave])
    {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
}

/// The width the Trophies popup takes over `screen` for `cached`'s trophies.
///
/// The grid reflows into three columns of whatever width it is given, so it has
/// no width of its own; what it must not be is wider than the names in it.
/// Three columns of the longest name with their gaps is that width, and 80
/// columns is as far as it goes however long a trophy's name runs. The
/// scrollbar's column is counted whether or not the grid is long enough to need
/// a bar, so the box does not change width as the user scrolls into the rows
/// that call for one.
///
/// The Notes popup takes this width too: both are opened from the Pirate popup
/// and read as one pair over it rather than as two boxes of their own sizes.
fn trophy_popup_width(screen: Rect, cached: Option<&CachedPirate>) -> u16 {
    const GRID_GAP: u16 = 2;
    let has_trophies = cached.is_some_and(|c| {
        c.trophies.sections.iter().any(|s| !s.trophies.is_empty())
    });
    let longest = cached
        .map(|c| {
            c.trophies
                .sections
                .iter()
                .flat_map(|s| s.trophies.iter().map(|t| t.chars().count()))
                .chain(
                    c.trophies
                        .sections
                        .iter()
                        .map(|s| s.category.chars().count()),
                )
                .max()
                .unwrap_or(0) as u16
        })
        .unwrap_or(0);
    let content_w = if has_trophies {
        3 * longest + 2 * GRID_GAP + crate::utils::SCROLLBAR_W
    } else {
        "Trophies not loaded yet.".len() as u16
    };
    (content_w + crate::utils::BOX_MARGIN)
        .max(offset_title_width("Trophies"))
        .min(80)
        .min(screen.width.max(1))
}

/// The trophies popup: 80 wide, a pinned search box, then the pirate's trophy
/// categories (each a centered name + 3-column grid), vertically scrollable.
fn render_trophy_popup(
    frame: &mut Frame,
    tp: &mut TrophyPopup,
    cache: &PirateCache,
    regions: &mut ClickMap,
) {
    let screen = frame.area();

    // With no trophies to sift through there is nothing for a search box to do,
    // and nothing to scroll either: the popup is then as tall as the one line
    // it has to say. A search that merely matches nothing keeps its box,
    // since the user needs it to clear the filter.
    let cached = cache.get_cached(&tp.name);
    let has_trophies = cached.is_some_and(|c| {
        c.trophies.sections.iter().any(|s| !s.trophies.is_empty())
    });

    let box_w = trophy_popup_width(screen, cached);
    let box_h = if has_trophies {
        screen
            .height
            .saturating_sub(2)
            .max(3)
            .min(screen.height.max(1))
    } else {
        (
            1 /*notice*/ + 1 /*blank*/ + 1 /*Close*/ + 2
            // borders
        )
        .min(screen.height.max(1))
    };
    let x = screen.x + screen.width.saturating_sub(box_w) / 2;
    let y = screen.y + screen.height.saturating_sub(box_h) / 2;
    let popup = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Trophies").0);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // What the filter let through, said as the words carry on from the ones
    // typed: the grid answers in full, but a long one cannot be counted at a
    // glance. With nothing typed there is nothing to answer.
    let filter = tp.search.as_ref().map(|s| s.value.trim().to_lowercase());
    let tally = filter.as_ref().filter(|f| !f.is_empty()).map(|needle| {
        let all: Vec<&String> = cached
            .map(|c| {
                c.trophies
                    .sections
                    .iter()
                    .flat_map(|s| &s.trophies)
                    .collect()
            })
            .unwrap_or_default();
        let shown = all
            .iter()
            .filter(|t| trophy_matches(&t.to_lowercase(), needle))
            .count();
        format!(
            "reveals {shown} of {} trophies",
            all.len()
        )
    });

    let rows = Layout::vertical([
        Constraint::Min(0), // scroll area
        // the filter's row, which says how to open one while none is. It sits
        // under the grid it filters and above the blank the buttons keep.
        Constraint::Length(u16::from(has_trophies) * crate::utils::SEARCH_H),
        Constraint::Length(1), // blank
        Constraint::Length(1), // Close
    ])
    .split(inner);

    // The whole popup is a scroll target so the wheel works anywhere over it.
    // It goes in before the parts that answer to the mouse in their own right —
    // the scrollbar and the Close button — so those win the reverse-iterating
    // hit test where they overlap it.
    regions.push(ClickRegion {
        rect: popup,
        target: ClickTarget::JobberTrophyArea,
    });

    if has_trophies {
        match &tp.search {
            // The popup is modal, so an open filter always has the letters.
            Some(search) => {
                crate::utils::render_search(
                    frame,
                    rows[1],
                    search,
                    tally.as_deref().map(crate::utils::SearchAnswer::Reading),
                    true,
                );
            }
            None => {
                crate::utils::render_search_invite(
                    frame,
                    rows[1],
                    "filter these trophies",
                )
            }
        }
    }

    crate::utils::render_close_button(frame, rows[3]);
    regions.push(ClickRegion {
        rect: rows[3],
        target: ClickTarget::JobberTrophyClose,
    });

    // All the category lines, laid out for a view `inner_w` columns wide.
    let build = |inner_w: usize| {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(c) = cached {
            // Two blank lines separate one group from the next.
            let mut first = true;
            for section in &c.trophies.sections {
                let sec = trophy_section_lines(
                    section,
                    filter.as_deref().unwrap_or(""),
                    inner_w,
                );
                if sec.is_empty() {
                    continue;
                }
                if !first {
                    lines.push(Line::from(""));
                }
                first = false;
                lines.extend(sec);
            }
        }
        lines
    };

    // Record the view height so the key handler can scroll by half a page.
    let grid = rows[0];
    let view_h = grid.height as usize;
    tp.view_h = view_h;
    // The grid reflows into whatever width it is given, so it is laid out again
    // once the scrollbar's column turns out to be wanted. Laying it out one
    // column narrower can only lengthen it, so a bar never un-needs itself.
    let mut lines = build(grid.width as usize);
    if view_h < lines.len() {
        lines = build(
            grid.width.saturating_sub(crate::utils::SCROLLBAR_W) as usize,
        );
    }

    // No grid to draw is one line to say, so it is said in the middle of the
    // room the grid would have had rather than at the top of it — there is
    // nothing above it for it to sit under.
    if lines.is_empty() {
        let notice = match (cached.is_some(), has_trophies) {
            (false, _) => "Trophies not loaded yet.",
            (true, true) => "No matching trophies.",
            (true, false) => "No trophies.",
        };
        crate::utils::render_notice(
            frame,
            grid,
            &[(
                notice,
                Style::default().fg(Color::DarkGray),
            )],
        );
        return;
    }

    // Clamp scroll, then render the visible window beside its bar.
    let max_off = lines.len().saturating_sub(view_h);
    if tp.offset > max_off {
        tp.offset = max_off;
    }
    let body = crate::utils::render_scrollbar(
        frame,
        regions,
        grid,
        crate::clickmap::ScrollView::JobberTrophies,
        tp.offset,
        lines.len(),
    );
    let visible: Vec<Line> =
        lines.into_iter().skip(tp.offset).take(view_h).collect();
    frame.render_widget(Paragraph::new(visible), body);
}

#[cfg(test)]
mod board_tests {
    use super::*;

    /// An Atlantis run: the dragoon tell has fired, so the run is one by its
    /// own account and not just by the picker's.
    fn atlantis() -> (GameState, Arc<str>) {
        let mut state = GameState::new();
        for line in [
            "====== 2026/06/16 ======",
            "[01:00:00] Going aboard the Test Vessel...",
            "[01:00:06] Playerone issued an order to set the vessel to sail.",
            "[01:01:00] Dragoons from the monster took advantage of their \
             proximity to board yer vessel!",
        ] {
            state.process_line(line);
        }
        let key = state.vessels_by_recency().first().cloned().expect("vessel");
        (state, key)
    }

    /// Hand the run a copied report, as the clipboard watcher would.
    fn copy(state: &mut GameState, text: &str) {
        let report = crate::duty::parse(text).expect("report");
        state.note_duty_report(&report, DateTime::UNIX_EPOCH);
    }

    /// Tokens at the sails, a haul below, over two intervals of one run.
    const FIRST: &str = r#"{"sail":{"Foo":{"performance":4,
        "maneuver_tokens":[4,1,0,2,0,0,0]}},
        "haul":{"Bar":{"performance":3,"m.treasure_hauled":[2,1,0]}}}"#;
    const SECOND: &str = r#"{"sail":{"Foo":{"performance":3,
        "maneuver_tokens":[1,6,0,0,0,0,0]},
        "Bar":{"performance":2,"maneuver_tokens":[3,0,0,0,0,0,0]}}}"#;

    /// The box waits on the tells: the same reports on a run that has shown
    /// no sign of Atlantis draw nothing, however much the picker says
    /// Atlantis. The figures are kept all the same — the run holds them, so
    /// the board that finally appears has them in its sums.
    #[test]
    fn a_run_with_no_tells_has_no_board() {
        let mut state = GameState::new();
        for line in [
            "====== 2026/06/16 ======",
            "[01:00:00] Going aboard the Test Vessel...",
            "[01:00:06] Playerone issued an order to set the vessel to sail.",
        ] {
            state.process_line(line);
        }
        let key = state.vessels_by_recency().first().cloned().expect("vessel");
        copy(&mut state, FIRST);
        assert!(boards(&state, Some(&key), VoyageType::Atlantis).is_none());
        assert_eq!(
            latest_run(state.vessels.get(&key).expect("vessel"))
                .expect("run")
                .duty_reports
                .len(),
            1
        );
    }

    /// And on the figures: an Atlantis run whose reports have rated people
    /// without counting anything has nothing to put on a board.
    #[test]
    fn a_run_with_no_figures_has_no_board() {
        let (mut state, key) = atlantis();
        copy(
            &mut state,
            r#"{"bilge":{"Foo":{"performance":4}}}"#,
        );
        assert!(boards(&state, Some(&key), VoyageType::Atlantis).is_none());
    }

    /// Nor is it the Atlantis box on another voyage type's page.
    #[test]
    fn another_layout_has_no_board() {
        let (mut state, key) = atlantis();
        copy(&mut state, FIRST);
        assert!(boards(&state, Some(&key), VoyageType::Pillage).is_none());
    }

    /// Each board lists whoever produced some of its own figure and ranks
    /// them on the sum of it, summed over every report of the run.
    #[test]
    fn each_board_ranks_who_produced_its_figure() {
        let (mut state, key) = atlantis();
        copy(&mut state, FIRST);
        copy(&mut state, SECOND);
        let boards =
            boards(&state, Some(&key), VoyageType::Atlantis).expect("boards");
        assert_eq!(
            boards.tabs(),
            vec![BoardTab::Tokens, BoardTab::Treasures]
        );

        let tokens = boards.tab_board(BoardTab::Tokens).expect("tokens");
        let ranked: Vec<_> = tokens
            .ranked(Ranking::default())
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.figures.clone(),
                    row.sum,
                )
            })
            .collect();
        assert_eq!(
            ranked,
            vec![("Foo", vec![5, 7, 0, 2], 14), ("Bar", vec![3, 0, 0, 0], 3),]
        );

        // Bar hauled and Foo did not, so the haul board is Bar's alone: a
        // pirate is on the board of what they made and no other.
        let hauled = boards.tab_board(BoardTab::Treasures).expect("haul");
        let ranked: Vec<_> = hauled
            .ranked(Ranking::default())
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.figures.clone(),
                    row.sum,
                )
            })
            .collect();
        assert_eq!(ranked, vec![("Bar", vec![2, 1, 0], 3)]);
    }

    /// A figure column ranks on itself, and the column already ranked on
    /// turns around rather than starting over.
    #[test]
    fn a_column_ranks_on_itself_and_then_turns_around() {
        let (mut state, key) = atlantis();
        copy(&mut state, FIRST);
        copy(&mut state, SECOND);
        let boards =
            boards(&state, Some(&key), VoyageType::Atlantis).expect("boards");
        let tokens = boards.tab_board(BoardTab::Tokens).expect("tokens");

        // Circles: Foo made 5 of them to Bar's 3.
        let circles = Ranking::default().clicked(tokens.key_at(0));
        assert_eq!(
            circles,
            Ranking {
                key: BoardKey::Figure(0),
                desc: true,
            }
        );
        let names: Vec<_> = tokens
            .ranked(circles)
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(names, vec!["Foo", "Bar"]);

        let again = circles.clicked(tokens.key_at(0));
        assert!(!again.desc);
        let names: Vec<_> = tokens
            .ranked(again)
            .iter()
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(names, vec!["Bar", "Foo"]);

        // The sum's own column is the one after the figures.
        assert_eq!(
            tokens.key_at(tokens.heads.len()),
            BoardKey::Sum
        );
    }

    /// A run that has put into port is the run whose rewards are being handed
    /// out, so its figures stay on show until the next one begins.
    #[test]
    fn a_ported_run_keeps_its_board() {
        let (mut state, key) = atlantis();
        copy(&mut state, FIRST);
        state.process_line(
            "[01:30:00] Playerone issued an order to put into port.",
        );
        assert!(
            state
                .vessels
                .get(&key)
                .expect("vessel")
                .current_voyage
                .is_none()
        );
        assert!(boards(&state, Some(&key), VoyageType::Atlantis).is_some());
    }

    /// Stacked, the panes are floored at a scrollable view's worth each and
    /// the rows left over go to the pane with the longer list waiting.
    #[test]
    fn stacked_panes_are_floored_before_they_are_shared() {
        let area = Rect::new(0, 0, 20, 25);
        let floors = [7, 6];

        // Neither has a list the room cannot hold: the slack goes to the
        // first, and both keep their floor.
        let areas = stacked_pane_areas(area, &[6, 2], &floors);
        assert_eq!(areas.len(), 2);
        assert_eq!(
            areas[0].height + areas[1].height,
            area.height
        );
        assert_eq!(areas[1].height, floors[1]);
        assert_eq!(areas[0].y, area.y);
        assert_eq!(areas[1].y, area.y + areas[0].height);

        // Both have more list than will fit: the longer one draws the larger
        // share of what is over their floors, and neither falls below its
        // own.
        let areas = stacked_pane_areas(area, &[30, 14], &floors);
        assert_eq!(
            areas[0].height + areas[1].height,
            area.height
        );
        assert!(floors[0] < areas[0].height);
        assert!(floors[1] < areas[1].height);
        assert!(areas[1].height < areas[0].height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sloop: max_mercenaries = 6, max_pirates = 7.
    fn sloop() -> &'static Ship {
        SHIPS.iter().find(|s| s.name == "Sloop").unwrap()
    }

    #[test]
    fn staffing_flags_understaffed_when_room_and_mercs_left() {
        // 2 players + 2 swabbies = 4 < 7 pirates, 2 < 6 mercs.
        assert_eq!(
            staffing(sloop(), 2, 2),
            Some(Staffing::Understaffed)
        );
    }

    #[test]
    fn staffing_is_clear_when_full_or_mercs_capped() {
        // Exactly at the pirate cap: not understaffed, not invalid.
        assert_eq!(staffing(sloop(), 1, 6), None);
        // Mercenary cap reached even with a free pirate slot: don't nag to
        // hire.
        assert_eq!(staffing(sloop(), 0, 6), None);
    }

    #[test]
    fn staffing_is_invalid_when_over_either_cap() {
        // Too many swabbies for the merc cap.
        assert_eq!(
            staffing(sloop(), 0, 7),
            Some(Staffing::Invalid)
        );
        // Too many bodies for the pirate cap (overstaffed beats understaffed).
        assert_eq!(
            staffing(sloop(), 5, 4),
            Some(Staffing::Invalid)
        );
    }

    /// A cache holding one pirate of the given rank and crew.
    fn crewed(name: &str, rank: &str, crew: &str) -> PirateCache {
        let mut cache = PirateCache::new();
        cache.fetched.insert(
            pirate::normalize_name(name).expect("a pirate name"),
            CachedPirate {
                basic: BasicInfo {
                    name: name.to_owned(),
                    crew_rank: rank.to_owned(),
                    crew_role: None,
                    crew_name: crew.to_owned(),
                    flag_rank: "Member".to_owned(),
                    flag_name: "Example Flag".to_owned(),
                    reputation: Default::default(),
                    skills: Default::default(),
                },
                trophies: Default::default(),
                basic_fetched_at: chrono::DateTime::<chrono::Utc>::MIN_UTC,
                trophies_fetched_at: chrono::DateTime::<chrono::Utc>::MIN_UTC,
            },
        );
        cache
    }

    const OUR_CREW: &str = "The Example Crew";

    #[test]
    fn a_crewmate_is_tagged_with_their_rank() {
        let bold = Style::default().bold();
        for (rank, tag, style) in [
            ("Captain", "Ca", bold.underlined()),
            ("Senior Officer", "SO", bold),
            ("Fleet Officer", "FO", bold),
            ("Officer", "Of", Style::default()),
            ("Pirate", "Pi", Style::default()),
            ("Cabin Person", "CP", Style::default()),
        ] {
            let cache = crewed("Playerone", rank, OUR_CREW);
            assert_eq!(
                crew_tag(&cache, Some(OUR_CREW), "Playerone"),
                Some((tag, style)),
                "a {rank} of our own crew",
            );
        }
    }

    /// The tag says "ours, and this is their rank", so it has nothing to say
    /// about anyone else aboard.
    #[test]
    fn only_our_own_crew_is_tagged() {
        let ours = crewed("Playerone", "Officer", OUR_CREW);
        // Another crew, and no crew at all.
        assert_eq!(
            crew_tag(
                &crewed("Playerone", "Officer", "The Other Crew"),
                Some(OUR_CREW),
                "Playerone",
            ),
            None,
        );
        assert_eq!(
            crew_tag(
                &crewed("Playerone", "Officer", ""),
                Some(OUR_CREW),
                "Playerone"
            ),
            None,
        );
        // A jobber sails with us without being one of us, and a rank we don't
        // know is no rank to tag.
        assert_eq!(
            crew_tag(
                &crewed("Playerone", "Jobbing Pirate", OUR_CREW),
                Some(OUR_CREW),
                "Playerone",
            ),
            None,
        );
        assert_eq!(
            crew_tag(
                &crewed("Playerone", "Deckhand", OUR_CREW),
                Some(OUR_CREW),
                "Playerone",
            ),
            None,
        );
        // A pirate the cache has yet to fetch, and a crew of our own we don't
        // know either.
        assert_eq!(
            crew_tag(&ours, Some(OUR_CREW), "Playertwo"),
            None
        );
        assert_eq!(crew_tag(&ours, None, "Playerone"), None);
    }

    /// Tags are laid against the right edge of the row they are given, so a
    /// roster's strips end in one column however long the names are and however
    /// many tags each row has earned.
    #[test]
    fn tags_are_laid_against_the_right_edge() {
        let ca = ("Ca", Style::default());
        let plank = ("!P", Style::default());
        let row = |name: &str, tags: Tags| {
            let line = Line::from(Span::raw(name.to_owned()));
            with_tags(&line, &tags, 12)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };

        assert_eq!(
            row("Playerone", vec![ca]),
            "Playerone Ca"
        );
        assert_eq!(row("Mate", vec![ca]), "Mate      Ca");
        assert_eq!(
            row("Mate", vec![plank, ca]),
            "Mate   !P Ca"
        );
        // One tag ends where two of them end: no column is held for the tag a
        // row has not earned.
        assert_eq!(row("Mate", vec![plank]), "Mate      !P");
        // Nothing to pad with leaves the tags where they will fit, and a row of
        // no tags spends nothing at all - the blank before the strip included.
        assert_eq!(
            row("Playertwelve", vec![ca]),
            "PlayertwelveCa"
        );
        assert_eq!(row("Mate", Vec::new()), "Mate");
    }

    /// A row asks for its tags' columns and the blank before them, and for
    /// neither when it wears none.
    #[test]
    fn a_row_asks_only_for_the_tags_it_wears() {
        let tag = ("Ca", Style::default());
        assert_eq!(tags_w(&Vec::new()), 0);
        assert_eq!(tags_w(&vec![tag]), TAG_GAP + TAG_W);
        assert_eq!(
            tags_w(&vec![tag, tag]),
            TAG_GAP + TAG_W + TAG_SEP + TAG_W,
        );
    }

    /// A note wraps at the blanks and at its newlines, and every byte of it
    /// lands in exactly one line: the offsets are what lets the caret be found
    /// in the lines later.
    #[test]
    fn a_note_wraps_at_the_blanks_and_keeps_its_offsets() {
        let text = "one two three four";
        assert_eq!(
            wrap_offsets(text, 9),
            vec![(0, "one two"), (8, "three"), (14, "four")],
        );
        // Each line starts where the one before it left off, blanks and all.
        for (start, line) in wrap_offsets(text, 9) {
            assert!(text[start ..].starts_with(line));
        }
        // A word longer than the line is broken at the margin rather than
        // hanging over it.
        assert_eq!(
            wrap_offsets("unbreakable", 4),
            vec![(0, "unbr"), (4, "eaka"), (8, "ble")],
        );
        // Nothing written is one empty line, not no lines: the caret has to sit
        // somewhere.
        assert_eq!(wrap_offsets("", 10), vec![(0, "")]);
    }

    /// A newline ends its line however much room is left on it, and one typed
    /// at the end of the note earns the empty line under it.
    #[test]
    fn a_note_breaks_at_its_newlines() {
        assert_eq!(
            wrap_offsets("one\ntwo", 20),
            vec![(0, "one"), (4, "two")]
        );
        // A blank line between two paragraphs is a line of its own.
        assert_eq!(
            wrap_offsets("one\n\ntwo", 20),
            vec![(0, "one"), (4, ""), (5, "two")],
        );
        // The caret has somewhere to sit after the newline just typed.
        assert_eq!(
            wrap_offsets("one\n", 20),
            vec![(0, "one"), (4, "")]
        );
    }

    /// The caret is found on the line that holds it, and rests at a line's end
    /// when it sits on the blank that line broke at.
    #[test]
    fn the_caret_is_found_in_the_wrapped_lines() {
        let text = "one two three";
        let lines = wrap_offsets(text, 7);
        assert_eq!(
            lines,
            vec![(0, "one two"), (8, "three")]
        );
        assert_eq!(caret_in(&lines, 0), (0, 0));
        assert_eq!(caret_in(&lines, 4), (0, 4));
        // The blank at offset 7 was eaten by the break, so the caret rests at
        // the end of the line it ended.
        assert_eq!(caret_in(&lines, 7), (0, 7));
        assert_eq!(caret_in(&lines, 8), (1, 0));
        assert_eq!(caret_in(&lines, text.len()), (1, 5));
    }

    /// ↑ and ↓ step a wrapped line at a time, keeping the column as far as the
    /// line reaches, and answer whether there was a line to step to — which is
    /// how ↓ off the last line comes to hand the keys to the buttons.
    #[test]
    fn the_notes_caret_steps_by_wrapped_line() {
        let mut np = NotePopup {
            name: "Playerone".to_owned(),
            field: crate::utils::PromptField::new(
                "Note",
                crate::utils::FieldKind::Paragraph,
            ),
            focus: NoteFocus::Text,
            offset: 0,
            wrap_w: 7,
            view_h: 4,
        };
        np.field.value = "one two three".to_owned();
        np.field.cursor = 4; // "one |two"

        assert!(note_caret_step(&mut np, true));
        assert_eq!(np.field.cursor, 12); // "thre|e", clamped to the line's end
        assert!(note_caret_step(&mut np, false));
        assert_eq!(np.field.cursor, 4);
        // Nowhere to step from the first line, nor from the last.
        assert!(!note_caret_step(&mut np, false));
        assert_eq!(np.field.cursor, 4);
        np.field.cursor = np.field.value.len();
        assert!(!note_caret_step(&mut np, true));
        assert_eq!(np.field.cursor, np.field.value.len());
    }

    /// The popup offers to add a note where none is written, to edit one where
    /// there is, and offers nothing at all where no note can be kept.
    #[test]
    fn the_note_button_says_which_of_the_two_it_does() {
        let labels = |note: Option<&str>| {
            pirate_popup_buttons(note)
                .into_iter()
                .map(|(label, _)| label)
                .collect::<Vec<&str>>()
        };
        assert_eq!(
            labels(Some("")),
            vec!["Add Note", "See Trophies", "Close"]
        );
        assert_eq!(
            labels(Some("   ")),
            vec!["Add Note", "See Trophies", "Close"]
        );
        assert_eq!(
            labels(Some("Fine gunner")),
            vec!["Edit Note", "See Trophies", "Close"],
        );
        assert_eq!(
            labels(None),
            vec!["See Trophies", "Close"]
        );
    }

    /// The plank mark is worn by a pirate we planked off this vessel who is
    /// aboard again, whether or not they are one of ours.
    #[test]
    fn the_plank_mark_marks_a_pirate_we_planked_before() {
        let plank = ("!P", Style::default());
        let pirate = ("Pi", Style::default());
        let mut vessel = Vessel::default();
        vessel.planked_by_us.insert("Playertwo".to_owned());
        let ours = crewed("Playertwo", "Pirate", OUR_CREW);
        let theirs = crewed("Playertwo", "Pirate", "The Other Crew");

        // Planked and ours: the mark, then the rank.
        assert_eq!(
            aboard_tags(
                &ours,
                Some(OUR_CREW),
                Some(&vessel),
                "Playertwo"
            ),
            vec![plank, pirate],
        );
        // Planked and not ours: the mark alone.
        assert_eq!(
            aboard_tags(
                &theirs,
                Some(OUR_CREW),
                Some(&vessel),
                "Playertwo"
            ),
            vec![plank],
        );
        // Ours and never planked: the rank alone.
        let clean = Vessel::default();
        assert_eq!(
            aboard_tags(
                &ours,
                Some(OUR_CREW),
                Some(&clean),
                "Playertwo"
            ),
            vec![pirate],
        );
        // Neither: no tags, and so no columns spent on them.
        assert_eq!(
            aboard_tags(
                &ours,
                Some(OUR_CREW),
                Some(&clean),
                "Playerone"
            ),
            Tags::new(),
        );
    }
}
