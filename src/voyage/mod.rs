//! Voyage statistics — the per-pillage record of a single sail->port run.
//!
//! This module owns the in-RAM data model ([`Voyage`], [`Battle`], and friends);
//! the chat-log state machine in [`crate::chatlog`] builds these as it parses,
//! and the submodules turn them into numbers, pixels, and JSON:
//! - [`stats`] — derived statistics (consumption, win-rate, box plots).
//! - [`ui`] — the Voyage Statistics page rendering and input handling.
//! - [`persistence`] — the RAM->disk path (`voyages.json`).
//!
//! See the `voyage-statistics-model` memory for the design rationale.

pub mod persistence;
pub mod stats;
pub mod ui;

use chrono::NaiveDateTime;

use crate::chatlog::JobKind;

// ---------------------------------------------------------------------------
// Voyage statistics (per sail->port run)
// ---------------------------------------------------------------------------

/// How a single sea engagement ended, from our point of view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BattleOutcome {
    /// Still in progress — no resolution line seen yet.
    #[default]
    Ongoing,
    /// We won the boarding (our own name was among the `Game over` winners).
    Won,
    /// We lost (our name not among the winners) — the plundered PoE went to them.
    Lost,
    /// Ended without a boarding conclusion: someone disengaged, the enemy ported,
    /// or we shook the pursuit.
    Disengaged,
}

/// What we fought in a battle. Detected from log telltales; defaults to a
/// generic `Brigand` when we can't tell (per the user: "brigands for those we
/// cannot parse"). The monster variants are reserved — their reliable per-fight
/// telltales live in other voyage types and are wired in a later pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // monster variants detected later
pub enum BattleCategory {
    #[default]
    Brigand,
    /// A named Brigand King / royalty (e.g. "Vargas the Mad", "Admiral Finius").
    BrigandKing(String),
    Vampirate,
    Skelly,
    Werewolf,
    Zombie,
}

/// One sea engagement, from interception to its resolution. Both the naval phase
/// (interception -> grapple) and the boarding melee (grapple -> `Game over`) are
/// timed; either may be absent (e.g. a disengage before grappling).
#[derive(Clone, Debug, Default)]
pub struct Battle {
    /// Enemy vessel name from the interception line (`None` if unparsed).
    pub enemy: Option<String>,
    /// True if we intercepted them; false if they intercepted us.
    pub we_intercepted: bool,
    /// Interception time — the engagement start.
    pub started_at: Option<NaiveDateTime>,
    /// Grapple time — the sea phase ends and the boarding melee begins. `None` if
    /// the fight never reached a boarding.
    pub grappled_at: Option<NaiveDateTime>,
    /// Resolution time (`Game over`, disengage, or enemy ported).
    pub ended_at: Option<NaiveDateTime>,
    /// Outcome from our perspective.
    pub outcome: BattleOutcome,
    /// Gross PoE the victors plundered. We store it signed by [`Self::outcome`]:
    /// positive when we won, negative when we lost (the PoE was taken from us).
    pub poe: Option<i64>,
    /// Units of goods in the plunder (a bare count — the log never itemizes).
    pub goods: Option<u32>,
    /// Our personal share from `Ye received N ... initial cut of the booty!`.
    pub my_cut: Option<u64>,
    /// Pirates aboard our vessel at resolution (real players incl. us).
    pub pirates: u32,
    /// Swabbies aboard our vessel at resolution.
    pub swabbies: u32,
    /// What we were fighting (best-effort; defaults to generic Brigand).
    pub category: BattleCategory,
    /// Damage advantage (ours − theirs) snapshotted from the Damage calculator at
    /// resolution: `[-0.5, +0.5]`. `None` if no damage was tracked for this fight.
    pub advantage_dmg: Option<f64>,
    /// Headcount advantage snapshotted at resolution (our pirates × our advantage
    /// − enemy swabbies × their advantage). `None` if no damage was tracked.
    pub advantage_crew: Option<f64>,
}

