//! The persisted form of a completed voyage.
//!
//! This is the **only** path from the in-RAM [`crate::voyage::Voyage`] data to
//! disk — written when the user confirms via the save prompt, never
//! automatically, and only the parts of a run that prompt agreed to keep (see
//! [`SaveParts`]). These land in the persistence file's voyage history (see
//! [`crate::persistence`]), which is **per-user-behind-keyboard**: one human's
//! voyages across all their pirates.
//!
//! The on-disk shape is decoupled from the runtime structs (like `cache.rs` and
//! `profits::persistence`): we store precomputed numeric fields so the format
//! doesn't churn with internal refactors and we never need to round-trip
//! `chrono` timestamps. Aggregates are computed at save time. See the
//! `voyage-statistics-model` memory.

use serde::{Deserialize, Serialize};

use crate::{
    ships::SHIPS,
    voyage::{
        Battle,
        BattleCategory,
        BattleOutcome,
        BattleSnapshot,
        FightTimeline,
        KoEvent,
        KoSide,
        TeamSide,
        Voyage,
        effective_outcome,
        stats::{ConsumptionStats, RumUse},
    },
};

/// A persisted Damage-calculator snapshot for a recorded fight. Ships are
/// stored by name (robust to `SHIPS` reordering). Our own hull is *not* stored
/// here — it's the same for every fight of a voyage, so it's derived from the
/// voyage's `ship_type` on load (see [`SavedBattle::to_battle`]).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedSnapshot {
    #[serde(default)]
    pub foe_ship: String,
    /// Hits *we* took: `[shots, rocks]`.
    #[serde(default)]
    pub our_hits: [u32; 2],
    /// Hits the *foe* took: `[shots, rocks]`.
    #[serde(default)]
    pub foe_hits: [u32; 2],
    /// Times rammed — a single shared count (a ram damages both ships; a
    /// head-on counts twice for a different-size-class foe).
    #[serde(default)]
    pub rams: u32,
    #[serde(default)]
    pub our_pirates: u32,
}

/// One side of a persisted melee — real players by name + disjoint NPC-crew
/// counts.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedTeam {
    #[serde(default)]
    pub players: Vec<String>,
    /// Genuine swabbies (excluding mercenaries). Total NPC crew = `swabbies +
    /// mercenaries`.
    #[serde(default)]
    pub swabbies: u32,
    /// Mercenaries — a distinct crew kind, disjoint with `swabbies`. Persisted
    /// to preserve the exact swabbie/mercenary split after a reload (for the
    /// roster and per-merc stats); mercenaries earn no divvy share, so this
    /// does not feed the "Value per Share" metric. Legacy files (pre-field)
    /// and the enemy side default to `0`, folding those bodies into
    /// `swabbies`.
    #[serde(default)]
    pub mercenaries: u32,
}

/// One persisted elimination on the per-fight advantage timeline. The KO'd name
/// is intentionally dropped (the graph never lists eliminations); only its
/// timing and side are kept.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedKo {
    /// Seconds from the fight start (`None` if the clock was unknown).
    #[serde(default)]
    pub secs: Option<i64>,
    /// "us" or "them".
    #[serde(default)]
    pub side: String,
}

/// One persisted sea battle (enough to rebuild the loot/timing histograms).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedBattle {
    /// "won" / "lost" / "disengaged" / "ongoing" / "unknown" (the last when
    /// our identity wasn't confirmed, so win/loss couldn't be determined).
    #[serde(default)]
    pub outcome: String,
    /// Enemy category: "Brigands", "King: <name>", "Vampirates", "Players"
    /// (PvP).
    #[serde(default)]
    pub category: String,
    /// The foe's known hull type (special encounters announce it; otherwise
    /// `None`). Stored by name, robust to `SHIPS` reordering.
    #[serde(default)]
    pub foe_ship: Option<String>,
    /// The enemy vessel's proper name from the interception line (e.g. a named
    /// brigand or PvP ship). `None` when the interception line was unparsed or
    /// carried no vessel name. Distinct from `foe_ship` (the hull type).
    #[serde(default)]
    pub enemy: Option<String>,
    #[serde(default)]
    pub poe: Option<i64>,
    #[serde(default)]
    pub goods: Option<u32>,
    #[serde(default)]
    pub pirates: u32,
    /// Total NPC crew (swabbies + mercenaries) for manpower; the genuine
    /// swabbie/mercenary split is in `our_team`.
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
    /// The foe's side, when the melee resolved it. `None` for a disengage or
    /// an unknown-identity fight.
    #[serde(default)]
    pub their_team: Option<SavedTeam>,
    /// The Damage-calculator snapshot, written only for recorded fights — its
    /// presence *is* the "recorded" flag (damage advantage is derived from it,
    /// not stored). Absent for unrecorded fights and older history.
    #[serde(default)]
    pub snapshot: Option<SavedSnapshot>,
    /// The side-tagged elimination timeline for the per-fight advantage graph.
    /// Always persisted (independent of `recorded`); empty when the fight
    /// logged no melee KOs, or for older history. See
    /// [`SavedBattle::to_timeline`].
    #[serde(default)]
    pub timeline: Vec<SavedKo>,
    /// Our / their starting headcounts for the advantage graph's absolute
    /// baseline.
    #[serde(default)]
    pub our_start: u32,
    #[serde(default)]
    pub their_start: Option<u32>,
}

