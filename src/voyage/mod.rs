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
    /// The fight reached a `Game over`, but we can't tell win from loss because
    /// our own identity is unconfirmed — no `--user` name, or a name that never
    /// actually appeared in the log (so a "loss" might be an undetected win). The
    /// PoE sign is therefore unknowable; [`Battle::poe`] is left `None`.
    Unknown,
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
    /// The Black Ship (El Pollo Diablo) — a rare special encounter that takes the
    /// place of our target. Always a Grand Frigate.
    BlackShip,
    /// A monkey boat — a special encounter whose vessel name identifies its hull
    /// (see [`crate::chatlog`]'s monkey-boat table). The hull is on [`Battle::foe_ship`].
    MonkeyBoat,
    /// Player-vs-player: the foe fielded at least one real player. Its own
    /// category, mutually exclusive with the rest — once a fight is PvP it stays
    /// PvP regardless of any king/monster telltale.
    Pvp,
}

/// One side of a boarding melee — the players on it (by name) and a bare swabbie
/// (NPC) count. Swabbie *identities* are intentionally dropped: we only persist
/// who the real players were and how many swabbies fought beside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamSide {
    /// Real-player names on this side.
    pub players: Vec<String>,
    /// Genuine **swabbies** on this side — the NPC crew that take only a pre-divvy
    /// skim, *excluding* mercenaries (a distinct crew kind, counted in
    /// [`Self::mercenaries`]). The two are disjoint: total NPC crew = `swabbies +
    /// mercenaries`. (The log's raw count lines lump the two; the split is resolved
    /// from a winners-roster ground truth — see the mercenary roster on `Vessel`.)
    pub swabbies: u32,
    /// **Mercenaries** on this side — the NPCs with the `[name] [epithet]` convention,
    /// distinct from and disjoint with [`Self::swabbies`]. Our side only, and only as
    /// accurate as the last winners-roster ground truth; `0` on the enemy side (never
    /// classified). Mercs take a full divvy share, swabbies none — so this is the merc
    /// half of a fight's [`Self::shares`].
    pub mercenaries: u32,
}

impl TeamSide {
    /// Total headcount on this side (players + all NPC crew, swabbies + mercenaries).
    pub fn headcount(&self) -> u32 {
        self.players.len() as u32 + self.swabbies + self.mercenaries
    }

    /// Divvy shares on this side: every real pirate and every mercenary earns one
    /// full share; free swabbies earn none (they're paid off the top). Drives the
    /// "Value per Share" metric.
    pub fn shares(&self) -> u32 {
        self.players.len() as u32 + self.mercenaries
    }
}

/// A snapshot of the Damage Calculator's state, captured the instant the boarding
/// melee begins (the grapple). By then the naval phase is over, so the
/// accumulated ship damage is final for the fight. Drives the Sea Battles
/// per-battle widget and the advantage metrics. Ship indices are into
/// [`crate::ships::SHIPS`] (Left = our vessel, Right = the foe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BattleSnapshot {
    /// Our ship type (the Damage calculator's "Left").
    pub our_ship: usize,
    /// The foe's ship type (the Damage calculator's "Right").
    pub foe_ship: usize,
    /// Hits *we* took: `[shots, rocks, rams]`.
    pub our_hits: [u32; 3],
    /// Hits the *foe* took: `[shots, rocks, rams]`.
    pub foe_hits: [u32; 3],
    /// Head-on collisions.
    pub headon: u32,
    /// Our full crew aboard at capture — real pirates + swabbies/named mercenaries
    /// (the manpower used for the crew advantage). Named `our_pirates` for history.
    pub our_pirates: u32,
}

