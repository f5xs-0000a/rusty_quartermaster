//! On-disk persistence for completed voyages.
//!
//! This is the **only** path from the in-RAM [`crate::voyage::Voyage`] data to
//! disk — written when the user confirms via the save/discard prompt, never
//! automatically. The file is **per-user-behind-keyboard** (one human's history
//! across all their pirates), a `voyages.json` sibling of the `--cache` file.
//!
//! The on-disk shape is decoupled from the runtime structs (like `cache.rs` and
//! `profits::persistence`): we store precomputed numeric fields so the format
//! doesn't churn with internal refactors and we never need to round-trip
//! `chrono` timestamps. Aggregates are computed at save time. See the
//! `voyage-statistics-model` memory.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ships::SHIPS;
use crate::voyage::{effective_outcome, BattleCategory, BattleOutcome, TeamSide, Voyage};

/// A persisted Damage-calculator snapshot for a recorded fight. Ships are stored
/// by name (robust to `SHIPS` reordering).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedSnapshot {
    #[serde(default)]
    pub our_ship: String,
    #[serde(default)]
    pub foe_ship: String,
    #[serde(default)]
    pub our_hits: [u32; 3],
    #[serde(default)]
    pub foe_hits: [u32; 3],
    #[serde(default)]
    pub headon: u32,
    #[serde(default)]
    pub our_pirates: u32,
}

/// One side of a persisted melee — real players by name + a swabbie count.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedTeam {
    #[serde(default)]
    pub players: Vec<String>,
    #[serde(default)]
    pub swabbies: u32,
}

/// One persisted sea battle (enough to rebuild the loot/timing histograms).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedBattle {
    /// "won" / "lost" / "disengaged" / "ongoing" / "unknown" (the last when our
    /// identity wasn't confirmed, so win/loss couldn't be determined).
    #[serde(default)]
    pub outcome: String,
    /// Enemy category: "Brigands", "King: <name>", "Vampirates", "Players" (PvP).
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub poe: Option<i64>,
    #[serde(default)]
    pub goods: Option<u32>,
    #[serde(default)]
    pub pirates: u32,
    #[serde(default)]
    pub swabbies: u32,
    #[serde(default)]
    pub total_secs: Option<i64>,
    #[serde(default)]
    pub naval_secs: Option<i64>,
    #[serde(default)]
    pub boarding_secs: Option<i64>,
    /// Our side of the melee — players (by name) + swabbie count. Swabbie
    /// identities are intentionally not stored.
    #[serde(default)]
    pub our_team: Option<SavedTeam>,
    /// The foe's side, when the melee resolved it. `None` for a disengage or an
    /// unknown-identity fight.
    #[serde(default)]
    pub their_team: Option<SavedTeam>,
    /// Whether the fight was recorded — only recorded fights carry the calculator
    /// snapshot on disk. (Damage advantage is derived from the snapshot, not stored.)
    #[serde(default)]
    pub recorded: bool,
    #[serde(default)]
    pub snapshot: Option<SavedSnapshot>,
}

/// One persisted voyage.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedVoyage {
    /// Human-readable timestamp the run ended (the ported time), for display.
    #[serde(default)]
    pub ended_at: String,
    #[serde(default)]
    pub vessel: Option<String>,
    #[serde(default)]
    pub job: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<i64>,
    /// Time-weighted average crew over the run.
    #[serde(default)]
    pub avg_pirates: Option<f64>,
    #[serde(default)]
    pub avg_swabbies: Option<f64>,
    #[serde(default)]
    pub battles: Vec<SavedBattle>,
}

/// The whole persisted history file.
#[derive(Serialize, Deserialize, Default)]
pub struct SavedVoyages {
    #[serde(default)]
    pub voyages: Vec<SavedVoyage>,
}

fn outcome_str(o: BattleOutcome) -> &'static str {
    match o {
        BattleOutcome::Won => "won",
        BattleOutcome::Lost => "lost",
        BattleOutcome::Disengaged => "disengaged",
        BattleOutcome::Ongoing => "ongoing",
        BattleOutcome::Unknown => "unknown",
    }
}