impl SavedBattle {
    /// Rebuild the in-RAM [`FightTimeline`] from the persisted form. Timestamps
    /// are synthesized from the stored second-offsets (origin at the Unix
    /// epoch) so the graph's wall-clock axis works; the absolute clock is
    /// irrelevant — only the gaps between KOs matter.
    pub fn to_timeline(&self) -> FightTimeline {
        let from_secs = |s: i64| {
            chrono::DateTime::from_timestamp(s, 0).map(|d| d.naive_utc())
        };
        let events = self
            .timeline
            .iter()
            .map(|k| {
                KoEvent {
                    at: k.secs.and_then(from_secs),
                    side: if k.side == "us" {
                        KoSide::Ours
                    } else {
                        KoSide::Theirs
                    },
                }
            })
            .collect();
        FightTimeline {
            events,
            our_start: self.our_start,
            their_start: self.their_start,
            started_at: from_secs(0),
            ended_at: None,
        }
    }
}

/// Consumables used over a voyage, snapshotted at save time from the Profits
/// stock delta (`Restock - Stock`). The live delta can't be reconstructed once
/// the hold is restocked, so it's frozen here. Rum is stored as the raw
/// per-tier counts (the potency-weighted total is derived). Cannonballs are
/// size-agnostic. Every figure is a whole-voyage total — the per-crew rates are
/// game constants and so aren't kept. `None` on a [`SavedVoyage`] means
/// consumption wasn't recorded for that run (e.g. older history, or the user
/// declined to store it).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedConsumption {
    /// Cannonballs fired, summed across all sizes (a ship burns only its own).
    #[serde(default)]
    pub cannonballs: u64,
    #[serde(default)]
    pub swill: u64,
    #[serde(default)]
    pub grog: u64,
    #[serde(default)]
    pub fine_rum: u64,
    #[serde(default)]
    pub rum_spice: u64,
}

/// One good won over a voyage, taken from the Profits "Booty" column
/// (Stock/Hold and Restock excluded). A plain quantity by commodity name — no
/// market value is stored (prices are volatile and gone on reload).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedBootyGood {
    #[serde(default)]
    pub commodity: String,
    #[serde(default)]
    pub quantity: u64,
}

/// The per-voyage booty figures snapshotted from the Profits page at save time
/// — the input side of the two `SavedVoyage` booty fields. Assembled by the
/// app, which owns the live Profits state.
#[derive(Default)]
pub struct BootySnapshot {
    /// PoE in the booty chest: the user-entered "Booty Chest" figure if given,
    /// else the auto-deduced net chest. `None` when not recorded.
    pub chest: Option<u64>,
    /// `(commodity name, quantity)` for each good in the Booty column.
    pub goods: Vec<(String, u64)>,
}

/// Which parts of a run the user agreed to keep, as the save prompt left them.
/// A declined part is written as absent rather than as empty, so a reloaded
/// voyage never reads as a run that fought nothing or won nothing when the
/// truth is that we were told not to record it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SaveParts {
    /// The per-fight records. The damage snapshots and melee timelines are
    /// part of a fight's record, so they go when it does.
    pub engagements: bool,
    /// Each recorded fight's Damage-calculator snapshot.
    pub damage: bool,
    /// Each fight's side-tagged elimination timeline and headcount baseline.
    pub melee: bool,
    /// The whole-voyage cannonball / rum / rum spice totals.
    pub consumption: bool,
    /// The booty: the chest's PoE and the Booty-column goods.
    pub booty: bool,
}

impl Default for SaveParts {
    /// Everything is kept; declining a part is the deliberate act.
    fn default() -> Self {
        SaveParts {
            engagements: true,
            damage: true,
            melee: true,
            consumption: true,
            booty: true,
        }
    }
}

impl SaveParts {
    /// Whether the Damage snapshots are written. They belong to a fight's
    /// record and cannot outlive it.
    pub fn writes_damage(&self) -> bool {
        self.engagements && self.damage
    }

    /// Whether the melee timelines are written, on the same terms.
    pub fn writes_melee(&self) -> bool {
        self.engagements && self.melee
    }
}