/// One sea engagement, from interception to its resolution. Both the naval phase
/// (interception -> grapple) and the boarding melee (grapple -> `Game over`) are
/// timed; either may be absent (e.g. a disengage before grappling).
#[derive(Clone, Debug, Default)]
pub struct Battle {
    /// Enemy vessel name from the interception line (`None` if unparsed).
    pub enemy: Option<String>,
    /// Interception time — the engagement start.
    pub started_at: Option<NaiveDateTime>,
    /// Grapple time — the sea phase ends and the boarding melee begins. `None` if
    /// the fight never reached a boarding.
    pub grappled_at: Option<NaiveDateTime>,
    /// Resolution time (`Game over`, disengage, or enemy ported).
    pub ended_at: Option<NaiveDateTime>,
    /// Outcome from our perspective.
    pub outcome: BattleOutcome,
    /// Gross PoE the victors plundered, signed by the (provisional) [`Self::outcome`]:
    /// positive when we won, negative when we lost (the PoE was taken from us).
    /// `None` only when there's no configured identity at all (direction
    /// unknowable). When the outcome is provisional-but-unconfirmed the sign is
    /// still stored, but the view layer presents it as absent until identity is
    /// confirmed (mirrors [`effective_outcome`]).
    pub poe: Option<i64>,
    /// Units of goods in the plunder (a bare count — the log never itemizes).
    pub goods: Option<u32>,
    /// Our personal share from `Ye received N ... initial cut of the booty!`.
    pub my_cut: Option<u64>,
    /// Pirates aboard our vessel at resolution (real players incl. us).
    pub pirates: u32,
    /// Total NPC crew aboard at resolution — swabbies **and** mercenaries, the raw
    /// combined count that drives manpower/strength (both kinds fight). The genuine
    /// swabbie-vs-mercenary split is kept separately in [`Self::our_team`]. Named
    /// `swabbies` for history.
    pub swabbies: u32,
    /// What we were fighting (best-effort; defaults to generic Brigand).
    pub category: BattleCategory,
    /// Damage advantage (ours − theirs) snapshotted from the Damage calculator at
    /// the grapple (melee start): `[-0.5, +0.5]`. `None` if no damage was tracked.
    pub advantage_dmg: Option<f64>,
    /// Headcount advantage snapshotted at the grapple (our pirates × our advantage
    /// − enemy complement × their advantage). `None` if no damage was tracked.
    pub advantage_crew: Option<f64>,
    /// The Damage-calculator state for this fight (auto-frozen from the live
    /// calculator at resolution, and editable afterward in the Sea Battles popup).
    /// `None` until anything is captured/entered. Always shown/editable — its
    /// presence does NOT mean "recorded".
    pub snapshot: Option<BattleSnapshot>,
    /// Whether this fight is **recorded** — i.e. written to the voyage history on
    /// disk. Independent of [`Self::snapshot`]: the calculator/strength/advantage
    /// always display; this flag only governs persistence. Default `false`.
    pub recorded: bool,
    /// Names knocked out during this fight's melee (`<Name> is eliminated!`, both
    /// sides), accumulated while the fight is open and cleared once resolved. The
    /// basis for [`Self::their_team`].
    pub melee_kos: Vec<String>,
    /// Ordered, side-tagged elimination timeline driving the per-fight
    /// advantage-over-time graph. Built in lockstep with [`Self::melee_kos`]
    /// (sides backfilled at resolution from the winners roster). See
    /// [`FightTimeline`].
    pub timeline: FightTimeline,
    /// Our side of the boarding melee — players (by name) + swabbie count.
    /// Captured at the grapple (so a crewmate who leaves mid-melee still counts)
    /// and finalized at resolution (unioned with the winners-resynced roster, the
    /// disconnected subtracted). `None` until grappled.
    pub our_team: Option<TeamSide>,
    /// The foe's side — players (by name) + swabbie count, computed at resolution
    /// from the melee: on a win, the eliminations that aren't our crew (all
    /// enemies are eliminated); on a loss, the winners' (enemy) roster. `None`
    /// when no melee resolved it (a disengage, or an unknown-identity fight where
    /// we can't tell which side the winners are) — the UI then falls back to the
    /// ship-type estimate.
    pub their_team: Option<TeamSide>,
    /// The foe's *known* hull type, as a [`crate::ships::SHIPS`] index, when we can
    /// determine it from the encounter itself (special encounters like the Black
    /// Ship and Monkey Boats announce their hull). Seeds the Damage calculator's
    /// foe ship; `None` when the hull is unknown and left to the user. Distinct
    /// from a [`BattleSnapshot::foe_ship`], which is whatever the user last set.
    pub foe_ship: Option<usize>,
}