fn category_str(c: &BattleCategory) -> String {
    match c {
        BattleCategory::Brigand => "Brigands".to_string(),
        BattleCategory::BrigandKing(name) => format!("King: {name}"),
        BattleCategory::Vampirate => "Vampirates".to_string(),
        BattleCategory::Skelly => "Skellies".to_string(),
        BattleCategory::Werewolf => "Werewolves".to_string(),
        BattleCategory::Zombie => "Zombies".to_string(),
        BattleCategory::BlackShip => "Black Ship".to_string(),
        BattleCategory::MonkeyBoat => "Monkey Boat".to_string(),
        BattleCategory::Pvp => "Players".to_string(),
    }
}

/// Persisted form of one melee side. Swabbie identities are dropped (count only).
fn saved_team(t: &TeamSide) -> SavedTeam {
    SavedTeam {
        players: t.players.clone(),
        swabbies: t.swabbies,
    }
}

/// Snapshot a completed voyage into its persisted form. Aggregates (duration,
/// average crew) are computed now, while the run is finalized. `self_confirmed`
/// masks unconfirmed win/loss verdicts (and their PoE sign) to "unknown" — see
/// [`effective_outcome`].
pub fn from_voyage(v: &Voyage, vessel: Option<&str>, self_confirmed: bool) -> SavedVoyage {
    SavedVoyage {
        ended_at: v.ported_at.map(|t| t.to_string()).unwrap_or_default(),
        vessel: vessel.map(str::to_string),
        job: v.job_kind.as_ref().map(|j| j.to_string()),
        duration_secs: v.duration_secs(),
        avg_pirates: v.avg_pirates(),
        avg_swabbies: v.avg_swabbies(),
        battles: v
            .battles
            .iter()
            .map(|b| {
                let outcome = effective_outcome(b.outcome, self_confirmed);
                // A masked (unknown) verdict can't carry a signed PoE.
                let poe = matches!(outcome, BattleOutcome::Won | BattleOutcome::Lost)
                    .then_some(b.poe)
                    .flatten();
                SavedBattle {
                    outcome: outcome_str(outcome).to_string(),
                    category: category_str(&b.category),
                    poe,
                    goods: b.goods,
                    pirates: b.pirates,
                    swabbies: b.swabbies,
                    total_secs: b.total_secs(),
                    naval_secs: b.sea_secs(),
                    boarding_secs: b.boarding_secs(),
                    our_team: b.our_team.as_ref().map(saved_team),
                    their_team: b.their_team.as_ref().map(saved_team),
                    // The calculator snapshot is written only for recorded fights —
                    // that's what "recording" means. Advantage is derived from it.
                    recorded: b.recorded,
                    snapshot: if b.recorded {
                        b.snapshot.map(saved_snapshot)
                    } else {
                        None
                    },
                }
            })
            .collect(),
    }
}

/// Convert an in-RAM [`crate::voyage::BattleSnapshot`] to its persisted form,
/// resolving ship indices to names.
fn saved_snapshot(s: crate::voyage::BattleSnapshot) -> SavedSnapshot {
    let name = |i: usize| SHIPS.get(i).map(|sh| sh.name.to_string()).unwrap_or_default();
    SavedSnapshot {
        our_ship: name(s.our_ship),
        foe_ship: name(s.foe_ship),
        our_hits: s.our_hits,
        foe_hits: s.foe_hits,
        headon: s.headon,
        our_pirates: s.our_pirates,
    }
}

/// Load the voyage history from `path`. A missing or unparseable file yields an
/// empty history rather than an error, so a first run just starts fresh.
pub fn load(path: &Path) -> SavedVoyages {
    let Ok(data) = std::fs::read_to_string(path) else {
        return SavedVoyages::default();
    };
    match serde_json::from_str(&data) {
        Ok(v) => {
            eprintln!("Loaded voyage history from {}", path.display());
            v
        }
        Err(e) => {
            eprintln!("warning: failed to parse voyage history: {e}");
            SavedVoyages::default()
        }
    }
}

/// Write the voyage history to `path` as pretty JSON.
pub fn save(path: &Path, voyages: &SavedVoyages) {
    let json = match serde_json::to_string_pretty(voyages) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("error: failed to serialize voyage history: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(path, json) {
        eprintln!(
            "error: failed to write voyage history to {}: {e}",
            path.display()
        );
    } else {
        eprintln!("Saved voyage history to {}", path.display());
    }
}