/// One persisted voyage.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SavedVoyage {
    /// Human-readable timestamp the run ended (the ported time), for display.
    #[serde(default)]
    pub ended_at: String,
    #[serde(default)]
    pub vessel: Option<String>,
    /// The vessel's ship type (hull) name, taken from the jobbers ship picker
    /// at save time. `None` if no ship was assigned. Stored by name,
    /// robust to `SHIPS` reordering.
    #[serde(default)]
    pub ship_type: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<i64>,
    /// The voyage's data has gaps (we left mid-run, or the hold ran too low on
    /// rum spice). Gates `ConsumptionStats::delta_unreliable` on reload. Older
    /// files default to `false`.
    #[serde(default)]
    pub poisoned: bool,
    /// The run reached a booty division. Gates the Divvy section (goods +
    /// booty PoE) in Voyage Statistics. Older files default to `false`.
    #[serde(default)]
    pub divvied: bool,
    /// Time-weighted average crew over the run.
    #[serde(default)]
    pub avg_pirates: Option<f64>,
    /// Time-weighted average total NPC crew (swabbies + mercenaries).
    #[serde(default)]
    pub avg_swabbies: Option<f64>,
    /// Time-weighted average mercenaries over the run, stored alongside the
    /// other crew averages. Legacy files default to `None` (old history
    /// didn't record it).
    #[serde(default)]
    pub avg_mercenaries: Option<f64>,
    /// Consumables used over the run, or `None` when not recorded. See
    /// [`SavedConsumption`].
    #[serde(default)]
    pub consumption: Option<SavedConsumption>,
    /// PoE remaining in the booty chest — the user-entered "Booty Chest"
    /// figure if given, else the auto-deduced net chest. `None` for older
    /// history or when it wasn't recorded.
    #[serde(default)]
    pub booty_chest: Option<u64>,
    /// Goods won this voyage, from the Profits "Booty" column only. One entry
    /// per commodity with a non-zero booty quantity. `None` means no booty
    /// was recorded at all (older history, or a run whose goods we don't
    /// stand behind), which an empty list does not: that says the column was
    /// read and held nothing. Per-voyage — the log can't attribute goods to
    /// individual battles (that split is shown only in-game).
    #[serde(default)]
    pub booty_goods: Option<Vec<SavedBootyGood>>,
    #[serde(default)]
    pub battles: Vec<SavedBattle>,
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

/// Persisted form of one melee side. Swabbie identities are dropped (count
/// only).
fn saved_team(t: &TeamSide) -> SavedTeam {
    SavedTeam {
        players: t.players.clone(),
        swabbies: t.swabbies,
        mercenaries: t.mercenaries,
    }
}

/// Snapshot a completed voyage into its persisted form. Aggregates (duration,
/// average crew) are computed now, while the run is finalized. `consumption` is
/// snapshotted from the live Profits state by the caller (the delta is gone
/// once the hold is restocked); the booty (chest + goods) was already frozen
/// onto the voyage at its divvy. `parts` is what the user agreed to keep — the
/// aggregates and the run's own identity always persist, the rest only when it
/// says so. `self_confirmed` masks unconfirmed win/loss verdicts (and their PoE
/// sign) to "unknown" — see [`effective_outcome`].
pub fn from_voyage(
    v: &Voyage,
    vessel: Option<&str>,
    ship_type: Option<&str>,
    consumption: Option<&ConsumptionStats>,
    parts: SaveParts,
    self_confirmed: bool,
) -> SavedVoyage {
    SavedVoyage {
        ended_at: v.ported_at.map(|t| t.to_string()).unwrap_or_default(),
        vessel: vessel.map(str::to_string),
        ship_type: ship_type.map(str::to_string),
        duration_secs: v.duration_secs(),
        poisoned: v.poisoned,
        divvied: v.divvied,
        avg_pirates: v.avg_pirates(),
        avg_swabbies: v.avg_swabbies(),
        avg_mercenaries: v.avg_mercenaries(),
        consumption: consumption.filter(|_| parts.consumption).map(|c| {
            SavedConsumption {
                cannonballs: c.balls,
                swill: c.rum.swill,
                grog: c.rum.grog,
                fine_rum: c.rum.fine_rum,
                rum_spice: c.rum_spice,
            }
        }),
        booty_chest: v.booty_chest.filter(|_| parts.booty),
        booty_goods: v.booty_goods.as_ref().filter(|_| parts.booty).map(
            |goods| {
                goods
                    .iter()
                    .map(|(commodity, quantity)| {
                        SavedBootyGood {
                            commodity: commodity.clone(),
                            quantity: *quantity,
                        }
                    })
                    .collect()
            },
        ),
        battles: if parts.engagements {
            v.battles
                .iter()
                .map(|b| saved_battle(b, parts, self_confirmed))
                .collect()
        } else {
            Vec::new()
        },
    }
}