impl Battle {
    /// The foe's headcount from the melee, or `None` when no melee resolved it
    /// (the UI then falls back to the foe ship type's pirate capacity). Derived
    /// from [`Self::their_team`].
    pub fn their_manpower(&self) -> Option<u32> {
        self.their_team.as_ref().map(TeamSide::headcount)
    }

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

/// Which side of a fight an eliminated combatant belonged to, from our point of
/// view. Drives the sign of each step in the advantage curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KoSide {
    /// One of ours fell (our headcount drops → advantage steps down).
    Ours,
    /// One of theirs fell (their headcount drops → advantage steps up).
    Theirs,
}

/// A single elimination during a fight: when it happened and whose it was. The
/// KO'd combatant's *name* is intentionally omitted — the UI shows only the
/// derived advantage curve, never an eliminations list.
#[derive(Clone, Copy, Debug)]
pub struct KoEvent {
    /// Log timestamp of the `... is eliminated!` line (`None` if the clock was
    /// unknown at the time — the curve then falls back to event-index spacing).
    pub at: Option<NaiveDateTime>,
    /// Which side the casualty was on (backfilled at resolution for sea battles).
    pub side: KoSide,
}

/// The X-axis of the per-fight advantage graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisMode {
    /// Wall-clock seconds from the fight start (realistic pacing). Default.
    #[default]
    Time,
    /// Evenly-spaced one tick per elimination (cleaner curve, no clustering).
    Event,
}

/// The ordered, side-tagged eliminations of one fight — the data behind the
/// per-fight advantage-over-time graph. Replaying [`Self::events`] from the
/// starting headcounts yields the signed advantage curve (see
/// [`Self::advantage_series`]). Attached to a sea [`Battle`] and to each
/// Cursed Isles / Vampirate wave record.
#[derive(Clone, Debug, Default)]
pub struct FightTimeline {
    /// Eliminations in the order they occurred.
    pub events: Vec<KoEvent>,
    /// Our headcount at the start of the fight.
    pub our_start: u32,
    /// Their headcount at the start, once known (`None` while a fight is still
    /// live and the foe roster is undetermined — the curve then plots the
    /// net-KO differential, which has the same shape, anchored at zero).
    pub their_start: Option<u32>,
    /// Fight start (the time axis origin).
    pub started_at: Option<NaiveDateTime>,
    /// Fight end.
    pub ended_at: Option<NaiveDateTime>,
}

impl FightTimeline {
    /// The signed advantage (`our_alive − their_alive`) sampled at the fight
    /// start and after each elimination, as `(x, advantage)` points. With
    /// [`Self::their_start`] known the curve is the absolute headcount gap; while
    /// it is `None` the baseline is zero and the curve is the net-KO differential
    /// (`theirsKO − oursKO`) — identical shape, only vertically offset.
    ///
    /// `x` is seconds-from-start under [`AxisMode::Time`] (falling back to the
    /// event index when a timestamp is missing) or the 1-based event index under
    /// [`AxisMode::Event`].
    pub fn advantage_series(&self, axis: AxisMode) -> Vec<(f64, i32)> {
        let base = match self.their_start {
            Some(theirs) => self.our_start as i32 - theirs as i32,
            None => 0,
        };
        let mut adv = base;
        let mut out = Vec::with_capacity(self.events.len() + 1);
        out.push((0.0, adv));
        for (i, ev) in self.events.iter().enumerate() {
            adv += match ev.side {
                KoSide::Ours => -1,
                KoSide::Theirs => 1,
            };
            let x = match axis {
                AxisMode::Time => self
                    .started_at
                    .zip(ev.at)
                    .map(|(s, a)| (a - s).num_seconds() as f64)
                    .unwrap_or((i + 1) as f64),
                AxisMode::Event => (i + 1) as f64,
            };
            out.push((x, adv));
        }
        out
    }
}