impl Battle {
    /// Naval-phase duration (interception -> grapple), in seconds.
    pub fn sea_secs(&self) -> Option<i64> {
        secs_between(self.started_at, self.grappled_at)
    }
    /// Boarding-melee duration (grapple -> resolution), in seconds.
    pub fn boarding_secs(&self) -> Option<i64> {
        secs_between(self.grappled_at, self.ended_at)
    }
    /// Whole-engagement duration (interception -> resolution), in seconds.
    pub fn total_secs(&self) -> Option<i64> {
        secs_between(self.started_at, self.ended_at)
    }
}

/// A snapshot of the headcount aboard at one instant — the basis for the
/// time-weighted average crew (the "integral" of crew over the run).
#[derive(Clone, Copy, Debug)]
pub struct CrewSample {
    pub at: NaiveDateTime,
    /// Real pirates aboard, including us.
    pub pirates: u32,
    /// Swabbies (NPC crew) aboard.
    pub swabbies: u32,
}

/// One complete sail->port run aboard a vessel — the unit of voyage statistics.
/// The job kind may change mid-run (`This vessel is now ...`) without resetting
/// stats; we keep the declaration in force when we set sail as the headline.
#[derive(Clone, Debug, Default)]
pub struct Voyage {
    /// Job kind in force when we set sail.
    pub job_kind: Option<JobKind>,
    /// When we set sail (first `set the vessel to sail` order of the run).
    pub sailed_at: Option<NaiveDateTime>,
    /// When we put into port — the end of the timed run. `None` while still out.
    pub ported_at: Option<NaiveDateTime>,
    /// The battle currently in progress, if any.
    pub current_battle: Option<Battle>,
    /// Resolved battles, in chronological order.
    pub battles: Vec<Battle>,
    /// Headcount samples at each crew change while underway (sail-time first).
    /// Drives the time-weighted average used for per-crew consumption stats.
    pub crew_samples: Vec<CrewSample>,
    /// We left the vessel mid-run, so this voyage's data has gaps.
    pub poisoned: bool,
    /// Runtime-only: the user has saved or dismissed this run via the
    /// save/discard prompt, so it shouldn't be offered again. Not persisted.
    pub saved: bool,
}

impl Voyage {
    /// Sail-to-port duration, in seconds (`None` until ported).
    pub fn duration_secs(&self) -> Option<i64> {
        secs_between(self.sailed_at, self.ported_at)
    }

    /// Time-weighted average of a crew field over the run (a step-function
    /// integral from sail to port, divided by the duration). Crew before the
    /// first sample is taken to equal the first sample. `None` until ported or
    /// if no samples were collected.
    #[allow(dead_code)] // reached only via avg_pirates/avg_swabbies (UI task #7)
    fn avg_crew(&self, field: impl Fn(&CrewSample) -> u32) -> Option<f64> {
        let start = self.sailed_at?;
        let end = self.ported_at?;
        let total = (end - start).num_seconds();
        if total <= 0 || self.crew_samples.is_empty() {
            return self.crew_samples.last().map(|s| field(s) as f64);
        }
        let mut area = 0.0f64;
        for (i, s) in self.crew_samples.iter().enumerate() {
            // Sample `i`'s value holds from its time (or `start` for the first)
            // until the next sample's time (or `end` for the last), clamped.
            let seg_start = if i == 0 { start } else { s.at.max(start) };
            let seg_end = self
                .crew_samples
                .get(i + 1)
                .map_or(end, |n| n.at)
                .min(end);
            let dur = (seg_end - seg_start).num_seconds();
            if dur > 0 {
                area += field(s) as f64 * dur as f64;
            }
        }
        Some(area / total as f64)
    }

    /// Time-weighted average pirates aboard (incl. us) over the run.
    #[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
    pub fn avg_pirates(&self) -> Option<f64> {
        self.avg_crew(|s| s.pirates)
    }

    /// Time-weighted average swabbies aboard over the run.
    #[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
    pub fn avg_swabbies(&self) -> Option<f64> {
        self.avg_crew(|s| s.swabbies)
    }
}

/// Seconds between two optional timestamps, or `None` if either is missing.
fn secs_between(a: Option<NaiveDateTime>, b: Option<NaiveDateTime>) -> Option<i64> {
    Some((b? - a?).num_seconds())
}