/// Snapshot one fight into its persisted form. `parts` decides whether the
/// Damage snapshot and the melee timeline come along; the fight's own metadata
/// (outcome, foe, timings, headcounts) is the record itself and always does.
fn saved_battle(
    b: &Battle,
    parts: SaveParts,
    self_confirmed: bool,
) -> SavedBattle {
    // Resolve a `SHIPS` index to its name, decoupling the file from index
    // churn.
    let ship_name = |i: usize| SHIPS.get(i).map(|sh| sh.name.to_string());
    let outcome = effective_outcome(b.outcome, self_confirmed);
    // A masked (unknown) verdict can't carry a signed PoE.
    let poe = matches!(
        outcome,
        BattleOutcome::Won | BattleOutcome::Lost
    )
    .then_some(b.poe)
    .flatten();
    SavedBattle {
        outcome: outcome_str(outcome).to_string(),
        category: category_str(&b.category),
        // Persist the known foe hull regardless of `recorded`: the
        // game-announced type, else the ship type set in the Damage calculator.
        // Lightweight metadata (the full snapshot below is still gated on
        // `recorded`), so the Ship Winrate history keeps this matchup even for
        // unrecorded saved fights.
        foe_ship: b
            .foe_ship
            .or_else(|| b.snapshot.map(|s| s.foe_ship))
            .and_then(ship_name),
        // The enemy vessel's proper name persists regardless of `recorded`
        // (log-derived metadata), so saved history keeps the named foe in the
        // Sea Battles popup rather than "Unknown vessel".
        enemy: b.enemy.clone(),
        poe,
        goods: b.goods,
        pirates: b.pirates,
        swabbies: b.swabbies,
        total_secs: b.total_secs(),
        naval_secs: b.sea_secs(),
        boarding_secs: b.boarding_secs(),
        our_team: b.our_team.as_ref().map(saved_team),
        their_team: b.their_team.as_ref().map(saved_team),
        // The calculator snapshot is written only for recorded fights — that's
        // what "recording" means, and its presence is what marks the fight
        // recorded on reload. Advantage is derived from it.
        snapshot: b
            .snapshot
            .filter(|_| b.recorded && parts.writes_damage())
            .map(saved_snapshot),
        // The advantage timeline is log-derived, not calculator state, so
        // `recorded` doesn't gate it. Event seconds are offsets from the fight
        // start.
        timeline: if parts.writes_melee() {
            b.timeline
                .events
                .iter()
                .map(|e| {
                    SavedKo {
                        secs: b
                            .timeline
                            .started_at
                            .zip(e.at)
                            .map(|(s, a)| (a - s).num_seconds()),
                        side: match e.side {
                            KoSide::Ours => "us",
                            KoSide::Theirs => "them",
                        }
                        .to_string(),
                    }
                })
                .collect()
        } else {
            Vec::new()
        },
        // The headcount baseline is the timeline's own axis; without the
        // eliminations it describes nothing.
        our_start: if parts.writes_melee() {
            b.timeline.our_start
        } else {
            0
        },
        their_start: b.timeline.their_start.filter(|_| parts.writes_melee()),
    }
}

/// Convert an in-RAM [`crate::voyage::BattleSnapshot`] to its persisted form,
/// resolving ship indices to names.
fn saved_snapshot(s: crate::voyage::BattleSnapshot) -> SavedSnapshot {
    let name = |i: usize| {
        SHIPS
            .get(i)
            .map(|sh| sh.name.to_string())
            .unwrap_or_default()
    };
    // `our_ship` is intentionally not persisted — it's derived from the voyage
    // hull on load. Only the foe hull (which varies per fight) is stored.
    SavedSnapshot {
        foe_ship: name(s.foe_ship),
        our_hits: s.our_hits,
        foe_hits: s.foe_hits,
        rams: s.rams,
        our_pirates: s.our_pirates,
    }
}

// ---------------------------------------------------------------------------
// Reconstruction (disk -> RAM) for read-only history pages
// ---------------------------------------------------------------------------
//
// The Voyage Statistics pager shows past voyages (loaded from this file) as
// read-only pages alongside the current login's live runs. Rendering reuses the
// live pipeline ([`crate::voyage::stats::battle_stats`], the chart/battle-row
// builders), so a `SavedVoyage` is rebuilt into an in-RAM [`Voyage`]. The shape
// is lossy — raw crew samples and exact clocks weren't persisted — so battle
// timestamps are synthesized from the stored per-fight durations (origin at the
// Unix epoch; only the gaps matter) and crew averages ride in via
// [`Voyage::avg_override`]. Consumption is rebuilt from the frozen counts, not
// recomputed from the (now-unrelated) live inventory.

/// A `SHIPS` index for a stored ship name, or `None` if the name isn't known.
fn ship_index(name: &str) -> Option<usize> {
    SHIPS.iter().position(|s| s.name == name)
}

fn outcome_from_str(s: &str) -> BattleOutcome {
    match s {
        "won" => BattleOutcome::Won,
        "lost" => BattleOutcome::Lost,
        "disengaged" => BattleOutcome::Disengaged,
        "unknown" => BattleOutcome::Unknown,
        _ => BattleOutcome::Ongoing,
    }
}

/// Inverse of [`category_str`]. Unrecognized labels fall back to a generic
/// Brigand (older files, or a category we no longer emit).
fn category_from_str(s: &str) -> BattleCategory {
    match s {
        "Vampirates" => BattleCategory::Vampirate,
        "Skellies" => BattleCategory::Skelly,
        "Werewolves" => BattleCategory::Werewolf,
        "Zombies" => BattleCategory::Zombie,
        "Black Ship" => BattleCategory::BlackShip,
        "Monkey Boat" => BattleCategory::MonkeyBoat,
        "Players" => BattleCategory::Pvp,
        other => {
            match other.strip_prefix("King: ") {
                Some(name) => BattleCategory::BrigandKing(name.to_string()),
                None => BattleCategory::Brigand,
            }
        }
    }
}

impl SavedTeam {
    fn to_team(&self) -> TeamSide {
        TeamSide {
            players: self.players.clone(),
            swabbies: self.swabbies,
            // Restored from disk; `0` for legacy files (pre-field) or the enemy
            // side, which falls back to counting only pirates as
            // divvy shares.
            mercenaries: self.mercenaries,
        }
    }
}

impl SavedSnapshot {
    /// `our_ship` is the voyage's hull index (derived by the caller from
    /// `SavedVoyage::ship_type`), since it isn't stored per-snapshot.
    fn to_snapshot(&self, our_ship: usize) -> BattleSnapshot {
        BattleSnapshot {
            our_ship,
            foe_ship: ship_index(&self.foe_ship).unwrap_or(0),
            our_hits: self.our_hits,
            foe_hits: self.foe_hits,
            rams: self.rams,
            our_pirates: self.our_pirates,
        }
    }
}