/// A snapshot of the headcount aboard at one instant — the basis for the
/// time-weighted average crew (the "integral" of crew over the run).
#[derive(Clone, Copy, Debug)]
pub struct CrewSample {
    pub at: NaiveDateTime,
    /// Real pirates aboard, including us.
    pub pirates: u32,
    /// Total NPC crew aboard (swabbies + mercenaries) — the manpower count.
    pub swabbies: u32,
    /// Mercenaries aboard. Best-effort while sampled live (mercs board invisibly);
    /// retroactively corrected to each winners-roster ground truth (see
    /// [`Voyage::merc_checkpoint`]). Drives the time-weighted average mercenaries the
    /// rum-spice-per-mercenary stat divides by.
    pub mercenaries: u32,
}

/// One complete sail->port run aboard a vessel — the unit of voyage statistics.
/// The job kind may change mid-run (`This vessel is now ...`) without resetting
/// stats; we keep the declaration in force when we set sail as the headline.
#[derive(Clone, Debug, Default)]
pub struct Voyage {
    /// Session-stable identifier, assigned from a [`crate::chatlog::GameState`]
    /// counter when the voyage is first created. Lets the Voyage Statistics pager
    /// pin a selection across promotion (`current_voyage` -> `voyages`) and new
    /// runs starting. Runtime-only; not persisted (`0` for a default/test voyage).
    pub id: u64,
    /// If this voyage was persisted to history this run, the index it occupies in
    /// [`crate::voyage::persistence::SavedVoyages::voyages`]. The pager keeps
    /// showing this live (read-write) page and hides its on-disk read-only twin,
    /// so a just-saved run isn't listed twice. `None` until saved. Runtime-only.
    pub saved_to: Option<usize>,
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
    /// Index into [`Self::crew_samples`] marking the start of the current
    /// not-yet-ground-truthed stretch. On each winners-roster ground truth (a won
    /// fight) every sample from here to the end is backfilled to the confirmed
    /// mercenary count and this advances to the end — so each inter-win stretch gets
    /// the count confirmed at its close. Runtime-only.
    pub merc_checkpoint: usize,
    /// This voyage's data has gaps and shouldn't be fully trusted: set when we
    /// leave the vessel mid-run (before booty is divided) or when the hold runs
    /// too low on rum spice (the mercenary-hiring-limit tell). Gates
    /// `rum_spice_unreliable`; persisted with the voyage.
    pub poisoned: bool,
    /// Runtime-only: the user has saved or dismissed this run via the
    /// save/discard prompt, so it shouldn't be offered again. Not persisted.
    pub saved: bool,
    /// Precomputed time-weighted average crew for a voyage **reconstructed from
    /// disk**, where the raw [`Self::crew_samples`] no longer exist (only the
    /// averages were persisted). `(pirates, swabbies, mercenaries)`, each optional.
    /// `None` for a live voyage, which derives its averages from `crew_samples`. See
    /// [`crate::voyage::persistence::SavedVoyage::to_voyage`].
    pub avg_override: Option<(Option<f64>, Option<f64>, Option<f64>)>,
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

    /// Time-weighted average pirates aboard (incl. us) over the run. A voyage
    /// reconstructed from disk returns its persisted average (see
    /// [`Self::avg_override`]); a live voyage computes it from `crew_samples`.
    #[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
    pub fn avg_pirates(&self) -> Option<f64> {
        if let Some((p, _, _)) = self.avg_override {
            return p;
        }
        self.avg_crew(|s| s.pirates)
    }