/// The Unix epoch as a naive timestamp — the synthetic origin for reconstructed
/// battle/voyage clocks (only the relative gaps are meaningful).
fn epoch() -> Option<chrono::NaiveDateTime> {
    chrono::DateTime::from_timestamp(0, 0).map(|d| d.naive_utc())
}

impl SavedBattle {
    /// Rebuild an in-RAM [`Battle`] for a read-only history page. Timestamps
    /// are synthesized so `sea_secs`/`boarding_secs`/`total_secs` reproduce
    /// the stored durations; `advantage_*` stay `None` (derived from the
    /// snapshot in the UI). `our_ship` is the voyage's hull index (all
    /// fights share it) used to rebuild the snapshot's own-ship, which
    /// isn't persisted per-fight.
    fn to_battle(&self, our_ship: usize) -> Battle {
        let base = epoch();
        let after = |secs: Option<i64>| {
            secs.zip(base)
                .map(|(s, b)| b + chrono::Duration::seconds(s))
        };
        Battle {
            enemy: self.enemy.clone(),
            started_at: base,
            grappled_at: after(self.naval_secs),
            ended_at: after(self.total_secs),
            outcome: outcome_from_str(&self.outcome),
            poe: self.poe,
            goods: self.goods,
            my_cut: None,
            pirates: self.pirates,
            swabbies: self.swabbies,
            category: category_from_str(&self.category),
            advantage_dmg: None,
            advantage_crew: None,
            snapshot: self.snapshot.as_ref().map(|s| s.to_snapshot(our_ship)),
            // A persisted snapshot *is* the record of a recorded fight.
            recorded: self.snapshot.is_some(),
            melee_kos: Vec::new(),
            timeline: self.to_timeline(),
            our_team: self.our_team.as_ref().map(SavedTeam::to_team),
            their_team: self.their_team.as_ref().map(SavedTeam::to_team),
            foe_ship: self.foe_ship.as_deref().and_then(ship_index),
        }
    }
}

impl SavedConsumption {
    /// Rebuild [`ConsumptionStats`] from the frozen counts plus the voyage's
    /// battles — the live inventory delta is long gone, so we reuse the stored
    /// figures rather than recompute. Mirrors the rate math in
    /// [`crate::voyage::stats::consumption_stats`].
    pub fn to_stats(&self, voyage: &Voyage) -> ConsumptionStats {
        let battles = voyage.battles.len() as u32;
        ConsumptionStats {
            balls: self.cannonballs,
            balls_per_battle: (0 < battles)
                .then(|| self.cannonballs as f64 / battles as f64),
            rum: RumUse {
                swill: self.swill,
                grog: self.grog,
                fine_rum: self.fine_rum,
            },
            rum_spice: self.rum_spice,
            delta_unreliable: crate::voyage::stats::delta_unreliable(voyage),
        }
    }
}

impl SavedVoyage {
    /// Reconstruct an in-RAM [`Voyage`] from its persisted form for a read-only
    /// history page. `saved` is set (never re-offered) and `avg_override`
    /// carries the persisted crew averages, since the raw samples weren't
    /// stored.
    pub fn to_voyage(&self) -> Voyage {
        let base = epoch();
        let ported = self
            .duration_secs
            .zip(base)
            .map(|(s, b)| b + chrono::Duration::seconds(s));
        // Our hull is voyage-wide; each fight's snapshot derives its own-ship
        // from it (unknown/legacy hull falls back to the first ship,
        // matching the old default).
        let our_ship =
            self.ship_type.as_deref().and_then(ship_index).unwrap_or(0);
        Voyage {
            id: 0,
            saved_to: None,
            sailed_at: base,
            ported_at: ported,
            current_battle: None,
            battles: self
                .battles
                .iter()
                .map(|b| b.to_battle(our_ship))
                .collect(),
            crew_samples: Vec::new(),
            merc_checkpoint: 0,
            poisoned: self.poisoned,
            divvied: self.divvied,
            booty_chest: self.booty_chest,
            booty_goods: self.booty_goods.as_ref().map(|goods| {
                goods
                    .iter()
                    .map(|g| (g.commodity.clone(), g.quantity))
                    .collect()
            }),
            saved: true,
            avg_override: Some((
                self.avg_pirates,
                self.avg_swabbies,
                self.avg_mercenaries,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voyage::AxisMode;

    #[test]
    fn timeline_round_trips_through_saved_battle() {
        let sb = SavedBattle {
            timeline: vec![
                SavedKo {
                    secs: Some(0),
                    side: "them".into(),
                },
                SavedKo {
                    secs: Some(12),
                    side: "us".into(),
                },
                SavedKo {
                    secs: Some(20),
                    side: "them".into(),
                },
            ],
            our_start: 6,
            their_start: Some(5),
            ..SavedBattle::default()
        };
        let tl = sb.to_timeline();
        assert_eq!(tl.our_start, 6);
        assert_eq!(tl.their_start, Some(5));
        let sides: Vec<KoSide> = tl.events.iter().map(|e| e.side).collect();
        assert_eq!(
            sides,
            vec![KoSide::Theirs, KoSide::Ours, KoSide::Theirs]
        );
        // The wall-clock axis reconstructs from the stored second-offsets (the
        // leading point is the fight start at 0).
        let xs: Vec<f64> = tl
            .advantage_series(AxisMode::Time)
            .iter()
            .map(|&(x, _)| x)
            .collect();
        assert_eq!(xs, vec![0.0, 0.0, 12.0, 20.0]);
    }

    #[test]
    fn enemy_vessel_name_round_trips() {
        // The enemy vessel's proper name survives save -> load, so a reloaded
        // fight shows the named foe rather than "Unknown vessel". A
        // legacy file without the field defaults to `None`.
        let sb = SavedBattle {
            enemy: Some("Some Enemy Vessel".into()),
            ..SavedBattle::default()
        };
        let json = serde_json::to_string(&sb).unwrap();
        let back: SavedBattle = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.enemy.as_deref(),
            Some("Some Enemy Vessel")
        );
        // The reconstructed in-RAM battle carries the name through to the UI
        // row.
        assert_eq!(
            back.to_battle(0).enemy.as_deref(),
            Some("Some Enemy Vessel")
        );

        let legacy: SavedBattle = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.enemy, None);
    }

    #[test]
    fn team_split_round_trips_and_legacy_defaults() {
        // A recorded roster carries genuine swabbies and mercenaries as
        // *disjoint* counts; the split survives save -> load.
        let team = TeamSide {
            players: vec!["Playerone".into()],
            swabbies: 4,    // genuine swabbies
            mercenaries: 2, // distinct crew kind
        };
        let saved = saved_team(&team);
        assert_eq!(saved.swabbies, 4);
        assert_eq!(saved.mercenaries, 2);
        let back = saved.to_team();
        assert_eq!(back.swabbies, 4);
        assert_eq!(back.mercenaries, 2);
        assert_eq!(back.shares(), 1); // players only (mercs and swabbies earn none)
        assert_eq!(back.headcount(), 1 + 4 + 2); // players + swabbies + mercenaries

        // A legacy file predates the merc field: `mercenaries` defaults to 0,
        // so those bodies stay folded in `swabbies` — headcount is
        // intact. Shares are pirates-only regardless.
        let legacy: SavedTeam =
            serde_json::from_str(r#"{"players":["Playerone"],"swabbies":6}"#)
                .unwrap();
        assert_eq!(legacy.mercenaries, 0);
        let lt = legacy.to_team();
        assert_eq!(lt.shares(), 1); // pirates only
        assert_eq!(lt.headcount(), 1 + 6);
    }

    #[test]
    fn poison_flag_round_trips_and_flags_the_delta() {
        // A run poisoned live (e.g. by the rum-spice hiring-limit tell)
        // persists the flag, and a reloaded poisoned run reports its stock
        // delta as unreliable even without a lost battle.
        let sv = SavedVoyage {
            duration_secs: Some(3600),
            avg_pirates: Some(5.0),
            avg_swabbies: Some(3.0),
            avg_mercenaries: Some(2.0),
            poisoned: true,
            consumption: Some(SavedConsumption {
                rum_spice: 40,
                ..SavedConsumption::default()
            }),
            battles: vec![SavedBattle {
                outcome: "won".into(),
                ..SavedBattle::default()
            }],
            ..SavedVoyage::default()
        };
        let voy = sv.to_voyage();
        assert!(
            voy.poisoned,
            "the poison flag survives save -> load"
        );
        let cs = sv.consumption.as_ref().unwrap().to_stats(&voy);
        assert!(
            cs.delta_unreliable,
            "a poisoned run's stock delta is untrustworthy despite no loss",
        );
    }

    #[test]
    fn booty_snapshot_round_trips() {
        // The booty chest PoE and the per-commodity goods (Booty column only)
        // are written by `from_voyage` and survive a JSON round-trip.
        // An older file with neither field defaults to `None` chest and
        // `None` goods — nothing recorded, which an empty list would not say.
        // Booty is frozen onto the voyage (at its divvy) before it's persisted.
        let v = Voyage {
            sailed_at: epoch(),
            ported_at: epoch().map(|b| b + chrono::Duration::seconds(600)),
            divvied: true,
            booty_chest: Some(4200),
            booty_goods: Some(vec![
                ("Iron".into(), 30),
                ("Hemp".into(), 12),
            ]),
            ..Default::default()
        };
        let saved = from_voyage(
            &v,
            Some("Test Vessel"),
            None,
            None,
            SaveParts::default(),
            true,
        );
        assert!(saved.divvied);
        assert_eq!(saved.booty_chest, Some(4200));
        let goods = saved.booty_goods.as_ref().expect("goods recorded");
        assert_eq!(goods.len(), 2);
        assert_eq!(goods[0].commodity, "Iron");
        assert_eq!(goods[0].quantity, 30);

        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedVoyage = serde_json::from_str(&json).unwrap();
        assert!(back.divvied);
        assert_eq!(back.booty_chest, Some(4200));
        let goods = back.booty_goods.as_ref().expect("goods survive the file");
        assert_eq!(goods.len(), 2);
        assert_eq!(goods[1].commodity, "Hemp");
        assert_eq!(goods[1].quantity, 12);
        // Reconstruction restores the divvy flag and booty onto the in-RAM
        // voyage (the Divvy section reads them straight off the
        // voyage).
        let rv = back.to_voyage();
        assert!(rv.divvied);
        assert_eq!(rv.booty_chest, Some(4200));
        assert_eq!(
            rv.booty_goods,
            Some(vec![
                ("Iron".into(), 30),
                ("Hemp".into(), 12)
            ])
        );

        // A recorded but bare Booty column stays distinct from one never read.
        let bare = SavedVoyage {
            booty_goods: Some(Vec::new()),
            ..SavedVoyage::default()
        };
        let json = serde_json::to_string(&bare).unwrap();
        let back: SavedVoyage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back.booty_goods.as_deref(),
            Some([])
        ));
        assert_eq!(
            back.to_voyage().booty_goods,
            Some(Vec::new())
        );

        // Legacy file: fields absent -> not divvied, chest None, goods never
        // recorded.
        let legacy: SavedVoyage = serde_json::from_str("{}").unwrap();
        assert!(!legacy.divvied);
        assert_eq!(legacy.booty_chest, None);
        assert!(legacy.booty_goods.is_none());
    }

    /// A finished run carrying every part the save prompt can decline: one
    /// recorded fight with a Damage snapshot and a melee timeline, a divvied
    /// booty, and a consumption delta.
    fn full_voyage() -> (Voyage, ConsumptionStats) {
        let v = Voyage {
            sailed_at: epoch(),
            ported_at: epoch().map(|b| b + chrono::Duration::seconds(600)),
            divvied: true,
            booty_chest: Some(4200),
            booty_goods: Some(vec![("Iron".into(), 30)]),
            battles: vec![Battle {
                outcome: BattleOutcome::Won,
                category: BattleCategory::Brigand,
                poe: Some(8000),
                started_at: epoch(),
                ended_at: epoch().map(|b| b + chrono::Duration::seconds(300)),
                recorded: true,
                snapshot: Some(BattleSnapshot {
                    our_ship: 0,
                    foe_ship: 0,
                    our_hits: [3, 1],
                    foe_hits: [5, 2],
                    rams: 1,
                    our_pirates: 6,
                }),
                timeline: FightTimeline {
                    events: vec![KoEvent {
                        at: epoch().map(|b| b + chrono::Duration::seconds(30)),
                        side: KoSide::Theirs,
                    }],
                    our_start: 6,
                    their_start: Some(5),
                    started_at: epoch(),
                    ended_at: None,
                },
                ..Battle::default()
            }],
            ..Default::default()
        };
        let consumption = ConsumptionStats {
            balls: 150,
            balls_per_battle: Some(150.0),
            rum: RumUse {
                swill: 0,
                grog: 60,
                fine_rum: 15,
            },
            rum_spice: 18,
            delta_unreliable: false,
        };
        (v, consumption)
    }

    #[test]
    fn every_part_reaches_the_file_by_default() {
        let (v, cs) = full_voyage();
        let saved = from_voyage(
            &v,
            Some("Test Vessel"),
            Some("Sloop"),
            Some(&cs),
            SaveParts::default(),
            true,
        );
        assert_eq!(saved.battles.len(), 1);
        assert!(saved.battles[0].snapshot.is_some());
        assert_eq!(saved.battles[0].timeline.len(), 1);
        assert_eq!(saved.battles[0].our_start, 6);
        assert_eq!(saved.battles[0].their_start, Some(5));
        assert!(saved.consumption.is_some());
        assert_eq!(saved.booty_chest, Some(4200));
        assert!(saved.booty_goods.is_some());
    }

    #[test]
    fn a_declined_part_is_written_absent_rather_than_empty() {
        // Each part the prompt can refuse leaves no trace of itself, so a
        // reloaded run can't mistake "we were told not to record it" for "the
        // run had none of it". The aggregates and the run's identity are not
        // the prompt's to refuse and stay either way.
        let (v, cs) = full_voyage();
        let saved = from_voyage(
            &v,
            Some("Test Vessel"),
            Some("Sloop"),
            Some(&cs),
            SaveParts {
                engagements: false,
                damage: true,
                melee: true,
                consumption: false,
                booty: false,
            },
            true,
        );
        assert!(saved.battles.is_empty());
        assert!(saved.consumption.is_none());
        assert_eq!(saved.booty_chest, None);
        assert!(saved.booty_goods.is_none());
        // The run itself is still on record.
        assert_eq!(
            saved.vessel.as_deref(),
            Some("Test Vessel")
        );
        assert_eq!(saved.duration_secs, Some(600));
        assert!(saved.divvied);
    }

    #[test]
    fn declining_a_fights_parts_keeps_the_fight() {
        // Damage and melee are parts of a fight's record, not records of their
        // own: refusing them leaves the fight — its outcome, PoE and timings —
        // and takes only what they held.
        let (v, cs) = full_voyage();
        let saved = from_voyage(
            &v,
            Some("Test Vessel"),
            Some("Sloop"),
            Some(&cs),
            SaveParts {
                engagements: true,
                damage: false,
                melee: false,
                consumption: true,
                booty: true,
            },
            true,
        );
        let b = &saved.battles[0];
        assert_eq!(b.outcome, "won");
        assert_eq!(b.poe, Some(8000));
        assert_eq!(b.total_secs, Some(300));
        assert!(b.snapshot.is_none());
        assert!(b.timeline.is_empty());
        assert_eq!(b.our_start, 0);
        assert_eq!(b.their_start, None);
        // A fight without its snapshot reloads as an unrecorded one, which is
        // what it now is.
        assert!(!b.to_battle(0).recorded);
    }

    #[test]
    fn the_nested_parts_fall_with_the_engagements() {
        // Dropping the fights drops what rode inside them, whatever the two
        // nested boxes were last left saying.
        let (v, cs) = full_voyage();
        let parts = SaveParts {
            engagements: false,
            ..SaveParts::default()
        };
        assert!(!parts.writes_damage());
        assert!(!parts.writes_melee());
        let saved = from_voyage(
            &v,
            Some("Test Vessel"),
            Some("Sloop"),
            Some(&cs),
            parts,
            true,
        );
        assert!(saved.battles.is_empty());
    }

    #[test]
    fn snapshot_presence_marks_recorded() {
        // `recorded` is no longer persisted — a stored snapshot *is* the
        // record, so a battle with a snapshot reconstructs as recorded
        // and one without does not.
        let with = SavedBattle {
            snapshot: Some(SavedSnapshot::default()),
            ..SavedBattle::default()
        };
        assert!(with.to_battle(0).recorded);
        let without = SavedBattle::default();
        assert!(!without.to_battle(0).recorded);
    }

    #[test]
    fn saved_voyage_reconstructs_read_only_stats() {
        let sv = SavedVoyage {
            vessel: Some("Test Vessel".into()),
            ship_type: Some("Sloop".into()),
            duration_secs: Some(3600),
            avg_pirates: Some(5.0),
            avg_swabbies: Some(3.0),
            consumption: Some(SavedConsumption {
                cannonballs: 150,
                swill: 0,
                grog: 60,
                fine_rum: 15,
                rum_spice: 18,
            }),
            battles: vec![
                SavedBattle {
                    outcome: "won".into(),
                    category: "Brigands".into(),
                    poe: Some(8000),
                    goods: Some(10),
                    naval_secs: Some(120),
                    total_secs: Some(300),
                    our_start: 5,
                    their_start: Some(4),
                    ..SavedBattle::default()
                },
                SavedBattle {
                    outcome: "lost".into(),
                    category: "Brigands".into(),
                    poe: Some(-2000),
                    goods: Some(50),
                    total_secs: Some(240),
                    ..SavedBattle::default()
                },
            ],
            ..SavedVoyage::default()
        };

        let voy = sv.to_voyage();
        assert!(
            voy.saved,
            "reconstructed runs are never re-offered for saving"
        );
        assert_eq!(voy.battles.len(), 2);
        // Per-fight durations reconstruct from the stored second-offsets.
        assert_eq!(voy.battles[0].total_secs(), Some(300));
        assert_eq!(voy.battles[0].sea_secs(), Some(120));
        assert_eq!(
            voy.battles[0].boarding_secs(),
            Some(180)
        ); // 300 − 120
        // Crew averages ride in via the override — the raw samples weren't
        // persisted.
        assert_eq!(voy.avg_pirates(), Some(5.0));
        assert_eq!(voy.avg_swabbies(), Some(3.0));

        // With `confirmed = true` (a saved verdict is final) the win/loss pass
        // through.
        let bs = crate::voyage::stats::battle_stats(&voy, true);
        assert_eq!((bs.wins, bs.losses), (1, 1));
        assert_eq!(bs.poe_won_total, 8000);
        assert_eq!(bs.poe_net_total, 6000);

        // Consumption rebuilds from the frozen counts (not the live inventory).
        let cs = sv.consumption.as_ref().unwrap().to_stats(&voy);
        assert_eq!(cs.balls, 150);
        assert!((cs.balls_per_battle.unwrap() - 75.0).abs() < 1e-9);
        assert_eq!(cs.rum.weighted(), 60 * 3 + 15 * 6);
        assert_eq!(cs.rum_spice, 18);
    }

    #[test]
    fn snapshot_our_ship_derives_from_voyage_hull() {
        // `our_ship` isn't stored per-snapshot; it's rebuilt from the voyage's
        // hull.
        let junk = ship_index("Junk").unwrap();
        let sv = SavedVoyage {
            ship_type: Some("Junk".into()),
            battles: vec![SavedBattle {
                outcome: "won".into(),
                snapshot: Some(SavedSnapshot {
                    foe_ship: "Sloop".into(),
                    ..SavedSnapshot::default()
                }),
                ..SavedBattle::default()
            }],
            ..SavedVoyage::default()
        };
        let voy = sv.to_voyage();
        let snap = voy.battles[0].snapshot.expect("snapshot present");
        assert_eq!(
            snap.our_ship, junk,
            "own hull comes from the voyage ship_type"
        );
        assert_eq!(
            snap.foe_ship,
            ship_index("Sloop").unwrap()
        );

        // An unknown/absent hull falls back to the first ship (index 0), as
        // before.
        let sv0 = SavedVoyage {
            ship_type: None,
            battles: vec![SavedBattle {
                snapshot: Some(SavedSnapshot::default()),
                ..SavedBattle::default()
            }],
            ..SavedVoyage::default()
        };
        assert_eq!(
            sv0.to_voyage().battles[0].snapshot.unwrap().our_ship,
            0
        );
    }
}