    /// Time-weighted average total NPC crew aboard over the run. See
    /// [`Self::avg_pirates`].
    #[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
    pub fn avg_swabbies(&self) -> Option<f64> {
        if let Some((_, s, _)) = self.avg_override {
            return s;
        }
        self.avg_crew(|s| s.swabbies)
    }

    /// Time-weighted average mercenaries aboard over the run — the denominator for
    /// the rum-spice-per-mercenary stat. See [`Self::avg_pirates`].
    #[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
    pub fn avg_mercenaries(&self) -> Option<f64> {
        if let Some((_, _, m)) = self.avg_override {
            return m;
        }
        self.avg_crew(|s| s.mercenaries)
    }
}

/// The outcome to *show* for a battle, given whether our identity is confirmed.
///
/// A [`Battle::outcome`] of `Won`/`Lost` is only *provisional* — computed against
/// the configured pirate name. Until that name is confirmed present in the log
/// (`self_confirmed`), a win/loss can't be trusted (a "loss" might be an
/// undetected win under a wrong name), so it's masked to [`BattleOutcome::Unknown`].
/// `Ongoing`/`Disengaged`/`Unknown` don't depend on our identity and pass through.
/// Because the mask keys off the *current* confirmation flag, a signal that
/// confirms us late retroactively reveals every earlier fight.
pub fn effective_outcome(raw: BattleOutcome, self_confirmed: bool) -> BattleOutcome {
    match raw {
        BattleOutcome::Won | BattleOutcome::Lost if !self_confirmed => BattleOutcome::Unknown,
        other => other,
    }
}

/// Seconds between two optional timestamps, or `None` if either is missing.
fn secs_between(a: Option<NaiveDateTime>, b: Option<NaiveDateTime>) -> Option<i64> {
    Some((b? - a?).num_seconds())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 6, 1)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    #[test]
    fn advantage_series_steps_and_axes() {
        let tl = FightTimeline {
            events: vec![
                KoEvent { at: Some(dt(1, 0, 10)), side: KoSide::Theirs },
                KoEvent { at: Some(dt(1, 0, 20)), side: KoSide::Theirs },
                KoEvent { at: Some(dt(1, 0, 35)), side: KoSide::Ours },
            ],
            our_start: 5,
            their_start: Some(4),
            started_at: Some(dt(1, 0, 0)),
            ended_at: None,
        };
        // Event axis: evenly spaced; baseline = our_start − their_start = 1.
        assert_eq!(
            tl.advantage_series(AxisMode::Event),
            vec![(0.0, 1), (1.0, 2), (2.0, 3), (3.0, 2)]
        );
        // Time axis: x = seconds from start; identical advantage values (same shape).
        let ti = tl.advantage_series(AxisMode::Time);
        let xs: Vec<f64> = ti.iter().map(|&(x, _)| x).collect();
        let vs: Vec<i32> = ti.iter().map(|&(_, v)| v).collect();
        assert_eq!(xs, vec![0.0, 10.0, 20.0, 35.0]);
        assert_eq!(vs, vec![1, 2, 3, 2]);
    }

    #[test]
    fn advantage_series_unknown_their_start_uses_net_differential() {
        let tl = FightTimeline {
            events: vec![
                KoEvent { at: None, side: KoSide::Ours },
                KoEvent { at: None, side: KoSide::Theirs },
                KoEvent { at: None, side: KoSide::Theirs },
            ],
            our_start: 5,
            their_start: None, // unknown → baseline 0, net-KO differential
            started_at: None,
            ended_at: None,
        };
        let vs: Vec<i32> = tl
            .advantage_series(AxisMode::Event)
            .iter()
            .map(|&(_, v)| v)
            .collect();
        assert_eq!(vs, vec![0, -1, 0, 1]);
    }
}
