//! Streaming reader + state machine for the Puzzle Pirates client chat log.
//!
//! The log is a single ever-growing file. A `====== YYYY/MM/DD ======` header
//! line is written on every login/relog. Every other line is `[HH:MM:SS]
//! <body>`.
//!
//! [`GameState`] is the state machine: `process_line` classifies a line and
//! delegates to a specific handler. State is organised per-vessel in a map
//! keyed by ship name, so we can hop between vessels and come back. A vessel is
//! marked `poisoned` if we leave it mid-run (before the booty is divided),
//! since we then miss whatever happens while it keeps sailing.
//!
//! [`spawn_tailer`] does the streaming: it reads bytes appended after a given
//! offset and sends complete lines over a channel. Whole-file ingestion is done
//! synchronously via [`GameState::process_existing`] before the tailer starts,
//! so history is in place before the first frame.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::{
    pirate,
    voyage::{
        Battle,
        BattleCategory,
        BattleOutcome,
        BattleSnapshot,
        CrewSample,
        FightTimeline,
        KoEvent,
        KoSide,
        TeamSide,
        Voyage,
    },
};

// ---------------------------------------------------------------------------
// Job kinds
// ---------------------------------------------------------------------------

/// Pillaging difficulty rungs, lowest to highest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Difficulty {
    Easy = 0,
    Average = 1,
    Hard = 2,
    VeryHard = 3,
}

impl Difficulty {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Easy" => Difficulty::Easy,
            "Average" => Difficulty::Average,
            "Hard" => Difficulty::Hard,
            "Very Hard" => Difficulty::VeryHard,
            _ => return None,
        })
    }
}

impl fmt::Display for Difficulty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Difficulty::Easy => "Easy",
            Difficulty::Average => "Average",
            Difficulty::Hard => "Hard",
            Difficulty::VeryHard => "Very Hard",
        })
    }
}

/// Which foe types a pillage targets (a set, joined by " and " in the log).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PillageTargets {
    pub pirates: bool,
    pub brigands: bool,
    pub barbarians: bool,
}

impl fmt::Display for PillageTargets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.pirates {
            parts.push("Pirates");
        }
        if self.brigands {
            parts.push("Brigands");
        }
        if self.barbarians {
            parts.push("Barbarians");
        }
        f.write_str(&parts.join(" and "))
    }
}

/// What a vessel is currently doing (`This vessel is now <...>.`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobKind {
    Pillaging {
        lower: Difficulty,
        upper: Difficulty,
        targets: PillageTargets,
    },
    Evading,
    SwabbieTransport,
    Trading,
    Exploring {
        monster: String,
    },
    AttackingFlotilla,
    /// Anything not yet modelled — keeps the raw text.
    Other(String),
}

impl JobKind {
    /// Parse the text after `This vessel is now ` (trailing `.` already
    /// removed).
    pub fn parse(s: &str) -> JobKind {
        if let Some(rest) = s.strip_prefix("Pillaging, ") {
            if let Some(job) = parse_pillaging(rest) {
                return job;
            }
        } else if s == "Evading" {
            return JobKind::Evading;
        } else if s == "Swabbie Ship Transporting" {
            return JobKind::SwabbieTransport;
        } else if s.starts_with("Trading") {
            return JobKind::Trading;
        } else if let Some(monster) = s.strip_prefix("Exploring the ") {
            return JobKind::Exploring {
                monster: monster.to_string(),
            };
        } else if s == "Attacking a Flotilla" {
            return JobKind::AttackingFlotilla;
        }
        JobKind::Other(s.to_string())
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobKind::Pillaging {
                lower,
                upper,
                targets,
            } => {
                if lower == upper {
                    write!(f, "Pillaging, {lower} {targets}")
                } else {
                    write!(
                        f,
                        "Pillaging, {lower} to {upper} {targets}"
                    )
                }
            }
            JobKind::Evading => f.write_str("Evading"),
            JobKind::SwabbieTransport => {
                f.write_str("Swabbie Ship Transporting")
            }
            JobKind::Trading => f.write_str("Trading"),
            JobKind::Exploring {
                monster,
            } => write!(f, "Exploring the {monster}"),
            JobKind::AttackingFlotilla => f.write_str("Attacking a Flotilla"),
            JobKind::Other(s) => f.write_str(s),
        }
    }
}

/// Split a pillaging descriptor into difficulty range + target set.
/// e.g. `Average to Hard Brigands and Barbarians`.
fn parse_pillaging(rest: &str) -> Option<JobKind> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    // Targets always come last; find where the first foe word starts.
    let split = words.iter().position(|w| {
        matches!(
            *w,
            "Pirates" | "Brigands" | "Barbarians"
        )
    })?;
    let diff_text = words[.. split].join(" ");
    let target_text = words[split ..].join(" ");

    let (lower, upper) = match diff_text.split_once(" to ") {
        Some((lo, hi)) => {
            (
                Difficulty::parse(lo)?,
                Difficulty::parse(hi)?,
            )
        }
        None => {
            let d = Difficulty::parse(&diff_text)?;
            (d, d)
        }
    };

    let mut targets = PillageTargets::default();
    for part in target_text.split(" and ") {
        match part {
            "Pirates" => targets.pirates = true,
            "Brigands" => targets.brigands = true,
            "Barbarians" => targets.barbarians = true,
            _ => return None,
        }
    }

    Some(JobKind::Pillaging {
        lower,
        upper,
        targets,
    })
}

// ---------------------------------------------------------------------------
// Vampirate lair wave model
// ---------------------------------------------------------------------------

/// Vampirate lair wave growth. Wave 1 = pirates aboard; each subsequent wave is
/// ~+20% larger. The projected "next wave" count is bracketed as a [low, high]
/// range with these multipliers; the midpoint [`LAIR_WAVE_GROWTH`] advances the
/// anchor used to project further waves. (Derived from a real lair run — see
/// `/tmp/ypp_runs` analysis; observed waves track 1.2× closely.)
pub const LAIR_WAVE_GROWTH: f64 = 1.2;
pub const LAIR_WAVE_LO: f64 = 1.175;
pub const LAIR_WAVE_HI: f64 = 1.225;

// ---------------------------------------------------------------------------
// Cursed Isles model
// ---------------------------------------------------------------------------

/// Crew-anchored forecast for a Cursed Isles island wave's enemy count. Unlike
/// a vampire lair (exactly one vampire per pirate on wave 1), island waves only
/// *loosely* scale with crew, and the multiplier here is a rough estimate from
/// a single run — one where we often left fights early, so the observed counts
/// that would calibrate it under-report. TODO: recalibrate from more recorded
/// runs.
pub const ISLAND_ANCHOR_MULT: f64 = 1.5;
pub const ISLAND_WAVE_GROWTH: f64 = 1.1;
pub const ISLAND_WAVE_LO: f64 = 0.8;
pub const ISLAND_WAVE_HI: f64 = 1.2;

/// Crew-anchored projected `[low, high]` enemy count for a 1-based island
/// `wave`. See the `ISLAND_*` constants — a deliberately wide, rough band.
pub fn island_wave_band(pirates: u32, wave: u32) -> (u32, u32) {
    let base = pirates as f64
        * ISLAND_ANCHOR_MULT
        * ISLAND_WAVE_GROWTH.powi(wave.saturating_sub(1) as i32);
    (
        (base * ISLAND_WAVE_LO).round() as u32,
        (base * ISLAND_WAVE_HI).round() as u32,
    )
}

/// The kind of a 1-based island wave. Island waves always start at Rumble (wave
/// 1) and alternate Rumble / Swordfight thereafter — so the kind is known the
///    moment a wave begins, even before the first kill (and matches the
///    observed enemy families: rumble waves field zombies / Enlightened Ones /
///    Vargas, swordfight waves cultists / homunculi). `Unknown` only before
///    landing (wave 0).
pub fn wave_kind_for(wave: u32) -> WaveKind {
    match wave {
        0 => WaveKind::Unknown,
        w if w % 2 == 1 => WaveKind::Rumble,
        _ => WaveKind::Swordfight,
    }
}

/// Whether the boss Vargas the Mad is present in a 1-based island wave. He's
/// *guaranteed* from wave 5 on, but only on Rumble waves — so it's derived from
/// the wave number, never detected. His arrival herald is "Ye be tremored by
/// the presence of Vargas the Mad! Man at arms!".
///
/// TODO: track when Vargas is *eliminated* (his `Vargas the Mad is eliminated!`
/// line) — useful for per-fight stats / "did we beat the boss this run" —
/// separate from this presence check.
pub fn vargas_in_wave(wave: u32) -> bool {
    wave >= 5 && matches!(wave_kind_for(wave), WaveKind::Rumble)
}

/// Which special-encounter mechanic is active on a vessel, inferred from the
/// voyage's tell. Gates which boarding lines are acted on, so a stray keyword
/// on an ordinary pillage can't feed the wrong mechanic. Each mechanic is
/// exclusive to its voyage type: `Atlantis` is set by the dragoon boarding
/// tells and is what separates a dragoon driven off the ship from an ordinary
/// foe driven off in a pillage; zombies are Cursed-Isles-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EncounterKind {
    #[default]
    None,
    Atlantis,
    CursedIsles,
    // TODO(Haunted Seas): add a `HauntedSeas` variant and count phantasm
    // boarders here, parallel to zombies (Cursed Isles).
}

impl EncounterKind {
    /// Whether a fight under this encounter is fought by only part of the
    /// crew.
    ///
    /// Atlantis is one: a dragoon boards a ship that goes on being sailed
    /// around it, so whoever answers the fray is some of the crew and never
    /// reliably all of it. A winners list from such a fight confirms the
    /// names on it and says nothing whatever about the names off it.
    pub fn partial_fray(self) -> bool {
        matches!(self, Self::Atlantis)
    }
}

/// A Cursed Isles island wave is either a swordfight or a rumble, and the two
/// alternate. Classified from the enemy family seen — cultists/homunculi are
/// swordfight foes; zombies, Enlightened Ones and Vargas are rumble foes — so
/// it stays `Unknown` until the wave's first kill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WaveKind {
    #[default]
    Unknown,
    Swordfight,
    Rumble,
}

/// One completed wave of a Cursed Isles island assault or a Vampirate lair —
/// its number, kind, and the side-tagged elimination timeline that drives the
/// per-fight advantage graph. Live-only (not persisted), unlike sea
/// [`Battle`] timelines.
#[derive(Clone, Debug)]
pub struct WaveRecord {
    /// 1-based wave number.
    pub wave: u32,
    /// Rumble / swordfight (Cursed Isles); `Unknown` for lair waves.
    pub kind: WaveKind,
    /// The wave's eliminations, in order.
    pub timeline: FightTimeline,
}

// ---------------------------------------------------------------------------
// Vessel
// ---------------------------------------------------------------------------

/// State accumulated for a single vessel we've been aboard.
#[derive(Default)]
pub struct Vessel {
    /// What the vessel is doing now; `None` once the booty has been divided.
    pub job_kind: Option<JobKind>,
    /// Pirates currently aboard with us.
    pub crewmates: HashSet<String>,
    /// Crewmates whose client has dropped — between their `X has
    /// disconnected.` and `X has reconnected.` lines. A disconnected
    /// pirate still holds a melee slot (and stays on the winners roster)
    /// but doesn't actually fight, so they're excluded from a battle's
    /// crew strength / manpower advantage. Cleared on reconnect or when
    /// they leave the vessel.
    pub disconnected: HashSet<String>,
    /// Swabbies (NPC crew) aboard. There's no absolute count line, so this is
    /// a running tally from the four "swabbie(s) (has|have) come aboard /
    /// left the vessel" delta lines, snapped to the authoritative roster
    /// whenever we win a fight (see [`on_battle_end`]). Departures
    /// saturate at zero so a poisoned vessel (we missed lines while away)
    /// can't underflow.
    pub swabbies: u32,
    /// Mercenaries believed aboard right now, by name — the `[name] [epithet]`
    /// NPCs, a distinct crew kind from swabbies but a **subset** of the
    /// bodies the log lumps into [`Self::swabbies`] (so genuine-swabbie
    /// count = `swabbies - mercenaries.len()`). NPCs never announce by
    /// name, so this can only be *ground truthed* from a won fight's
    /// winners roster (classified via [`crate::cache::NameSegments`]);
    /// between wins it's maintained best-effort: swabbies leave before
    /// mercs, and the rum-spice depletion swap sheds one merc. A merc both
    /// hired and lost between two wins is invisible until the next roster
    /// re-truths it. Tracked for the roster and per-merc stats; mercenaries
    /// earn no divvy share.
    pub mercenaries: BTreeSet<String>,
    /// --- Vampirate lair tracking (Vampirates voyages) ---
    /// Whether we're currently inside a vampire lair (between `Welcome to the
    /// vampire sanctum.` / first `slaps mother` and the first lost `Game
    /// over`).
    pub lair_active: bool,
    /// Which wave we're in: 1 on lair entry, +1 per swordfight conclusion
    /// (`Game over` the crew won). Stays at the final wave after the lair
    /// ends (a lost swordfight); `0` if no lair has happened this run.
    pub lair_wave: u32,
    /// Real pirates aboard at lair entry — wave 1's vampire count (the anchor
    /// the per-wave projection grows from at ~+20%).
    pub lair_pirates: u32,
    /// Vampires defeated this lair so far (cumulative `<NPC> is eliminated!`).
    /// Note: undercounts while we're out of the fight — see
    /// [`Self::lair_warn`].
    pub vampires_defeated: u32,
    /// Vampires eliminated in the current wave only (reset each wave); checked
    /// against the wave's projected range to detect that we left the fight.
    pub wave_observed: u32,
    /// Projected `[low, high]` vampire count for the *current* wave (wave 1 =
    /// pirates exactly). Set on entry and on each wave advance.
    pub wave_lo: u32,
    pub wave_hi: u32,
    /// Latched once any wave's observed count falls outside its projected
    /// range — i.e. we left the swordfight and miscounted. Drives the
    /// on-screen reminder.
    pub lair_warn: bool,
    /// Completed lair waves this run (one per cleared swordfight), each with
    /// its side-tagged elimination timeline for the per-fight advantage
    /// graph. Live-only; reset on a fresh run.
    pub lair_waves: Vec<WaveRecord>,
    /// --- Cursed Isles tracking (Cursed Isles voyages) ---
    /// Which special encounter this vessel's current run is, inferred from its
    /// tell (the noxious fog, or any zombie/island line as a fallback).
    /// Gates the zombie and island lines below, and the Game-over routing.
    /// Reset at each run's start.
    pub encounter: EncounterKind,
    /// Zombies currently aboard during the raft-boarding (sea) phase: +1 per
    /// "Boarders from the raft..." line; -1 when one is thralled or driven
    /// off. Reset to zero on landing. Best-effort — a raft's exact head
    /// count isn't logged, so this counts boarding events, not necessarily
    /// heads.
    pub zombies_aboard: u32,
    /// Live thralls per pirate (zombies turned to our side and still alive):
    /// +1 on "<p> has taken control of a zombie.", -1 on "<p>'s Thrall is
    /// eliminated!". The sum is our bonus melee manpower on the island.
    pub thralls_alive: HashMap<String, u32>,
    /// Lifetime thralls per pirate over the run (never decremented) — the
    /// "enthralled" total the Enthralled leaderboard ranks by.
    pub thralls_total: HashMap<String, u32>,
    /// Whether we're in the island-foraging phase (between "Ye land on the
    /// island" and a retreat / a lost wave / the run ending).
    pub island_active: bool,
    /// Island wave number: 1 on landing, +1 per cleared wave (a `Game over` we
    /// won).
    pub island_wave: u32,
    /// Real pirates aboard at landing — the crew anchor the wave forecast
    /// grows from.
    pub island_pirates: u32,
    /// Enemy NPCs eliminated in the current wave only (reset each wave).
    /// Undercounts when we leave the fight — see
    /// [`Self::island_left_warn`].
    pub wave_enemies_observed: u32,
    /// Crew-anchored projected `[low, high]` enemy count for the current wave.
    pub wave_enemies_lo: u32,
    pub wave_enemies_hi: u32,
    /// Latched once a wave's observed kills fell short of its projection —
    /// i.e. we left the fight early, so the counts are unreliable. Drives
    /// an on-screen note.
    pub island_left_warn: bool,
    /// The current wave's kind (rumble / swordfight). Waves start at Rumble
    /// (wave 1) and alternate, so this is set deterministically from the
    /// wave number — see [`wave_kind_for`]. Whether the Vargas boss is
    /// present is likewise derived from the wave number (see
    /// [`vargas_in_wave`]), not stored.
    pub wave_kind: WaveKind,
    /// Completed island waves this run (one per cleared wave), each with its
    /// side-tagged elimination timeline for the per-fight advantage graph.
    /// Live-only; reset on a fresh run.
    pub island_waves: Vec<WaveRecord>,
    /// The in-progress wave's elimination timeline (Cursed Isles island *or*
    /// Vampirate lair — only one is active at a time). Enemy KOs step it up,
    /// our own KOs step it down; finalized into [`Self::island_waves`] /
    /// [`Self::lair_waves`] when the wave closes.
    pub wave_timeline: FightTimeline,
    /// Greedy strikes tallied per attacking pirate, over the whole run.
    pub greedy_by_pirate: HashMap<String, u32>,
    /// Greedy strikes during the current/most-recent battle only. Reset when a
    /// new battle is joined (intercept), so between battles it holds the last
    /// battle's tally. Displayed as `(total - current) + current`.
    pub greedy_current: HashMap<String, u32>,
    /// Jobbers we (the player) planked. A set: each victim shows once even if
    /// planked across multiple battles. BTreeSet so iteration is alphabetical.
    pub planked_by_us: BTreeSet<String>,
    /// We left this vessel mid-run, so its data has gaps.
    pub poisoned: bool,
    /// We jobbed aboard via an offer, so the vessel's real name isn't known
    /// yet — it's keyed by a provisional `Ship of <crew>` label until a
    /// grapple reveals the hull's name and
    /// [`GameState::promote_current_vessel`] re-keys it. Drives the italic
    /// styling of the placeholder in the vessel selector.
    pub provisional: bool,
    /// Monotonic board sequence; higher = boarded more recently. Updated on
    /// every (re)boarding so the selector can show the latest vessel on top.
    /// Always present, so it drives ordering even when timestamps are missing.
    pub order: u64,
    /// Wall-clock time we last boarded, from the line's `[HH:MM:SS]` combined
    /// with the most recent `====== Y/M/D ======` date header. For display.
    pub boarded_at: Option<NaiveDateTime>,
    /// The voyage currently underway aboard this vessel (sail -> port), if
    /// any. Accumulates per-battle stats; promoted into [`Self::voyages`]
    /// at port/divvy.
    pub current_voyage: Option<Voyage>,
    /// Completed sail->port runs, in order. RAM-only this session — nothing is
    /// written to disk until the user is prompted to save or discard
    /// (deferred).
    pub voyages: Vec<Voyage>,
}

impl Vessel {
    #[allow(dead_code)] // used in tests / handy accessor
    pub fn total_greedy(&self) -> u32 {
        self.greedy_by_pirate.values().sum()
    }
}

/// A brief of the current open battle, surfaced to the new-battle prompt.
pub struct BattleBrief {
    /// The foe vessel's own name, if known.
    pub name: Option<String>,
    /// The foe's hull, as a [`crate::ships::SHIPS`] index, if known.
    pub foe_ship: Option<usize>,
    /// A short note for a noteworthy foe (special encounter / player), else
    /// `None` for an ordinary brigand.
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// GameState — the state machine
// ---------------------------------------------------------------------------

pub struct GameState {
    /// Our own pirate name (from `--user`), used to attribute planks to us.
    pub player_name: Option<Arc<str>>,
    /// True once [`Self::player_name`] has been *confirmed* present in the log
    /// via a strong, unspoofable signal (a `Game over` winners list, an `X
    /// is eliminated!`, or an `X issued an order` line). Until then a
    /// battle's win/loss is indeterminate: our name not appearing among
    /// the winners could mean we lost, or that the configured name is
    /// simply wrong/absent. Chat (`X says`) is deliberately excluded —
    /// it's forgeable.
    pub self_confirmed: bool,
    /// True once a chat log has been attached via `--chat-log`.
    pub attached: bool,

    /// Every vessel we've been aboard this session, keyed by ship name.
    pub vessels: HashMap<Arc<str>, Vessel>,
    /// The vessel we're aboard right now, if any.
    pub current: Option<Arc<str>>,

    /// Crewmates/hearties currently logged on (global; wiped on relog).
    /// Tracked from presence lines but not yet surfaced anywhere — kept
    /// for a future "who's online" view. (No longer feeds the fetch
    /// worklist, which is now scoped to the aboard/planked sets.)
    #[allow(dead_code)]
    pub online: HashSet<String>,

    /// Date as we currently believe it to be: the most recent
    /// `====== Y/M/D ======` header, advanced by one day each time the line
    /// clock wraps past midnight without a new header.
    pub current_date: Option<NaiveDate>,
    /// Time of the previous timestamped line; used to detect midnight
    /// rollover.
    last_time: Option<NaiveTime>,
    /// Timestamp of the line currently being processed (date + line time).
    now: Option<NaiveDateTime>,
    /// Monotonic counter handing out [`Vessel::order`] values.
    order_counter: u64,
    /// Monotonic counter handing out [`Voyage::id`] values. Not reset on relog
    /// — ids only need to stay unique within the process so the pager's
    /// selection pin never collides after a `vessels.clear()`.
    next_voyage_id: u64,
    /// Set for the duration of one line when a sea battle just resolved (`Game
    /// over` / disengage). Lets the app freeze the live Damage calculator onto
    /// that fight. Reset at the top of each [`Self::process_line`].
    battle_just_resolved: bool,
    /// Set for the duration of one line when a new sea battle just began (an
    /// interception). Lets the app jump to the live Damage calculator for the
    /// fight. Reset at the top of each [`Self::process_line`]; consumed by
    /// [`Self::take_battle_started`].
    battle_just_started: bool,
    /// Set for the duration of one line when a special encounter announced the
    /// foe's hull type (e.g. the Black Ship herald). Lets the app seed the
    /// live Damage calculator's foe ship. Reset at the top of each
    /// [`Self::process_line`]; consumed by [`Self::take_detected_foe_ship`].
    detected_foe_ship: Option<usize>,
    /// Set for the duration of one line when we just entered a vampire lair.
    /// Lets the app jump to the Jobbers page and switch it to the
    /// Vampirates voyage layout. Reset at the top of each
    /// [`Self::process_line`]; consumed by [`Self::take_lair_entered`].
    lair_just_entered: bool,
    /// Set to the vessel key for the duration of one line when we just boarded
    /// a vessel. Lets the app snap the Jobbers/Voyage vessel selector to
    /// the ship we just stepped onto. Reset at the top of each
    /// [`Self::process_line`]; consumed by [`Self::take_boarded_vessel`].
    boarded_vessel: Option<Arc<str>>,
    /// Set for the duration of one line when the Cursed Isles tell (the
    /// noxious fog) first fires on a run. Lets the app jump to the Jobbers
    /// page and switch it to the Cursed Isles voyage layout — mirrors
    /// [`Self::lair_just_entered`]. Reset at the top of each
    /// [`Self::process_line`]; consumed by
    /// [`Self::take_cursed_isles_detected`].
    cursed_isles_just_detected: bool,
    /// Set for the duration of one line when the booty was divided. Lets the
    /// app freeze the just-divvied run's booty (chest PoE + goods) from
    /// the live Profits state onto the voyage, so a later pillage
    /// overwriting that state doesn't blank the Divvy section. Reset at
    /// the top of each [`Self::process_line`]; consumed by
    /// [`Self::take_booty_divided`].
    booty_divided: bool,
    /// Set for the duration of one line when a grappled sea battle's *first*
    /// melee elimination landed. Lets the app surface the Sea Battles
    /// graph mid-fight (lair / island runs already surfaced their layout
    /// on the entry tell, so they don't use this). Reset at the top of
    /// each [`Self::process_line`]; consumed by
    /// [`Self::take_battle_first_blood`].
    battle_first_blood: bool,
    /// Armed by the rum-spice limit tell (`Avast, yer mercenary hirin' is
    /// limited by the rum spice…`) and consumed by the *next single* `A
    /// swabbie has left the vessel.` — that departure is really a
    /// mercenary shed to spice, not a swabbie. Persists across intervening
    /// lines (chat, etc.); cleared by any swabbie delta (so a bulk
    /// board/leave disarms it without a swap) and at battle/relog resets.
    spice_swap_armed: bool,

    /// Learned brigand naming vocabulary (see [`crate::cache::NameSegments`]),
    /// accumulated from brigand-victory rosters and persisted in the cache.
    /// Used to tell swabbies from mercenaries among the NPCs aboard.
    /// Seeded from the cache at startup; not reset on relog (the
    /// vocabulary is game-wide, not session-scoped).
    pub name_segments: crate::cache::NameSegments,
}

impl GameState {
    pub fn new() -> Self {
        Self {
            player_name: None,
            self_confirmed: false,
            attached: false,
            vessels: HashMap::new(),
            current: None,
            online: HashSet::new(),
            current_date: None,
            last_time: None,
            now: None,
            order_counter: 0,
            next_voyage_id: 0,
            battle_just_resolved: false,
            battle_just_started: false,
            detected_foe_ship: None,
            lair_just_entered: false,
            boarded_vessel: None,
            cursed_isles_just_detected: false,
            booty_divided: false,
            battle_first_blood: false,
            spice_swap_armed: false,
            name_segments: crate::cache::NameSegments::default(),
        }
    }

    /// Process an entire existing buffer (whole-file ingest). Returns the byte
    /// offset of the start of any trailing incomplete line — where the tailer
    /// should resume so a half-written final line is not parsed twice.
    pub fn process_existing(&mut self, data: &[u8]) -> u64 {
        let mut start = 0usize;
        let mut resume = 0u64;
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                let line = String::from_utf8_lossy(&data[start .. i]);
                self.process_line(line.trim_end_matches('\r'));
                start = i + 1;
                resume = start as u64;
            }
        }
        resume
    }

    // -- parsing / dispatch --

    /// Parse a single log line and delegate to the appropriate handler.
    pub fn process_line(&mut self, line: &str) {
        self.battle_just_resolved = false;
        self.battle_just_started = false;
        self.detected_foe_ship = None;
        self.lair_just_entered = false;
        self.boarded_vessel = None;
        self.cursed_isles_just_detected = false;
        self.booty_divided = false;
        self.battle_first_blood = false;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return;
        }

        // Date header => the player relogged: wipe the whole state, but keep
        // the parsed date so subsequent lines can be fully timestamped.
        if line.starts_with("======") {
            self.on_relog(parse_date_header(line));
            return;
        }

        // Everything else is `[HH:MM:SS] <body>`.
        let Some((time, body)) = parse_line(line) else {
            return;
        };
        self.advance_clock(time);
        self.classify(body);
    }

    /// Advance our notion of "now" to this line's time. The log only stamps a
    /// time of day, and the date header is *not* reprinted at midnight, so a
    /// time that goes backwards versus the previous line means the day rolled
    /// over — bump the date to keep timestamps monotonic.
    fn advance_clock(&mut self, time: NaiveTime) {
        if let Some(prev) = self.last_time
            && time < prev
        {
            self.current_date = self.current_date.and_then(|d| d.succ_opt());
        }
        self.last_time = Some(time);
        self.now = self.current_date.map(|d| d.and_time(time));
    }

    fn classify(&mut self, body: &str) {
        // Chat is NOT an identity signal: "tells ye," is second-person (the
        // speaker is by definition not us), and "says,"/"chats," only prove
        // *someone* spoke — the name could be anyone. Identity is confirmed
        // only by game-generated, position-bound signals (winners list,
        // eliminations, issued orders); see confirm_self call sites.

        // Greedy strikes: "<attacker> <verb phrase> against <Brigand>, <tail>!"
        // We only care who landed it, not the flavour.
        const GREEDY: &[&str] = &[
            " delivers an overwhelming barrage against ",
            " executes a masterful strike against ",
            " performs a powerful attack against ",
            " swings a devious blow against ",
        ];
        for marker in GREEDY {
            if let Some(idx) = body.find(marker) {
                self.on_greedy_strike(&body[.. idx]);
                return;
            }
        }

        // Battle start: a fresh battle resets the current-battle greedy tally
        // and opens a new [`Battle`] record. The interception line names the
        // foe as `<Hull> '<Name>'`; the hull seeds the Damage calculator and
        // the quoted portion is kept as the enemy name. Only that form opens a
        // battle — legacy bare descriptors (an unnamed brigand/monster name, or
        // the generic `Brigands`/`Barbarians`) no longer occur, so a
        // strip_prefix that fails to resolve a hull is ignored. Direction (who
        // intercepted whom) isn't tracked.
        let intercept = body
            .strip_prefix("You intercepted the ")
            .or_else(|| body.strip_prefix("You have been intercepted by the "));
        if let Some(rest) = intercept {
            let descriptor = rest.trim_end_matches(['!', '.']).trim();
            if let (Some(foe_ship), name) = parse_foe_vessel(descriptor) {
                self.on_battle_start(foe_ship, name);
            }
            return;
        }

        // Set-sail order: starts the voyage on its first occurrence (the order
        // also fires on every subsequent navigation move — those are
        // ignored once a voyage is underway).
        if let Some(who) =
            body.strip_suffix(" issued an order to set the vessel to sail.")
        {
            self.confirm_self(who);
            self.on_set_sail();
            return;
        }
        // Put-into-port order: ends the timed sail->port run.
        if let Some(who) =
            body.strip_suffix(" issued an order to put into port.")
        {
            self.confirm_self(who);
            self.on_put_into_port();
            return;
        }
        // A grapple begins the boarding melee: "<A> has grappled <B>. A melee
        // breaks out between the crews!" (logged for both sides; we keep the
        // first). The two vessels named here also let us learn a jobbed
        // vessel's real name — whichever grappler is the open battle's foe, the
        // other is ours.
        if body.contains(" has grappled ")
            && body.ends_with("A melee breaks out between the crews!")
        {
            if let Some((a, b)) = parse_grapple(body) {
                self.promote_from_grapple(a, b);
            }
            self.on_grapple();
            return;
        }
        // A disengage ends a battle with no boarding conclusion. Only an
        // explicit disengage line counts: "<X> issued an order to
        // disengage." (we broke off) or "<vessel> disengaged from the
        // battle." (the foe did). We do NOT treat pursuit-ended lines
        // ("Arr, ye can no longer pursue ...: That vessel has put into
        // port.") as disengages — they fire for stale or
        // cancelled targets (e.g. a brigand-king expedition) and can't be told
        // apart from our active foe, so they'd wrongly disengage the open
        // fight.
        if body.ends_with(" issued an order to disengage.")
            || body.ends_with(" disengaged from the battle.")
        {
            self.on_disengage();
            return;
        }
        // Per-fight loot: "The victors plundered N pieces of eight and M units
        // of goods from the defeated vessel." Attaches to the
        // just-resolved battle.
        if let Some(rest) = body.strip_prefix("The victors plundered ") {
            self.on_plunder(rest);
            return;
        }
        // Our cut: "Ye received N pieces of eight as your initial cut of the
        // booty!"
        if let Some(rest) = body
            .strip_prefix("Ye received ")
            .and_then(|s| s.strip_suffix(" as your initial cut of the booty!"))
        {
            if let Some(poe) = parse_num_commas(rest) {
                self.on_my_cut(poe);
            }
            return;
        }

        // A Vampirate vessel closes in (fires right after interception, so the
        // battle is open).
        if body.starts_with("Avast! Yer blood runs cold") {
            self.categorize_current(BattleCategory::Vampirate);
            return;
        }
        // Werewolves catch our scent (same timing as the Vampirate herald).
        if body.starts_with("Unearthly howling echos o'er the waves") {
            self.categorize_current(BattleCategory::Werewolf);
            return;
        }
        // The Black Ship (El Pollo Diablo) takes the place of our target — a
        // rare special encounter heralded right after interception,
        // like the monster heralds above. Always a Grand Frigate.
        // Matched in full (not a substring) so a player can't trigger
        // it by parroting the line in chat: a chat body is prefixed
        // with `<Name> says, "…`, never exactly the system message.
        if body
            == "Dark clouds gather as ye bear down upon yer hapless victims, \
                and from the miasma emerges the Black Ship to take the place \
                of yer target in battle! Arrrrgh! Ye be doomed fer sure!"
        {
            self.on_black_ship();
            return;
        }

        // Brigand King — the victory line names the king unambiguously
        // ("<King>'s ship disappears into the mists."); the reward
        // chest names them too. Both fire after `Game over`, so they
        // tag the just-resolved battle.
        if let Some(name) =
            body.strip_suffix("'s ship disappears into the mists.")
            && BRIGAND_KINGS.contains(&name)
        {
            self.categorize_recent(BattleCategory::BrigandKing(
                name.to_string(),
            ));
            return;
        }
        if let Some(name) = body
            .strip_prefix("Ye have received one ")
            .and_then(|s| s.strip_suffix(" Chest as part of yer reward!"))
            && BRIGAND_KINGS.contains(&name)
        {
            self.categorize_recent(BattleCategory::BrigandKing(
                name.to_string(),
            ));
            return;
        }

        // Atlantis: a lone dragoon sneaks aboard, or the monster lands a whole
        // boarding party. The game reports how many are aboard itself, so
        // neither tell is counted; both mark the encounter, which is what
        // tells a driven-off dragoon from an ordinary foe.
        if body == "Ye hear a splash, and the sound of foreign footsteps."
            || body
                == "Dragoons from the monster took advantage of their \
                    proximity to board yer vessel!"
        {
            self.on_atlantis_tell();
            return;
        }

        // Cursed Isles: the noxious fog is our (late) tell that this run is a
        // Cursed Isles voyage. Mark the encounter and fire the one-shot
        // auto-jump the first time it shows. The cure line ("Another
        // draft from the rum kegs...") and the CI loot lines are
        // intentionally not parsed.
        if body
            == "The crew inhales the noxious fog, and starts to lose fine \
                motor control."
        {
            self.on_cursed_isles_tell();
            return;
        }
        // Cursed Isles: a raft sinks and dumps zombie boarders onto us (sea
        // phase).
        if body
            == "Boarders from the raft clamber onto yer vessel as theirs sinks \
                to the depths."
        {
            self.on_zombie_aboard();
            return;
        }
        // Cursed Isles: a crewmate turns a boarding zombie to our side (a
        // thrall).
        if let Some(who) = body.strip_suffix(" has taken control of a zombie.")
        {
            self.confirm_self(who);
            self.on_thrall_taken(who);
            return;
        }
        // Cursed Isles: a boarding zombie is driven back off the ship
        // (defeated, not thralled): "<p> has driven <Adjective> Zombie
        // from the ship!". The trailing "Zombie" keeps this from
        // matching an ordinary foe driven off in a pillage.
        if let Some(rest) = body.strip_suffix(" from the ship!")
            && let Some((who, foe)) = rest.split_once(" has driven ")
        {
            if foe.ends_with("Zombie") {
                self.confirm_self(who);
                self.note_pirate_aboard(who); // driving a zombie off proves they're aboard
                self.on_zombie_driven_off();
                return;
            }
            // Atlantis: a dragoon driven off the ship. The dragoon itself is
            // the game's business to count; what the line proves is that
            // the driver is aboard. Gated on the Atlantis encounter (set
            // by the boarding tells) so an ordinary foe driven off in a
            // pillage doesn't match: dragoon names are bare Greek words,
            // so we can't key off the foe name — context is the tell.
            if self
                .current_vessel()
                .is_some_and(|v| v.encounter == EncounterKind::Atlantis)
            {
                self.confirm_self(who);
                self.note_pirate_aboard(who);
                return;
            }
        }
        // Cursed Isles: we land on the island — the boarding phase ends and the
        // foraging waves begin (wave 1).
        if body
            == "Ye land on the island, but an angry mob of its inhabitants \
                stands between ye and yer rightful plunderin'!"
        {
            self.on_island_land();
            return;
        }
        // Cursed Isles: an officer recalls the crew aboard — the island phase
        // ends.
        if body
            .strip_suffix(" ordered everyone back aboard the ship!")
            .is_some()
        {
            self.on_island_retreat();
            return;
        }

        // Vampirates: we entered a lair (wave 1 begins). Waves run from here
        // (and the first slap of Mother) through each swordfight
        // conclusion below — the "rustling in coffins" line is only a
        // "swordfight imminent" herald and does NOT delimit waves, so
        // it isn't parsed.
        if body.starts_with("Welcome to the vampire sanctum") {
            self.on_lair_enter();
            return;
        }
        // Vampirates: someone slapping Mother is the wave-1 fight kickoff and a
        // fallback lair-start signal if we missed the sanctum line (only starts
        // a lair if we aren't already in one).
        if body.ends_with(" slaps mother") {
            self.on_lair_slap();
            return;
        }
        // A melee knockout: "<Name> is eliminated!".
        if let Some(name) = body.strip_suffix(" is eliminated!") {
            self.on_eliminated(name);
            return;
        }

        // Battle end: "Game over.  Winners: a, b, Playerone." — if we're in the
        // winning side, it's an authoritative roster of who's aboard; if the
        // winners are vampires, we lost the board, which ends a lair.
        if let Some(summary) = body.strip_prefix("Game over.") {
            let summary = summary.trim_start();
            // Cursed Isles winner lists include our "<p>'s Thrall" allies,
            // whose spaces the sea-battle crew-resync would
            // miscount as swabbies (and corrupt the crew roster).
            // So on a CI run the island wave engine owns
            // Game over outright; the pillage/lair handlers are bypassed.
            if self
                .current_vessel()
                .is_some_and(|v| v.encounter == EncounterKind::CursedIsles)
            {
                self.on_island_gameover(summary);
            } else {
                self.on_battle_end(summary); // resync crew roster when we won
                self.on_sea_battle_resolve(summary); // record the sea-battle outcome (pillage)
                self.on_lair_gameover(summary); // vampirate wave engine
                self.sample_crew(); // roster may have been resynced
            }
            return;
        }

        // Going aboard a vessel — gives us the ship name.
        if let Some(ship) = body
            .strip_prefix("Going aboard the ")
            .and_then(|s| s.strip_suffix("..."))
        {
            self.on_board_vessel(ship);
            return;
        }

        // Job start / end.
        if let Some(kind) = body
            .strip_prefix("This vessel is now ")
            .and_then(|s| s.strip_suffix('.'))
        {
            if let Some(v) = self.current_vessel_mut() {
                v.job_kind = Some(JobKind::parse(kind));
            }
            return;
        }
        if body == "The booty has been divided!" {
            let now = self.now;
            // Signal the app to freeze this run's booty from the live Profits
            // state.
            self.booty_divided = true;
            if let Some(v) = self.current_vessel_mut() {
                v.job_kind = None;
                // Finalize the run if it wasn't already closed at port
                // (defensive: some runs end at divvy without a
                // port order we saw).
                if let Some(mut voy) = v.current_voyage.take() {
                    if voy.ported_at.is_none() {
                        voy.ported_at = now;
                    }
                    if let Some(b) = voy.current_battle.take() {
                        voy.battles.push(b);
                    }
                    voy.divvied = true;
                    v.voyages.push(voy);
                } else if let Some(voy) = v.voyages.last_mut() {
                    // The port order already promoted this run (port precedes
                    // the divvy in the log); the divvy just
                    // confirms it reached a booty division.
                    voy.divvied = true;
                }
            }
            return;
        }

        // Rum-spice hiring-limit tell: arms the very next *single* swabbie
        // departure as a mercenary shed to spice, not a genuine swabbie
        // (see `spice_swap_armed`).
        if body
            == "Avast, yer mercenary hirin' is limited by the rum spice in yer \
                hold. Ye need at least 5 spice per mercenary."
        {
            self.spice_swap_armed = true;
            // Too little rum spice in the hold to sustain the mercenaries: the
            // hold ran short and mercs are being shed to spice, so this run's
            // headcounts and stock deltas can no longer be trusted. Poison the
            // voyage — the flag persists and gates `delta_unreliable`.
            if let Some(voy) = self.current_voyage_mut() {
                voy.poisoned = true;
            }
            return;
        }

        // NPC head-count deltas. The log's `… swabbie …` lines fire for
        // mercenaries too (mercs never get their own board/leave line),
        // so this count lumps both crew kinds — a won fight later
        // resyncs the tally *and* the merc roster from the winners
        // roster.
        if let Some(delta) = parse_swabbie_delta(body) {
            // Only a *single* departure right after the tell is a depletion
            // swap; a bulk board/leave (re-staffing) just disarms
            // it.
            let spice_swap = self.spice_swap_armed && delta == -1;
            self.spice_swap_armed = false;
            if let Some(v) = self.current_vessel_mut() {
                if delta >= 0 {
                    // New bodies always board as swabbies (mercs are hired, not
                    // boarded via a delta line).
                    v.swabbies = v.swabbies.saturating_add(delta as u32);
                } else {
                    let n = delta.unsigned_abs() as u32;
                    if spice_swap {
                        // A merc's spice ran out: it departs (logged as a
                        // swabbie leaving) and a
                        // genuine swabbie replaces it via the paired
                        // come-aboard. Shed one merc; the total dips here and
                        // is restored by the come,
                        // netting -1 merc / +1 genuine swabbie.
                        shed_mercs(&mut v.mercenaries, 1);
                        v.swabbies = v.swabbies.saturating_sub(1);
                    } else {
                        // Ordinary departure: genuine swabbies leave first;
                        // only once they're exhausted
                        // do mercs start leaving (the overflow past
                        // the genuine-swabbie pool).
                        let genuine = v
                            .swabbies
                            .saturating_sub(v.mercenaries.len() as u32);
                        shed_mercs(
                            &mut v.mercenaries,
                            n.saturating_sub(genuine),
                        );
                        v.swabbies = v.swabbies.saturating_sub(n);
                    }
                }
            }
            self.sample_crew();
            return;
        }

        // Others boarding / leaving the vessel. Only real player pirates are
        // tracked — NPCs like "A swabbie" or named swabbies ("Tony Ironsides")
        // contain a space and are rejected by `is_player_name`.
        if let Some(name) = body.strip_suffix(" has come aboard.") {
            if pirate::is_player_name(name) {
                if let Some(v) = self.current_vessel_mut() {
                    v.crewmates.insert(name.to_string());
                }
                self.sample_crew();
            }
            return;
        }
        if let Some(name) = body.strip_suffix(" has left the vessel.") {
            if let Some(v) = self.current_vessel_mut() {
                v.crewmates.remove(name);
                v.disconnected.remove(name); // gone for good — not just a dropout
            }
            self.sample_crew();
            return;
        }

        // A crewmate's client dropped / came back. They stay aboard (and on the
        // winners roster) while disconnected, but don't fight — tracked so
        // they're excluded from a battle's crew strength. Players only
        // (NPCs don't drop).
        if let Some(name) = body.strip_suffix(" has disconnected.") {
            if pirate::is_player_name(name)
                && let Some(v) = self.current_vessel_mut()
            {
                v.disconnected.insert(name.to_string());
            }
            return;
        }
        if let Some(name) = body.strip_suffix(" has reconnected.") {
            if let Some(v) = self.current_vessel_mut() {
                v.disconnected.remove(name);
            }
            return;
        }

        // We left the vessel, or were planked off it.
        if (body.starts_with("Ye have left '") && body.ends_with("'."))
            || body.starts_with("Ye were made to walk the plank by ")
        {
            self.leave_vessel();
            return;
        }
        // Whisking home ends time aboard whatever we were on (jobbing home mid
        // run, retreating from an island, etc.).
        if body == "Whisking away to yer home on the magical winds." {
            self.leave_vessel();
            return;
        }
        // Jobbing aboard via an offer. The vessel we join is never named in the
        // log until a grapple reveals it, so board it under a provisional
        // `Ship of <crew>` label; a later grapple promotes it to the real name.
        // Accepting an offer swaps vessels directly, so any current run is left
        // (and poisoned) first.
        if let Some(crew) = body
            .strip_prefix("Ye accepted the offer to job with '")
            .and_then(|s| s.strip_suffix("'."))
        {
            self.on_job_accept(crew);
            return;
        }

        // Third-person plank: "<Planker> forced <Victim> to walk the plank."
        if let Some(mid) = body.strip_suffix(" to walk the plank.")
            && let Some((planker, victim)) = mid.split_once(" forced ")
        {
            self.on_plank(planker, victim);
            self.sample_crew(); // a crewmate left the roster
            return;
        }

        // Presence: crewmates and hearties going online/offline.
        if let Some(rest) = body.strip_prefix("Yer crew member ") {
            if let Some(name) = rest.strip_suffix(" has logged on.") {
                self.online.insert(name.to_string());
                return;
            }
            if let Some(name) = rest.strip_suffix(" has logged off.") {
                self.online.remove(name);
                return;
            }
        }
        if let Some(rest) = body.strip_prefix("Yer hearty, ") {
            if let Some(name) = rest.strip_suffix(", has logged on.") {
                self.online.insert(name.to_string());
                return;
            }
            if let Some(name) = rest.strip_suffix(", has logged off.") {
                self.online.remove(name);
                return;
            }
        }

        // Catch-all (runs only for otherwise-unhandled lines, so it can't
        // swallow a `Game over` etc.): a Brigand King's engagement /
        // flavour chant names the king. Tag the open battle. Player
        // chatter is skipped so a mention of a king in chat doesn't
        // mislabel a fight.
        if !is_chat_line(body)
            && let Some(king) = find_brigand_king(body)
        {
            self.categorize_current(BattleCategory::BrigandKing(
                king.to_string(),
            ));
        }
    }

    // -- handlers (the "further specific logic") --

    /// Player relogged (a new `====== Y/M/D ======` header) — wipe everything
    /// except configuration (player name stays). Adopts the header's date.
    fn on_relog(&mut self, date: Option<NaiveDate>) {
        self.vessels.clear();
        self.current = None;
        self.online.clear();
        self.order_counter = 0;
        self.current_date = date;
        self.last_time = None;
        self.now = None;
    }

    /// Player went aboard a vessel. Keeps existing data if we're returning to a
    /// vessel we've seen (including its poisoned flag), but refreshes its board
    /// order and timestamp so it floats to the top of the selector.
    fn on_board_vessel(&mut self, ship: &str) {
        let key: Arc<str> = Arc::from(ship);
        self.order_counter += 1;
        let order = self.order_counter;
        let now = self.now;
        let v = self.vessels.entry(key.clone()).or_default();
        v.order = order;
        v.boarded_at = now;
        self.current = Some(key.clone());
        // Surface this vessel in the selector (consumed once by the app's
        // auto-navigation).
        self.boarded_vessel = Some(key);
    }

    /// Player jobbed aboard a vessel via an offer from `crew`. Since accepting
    /// swaps vessels outright, any current vessel is left first (poisoning an
    /// unfinished run). The joined vessel isn't named until a grapple reveals
    /// it, so it's keyed provisionally by crew and marked
    /// [`Vessel::provisional`] for the italic placeholder;
    /// [`Self::promote_current_vessel`] re-keys it once the name is known.
    fn on_job_accept(&mut self, crew: &str) {
        self.leave_vessel();
        let key: Arc<str> = Arc::from(format!("Ship of {crew}"));
        self.order_counter += 1;
        let order = self.order_counter;
        let now = self.now;
        let v = self.vessels.entry(key.clone()).or_default();
        v.order = order;
        v.boarded_at = now;
        v.provisional = true;
        self.current = Some(key.clone());
        self.boarded_vessel = Some(key);
    }

    /// Re-key the current provisional (jobbed) vessel to its real name, now
    /// that a grapple has revealed it. No-op unless the current vessel is
    /// provisional and `real` differs from its placeholder key. If a vessel of
    /// that real name already exists (e.g. we boarded it earlier), the
    /// placeholder is dropped and the existing one becomes current rather than
    /// clobbering its accumulated data.
    fn promote_current_vessel(&mut self, real: &str) {
        let Some(old_key) = self.current.clone() else {
            return;
        };
        if !self.vessels.get(&old_key).is_some_and(|v| v.provisional)
            || &*old_key == real
        {
            return;
        }
        let new_key: Arc<str> = Arc::from(real);
        // A vessel by this real name already exists (we boarded it earlier):
        // leave the provisional one in place rather than clobbering or
        // discarding either's accumulated data.
        if self.vessels.contains_key(&new_key) {
            return;
        }
        let Some(mut v) = self.vessels.remove(&old_key) else {
            return;
        };
        v.provisional = false;
        self.vessels.insert(new_key.clone(), v);
        self.current = Some(new_key.clone());
        self.boarded_vessel = Some(new_key);
    }

    /// Player left the current vessel (left the crew, or was planked). If the
    /// vessel's run wasn't finished (booty not yet divided), it's now poisoned.
    fn leave_vessel(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            if v.job_kind.is_some() {
                v.poisoned = true;
            }
            if let Some(voy) = v.current_voyage.as_mut() {
                voy.poisoned = true;
            }
        }
        self.current = None;
    }

    /// Someone (possibly us) planked someone — third-person line.
    fn on_plank(&mut self, planker: &str, victim: &str) {
        let is_us = self
            .player_name
            .as_deref()
            .is_some_and(|me| planker.eq_ignore_ascii_case(me));
        if let Some(v) = self.current_vessel_mut() {
            v.crewmates.remove(victim);
            if is_us {
                v.planked_by_us.insert(victim.to_string());
            }
        }
    }

    /// A greedy strike landed by `attacker` — counts toward both the run total
    /// and the current battle.
    fn on_greedy_strike(&mut self, attacker: &str) {
        if let Some(v) = self.current_vessel_mut() {
            *v.greedy_by_pirate.entry(attacker.to_string()).or_insert(0) += 1;
            *v.greedy_current.entry(attacker.to_string()).or_insert(0) += 1;
        }
    }

    /// A new battle began — start a fresh current-battle greedy tally and open
    /// a new [`Battle`] record on the current voyage (creating the voyage if a
    /// fight somehow starts before we saw a sail order). `foe_ship` is the hull
    /// the interception line named (resolved by [`parse_foe_vessel`]); `enemy`
    /// is the foe vessel's own name, kept for display.
    fn on_battle_start(&mut self, foe_ship: usize, enemy: &str) {
        let now = self.now;
        self.battle_just_started = true;
        if let Some(v) = self.current_vessel_mut() {
            v.greedy_current.clear();
        }
        // The named hull seeds the live Damage calculator's foe ship. A monkey
        // boat additionally carries its fixed fruit name in the quoted portion,
        // which tags the fight; all these names come from the system line, so
        // none can be spoofed via chat.
        self.detected_foe_ship = Some(foe_ship);
        let monkey_ship = monkey_boat_ship(enemy);
        let enemy = (!enemy.is_empty()).then(|| enemy.to_string());
        if let Some(voy) = self.ensure_voyage() {
            // A still-open previous battle means we never saw its resolution;
            // keep it as a dangling record rather than dropping it.
            if let Some(prev) = voy.current_battle.take() {
                voy.battles.push(prev);
            }
            let mut battle = Battle {
                enemy,
                started_at: now,
                // Live-tracked fights are recorded (persisted) by default; the
                // Sea Battles popup's Recorded toggle can still
                // opt an individual fight out. On save this
                // writes the fight's snapshot when one exists.
                recorded: true,
                foe_ship: Some(foe_ship),
                ..Battle::default()
            };
            if monkey_ship.is_some() {
                battle.category = BattleCategory::MonkeyBoat;
            }
            voy.current_battle = Some(battle);
        }
    }

    /// First `set the vessel to sail` order of a run starts the voyage; later
    /// move orders during the run just backfill a missing sail time (if the
    /// voyage was lazily created by an early battle).
    fn on_set_sail(&mut self) {
        let now = self.now;
        // A fresh run (no voyage underway yet) wipes any prior run's Cursed
        // Isles state so the encounter and the fog auto-jump re-arm per
        // run.
        let fresh = self
            .current_vessel()
            .is_some_and(|v| v.current_voyage.is_none());
        if fresh {
            self.reset_cursed_isles();
        }
        // Reserve a voyage id before borrowing the vessel, but only when we're
        // actually about to create a new run (a re-sail just backfills sail
        // time).
        let new_id = fresh.then(|| {
            self.next_voyage_id += 1;
            self.next_voyage_id
        });
        let Some(v) = self.current_vessel_mut() else {
            return;
        };
        match &mut v.current_voyage {
            Some(voy) => {
                if voy.sailed_at.is_none() {
                    voy.sailed_at = now;
                }
            }
            None => {
                v.current_voyage = Some(Voyage {
                    id: new_id.unwrap_or_default(),
                    sailed_at: now,
                    ..Voyage::default()
                });
            }
        }
        // Anchor the crew timeline at sail time with the starting headcount.
        self.sample_crew();
    }

    /// A `put into port` order ends the timed run: stamp the port time, fold
    /// any dangling battle in, and promote the voyage into the completed
    /// list.
    fn on_put_into_port(&mut self) {
        let now = self.now;
        let Some(v) = self.current_vessel_mut() else {
            return;
        };
        if let Some(mut voy) = v.current_voyage.take() {
            voy.ported_at = now;
            if let Some(mut b) = voy.current_battle.take() {
                if b.outcome == BattleOutcome::Ongoing {
                    b.outcome = BattleOutcome::Disengaged;
                    b.ended_at = now;
                }
                voy.battles.push(b);
            }
            v.voyages.push(voy);
        }
    }

    /// Learn a jobbed vessel's real name from a grapple. No-op unless the
    /// current vessel is still provisional. The open battle's `enemy` is the
    /// foe; whichever grappler matches it, the other party is our vessel, so we
    /// promote the placeholder to that name.
    fn promote_from_grapple(&mut self, a: &str, b: &str) {
        if !self.current_vessel().is_some_and(|v| v.provisional) {
            return;
        }
        let foe = self
            .current_vessel()
            .and_then(|v| v.current_voyage.as_ref())
            .and_then(|voy| voy.current_battle.as_ref())
            .and_then(|batt| batt.enemy.as_deref());
        let ours = match foe {
            Some(f) if f == a => Some(b),
            Some(f) if f == b => Some(a),
            _ => None,
        };
        if let Some(name) = ours {
            self.promote_current_vessel(name);
        }
    }

    /// The boarding melee started — record the sea/boarding boundary once, and
    /// snapshot the crew aboard so a crewmate who leaves mid-melee still counts
    /// as a boarder (our manpower is "who fought", measured at boarding
    /// start).
    fn on_grapple(&mut self) {
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            let players: Vec<String> = v.crewmates.iter().cloned().collect();
            let mercenaries = v.mercenaries.len() as u32;
            // `v.swabbies` is the raw lumped NPC count; the roster stores
            // genuine swabbies and mercenaries disjointly, so
            // subtract the known mercs.
            let swabbies = v.swabbies.saturating_sub(mercenaries);
            if let Some(b) = v
                .current_voyage
                .as_mut()
                .and_then(|voy| voy.current_battle.as_mut())
                && b.grappled_at.is_none()
            {
                b.grappled_at = now;
                // Our side as it stood at boarding start; finalized at
                // resolution (resynced roster ∪ this, minus the
                // disconnected).
                b.our_team = Some(TeamSide {
                    players,
                    swabbies,
                    mercenaries,
                });
            }
        }
    }

    /// A battle ended without a boarding conclusion (someone disengaged / enemy
    /// ported / we shook the pursuit). Resolve the open battle as `Disengaged`.
    fn on_disengage(&mut self) {
        let now = self.now;
        let mut resolved = false;
        if let Some(voy) = self.current_voyage_mut()
            && let Some(mut b) = voy.current_battle.take()
        {
            b.outcome = BattleOutcome::Disengaged;
            b.ended_at = now;
            voy.battles.push(b);
            resolved = true;
        }
        self.battle_just_resolved |= resolved;
    }

    /// Resolve the open sea battle at `Game over`. Won iff our own name is
    /// among the winners. The verdict stored here is *provisional*: it's
    /// shown as [`BattleOutcome::Unknown`] until our identity is confirmed
    /// (a strong signal — winners list, elimination, order, or chat), at
    /// which point every past fight is revealed retroactively. With no
    /// `--user` name there's nothing to confirm against, so it stays
    /// Unknown. Skipped inside a vampirate lair, whose per-wave swordfights
    /// aren't sea battles. Run *after* [`Self::on_battle_end`] so the crew
    /// snapshot uses the resynced roster.
    fn on_sea_battle_resolve(&mut self, summary: &str) {
        // A fight's conclusion closes any pending rum-spice swap window: the
        // tell and its paired departure are always adjacent, so an arm
        // that survived a whole battle is stale and must not mis-tag a
        // later swabbie departure.
        self.spice_swap_armed = false;
        if self.current_vessel().is_some_and(|v| v.lair_active) {
            return;
        }
        let me = self.player_name.clone();
        let me = me.as_deref();
        // The winners roster, parsed once. On a win it's our ship; on a loss
        // it's the foe's crew. On an unknown-identity fight we can't
        // say which.
        let winners: Vec<String> = summary
            .split_once(':')
            .map(|(_, list)| {
                list.trim()
                    .trim_end_matches('.')
                    .split(", ")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // Our name in the winners list both decides a win and confirms our
        // identity (an unspoofable strong signal).
        let in_winners = winners
            .iter()
            .any(|n| me.is_some_and(|me| n.eq_ignore_ascii_case(me)));
        if in_winners {
            self.self_confirmed = true;
        }
        // The *provisional* verdict, computed against the configured name. It's
        // masked to [`BattleOutcome::Unknown`] at the view/stats/persistence
        // layer (see [`crate::voyage::effective_outcome`]) until our
        // identity is confirmed — so a confirmation arriving much later
        // retroactively reveals every earlier fight. With no configured
        // name there's nothing to confirm, so it stays genuinely
        // unknown.
        let outcome = match me {
            None => BattleOutcome::Unknown,
            Some(_) if in_winners => BattleOutcome::Won,
            Some(_) => BattleOutcome::Lost,
        };
        // Inputs for our manpower, captured before the mutable voyage borrow
        // below. `live_crew` is the resynced roster after
        // `on_battle_end` (on a win it's the winners' players, which
        // catches crew we never saw board); it's unioned
        // with the battle's grapple-time team (which catches crew who left
        // mid-melee).
        let live_crew: HashSet<String> = self
            .current_vessel()
            .map(|v| v.crewmates.clone())
            .unwrap_or_default();
        let disconnected: HashSet<String> = self
            .current_vessel()
            .map(|v| v.disconnected.clone())
            .unwrap_or_default();
        let swabbies = self.current_vessel().map(|v| v.swabbies).unwrap_or(0);
        // Mercenary count for this fight's roster/per-merc stats (mercenaries
        // earn no divvy share), read from the roster that `on_battle_end`
        // (run just before us on this same `Game over`) re-truthed from the
        // winners list on a win. On a loss/disengage our side isn't named, so
        // the roster is the carried best-effort estimate. See the mercenary
        // roster on `Vessel`.
        let mercenaries = self
            .current_vessel()
            .map(|v| v.mercenaries.len() as u32)
            .unwrap_or(0);
        // A king on the winning side means we lost to that king — name the
        // fight.
        let king = find_brigand_king(summary);
        // PvP on a loss: a winner who's a real player and not ours = an enemy
        // player. (A win's eliminations already flagged PvP via
        // `on_eliminated`, since all enemies are eliminated. On an
        // unknown outcome we can't tell which side the winners are, so
        // we rely solely on melee eliminations.)
        let lost_to_players = outcome == BattleOutcome::Lost
            && winners
                .iter()
                .any(|n| pirate::is_player_name(n) && !self.is_own_crew(n));
        // A *pure brigand victory* (we lost, no enemy players, no Brigand King)
        // has a uniformly `[adjective] [name]` winners roster, so we
        // can learn the naming vocabulary from it (feeds
        // swabbie-vs-mercenary classification). Our own wins and PvP
        // losses are skipped — those crews mix mercenaries and
        // swabbies, so the split is ambiguous. Specials are excluded by
        // `learn_brigand`.
        if outcome == BattleOutcome::Lost && !lost_to_players && king.is_none()
        {
            for w in &winners {
                self.name_segments.learn_brigand(w);
            }
        }
        // Split a roster into real players (kept by name) and a bare NPC count.
        // Used for the enemy side, which we never classify —
        // `mercenaries` stays 0 (we only classify our own crew; see the PvP
        // note in the design).
        let split_side = |names: &[String]| {
            TeamSide {
                players: names
                    .iter()
                    .filter(|n| pirate::is_player_name(n))
                    .cloned()
                    .collect(),
                swabbies: names
                    .iter()
                    .filter(|n| !pirate::is_player_name(n))
                    .count() as u32,
                mercenaries: 0,
            }
        };
        let now = self.now;
        let mut resolved = false;
        if let Some(voy) = self.current_voyage_mut()
            && let Some(mut b) = voy.current_battle.take()
        {
            b.outcome = outcome;
            b.ended_at = now;
            // Our manpower = who actually fought: the grapple-time team
            // unioned with the resynced crew, minus the
            // disconnected (a held-but-idle melee slot),
            // plus us. A leaver stays counted (captured at grapple);
            // a never-reconnecting dropout is dropped even if the winners
            // roster still lists them.
            let mut roster: HashSet<String> = live_crew;
            if let Some(team) = &b.our_team {
                roster.extend(team.players.iter().cloned());
            }
            let mut our_players: Vec<String> = roster
                .into_iter()
                .filter(|n| !disconnected.contains(n.as_str()))
                .collect();
            let fought = our_players.len() as u32;
            b.pirates = fought + 1;
            // `b.swabbies` keeps the total NPC crew (for manpower); the
            // roster splits it into genuine swabbies and
            // mercenaries, disjointly.
            b.swabbies = swabbies;
            let genuine_swabbies = swabbies.saturating_sub(mercenaries);
            // Record our side by name. Include ourselves when our name is
            // known so the roster is complete.
            if let Some(me) = me
                && !our_players.iter().any(|n| n.eq_ignore_ascii_case(me))
            {
                our_players.push(me.to_string());
            }
            b.our_team = Some(TeamSide {
                players: our_players,
                swabbies: genuine_swabbies,
                mercenaries,
            });
            // PvP (its own category) may already be set from the melee; a
            // loss to a real-player crew flags it too. PvP
            // overrides a king label.
            if b.category == BattleCategory::Pvp || lost_to_players {
                b.category = BattleCategory::Pvp;
            } else if let Some(k) = king {
                b.category = BattleCategory::BrigandKing(k.to_string());
            }
            // The foe's side: on a win, the eliminations that aren't our
            // crew (all enemies are eliminated, so they're
            // the KOs not in the winners roster, which is
            // our ship on a win); on a loss, the winners' (enemy)
            // roster. On an unknown outcome we can't tell, so leave it
            // absent.
            b.their_team = match outcome {
                BattleOutcome::Won => {
                    let foe: Vec<String> = b
                        .melee_kos
                        .iter()
                        .filter(|ko| {
                            !winners.iter().any(|w| w.eq_ignore_ascii_case(ko))
                        })
                        .cloned()
                        .collect();
                    Some(split_side(&foe))
                }
                BattleOutcome::Lost => Some(split_side(&winners)),
                _ => None,
            };
            // Backfill the per-fight advantage timeline. Each event lines
            // up with `melee_kos` (pushed in lockstep in
            // `on_eliminated`): tag it `Ours` when the KO'd
            // name is on our finalized roster, else
            // `Theirs` — outcome-independent, so it's robust on wins,
            // losses, and unknown fights alike. (Our own
            // swabbie KOs can't be told from enemy NPCs, so
            // they fall to `Theirs`; best-effort.)
            let our_lc: HashSet<String> = b
                .our_team
                .as_ref()
                .map(|t| {
                    t.players.iter().map(|n| n.to_ascii_lowercase()).collect()
                })
                .unwrap_or_default();
            let mut theirs = 0u32;
            for (ev, ko) in b.timeline.events.iter_mut().zip(b.melee_kos.iter())
            {
                ev.side = if our_lc.contains(&ko.to_ascii_lowercase()) {
                    KoSide::Ours
                } else {
                    theirs += 1;
                    KoSide::Theirs
                };
            }
            b.timeline.our_start = b.pirates + b.swabbies;
            // Their starting headcount: on a win all enemies were
            // eliminated, so it's the enemy-KO count; on a
            // loss it's those plus the enemy survivors (the
            // winners). Unknown/disengage leaves it `None` (the
            // graph then plots the net-KO differential).
            b.timeline.their_start = match outcome {
                BattleOutcome::Won => Some(theirs),
                BattleOutcome::Lost => {
                    Some(theirs + split_side(&winners).headcount())
                }
                _ => None,
            };
            b.timeline.started_at = b.grappled_at;
            b.timeline.ended_at = b.ended_at;
            b.melee_kos.clear();
            voy.battles.push(b);
            resolved = true;
        }
        self.battle_just_resolved |= resolved;
    }

    /// Attach plundered PoE + goods to the most-recently-resolved battle. The
    /// plunder line always follows a `Game over`, so `battles.last_mut()` is
    /// it. PoE is signed by the battle's outcome (negative on a loss — it
    /// went to them).
    fn on_plunder(&mut self, rest: &str) {
        let poe = rest
            .split(" pieces of eight")
            .next()
            .and_then(parse_num_commas);
        let goods = rest.split(" and ").nth(1).and_then(|g| {
            if g.starts_with("no goods") {
                Some(0)
            } else {
                g.split(" units of goods")
                    .next()
                    .and_then(parse_num_commas)
                    .map(|n| n as u32)
            }
        });
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy.battles.last_mut()
        {
            if let Some(poe) = poe {
                // Signed by the (provisional) outcome: positive on a win,
                // negative on a loss. With no candidate identity the
                // direction is unknowable, so we keep
                // no signed value.
                b.poe = match b.outcome {
                    BattleOutcome::Won => Some(poe as i64),
                    BattleOutcome::Lost => Some(-(poe as i64)),
                    _ => None,
                };
            }
            if goods.is_some() {
                b.goods = goods;
            }
        }
    }

    /// Attach our personal cut to the most-recently-resolved battle.
    fn on_my_cut(&mut self, poe: u64) {
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy.battles.last_mut()
        {
            b.my_cut = Some(poe);
        }
    }

    /// The Black Ship (El Pollo Diablo) replaced our target. Tag the open fight
    /// and pin its hull to a Grand Frigate; its crew count is left to the
    /// usual melee elimination tally (so it tracks however the devs staff
    /// it). Also flags the foe hull for the live Damage calculator via
    /// [`Self::take_detected_foe_ship`].
    fn on_black_ship(&mut self) {
        let grand = crate::ships::ship_index("Grand Frigate");
        self.detected_foe_ship = grand;
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy.current_battle.as_mut()
        {
            b.category = BattleCategory::BlackShip;
            b.foe_ship = grand;
        }
    }

    /// Tag the battle currently in progress (engagement-time signals).
    fn categorize_current(&mut self, cat: BattleCategory) {
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy.current_battle.as_mut()
        {
            b.category = cat;
        }
    }

    /// Tag the open battle, or the just-resolved one if none is open (end-time
    /// signals like the victory/reward lines arrive after `Game over`).
    fn categorize_recent(&mut self, cat: BattleCategory) {
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy
                .current_battle
                .as_mut()
                .or_else(|| voy.battles.last_mut())
        {
            b.category = cat;
        }
    }

    /// The current vessel's active voyage, mutably.
    fn current_voyage_mut(&mut self) -> Option<&mut Voyage> {
        self.current_vessel_mut()?.current_voyage.as_mut()
    }

    /// Find a current-login voyage across all vessels by its stable
    /// [`Voyage::id`] — completed runs and the in-progress one alike. Used by
    /// the Voyage Statistics pager's save/discard, which act on the
    /// selected run.
    pub fn voyage_by_id_mut(&mut self, id: u64) -> Option<&mut Voyage> {
        self.vessels.values_mut().find_map(|v| {
            v.voyages
                .iter_mut()
                .chain(v.current_voyage.iter_mut())
                .find(|vy| vy.id == id)
        })
    }

    /// Record the current headcount onto the active voyage's crew timeline. A
    /// no-op when not aboard, not on a voyage, or before the clock is set. Call
    /// after any change to the crewmate/swabbie counts.
    fn sample_crew(&mut self) {
        let Some(now) = self.now else {
            return;
        };
        let Some(v) = self.current_vessel_mut() else {
            return;
        };
        let pirates = v.crewmates.len() as u32 + 1; // incl. us
        let swabbies = v.swabbies;
        // Provisional merc count (mercs board invisibly, so this is an estimate
        // until the next winners-roster ground truth backfills the
        // stretch — see `on_battle_end`).
        let mercenaries = v.mercenaries.len() as u32;
        if let Some(voy) = v.current_voyage.as_mut() {
            voy.crew_samples.push(CrewSample {
                at: now,
                pirates,
                swabbies,
                mercenaries,
            });
        }
    }

    /// Ensure the current vessel has an active voyage, creating a job-tagged
    /// one if needed. `sailed_at` stays `None` for lazily-created voyages
    /// (a battle began before we saw a sail order); [`Self::on_set_sail`]
    /// backfills it.
    fn ensure_voyage(&mut self) -> Option<&mut Voyage> {
        if self.current_vessel()?.current_voyage.is_none() {
            // Reserve the id before the mutable vessel borrow.
            self.next_voyage_id += 1;
            let id = self.next_voyage_id;
            let v = self.current_vessel_mut()?;
            v.current_voyage = Some(Voyage {
                id,
                ..Voyage::default()
            });
        }
        self.current_vessel_mut()?.current_voyage.as_mut()
    }

    /// A dragoon boarding tell fired — this run is an Atlantis one.
    fn on_atlantis_tell(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::Atlantis;
        }
    }

    /// The Cursed Isles tell (the noxious fog) fired. Mark the encounter and,
    /// the first time per run, request the one-shot auto-jump to the Cursed
    /// Isles layout.
    fn on_cursed_isles_tell(&mut self) {
        let already = self
            .current_vessel()
            .is_some_and(|v| v.encounter == EncounterKind::CursedIsles);
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::CursedIsles;
        }
        if !already {
            self.cursed_isles_just_detected = true;
        }
    }

    /// A zombie raft boarded us (Cursed Isles sea phase). Also marks the
    /// encounter as a fallback if we missed the fog tell.
    fn on_zombie_aboard(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::CursedIsles;
            v.zombies_aboard = v.zombies_aboard.saturating_add(1);
        }
    }

    /// Record a real-player pirate as aboard from an action that proves their
    /// presence (enthralling or driving off a boarding zombie) — the same
    /// roster signal as a "has come aboard" line, useful on a Cursed Isles
    /// run where the boarding melees may be the first time we see a jobber
    /// act. A no-op for NPC names and for us (we're never in the crewmate
    /// set).
    fn note_pirate_aboard(&mut self, who: &str) {
        let me = self.player_name.clone();
        let is_other = pirate::is_player_name(who)
            && me.as_deref().is_none_or(|me| !who.eq_ignore_ascii_case(me));
        if !is_other {
            return;
        }
        if let Some(v) = self.current_vessel_mut() {
            v.crewmates.insert(who.to_string());
        }
        self.sample_crew();
    }

    /// A crewmate enthralled a boarding zombie: it leaves the hostile count and
    /// joins `who`'s thralls (both the live count and the lifetime total).
    /// Enthralling proves `who` is aboard, so they're folded into the crew
    /// roster too.
    fn on_thrall_taken(&mut self, who: &str) {
        self.note_pirate_aboard(who);
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::CursedIsles;
            *v.thralls_alive.entry(who.to_string()).or_insert(0) += 1;
            *v.thralls_total.entry(who.to_string()).or_insert(0) += 1;
            v.zombies_aboard = v.zombies_aboard.saturating_sub(1);
        }
    }

    /// A boarding zombie was driven back off the ship (Cursed Isles sea phase).
    fn on_zombie_driven_off(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::CursedIsles;
            v.zombies_aboard = v.zombies_aboard.saturating_sub(1);
        }
    }

    /// We landed on the island — the raft-boarding phase ends and the foraging
    /// waves begin at wave 1. Anchors the crew-based wave forecast on the
    /// pirates aboard.
    fn on_island_land(&mut self) {
        let pirates = self.current_pirates();
        let (lo, hi) = island_wave_band(pirates, 1);
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::CursedIsles;
            v.zombies_aboard = 0;
            v.island_active = true;
            v.island_wave = 1;
            v.island_pirates = pirates;
            v.wave_enemies_observed = 0;
            v.wave_enemies_lo = lo;
            v.wave_enemies_hi = hi;
            v.island_left_warn = false;
            v.wave_kind = wave_kind_for(1); // wave 1 is always a Rumble
            // Open a fresh assault: archive nothing yet, start wave 1's
            // timeline anchored on our landing manpower (pirates +
            // any thralls we kept).
            v.island_waves.clear();
            v.wave_timeline = FightTimeline {
                our_start: pirates + v.thralls_alive.values().sum::<u32>(),
                started_at: now,
                ..FightTimeline::default()
            };
        }
    }

    /// An island enemy NPC was knocked out: tally it for the wave. (The wave's
    /// kind and the Vargas boss's presence are both derived from the wave
    /// number, not the enemy seen — so Vargas just counts as another kill
    /// here.)
    fn on_island_enemy_defeated(&mut self) {
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            if !v.island_active {
                return;
            }
            v.wave_enemies_observed = v.wave_enemies_observed.saturating_add(1);
            // Their headcount drops → advantage steps up.
            v.wave_timeline.events.push(KoEvent {
                at: now,
                side: KoSide::Theirs,
            });
        }
    }

    /// A `Game over` while on the island (Cursed Isles). Mirrors the lair
    /// engine: if the wave's kills fell short of its projection we left
    /// early (flag it); on a win advance to the next wave and re-project;
    /// on a loss the island phase ends.
    fn on_island_gameover(&mut self, summary: &str) {
        let Some((_, list)) = summary.split_once(':') else {
            return;
        };
        let me = self.player_name.clone();
        let names: Vec<&str> = list
            .trim()
            .trim_end_matches('.')
            .split(", ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let players_won = names.iter().any(|n| pirate::is_player_name(n));
        // The winning side's real players are an authoritative crew roster (a
        // CI win lists our crew). Add them as proof of presence —
        // additively, so a KO'd or late-seen jobber is captured without
        // dropping anyone. Thralls ("<p>'s Thrall") and named swabbies
        // carry spaces, so `is_player_name` rejects them; ourselves are
        // excluded (we're never in the crewmate set).
        let crew_winners: Vec<String> = names
            .iter()
            .filter(|n| pirate::is_player_name(n))
            .filter(|n| {
                me.as_deref().is_none_or(|me| !n.eq_ignore_ascii_case(me))
            })
            .map(|n| n.to_string())
            .collect();
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            if !v.island_active {
                return;
            }
            for n in &crew_winners {
                v.crewmates.insert(n.clone());
            }
            if v.wave_enemies_observed < v.wave_enemies_lo {
                v.island_left_warn = true;
            }
            // The wave concluded (win or loss): close its timeline (every enemy
            // we saw is gone) and archive it for the per-fight graph.
            v.wave_timeline.their_start = Some(v.wave_enemies_observed);
            v.wave_timeline.ended_at = now;
            let finished = std::mem::take(&mut v.wave_timeline);
            v.island_waves.push(WaveRecord {
                wave: v.island_wave,
                kind: v.wave_kind,
                timeline: finished,
            });
            if !players_won {
                v.island_active = false;
                return;
            }
            v.island_wave = v.island_wave.saturating_add(1);
            v.wave_enemies_observed = 0;
            v.wave_kind = wave_kind_for(v.island_wave); // alternates from the Rumble start
            let (lo, hi) = island_wave_band(v.island_pirates, v.island_wave);
            v.wave_enemies_lo = lo;
            v.wave_enemies_hi = hi;
            // Open the next wave's timeline.
            v.wave_timeline = FightTimeline {
                our_start: v.island_pirates
                    + v.thralls_alive.values().sum::<u32>(),
                started_at: now,
                ..FightTimeline::default()
            };
        }
    }

    /// The crew was recalled aboard — the island foraging phase ends.
    fn on_island_retreat(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.island_active = false;
        }
    }

    /// Reset all Cursed Isles state at the start of a fresh run, so a later run
    /// on the same vessel doesn't inherit the previous encounter (and the
    /// fog tell re-arms the auto-jump once per run).
    fn reset_cursed_isles(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.encounter = EncounterKind::None;
            v.zombies_aboard = 0;
            v.thralls_alive.clear();
            v.thralls_total.clear();
            v.island_active = false;
            v.island_wave = 0;
            v.island_pirates = 0;
            v.wave_enemies_observed = 0;
            v.wave_enemies_lo = 0;
            v.wave_enemies_hi = 0;
            v.island_left_warn = false;
            v.wave_kind = WaveKind::Unknown;
            v.island_waves.clear();
            v.wave_timeline = FightTimeline::default();
        }
    }

    /// Entered a vampire lair — wave 1 begins with one vampire per pirate
    /// aboard. Resets the lair counters (a re-entry / new lair starts
    /// fresh).
    fn on_lair_enter(&mut self) {
        // Real pirates aboard = tracked crewmates + ourselves (NPC swabbies
        // don't count). This is wave 1's vampire count.
        let pirates = self
            .current_vessel()
            .map(|v| v.crewmates.len() as u32 + 1)
            .unwrap_or(0);
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            v.lair_active = true;
            v.lair_wave = 1;
            v.lair_pirates = pirates;
            v.vampires_defeated = 0;
            v.wave_observed = 0;
            v.wave_lo = pirates; // wave 1 is exactly the pirate count
            v.wave_hi = pirates;
            v.lair_warn = false;
            // Fresh lair: start wave 1's timeline anchored on the crew aboard.
            v.lair_waves.clear();
            v.wave_timeline = FightTimeline {
                our_start: pirates,
                started_at: now,
                ..FightTimeline::default()
            };
        }
        // Surface the Jobbers page in its Vampirates layout for the lair
        // (consumed once by the app's auto-navigation).
        self.lair_just_entered = true;
    }

    /// A `slaps mother` line: start the lair only if we aren't already in one
    /// (so repeated slaps mid-fight don't reset the counters).
    fn on_lair_slap(&mut self) {
        if !self.current_vessel().is_some_and(|v| v.lair_active) {
            self.on_lair_enter();
        }
    }

    /// A vampire was defeated (only counted while in a lair).
    /// A combatant was knocked out in a melee. In a vampirate lair, NPC names
    /// count as defeated vampires. In a sea battle we record every KO on the
    /// open fight (the basis for the foe's headcount) and flag PvP when an
    /// eliminated real player isn't our own crew.
    fn on_eliminated(&mut self, name: &str) {
        // Cursed Isles: one of our controlled zombies (a thrall) died — drop
        // its controller's live count. A thrall is never an enemy, so
        // bail before any lair / island / sea-battle counting (in any
        // phase).
        if let Some(who) = name.strip_suffix("'s Thrall") {
            let who = who.to_string();
            if let Some(v) = self.current_vessel_mut()
                && let Some(n) = v.thralls_alive.get_mut(&who)
            {
                *n = n.saturating_sub(1);
            }
            return;
        }
        // Cursed Isles island wave: count enemy NPC kills (classify the wave
        // and flag Vargas). A real-player KO is one of our crew
        // (islanders are all NPCs) — it isn't an enemy, but it does
        // prove that pirate is aboard.
        if self.current_vessel().is_some_and(|v| v.island_active) {
            self.confirm_self(name);
            if pirate::is_player_name(name) {
                self.note_pirate_aboard(name);
                // Our headcount drops → advantage steps down on the wave graph.
                let now = self.now;
                if let Some(v) = self.current_vessel_mut() {
                    v.wave_timeline.events.push(KoEvent {
                        at: now,
                        side: KoSide::Ours,
                    });
                }
            } else {
                self.on_island_enemy_defeated();
            }
            return;
        }
        // Cursed Isles raft phase (pre-landing): a zombie can *challenge* a
        // pirate to a duel, whose KO we must NOT count as a wave/sea
        // elimination — only the on-island assault counts. (The island
        // branch above already handled the landed phase, so reaching
        // here on a CI run means the raft phase.)
        if self
            .current_vessel()
            .is_some_and(|v| v.encounter == EncounterKind::CursedIsles)
        {
            self.confirm_self(name);
            return;
        }
        // Vampirate lairs: tally defeated vampires (NPC names); a real-player
        // KO is one of our crew falling — record it as a loss on the
        // wave graph.
        if self.current_vessel().is_some_and(|v| v.lair_active) {
            if pirate::is_player_name(name) {
                self.confirm_self(name);
                let now = self.now;
                if let Some(v) = self.current_vessel_mut() {
                    v.wave_timeline.events.push(KoEvent {
                        at: now,
                        side: KoSide::Ours,
                    });
                }
            } else {
                self.on_vampire_defeated();
            }
            return;
        }
        // Our own elimination is a strong, unspoofable confirmation we're here.
        self.confirm_self(name);
        // Sea battle: record the KO and detect an enemy player.
        let enemy_player =
            pirate::is_player_name(name) && !self.is_own_crew(name);
        let now = self.now;
        let mut first_blood = false;
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy.current_battle.as_mut()
        {
            b.melee_kos.push(name.to_string());
            // Mirror the KO onto the per-fight timeline (same order as
            // `melee_kos`); the side is provisional and backfilled at
            // resolution from the rosters.
            b.timeline.events.push(KoEvent {
                at: now,
                side: KoSide::Theirs,
            });
            if enemy_player {
                // PvP is its own, mutually exclusive category — once set it
                // overrides any king/monster telltale.
                b.category = BattleCategory::Pvp;
            }
            // First melee KO of a grappled fight surfaces the live
            // advantage graph.
            first_blood =
                b.grappled_at.is_some() && b.timeline.events.len() == 1;
        }
        if first_blood {
            self.battle_first_blood = true;
        }
    }

    /// Confirm our identity if `name` matches our configured pirate name.
    /// Called only from strong, unspoofable signals (winners list,
    /// elimination, order). Once confirmed, a battle's win/loss becomes
    /// determinate; until then it stays [`BattleOutcome::Unknown`]. A no-op
    /// when no name is configured.
    fn confirm_self(&mut self, name: &str) {
        if self
            .player_name
            .as_deref()
            .is_some_and(|me| me.eq_ignore_ascii_case(name))
        {
            self.self_confirmed = true;
        }
    }

    /// Whether `name` is us or one of our current vessel's crewmates.
    fn is_own_crew(&self, name: &str) -> bool {
        if self.player_name.as_deref() == Some(name) {
            return true;
        }
        self.current_vessel()
            .is_some_and(|v| v.crewmates.contains(name))
    }

    fn on_vampire_defeated(&mut self) {
        let now = self.now;
        if let Some(v) = self.current_vessel_mut()
            && v.lair_active
        {
            v.vampires_defeated = v.vampires_defeated.saturating_add(1);
            v.wave_observed = v.wave_observed.saturating_add(1);
            // Their headcount drops → advantage steps up.
            v.wave_timeline.events.push(KoEvent {
                at: now,
                side: KoSide::Theirs,
            });
        }
    }

    /// A swordfight concluded (`Game over`) — the boundary between lair waves.
    /// While in a lair we close out the current wave (flagging if its
    /// observed count fell outside the projection — i.e. we left the
    /// fight), then either:
    ///   * the winners are all vampires => we lost => the lair ends (first
    ///     loss), or
    ///   * the crew won => advance to the next wave, projecting its range from
    ///     the concluded wave's anchor count (`pirates * growth^(wave-1)`).
    fn on_lair_gameover(&mut self, summary: &str) {
        let Some((_, list)) = summary.split_once(':') else {
            return;
        };
        let players_won = list
            .trim()
            .trim_end_matches('.')
            .split(", ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(pirate::is_player_name);
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            if !v.lair_active {
                return;
            }
            // Close out the wave that just concluded.
            if v.wave_observed < v.wave_lo || v.wave_observed > v.wave_hi {
                v.lair_warn = true;
            }
            // Archive the concluded wave's timeline for the per-fight graph.
            v.wave_timeline.their_start = Some(v.wave_observed);
            v.wave_timeline.ended_at = now;
            let finished = std::mem::take(&mut v.wave_timeline);
            v.lair_waves.push(WaveRecord {
                wave: v.lair_wave,
                kind: WaveKind::Swordfight,
                timeline: finished,
            });
            if !players_won {
                // First loss ends the lair.
                v.lair_active = false;
                return;
            }
            // Crew won: advance to the next wave and project its range.
            let base = v.lair_pirates as f64
                * LAIR_WAVE_GROWTH.powi((v.lair_wave - 1) as i32);
            v.lair_wave += 1;
            v.wave_observed = 0;
            v.wave_lo = (base * LAIR_WAVE_LO).round() as u32;
            v.wave_hi = (base * LAIR_WAVE_HI).round() as u32;
            // Open the next wave's timeline.
            v.wave_timeline = FightTimeline {
                our_start: v.lair_pirates,
                started_at: now,
                ..FightTimeline::default()
            };
        }
    }

    /// A battle ended with `summary` like `Winners: a, b, Playerone.`. When
    /// the player is among the listed side, that side is the crew that fought,
    /// which on an ordinary pillage is the crew aboard: the crewmate set is
    /// overwritten with it (minus ourselves and NPC swabbies).
    ///
    /// Where the fray is only part of the crew's
    /// ([`EncounterKind::partial_fray`]) the list is read as a floor instead.
    /// The names in it are confirmed aboard and join the roster; the rest are
    /// left exactly as they stood, because being absent from a fight is no
    /// evidence of being absent from the ship. The cost is that a crewmate or
    /// mercenary who really did leave such a run lingers until something else
    /// says they went, which is the right way round: we would rather carry
    /// someone who has gone than drop someone who is below decks.
    fn on_battle_end(&mut self, summary: &str) {
        let Some((_, list)) = summary.split_once(':') else {
            return;
        };
        let names: Vec<&str> = list
            .trim()
            .trim_end_matches('.')
            .split(", ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        // Clone our name out so we don't hold a borrow of `self` across the
        // mutable vessel access below.
        let me = self.player_name.clone();
        let me = me.as_deref();
        let player_listed = me
            .is_some_and(|me| names.iter().any(|n| n.eq_ignore_ascii_case(me)));
        if !player_listed {
            return;
        }

        let new_crew: HashSet<String> = names
            .iter()
            .filter(|n| pirate::is_player_name(n))
            .filter(|n| me.is_none_or(|me| !n.eq_ignore_ascii_case(me)))
            .map(|n| n.to_string())
            .collect();

        // The roster is authoritative for swabbies too: everything that isn't a
        // player name is one. This resyncs the running delta tally, correcting
        // any drift accumulated while the vessel was poisoned.
        let swabbies =
            names.iter().filter(|n| !pirate::is_player_name(n)).count() as u32;
        // ...and for the mercenary roster: the winners list is the only place
        // NPCs are named, so it's our sole merc/swabbie ground truth.
        // Mercenaries are the `[name][epithet]` NPCs; everyone else
        // non-player is a genuine swabbie. This re-truthing corrects
        // any drift (departures, rum-spice swaps) since the last
        // win. See the merc roster on `Vessel`.
        let mercenaries: BTreeSet<String> = names
            .iter()
            .filter(|n| !pirate::is_player_name(n))
            // A generic unnamed swabbie ("A swabbie") isn't a real NPC name;
            // the classifier would default its unknown tokens to
            // Mercenary, so skip it.
            .filter(|n| !n.eq_ignore_ascii_case("A swabbie"))
            .filter(|n| {
                self.name_segments.classify(n)
                    == Some(crate::cache::NpcKind::Mercenary)
            })
            .map(|n| n.to_string())
            .collect();

        // Whether the fray was the whole crew's. Where it was not, each figure
        // the list re-truths becomes a floor: a name is added, a count is
        // raised, and nothing is taken away.
        let partial = self
            .current_vessel()
            .is_some_and(|v| v.encounter.partial_fray());

        if let Some(v) = self.current_vessel_mut() {
            if partial {
                v.crewmates.extend(new_crew);
                v.swabbies = v.swabbies.max(swabbies);
                v.mercenaries.extend(mercenaries);
            } else {
                v.crewmates = new_crew;
                v.swabbies = swabbies;
                v.mercenaries = mercenaries;
            }
            // Ground truth: backfill every crew sample since the last
            // checkpoint (or the voyage start) to this confirmed
            // mercenary count, then advance the checkpoint. So each
            // inter-win stretch is attributed the count confirmed
            // at its close — correcting the invisible initial hire and any
            // mid-voyage hires we couldn't see live. Off a partial fray the
            // count is a floor rather than a confirmation, and one that only
            // ever rises, so the stretch still gets the best figure the run
            // has reached.
            let confirmed = v.mercenaries.len() as u32;
            if let Some(voy) = v.current_voyage.as_mut() {
                let from = voy.merc_checkpoint.min(voy.crew_samples.len());
                for s in &mut voy.crew_samples[from ..] {
                    s.mercenaries = confirmed;
                }
                voy.merc_checkpoint = voy.crew_samples.len();
            }
        }
    }

    /// The foe headcount computed for the just-resolved (last) battle, if any.
    pub fn last_resolved_their_manpower(&self) -> Option<u32> {
        self.current_voyage()?.battles.last()?.their_manpower()
    }

    /// Our crew strength (fighting pirates + swabbies) recorded for the
    /// just-resolved (last) battle — the grapple-roster count with the
    /// disconnected already subtracted. The app feeds this into the frozen
    /// snapshot so the crew advantage matches the battle record.
    pub fn last_resolved_our_strength(&self) -> Option<u32> {
        let b = self.current_voyage()?.battles.last()?;
        Some(b.pirates + b.swabbies)
    }

    /// Mutable access to the vessel we're currently aboard.
    fn current_vessel_mut(&mut self) -> Option<&mut Vessel> {
        let cur = self.current.clone()?;
        self.vessels.get_mut(&cur)
    }

    /// The vessel we're currently aboard, if any.
    #[allow(dead_code)] // used in tests
    pub fn current_vessel(&self) -> Option<&Vessel> {
        let cur = self.current.as_ref()?;
        self.vessels.get(cur)
    }

    /// The voyage currently underway aboard the current vessel, if any.
    #[allow(dead_code)] // used in tests / UI
    pub fn current_voyage(&self) -> Option<&Voyage> {
        self.current_vessel()?.current_voyage.as_ref()
    }

    /// Pillage PoE over the current pillage — the voyage underway on the
    /// current vessel, or its most recent completed one. Walked in order
    /// over the signed per-battle [`Battle::poe`] ledger, so it's immune to
    /// the booty chest being raided on lost boardings. Returns `(gross_won,
    /// stolen, chest)`, where `chest` is the retained half kept per win
    /// (each contributes `ceil(M/2)` — the odd PoE rounds up into the
    /// chest, matching the in-game booty) and `stolen` is the PoE enemies
    /// actually took, **capped by the chest balance at the time** (they
    /// can't steal from an empty chest). `chest − stolen` is thus
    /// always ≥ 0.
    pub fn current_pillage_poe(&self) -> (u64, u64, u64) {
        let Some(v) = self.current_vessel() else {
            return (0, 0, 0);
        };
        let Some(voy) = v.current_voyage.as_ref().or_else(|| v.voyages.last())
        else {
            return (0, 0, 0);
        };
        let mut gross: u64 = 0;
        let mut chest: u64 = 0; // retained-half total (before theft)
        let mut stolen: u64 = 0;
        let mut balance: i64 = 0; // running chest balance, to cap each theft
        for b in &voy.battles {
            match b.poe {
                Some(p) if p > 0 => {
                    let m = p as u64;
                    let half = m.div_ceil(2);
                    gross += m;
                    chest += half;
                    balance += half as i64;
                }
                Some(p) if p < 0 => {
                    // Enemies can only plunder what the chest currently holds.
                    let take = (-p).min(balance);
                    balance -= take;
                    stolen += take as u64;
                }
                _ => {}
            }
        }
        (gross, stolen, chest)
    }

    /// Timestamp of the most recently processed line (the chat-log "now"). Used
    /// to show live elapsed time for an in-progress voyage.
    #[allow(dead_code)] // used by the Voyage Statistics UI
    pub fn now(&self) -> Option<NaiveDateTime> {
        self.now
    }

    /// Take the "a sea battle just resolved this line" flag (true once per
    /// resolution). The app uses it to freeze the live Damage calculator onto
    /// the fight that just ended.
    pub fn take_resolved(&mut self) -> bool {
        std::mem::take(&mut self.battle_just_resolved)
    }

    /// Take the "a sea battle just began this line" flag (true once per
    /// interception). The app uses it to jump to the live Damage calculator so
    /// the fight is tracked from the first hit.
    pub fn take_battle_started(&mut self) -> bool {
        std::mem::take(&mut self.battle_just_started)
    }

    /// Take the foe-hull index detected from a special encounter this line
    /// (once per detection). The app uses it to seed the live Damage
    /// calculator's foe ship so live tracking — and the captured snapshot —
    /// use the right hull.
    pub fn take_detected_foe_ship(&mut self) -> Option<usize> {
        self.detected_foe_ship.take()
    }

    /// Take the "a grappled sea battle's first melee KO landed this line" flag
    /// (once per fight). The app uses it to surface the Sea Battles graph
    /// mid-fight.
    pub fn take_battle_first_blood(&mut self) -> bool {
        std::mem::take(&mut self.battle_first_blood)
    }

    /// Take the "we just entered a vampire lair this line" flag (true once per
    /// lair entry). The app uses it to jump to the Jobbers page and switch it
    /// to the Vampirates voyage layout.
    pub fn take_lair_entered(&mut self) -> bool {
        std::mem::take(&mut self.lair_just_entered)
    }

    /// Take the "the Cursed Isles tell just fired this line" flag (true once
    /// per run). The app uses it to jump to the Jobbers page and switch it
    /// to the Cursed Isles voyage layout.
    pub fn take_cursed_isles_detected(&mut self) -> bool {
        std::mem::take(&mut self.cursed_isles_just_detected)
    }

    /// Take the vessel we just boarded this line (once per boarding). The app
    /// uses it to snap the Jobbers/Voyage vessel selector to that ship.
    pub fn take_boarded_vessel(&mut self) -> Option<Arc<str>> {
        self.boarded_vessel.take()
    }

    /// Whether the booty was divided this line (once per divvy). The app uses
    /// it to freeze the just-divvied run's booty onto the voyage.
    pub fn take_booty_divided(&mut self) -> bool {
        std::mem::take(&mut self.booty_divided)
    }

    /// The current-pillage run on the current vessel — the current voyage, else
    /// the most recent one — for writing the divvy booty snapshot. Mirrors
    /// [`Self::current_pillage_poe`]'s selection. `None` if we're not aboard a
    /// vessel with any run.
    pub fn current_pillage_voyage_mut(&mut self) -> Option<&mut Voyage> {
        let v = self.current_vessel_mut()?;
        if v.current_voyage.is_some() {
            v.current_voyage.as_mut()
        } else {
            v.voyages.last_mut()
        }
    }

    /// Freeze the live Damage-calculator snapshot + advantage onto the
    /// just-resolved (last) battle of the current voyage. Called at Game
    /// over / disengage when the calculator had input, so fights we tracked
    /// live land in the history recorded. A **disengaged** fight is
    /// skipped: there's nothing worth recording beyond the disengage itself
    /// (who, how long), so we don't pin damage to it.
    pub fn record_resolved_battle(
        &mut self,
        snap: BattleSnapshot,
        dmg: f64,
        crew: f64,
    ) {
        if let Some(voy) = self.current_voyage_mut()
            && let Some(b) = voy.battles.last_mut()
        {
            if b.outcome == BattleOutcome::Disengaged {
                return;
            }
            b.snapshot = Some(snap);
            b.advantage_dmg = Some(dmg);
            b.advantage_crew = Some(crew);
        }
    }

    /// A brief of the current open battle for the new-battle prompt: the foe's
    /// vessel name, its hull (when known), and a note for a noteworthy foe.
    /// Refreshed each line so a mid-fight reveal (Black Ship, monster telltale)
    /// updates the prompt live.
    pub fn current_battle_summary(&self) -> Option<BattleBrief> {
        let b = self
            .current_vessel()?
            .current_voyage
            .as_ref()?
            .current_battle
            .as_ref()?;
        Some(BattleBrief {
            name: b.enemy.clone(),
            foe_ship: b.foe_ship,
            note: b.category.special_note(),
        })
    }

    /// Whether the most recently recorded battle on the current voyage already
    /// carries a Damage-calculator snapshot (its tally was staged into the
    /// voyage history). Read when a new fight opens to tell the user whether
    /// clearing the calculator loses anything.
    pub fn last_recorded_battle_saved(&self) -> bool {
        self.current_vessel()
            .and_then(|v| v.current_voyage.as_ref())
            .and_then(|voy| voy.battles.last())
            .is_some_and(|b| b.snapshot.is_some())
    }

    /// Real pirates aboard the current vessel right now (crewmates + us), or 0.
    pub fn current_pirates(&self) -> u32 {
        self.current_vessel()
            .map(|v| v.crewmates.len() as u32 + 1)
            .unwrap_or(0)
    }

    /// Swabbies (NPC crew, incl. named mercenaries) aboard the current vessel,
    /// or 0.
    pub fn current_swabbies(&self) -> u32 {
        self.current_vessel().map(|v| v.swabbies).unwrap_or(0)
    }

    /// The battle at display index `idx` of the displayed voyage on vessel
    /// `key`. The displayed list is the resolved battles, then the
    /// in-progress one (if any), so an index one past the resolved set
    /// addresses `current_battle`.
    fn displayed_battle_mut(
        &mut self,
        key: &Arc<str>,
        idx: usize,
    ) -> Option<&mut Battle> {
        let v = self.vessels.get_mut(key)?;
        let voy = v.current_voyage.as_mut().or_else(|| v.voyages.last_mut())?;
        let n = voy.battles.len();
        if idx < n {
            voy.battles.get_mut(idx)
        } else if idx == n {
            voy.current_battle.as_mut()
        } else {
            None
        }
    }

    /// Write a fight's Damage-calculator snapshot + recomputed advantage.
    /// Driven by the Sea Battles popup on every edit (the calculator is
    /// always editable; this is independent of whether the fight is
    /// recorded).
    pub fn set_battle_snapshot(
        &mut self,
        key: &Arc<str>,
        idx: usize,
        snap: BattleSnapshot,
        dmg: f64,
        crew: f64,
    ) {
        if let Some(b) = self.displayed_battle_mut(key, idx) {
            b.snapshot = Some(snap);
            b.advantage_dmg = Some(dmg);
            b.advantage_crew = Some(crew);
        }
    }

    /// Set whether a fight is recorded (persisted to disk). Does not touch its
    /// snapshot/advantage — those always exist and display regardless.
    pub fn set_battle_recorded(
        &mut self,
        key: &Arc<str>,
        idx: usize,
        recorded: bool,
    ) {
        if let Some(b) = self.displayed_battle_mut(key, idx) {
            b.recorded = recorded;
        }
    }

    /// Vessel keys ordered latest-boarded first (for the selector).
    pub fn vessels_by_recency(&self) -> Vec<Arc<str>> {
        let mut keys: Vec<Arc<str>> = self.vessels.keys().cloned().collect();
        keys.sort_by(|a, b| {
            self.vessels[b]
                .order
                .cmp(&self.vessels[a].order)
                .then_with(|| a.cmp(b))
        });
        keys
    }

    /// Pirates aboard a given vessel: its recorded crewmates, plus ourselves
    /// when it's the vessel we're currently on.
    pub fn aboard(&self, key: &Arc<str>) -> HashSet<String> {
        let mut set = self
            .vessels
            .get(key)
            .map(|v| v.crewmates.clone())
            .unwrap_or_default();
        if self.current.as_ref() == Some(key)
            && let Some(me) = self.player_name.as_deref()
        {
            set.insert(me.to_string());
        }
        set
    }

    /// Pirates aboard the vessel we're on now, ourselves included. `None` when
    /// we're on no vessel at all, which is a different claim from a vessel
    /// whose roster we happen to hold nobody for.
    pub fn current_aboard(&self) -> Option<HashSet<String>> {
        let key = self.current.clone()?;
        Some(self.aboard(&key))
    }

    /// Keep a copied duty report with the run under way, starting that run if
    /// the report is the first we have seen of it. A rated interval is
    /// evidence of a voyage being worked, which is all a voyage needs to be
    /// one: a run that never fights a battle, as most voyages outside a
    /// pillage never do, is a run on the strength of its reports alone.
    ///
    /// Answers whether the report was kept. It is not when there is no vessel
    /// under us to have a run, nor when it repeats the run's last report: the
    /// same report reaches the clipboard twice often enough, the interval it
    /// covers happened once, and the copy that first brought it is the one
    /// that dates it.
    pub fn note_duty_report(
        &mut self,
        report: &crate::duty::DutyReport,
        copied_at: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let Some(voyage) = self.ensure_voyage() else {
            return false;
        };
        if voyage
            .duty_reports
            .last()
            .is_some_and(|last| last.holds(report))
        {
            return false;
        }
        voyage.duty_reports.push(crate::duty::CopiedReport::new(
            report, copied_at,
        ));
        true
    }

    /// Record pirates as aboard the current vessel, returning how many were
    /// news to its roster. Ourselves is counted apart from the crewmates
    /// everywhere it matters, so our own name is never one of them.
    pub fn note_aboard<'a>(
        &mut self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> usize {
        let me = self.player_name.clone();
        let Some(v) = self.current_vessel_mut() else {
            return 0;
        };
        names
            .into_iter()
            .filter(|name| me.as_deref() != Some(*name))
            .filter(|name| v.crewmates.insert((*name).to_owned()))
            .count()
    }
}

impl Default for GameState {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a swabbie board-count delta from a message body, returning the signed
/// change to the aboard count, or `None` if the line isn't one of the four
/// forms. Singular uses "has", plural uses "have"; departures are negative:
///   "A swabbie has come aboard."        -> +1
///   "N swabbies have come aboard."      -> +N
///   "A swabbie has left the vessel."    -> -1
///   "N swabbies have left the vessel."  -> -N
/// Remove `n` mercenaries from the roster. NPC departures are anonymous, so
/// *which* merc leaves is unknowable — we drop arbitrary (lowest-sorted) names.
/// The roster is re-truthed from the next winners roster anyway, so only the
/// count matters here.
fn shed_mercs(mercs: &mut BTreeSet<String>, n: u32) {
    for _ in 0 .. n {
        let Some(first) = mercs.iter().next().cloned() else {
            break;
        };
        mercs.remove(&first);
    }
}

fn parse_swabbie_delta(body: &str) -> Option<i64> {
    match body {
        "A swabbie has come aboard." => return Some(1),
        "A swabbie has left the vessel." => return Some(-1),
        _ => {}
    }
    if let Some(n) = body
        .strip_suffix(" swabbies have come aboard.")
        .and_then(|n| n.parse::<u32>().ok())
    {
        return Some(n as i64);
    }
    if let Some(n) = body
        .strip_suffix(" swabbies have left the vessel.")
        .and_then(|n| n.parse::<u32>().ok())
    {
        return Some(-(n as i64));
    }
    None
}

/// The eight Brigand Kings (full in-game names). Each king's chants, victory
/// line, reward chest, and winners-list entry all carry the full name, so we
/// key categorization off the name rather than per-king chant text — robust
/// across every king without a brittle chant table. (Roster from yppedia.)
const BRIGAND_KINGS: &[&str] = &[
    "Admiral Finius",
    "Azarbad the Great",
    "Barnabas the Pale",
    "Brynhild Skullsplitter",
    "Gretchen Goldfang",
    "Madam Yu Jian",
    "The Widow Queen",
    "Vargas the Mad",
];

/// The first Brigand King whose full name appears in `text`, if any.
fn find_brigand_king(text: &str) -> Option<&'static str> {
    BRIGAND_KINGS.iter().copied().find(|k| text.contains(k))
}

/// Monkey-boat vessels and the hull each one sails, per yppedia. A monkey boat
/// is identified by its (fixed) vessel name in the interception line, which
/// maps to a known ship type — there are exactly twelve, one per non-niche
/// hull.
const MONKEY_BOATS: &[(&str, &str)] = &[
    ("Petulant Kumquat", "Sloop"),
    ("Itinerant Pomegranate", "Cutter"),
    ("Resplendent Peach", "Dhow"),
    ("Succulent Pear", "Baghlah"),
    ("Appealing Orange", "Longship"),
    (
        "Adventurous Huckleberry",
        "Merchant Brig",
    ),
    ("Scrumptious Strawberry", "Junk"),
    ("Dogged Rhubarb", "War Brig"),
    ("Overbearing Pineapple", "Xebec"),
    ("Vainglorious Plum", "Merchant Galleon"),
    ("Determined Pumpkin", "War Frigate"),
    ("Juicy Watermelon", "Grand Frigate"),
];

/// Split a grapple line into its two vessel names: `<A> has grappled <B>. A
/// melee breaks out between the crews!` -> `(A, B)`.
fn parse_grapple(body: &str) -> Option<(&str, &str)> {
    let (a, rest) = body.split_once(" has grappled ")?;
    let b = rest
        .strip_suffix("A melee breaks out between the crews!")?
        .trim_end_matches([' ', '.']);
    Some((a, b))
}

/// Split an interception's foe descriptor into its named hull and the vessel's
/// display name. Interceptions are logged as `<Hull> '<Name>'`: the hull
/// resolves through [`crate::ships::ship_index`] and the display name is the
/// quoted portion (for a monkey boat that quoted name is its fixed fruit name,
/// which [`monkey_boat_ship`] uses to tag the fight). A descriptor whose hull
/// doesn't resolve yields `(None, raw)`, which the caller ignores rather than
/// opening a battle.
fn parse_foe_vessel(raw: &str) -> (Option<usize>, &str) {
    if let Some((hull, quoted)) = raw.split_once(" '")
        && let Some(name) = quoted.strip_suffix('\'')
        && let Some(idx) = crate::ships::ship_index(hull)
    {
        return (Some(idx), name);
    }
    (None, raw)
}

/// The [`crate::ships::SHIPS`] index of the monkey boat with this exact vessel
/// name, if `name` is one. The name comes from the (system) interception line,
/// so it can't be spoofed via chat.
fn monkey_boat_ship(name: &str) -> Option<usize> {
    MONKEY_BOATS
        .iter()
        .find(|(vessel, _)| *vessel == name)
        .and_then(|(_, ship)| crate::ships::ship_index(ship))
}

/// Whether a line is player chatter (so king names mentioned in chat don't
/// mislabel a fight). Player speech reaches us over several channels, each
/// tagged by a verb token on its first line: `says,` / `tells ye,` / `shouts,`
/// / `broadcasts,` and the `<scope> chats,` family — `chats,` (crew), plus
/// `officer chats,`, `flag officer chats,`, `global chats,`, `trade chats,` and
/// `battle chats,`. The bare ` chats,` substring matches every member of that
/// family regardless of scope prefix, so we don't enumerate them.
///
/// A quoted message can span several log lines (e.g. a multi-line trade-chat
/// listing): only the first line carries the verb token and only the last ends
/// in the closing `"`. The verb tokens above catch every first line, and the
/// `ends_with('"')` arm catches the last; the king chants this guards against
/// are single system lines ending in `!`/`.`, so neither arm false-positives on
/// them. Residual gap: a *middle* continuation line carries neither marker —
/// accepted, as it would only matter if such a line contained an exact Brigand
/// King name.
fn is_chat_line(body: &str) -> bool {
    body.ends_with('"')
        || body.contains(" says,")
        || body.contains(" chats,")
        || body.contains(" shouts,")
        || body.contains(" broadcasts,")
        || body.contains(" tells ye,")
}

/// Parse a leading integer that may contain thousands separators, ignoring any
/// trailing text: `"7,756 pieces of eight"` -> `7756`. Returns `None` if no
/// leading digits are present.
fn parse_num_commas(s: &str) -> Option<u64> {
    let digits: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Split a `[HH:MM:SS] <body>` line into its parsed time and message body.
fn parse_line(line: &str) -> Option<(NaiveTime, &str)> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    let time = NaiveTime::parse_from_str(&rest[.. end], "%H:%M:%S").ok()?;
    Some((time, rest[end + 1 ..].trim_start()))
}

/// Parse the date out of a `====== YYYY/MM/DD ======` header line.
fn parse_date_header(line: &str) -> Option<NaiveDate> {
    let inner = line.trim_matches('=').trim();
    NaiveDate::parse_from_str(inner, "%Y/%m/%d").ok()
}

// ---------------------------------------------------------------------------
// Streaming tailer
// ---------------------------------------------------------------------------

/// Spawn a background thread that tails `path` from `start_offset`, sending
/// each complete (newline-terminated) line over `tx`. Lines are decoded lossily
/// and stripped of trailing CR/LF. A trailing incomplete line is buffered until
/// its newline arrives. Stops when the receiver is dropped.
///
/// This is a detached **std** thread, not a Tokio blocking task: a blocking
/// task looping forever would make the runtime's shutdown (on `main` returning)
/// hang waiting for it. A plain thread is simply abandoned when the process
/// exits.
pub fn spawn_tailer(
    path: std::path::PathBuf,
    start_offset: u64,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    std::thread::spawn(move || {
        use std::io::{Read, Seek, SeekFrom};

        let mut offset = start_offset;
        let mut leftover: Vec<u8> = Vec::new();

        loop {
            let Ok(mut file) = std::fs::File::open(&path) else {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            };
            let len = file.metadata().map(|m| m.len()).unwrap_or(offset);

            // Defensive: if the file shrank (e.g. replaced), restart from 0.
            if len < offset {
                offset = 0;
                leftover.clear();
            }

            if offset < len && file.seek(SeekFrom::Start(offset)).is_ok() {
                let mut buf = Vec::new();
                if let Ok(n) = file.take(len - offset).read_to_end(&mut buf) {
                    offset += n as u64;
                    leftover.extend_from_slice(&buf);

                    // Emit every complete line; keep the remainder.
                    while let Some(pos) =
                        leftover.iter().position(|&b| b == b'\n')
                    {
                        let line_bytes: Vec<u8> =
                            leftover.drain(..= pos).collect();
                        let line = String::from_utf8_lossy(&line_bytes);
                        let line =
                            line.trim_end_matches(['\n', '\r']).to_string();
                        if tx.send(line).is_err() {
                            return; // receiver gone
                        }
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(300));
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pillage(s: &str) -> JobKind {
        JobKind::parse(s)
    }

    #[test]
    fn parses_pillaging_range_and_targets() {
        let JobKind::Pillaging {
            lower,
            upper,
            targets,
        } = pillage("Pillaging, Average to Hard Brigands and Barbarians")
        else {
            panic!("expected pillaging");
        };
        assert_eq!(lower, Difficulty::Average);
        assert_eq!(upper, Difficulty::Hard);
        assert!(!targets.pirates && targets.brigands && targets.barbarians);
    }

    #[test]
    fn parses_single_difficulty() {
        let JobKind::Pillaging {
            lower,
            upper,
            ..
        } = pillage("Pillaging, Average Barbarians")
        else {
            panic!("expected pillaging");
        };
        assert_eq!(lower, Difficulty::Average);
        assert_eq!(upper, Difficulty::Average);
    }

    #[test]
    fn parses_very_hard_and_all_targets() {
        let JobKind::Pillaging {
            upper,
            targets,
            ..
        } = pillage(
            "Pillaging, Easy to Very Hard Pirates and Brigands and Barbarians",
        )
        else {
            panic!("expected pillaging");
        };
        assert_eq!(upper, Difficulty::VeryHard);
        assert!(targets.pirates && targets.brigands && targets.barbarians);
    }

    #[test]
    fn parses_other_job_kinds() {
        assert_eq!(pillage("Evading"), JobKind::Evading);
        assert_eq!(
            pillage("Swabbie Ship Transporting"),
            JobKind::SwabbieTransport
        );
        assert_eq!(
            pillage("Attacking a Flotilla"),
            JobKind::AttackingFlotilla
        );
        // The per-league figure is discarded — we don't track leagues.
        assert_eq!(
            pillage(
                "Trading, offering an average of 10 pieces of eight per league"
            ),
            JobKind::Trading
        );
        assert_eq!(
            pillage("Exploring the Sucker-bearing Destroyer of the Briny Deep"),
            JobKind::Exploring {
                monster: "Sucker-bearing Destroyer of the Briny Deep"
                    .to_string()
            }
        );
    }

    #[test]
    fn tracks_board_crew_and_greedy() {
        let mut gs = GameState::new();
        gs.process_line("[14:55:00] Going aboard the Test Vessel...");
        gs.process_line("[14:56:16] Matetwo has come aboard.");
        gs.process_line(
            "[14:56:24] This vessel is now Pillaging, Average to Hard \
             Barbarians.",
        );
        gs.process_line(
            "[14:56:27] Matethree delivers an overwhelming barrage against \
             Hunched Alice, causing some treasure to fall from their grip!",
        );
        gs.process_line(
            "[14:56:28] Matethree executes a masterful strike against \
             Demented Carlos, who drops some treasure in surprise!",
        );

        assert_eq!(
            gs.current.as_deref(),
            Some("Test Vessel")
        );
        let v = gs.current_vessel().unwrap();
        assert!(v.crewmates.contains("Matetwo"));
        assert_eq!(
            v.greedy_by_pirate.get("Matethree"),
            Some(&2)
        );
        assert!(v.job_kind.is_some());
        assert!(!v.poisoned);
    }

    #[test]
    fn poisons_on_leaving_mid_run() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Sugared Bass...");
        gs.process_line("[01:00:05] This vessel is now Evading.");
        gs.process_line("[01:05:00] Ye have left 'Some Crew'.");
        assert_eq!(gs.current, None);
        let v = &gs.vessels["Sugared Bass"];
        assert!(v.poisoned);
    }

    #[test]
    fn booty_divided_marks_run_divvied() {
        // Port precedes the divvy in the log, so the run is already promoted to
        // `voyages` when the division lands — the flag must reach it there.
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
        gs.process_line(
            "[01:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[01:00:06] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:29:00] Playerone issued an order to put into port.",
        );
        gs.process_line("[01:30:00] The booty has been divided!");
        {
            let v = &gs.vessels["Abyssal Grunion"];
            assert!(v.current_voyage.is_none());
            assert!(v.voyages.last().unwrap().divvied);
        }
        // The divvy also signals the app (once) to freeze the run's booty.
        assert!(gs.take_booty_divided());
        assert!(
            !gs.take_booty_divided(),
            "the signal is one-shot"
        );
    }

    /// A voyage that fights nothing is a voyage all the same. A duty report
    /// starts the run when nothing else has, the port order closes it like
    /// any other, and the reports are what the run has on record.
    #[test]
    fn a_duty_report_is_all_a_run_needs_to_be_one() {
        let mut gs = GameState::new();
        for line in [
            "====== 2026/06/16 ======",
            "[01:00:00] Going aboard the Test Vessel...",
            "[01:00:05] This vessel is now Evading.",
        ] {
            gs.process_line(line);
        }
        let report =
            crate::duty::parse(r#"{"bilge":{"Foo":{"performance":3}}}"#)
                .expect("a report");
        let copied = chrono::Utc::now();
        assert!(gs.note_duty_report(&report, copied));
        // the same report again is the same interval, however it reached us
        assert!(!gs.note_duty_report(
            &report,
            copied + chrono::Duration::minutes(5)
        ));
        gs.process_line(
            "[01:29:00] Playerone issued an order to put into port.",
        );

        let v = &gs.vessels["Test Vessel"];
        assert!(v.current_voyage.is_none());
        let voyage = v.voyages.last().expect("a finished run");
        assert!(voyage.ported_at.is_some());
        assert!(voyage.battles.is_empty());
        assert_eq!(voyage.duty_reports.len(), 1);
        assert_eq!(voyage.duty_reports[0].copied_at, copied);
    }

    #[test]
    fn divvy_without_port_marks_current_voyage() {
        // Defensive path: a divvy with no port order we saw still finalizes and
        // flags the current voyage.
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Sugared Bass...");
        gs.process_line(
            "[01:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[01:00:06] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line("[01:30:00] The booty has been divided!");
        let v = &gs.vessels["Sugared Bass"];
        assert!(v.voyages.last().unwrap().divvied);
    }

    #[test]
    fn clean_run_then_leave_is_not_poisoned() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
        gs.process_line(
            "[01:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line("[01:30:00] The booty has been divided!");
        gs.process_line("[01:31:00] Ye have left 'Some Crew'.");
        let v = &gs.vessels["Abyssal Grunion"];
        assert!(!v.poisoned);
        assert!(v.job_kind.is_none());
    }

    #[test]
    fn re_boarding_keeps_poison() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Enchanting Pike...");
        gs.process_line(
            "[01:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line("[01:05:00] Ye have left 'Crew'."); // poisoned
        gs.process_line("[01:10:00] Going aboard the Enchanting Pike..."); // return
        assert_eq!(
            gs.current.as_deref(),
            Some("Enchanting Pike")
        );
        assert!(gs.current_vessel().unwrap().poisoned);
    }

    #[test]
    fn either_dragoon_boarding_tell_marks_the_encounter() {
        for tell in [
            "Ye hear a splash, and the sound of foreign footsteps.",
            "Dragoons from the monster took advantage of their proximity to \
             board yer vessel!",
        ] {
            let mut gs = GameState::new();
            gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
            gs.process_line(&format!("[01:01:00] {tell}"));
            assert_eq!(
                gs.current_vessel().unwrap().encounter,
                EncounterKind::Atlantis
            );
        }
    }

    #[test]
    fn a_dragoon_driven_off_seats_the_driver_aboard() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
        gs.process_line(
            "[01:01:00] Ye hear a splash, and the sound of foreign footsteps.",
        );
        gs.process_line(
            "[01:01:10] Playerone has driven Bellator from the ship!",
        );
        assert!(gs.current_vessel().unwrap().crewmates.contains("Playerone"));
    }

    #[test]
    fn a_foe_driven_off_outside_atlantis_seats_nobody() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
        // No boarding tell → not an Atlantis encounter, and a bare "driven
        // from the ship" is too weak on its own to seat anyone aboard.
        gs.process_line(
            "[01:01:00] Playerone has driven Jack Irascible from the ship!",
        );
        assert!(gs.current_vessel().unwrap().crewmates.is_empty());
    }

    #[test]
    fn attributes_planks_to_us() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line(
            "[01:00:00] Going aboard the Captiviating Mummichog...",
        );
        gs.process_line("[01:01:00] Matefour has come aboard.");
        gs.process_line(
            "[01:02:00] Playerone forced Matefour to walk the plank.",
        );
        let v = gs.current_vessel().unwrap();
        assert_eq!(
            v.planked_by_us,
            BTreeSet::from(["Matefour".to_string()])
        );
        assert!(!v.crewmates.contains("Matefour"));
    }

    #[test]
    fn relog_wipes_state() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Sugared Bass...");
        gs.process_line("[01:01:00] Yer crew member Mateone has logged on.");
        gs.process_line("====== 2026/05/15 ======");
        assert!(gs.vessels.is_empty());
        assert!(gs.current.is_none());
        assert!(gs.online.is_empty());
        assert_eq!(
            gs.player_name.as_deref(),
            Some("Playerone")
        ); // config survives
    }

    /// Dev tool: ingest a real log via `YPP_LOG=/path cargo test -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn ingest_real_log() {
        let Ok(path) = std::env::var("YPP_LOG") else {
            eprintln!("set YPP_LOG to a chat log path");
            return;
        };
        let data = std::fs::read(&path).expect("read log");
        let mut gs = GameState::new();
        gs.player_name = std::env::var("YPP_USER")
            .ok()
            .map(|u| Arc::from(u.as_str()));
        gs.process_existing(&data);

        eprintln!("vessels seen: {}", gs.vessels.len());
        let mut names: Vec<&Arc<str>> = gs.vessels.keys().collect();
        names.sort_unstable();
        for n in &names {
            let v = &gs.vessels[*n];
            eprintln!(
                "  {:<28} job={:<48} greedy={:<4} crew={:<3} planked_by_us={} \
                 {}",
                n,
                v.job_kind
                    .as_ref()
                    .map(|j| j.to_string())
                    .unwrap_or_default(),
                v.total_greedy(),
                v.crewmates.len(),
                v.planked_by_us.len(),
                if v.poisoned { "[poisoned]" } else { "" },
            );
        }
        eprintln!("current: {:?}", gs.current);
        eprintln!("online now: {}", gs.online.len());
    }

    #[test]
    fn presence_tracking() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Yer crew member Mateone has logged on.");
        gs.process_line("[01:00:01] Yer hearty, Matefive, has logged on.");
        gs.process_line("[01:00:02] Yer crew member Mateone has logged off.");
        assert!(!gs.online.contains("Mateone"));
        assert!(gs.online.contains("Matefive"));
    }

    #[test]
    fn records_board_timestamp_from_header_and_line() {
        let mut gs = GameState::new();
        gs.process_line("====== 2026/06/16 ======");
        gs.process_line("[14:55:00] Going aboard the Test Vessel...");
        let v = &gs.vessels["Test Vessel"];
        let expected = NaiveDate::from_ymd_opt(2026, 6, 16)
            .unwrap()
            .and_hms_opt(14, 55, 0)
            .unwrap();
        assert_eq!(v.boarded_at, Some(expected));
    }

    #[test]
    fn jobbing_boards_a_provisional_vessel() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line(
            "[01:00:00] Ye accepted the offer to job with 'Test Crew'.",
        );
        assert_eq!(
            gs.current.as_deref(),
            Some("Ship of Test Crew")
        );
        assert!(gs.current_vessel().unwrap().provisional);
    }

    #[test]
    fn grapple_promotes_jobbed_vessel_to_its_real_name() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line(
            "[01:00:00] Ye accepted the offer to job with 'Test Crew'.",
        );
        gs.process_line(
            "[01:00:05] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:01:00] You have been intercepted by the War Frigate 'Enemy \
             Boat'!",
        );
        // The battle opened under the provisional vessel.
        assert_eq!(
            gs.current.as_deref(),
            Some("Ship of Test Crew")
        );
        // The grapple names both vessels; the foe is 'Enemy Boat', so ours is
        // 'Our Boat' -> promote.
        gs.process_line(
            "[01:02:00] Enemy Boat has grappled Our Boat. A melee breaks out \
             between the crews!",
        );
        assert_eq!(gs.current.as_deref(), Some("Our Boat"));
        assert!(!gs.vessels.contains_key("Ship of Test Crew"));
        let v = gs.current_vessel().unwrap();
        assert!(!v.provisional);
        // The battle recorded before the promotion moved with the vessel.
        assert!(v.current_voyage.as_ref().unwrap().current_battle.is_some());
    }

    #[test]
    fn jobbing_onto_another_ship_leaves_the_previous() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line(
            "[01:00:00] Ye accepted the offer to job with 'Crew A'.",
        );
        gs.process_line(
            "[01:00:01] This vessel is now Pillaging, Hard Barbarians.",
        );
        gs.process_line(
            "[01:00:05] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:10:00] Ye accepted the offer to job with 'Crew B'.",
        );
        assert_eq!(
            gs.current.as_deref(),
            Some("Ship of Crew B")
        );
        // The mid-run vessel we left is poisoned.
        assert!(gs.vessels["Ship of Crew A"].poisoned);
    }

    #[test]
    fn whisking_home_leaves_the_jobbed_vessel() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line(
            "[01:00:00] Ye accepted the offer to job with 'Crew A'.",
        );
        gs.process_line(
            "[01:00:01] This vessel is now Pillaging, Hard Barbarians.",
        );
        gs.process_line(
            "[01:00:05] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:20:00] Whisking away to yer home on the magical winds.",
        );
        assert!(gs.current.is_none());
        assert!(gs.vessels["Ship of Crew A"].poisoned);
    }

    #[test]
    fn rolls_date_over_midnight_without_a_new_header() {
        let mut gs = GameState::new();
        gs.process_line("====== 2026/06/16 ======");
        // Late-night chatter, then the clock wraps with no new date header.
        gs.process_line("[23:58:37] Harcastle says, \"its a pufferfish\"");
        gs.process_line("[00:00:05] Going aboard the Midnight Tuna...");
        let v = &gs.vessels["Midnight Tuna"];
        let expected = NaiveDate::from_ymd_opt(2026, 6, 17)
            .unwrap()
            .and_hms_opt(0, 0, 5)
            .unwrap();
        assert_eq!(v.boarded_at, Some(expected));
        assert_eq!(
            gs.current_date,
            NaiveDate::from_ymd_opt(2026, 6, 17)
        );
    }

    #[test]
    fn ignores_swabbie_npc_aboard() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Test Tuna...");
        gs.process_line("[01:00:01] A swabbie has come aboard.");
        gs.process_line("[01:00:02] Mateeight has come aboard.");
        let v = gs.current_vessel().unwrap();
        assert!(!v.crewmates.contains("A swabbie"));
        assert!(v.crewmates.contains("Mateeight"));
    }

    #[test]
    fn ignores_named_swabbie_aboard() {
        // Named swabbies (NPCs) carry a space; only real player names are kept.
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Test Tuna...");
        gs.process_line("[01:00:01] Tony Ironsides has come aboard.");
        gs.process_line("[01:00:02] Master Hogan has come aboard.");
        gs.process_line("[01:00:03] Mateeight has come aboard.");
        let v = gs.current_vessel().unwrap();
        assert!(!v.crewmates.contains("Tony Ironsides"));
        assert!(!v.crewmates.contains("Master Hogan"));
        assert!(v.crewmates.contains("Mateeight"));
    }

    #[test]
    fn counts_swabbies_from_deltas_and_saturates() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Swab Tuna...");
        gs.process_line("[01:00:01] A swabbie has come aboard.");
        gs.process_line("[01:00:02] 3 swabbies have come aboard.");
        assert_eq!(gs.current_vessel().unwrap().swabbies, 4);
        gs.process_line("[01:00:03] 2 swabbies have left the vessel.");
        assert_eq!(gs.current_vessel().unwrap().swabbies, 2);
        gs.process_line("[01:00:04] A swabbie has left the vessel.");
        assert_eq!(gs.current_vessel().unwrap().swabbies, 1);
        // Underflow (poison-induced over-counting of departures) saturates at
        // 0.
        gs.process_line("[01:00:05] 5 swabbies have left the vessel.");
        assert_eq!(gs.current_vessel().unwrap().swabbies, 0);
    }

    #[test]
    fn won_fight_resyncs_swabbie_count() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Resync Roughy...");
        // Drift the delta tally away from the truth first.
        gs.process_line("[01:00:01] A swabbie has come aboard.");
        // Winners roster: 1 player (us) + 3 swabbies (named + generic).
        gs.process_line(
            "[01:30:00] Game over.  Winners: Playerone, Tony Ironsides, \
             Master Hogan, A swabbie.",
        );
        assert_eq!(gs.current_vessel().unwrap().swabbies, 3);
    }

    #[test]
    fn greedy_splits_total_and_current_battle() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the War Carp...");
        gs.process_line("[01:00:05] You intercepted the Sloop 'Foo'!");
        gs.process_line(
            "[01:00:06] Mateone delivers an overwhelming barrage against X, \
             who drops treasure!",
        );
        gs.process_line(
            "[01:00:07] Mateone executes a masterful strike against Y, who \
             drops treasure!",
        );
        gs.process_line("[01:05:00] Game over.  Winner: Playerone.");
        // Second battle: current tally resets, but Mateone's run total carries.
        gs.process_line(
            "[01:10:00] You have been intercepted by the Sloop 'Bar'!",
        );
        gs.process_line(
            "[01:10:06] Mateone performs a powerful attack against Z, who \
             drops treasure!",
        );
        let v = gs.current_vessel().unwrap();
        assert_eq!(
            v.greedy_by_pirate.get("Mateone"),
            Some(&3)
        ); // total
        assert_eq!(
            v.greedy_current.get("Mateone"),
            Some(&1)
        ); // current battle
    }

    #[test]
    fn game_over_winners_override_crew_when_player_listed() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Royal Roughy...");
        gs.process_line("[01:00:01] Mateseven has come aboard.");
        gs.process_line(
            "[01:30:00] Game over.  Winners: Mateone, Playerone, Matesix, A \
             swabbie.",
        );
        let v = gs.current_vessel().unwrap();
        // Crew replaced with the winning side (minus us and the swabbie).
        assert!(v.crewmates.contains("Mateone"));
        assert!(v.crewmates.contains("Matesix"));
        assert!(!v.crewmates.contains("Playerone"));
        assert!(!v.crewmates.contains("A swabbie"));
        assert!(!v.crewmates.contains("Mateseven")); // overwritten
    }

    /// A dragoon boards a ship that is still being sailed, so the fray is
    /// whoever was free for it. Those in it are confirmed aboard; those who
    /// were elsewhere on the ship stay aboard, and the NPC count can only be
    /// raised by what the fray showed.
    #[test]
    fn an_atlantis_fray_confirms_who_fought_and_drops_nobody() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        gs.process_line("[01:00:01] Mateseven has come aboard.");
        gs.process_line(
            "[01:01:00] Ye hear a splash, and the sound of foreign footsteps.",
        );
        gs.process_line(
            "[01:02:00] Game over.  Winners: Mateone, Playerone, A swabbie.",
        );
        let v = gs.current_vessel().unwrap();
        // the one who fought is now known to be aboard
        assert!(v.crewmates.contains("Mateone"));
        // and the one who was below decks is still aboard
        assert!(v.crewmates.contains("Mateseven"));
        assert!(!v.crewmates.contains("Playerone"));
        assert!(!v.crewmates.contains("A swabbie"));
        assert_eq!(v.swabbies, 1);

        // a second fray that nobody but us answered takes nothing away
        gs.process_line("[01:10:00] Game over.  Winners: Playerone.");
        let v = gs.current_vessel().unwrap();
        assert!(v.crewmates.contains("Mateone"));
        assert!(v.crewmates.contains("Mateseven"));
        assert_eq!(v.swabbies, 1);
    }

    #[test]
    fn game_over_without_player_leaves_crew_alone() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Royal Roughy...");
        gs.process_line("[01:00:01] Mateseven has come aboard.");
        gs.process_line(
            "[01:30:00] Game over.  Winners: Master Hogan, Brigand Bob.",
        );
        let v = gs.current_vessel().unwrap();
        assert!(v.crewmates.contains("Mateseven")); // untouched — we weren't listed
    }

    #[test]
    fn vampire_lair_tracks_waves_and_flags_leaving() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Thin Tigerfish...");
        gs.process_line("[01:00:01] Matetwo has come aboard.");
        gs.process_line("[01:00:02] Matethree has come aboard.");
        // Enter the lair: wave 1's vampires = pirates aboard (Playerone + 2 =
        // 3).
        gs.process_line("[01:01:00] Welcome to the vampire sanctum. ");
        {
            let v = gs.current_vessel().unwrap();
            assert!(v.lair_active);
            assert_eq!(v.lair_wave, 1);
            assert_eq!(v.lair_pirates, 3);
        }
        // Wave 1: 3 vampires defeated (NPCs have a space); a crew KO must not
        // count.
        gs.process_line("[01:01:10] Stygian Lilith is eliminated!");
        gs.process_line("[01:01:11] Matethree is eliminated!"); // crew KO — ignored
        gs.process_line("[01:01:12] Immortal Schreck is eliminated!");
        gs.process_line("[01:01:13] Craving Silvia is eliminated!");
        // Wave 1's swordfight concludes with a crew win -> advance to wave 2.
        // Wave 1 hit its target (3 = pirates), so no warning yet.
        gs.process_line("[01:02:00] Game over.  Winners: Playerone, Matetwo.");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.lair_wave, 2);
            assert_eq!(v.vampires_defeated, 3);
            assert!(!v.lair_warn);
        }
        // Wave 2 should hold ~4 vampires, but we leave the fight and see only
        // 1...
        gs.process_line("[01:02:10] Sunless Collins is eliminated!");
        // ...then lose the next swordfight (winners all vampires), ending the
        // lair.
        gs.process_line(
            "[01:03:00] Game over.  Winners: Revenant Drac, Gloaming Lucy.",
        );
        let v = gs.current_vessel().unwrap();
        assert!(!v.lair_active);
        assert_eq!(v.lair_wave, 2);
        assert_eq!(v.vampires_defeated, 4);
        assert!(v.lair_warn); // wave 2 fell short of its projection -> we left
    }

    #[test]
    fn cursed_isles_tell_marks_encounter_and_fires_jump_once() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Cursed Tuna...");
        // The noxious fog is the tell: it marks the encounter and requests the
        // jump.
        gs.process_line(
            "[01:00:10] The crew inhales the noxious fog, and starts to lose \
             fine motor control.",
        );
        assert!(gs.take_cursed_isles_detected());
        assert_eq!(
            gs.current_vessel().unwrap().encounter,
            EncounterKind::CursedIsles
        );
        // A second fog line must not re-fire the jump (already a known CI run).
        gs.process_line(
            "[01:00:20] The crew inhales the noxious fog, and starts to lose \
             fine motor control.",
        );
        assert!(!gs.take_cursed_isles_detected());
    }

    #[test]
    fn cursed_isles_counts_zombies_and_thralls() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Cursed Tuna...");
        // Four rafts board us with zombies.
        for _ in 0 .. 4 {
            gs.process_line(
                "[01:00:01] Boarders from the raft clamber onto yer vessel as \
                 theirs sinks to the depths.",
            );
        }
        assert_eq!(
            gs.current_vessel().unwrap().zombies_aboard,
            4
        );
        // Two are enthralled (leave the hostile count, join their controllers).
        gs.process_line("[01:00:02] Playerone has taken control of a zombie.");
        gs.process_line("[01:00:03] Matetwo has taken control of a zombie.");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.zombies_aboard, 2);
            assert_eq!(
                v.thralls_alive.get("Playerone"),
                Some(&1)
            );
            assert_eq!(
                v.thralls_total.get("Playerone"),
                Some(&1)
            );
            assert_eq!(v.thralls_alive.get("Matetwo"), Some(&1));
            // Enthralling proves a pirate is aboard: Matetwo joins the crew,
            // but we (Playerone) are never in the crewmate set.
            assert!(v.crewmates.contains("Matetwo"));
            assert!(!v.crewmates.contains("Playerone"));
        }
        // One zombie is driven back off the ship.
        gs.process_line(
            "[01:00:04] Playerone has driven Controlled Zombie from the ship!",
        );
        assert_eq!(
            gs.current_vessel().unwrap().zombies_aboard,
            1
        );
        // Playerone's thrall dies: the live count drops, the lifetime total
        // holds.
        gs.process_line("[01:00:05] Playerone's Thrall is eliminated!");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(
                v.thralls_alive.get("Playerone"),
                Some(&0)
            );
            assert_eq!(
                v.thralls_total.get("Playerone"),
                Some(&1)
            );
        }
    }

    #[test]
    fn cursed_isles_island_waves_count_enemies_and_classify() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Cursed Tuna...");
        gs.process_line("[01:00:01] Matetwo has come aboard.");
        gs.process_line("[01:00:02] Matethree has come aboard.");
        gs.process_line(
            "[01:00:03] The crew inhales the noxious fog, and starts to lose \
             fine motor control.",
        );
        let _ = gs.take_cursed_isles_detected();
        // Land on the island: wave 1 opens, anchored on the pirates aboard (3).
        gs.process_line(
            "[01:05:00] Ye land on the island, but an angry mob of its \
             inhabitants stands between ye and yer rightful plunderin'!",
        );
        {
            let v = gs.current_vessel().unwrap();
            assert!(v.island_active);
            assert_eq!(v.island_wave, 1);
            assert_eq!(v.island_pirates, 3);
            assert_eq!(v.wave_kind, WaveKind::Rumble); // wave 1 is always a Rumble
        }
        // Wave 1 is a rumble (zombies). A crew KO and a thrall KO are NOT
        // enemies; the four zombies meet the projected band, so no
        // "left early" flag is raised. `Matefive` is seen only via this
        // KO — it proves they're aboard.
        gs.process_line("[01:05:10] Servile Zombie is eliminated!");
        gs.process_line("[01:05:11] Matefive is eliminated!"); // crew KO -> proof of presence
        gs.process_line("[01:05:12] Playerone's Thrall is eliminated!"); // thrall
        gs.process_line("[01:05:13] Enlightened One is eliminated!");
        gs.process_line("[01:05:14] Cursed Zombie is eliminated!");
        gs.process_line("[01:05:15] Mindless Zombie is eliminated!");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.wave_enemies_observed, 4);
            assert_eq!(v.wave_kind, WaveKind::Rumble);
            assert!(v.crewmates.contains("Matefive")); // KO proved them aboard
        }
        // Clearing wave 1 advances to wave 2. The winners list is an
        // authoritative roster: `Matefour` (seen only here) joins the
        // crew, while our thralls ("<p>'s Thrall", which contain
        // spaces) must NOT be miscounted as swabbies — the sea-battle
        // crew-resync is bypassed on a CI run.
        gs.process_line(
            "[01:06:00] Game over.  Winners: Playerone, Matetwo, Matefour, \
             Playerone's Thrall.",
        );
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.island_wave, 2);
            assert_eq!(v.wave_enemies_observed, 0);
            assert_eq!(v.wave_kind, WaveKind::Swordfight); // wave 2 alternates to Swordfight
            assert!(!v.island_left_warn); // wave 1 met its band
            assert_eq!(v.swabbies, 0); // thralls not miscounted as swabbies
            assert!(v.crewmates.contains("Matefour")); // winners list proved them aboard
            assert!(!v.crewmates.contains("Playerone's Thrall"));
        }
        // Wave 2 is a swordfight (cultists / homunculi), and we under-count it
        // (we leave early): only 2 kills against a projected band that
        // floors higher.
        gs.process_line("[01:06:10] Berserk Cultist is eliminated!");
        gs.process_line("[01:06:11] Foaming Homunculus is eliminated!");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.wave_kind, WaveKind::Swordfight);
            assert!(v.wave_enemies_observed < v.wave_enemies_lo); // short of the band
        }
        gs.process_line("[01:07:00] Game over.  Winners: Playerone, Matetwo.");
        {
            let v = gs.current_vessel().unwrap();
            assert!(v.island_left_warn); // we left wave 2 early
            assert_eq!(v.island_wave, 3);
            assert_eq!(v.wave_kind, WaveKind::Rumble); // back to a rumble
        }
    }

    #[test]
    fn vargas_only_on_rumble_waves_five_plus() {
        assert!(!vargas_in_wave(0));
        assert!(!vargas_in_wave(1)); // rumble, but before wave 5
        assert!(!vargas_in_wave(3));
        assert!(!vargas_in_wave(4)); // before wave 5 (and a swordfight)
        assert!(vargas_in_wave(5)); // rumble, wave 5
        assert!(!vargas_in_wave(6)); // swordfight
        assert!(vargas_in_wave(7)); // rumble, wave 7
        assert!(!vargas_in_wave(8)); // swordfight
    }

    #[test]
    fn island_wave_band_grows_with_wave() {
        let (lo1, hi1) = island_wave_band(6, 1);
        let (lo2, hi2) = island_wave_band(6, 2);
        assert!(lo1 <= hi1 && lo2 <= hi2);
        assert!(lo2 >= lo1 && hi2 >= hi1); // later waves project at least as large
    }

    #[test]
    fn new_run_resets_cursed_isles_state() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Cursed Tuna...");
        gs.process_line(
            "[01:00:01] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:00:02] The crew inhales the noxious fog, and starts to lose \
             fine motor control.",
        );
        let _ = gs.take_cursed_isles_detected();
        gs.process_line(
            "[01:00:03] Boarders from the raft clamber onto yer vessel as \
             theirs sinks to the depths.",
        );
        assert_eq!(
            gs.current_vessel().unwrap().zombies_aboard,
            1
        );
        // End the run and start a fresh one: the CI state clears so a later
        // pillage on this vessel isn't treated as Cursed Isles (and the
        // fog re-arms the jump).
        gs.process_line(
            "[01:10:00] Playerone issued an order to put into port.",
        );
        gs.process_line(
            "[02:00:00] Playerone issued an order to set the vessel to sail.",
        );
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.encounter, EncounterKind::None);
        assert_eq!(v.zombies_aboard, 0);
    }

    #[test]
    fn cursed_isles_wave_records_capture_advantage_timeline() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Cursed Tuna...");
        gs.process_line("[01:00:01] Matetwo has come aboard.");
        gs.process_line("[01:00:02] Matethree has come aboard.");
        gs.process_line(
            "[01:05:00] Ye land on the island, but an angry mob of its \
             inhabitants stands between ye and yer rightful plunderin'!",
        );
        // Wave 1: three enemy KOs (advantage steps up) interleaved with one
        // crew KO (steps down). A thrall death is excluded from the
        // curve entirely.
        gs.process_line("[01:05:10] Servile Zombie is eliminated!");
        gs.process_line("[01:05:20] Cursed Zombie is eliminated!");
        gs.process_line("[01:05:30] Matethree is eliminated!"); // our crew falls
        gs.process_line("[01:05:35] Playerone's Thrall is eliminated!"); // excluded
        gs.process_line("[01:05:40] Mindless Zombie is eliminated!");
        gs.process_line("[01:06:00] Game over.  Winners: Playerone, Matetwo.");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.island_waves.len(), 1);
        let w = &v.island_waves[0];
        assert_eq!(w.wave, 1);
        assert_eq!(w.kind, WaveKind::Rumble);
        // our_start = pirates at landing (3); their_start = enemies seen this
        // wave (3).
        assert_eq!(w.timeline.our_start, 3);
        assert_eq!(w.timeline.their_start, Some(3));
        let sides: Vec<KoSide> =
            w.timeline.events.iter().map(|e| e.side).collect();
        assert_eq!(
            sides,
            vec![KoSide::Theirs, KoSide::Theirs, KoSide::Ours, KoSide::Theirs]
        );
        // base = our_start − their_start = 0; curve steps +1,+1,−1,+1.
        let series =
            w.timeline.advantage_series(crate::voyage::AxisMode::Event);
        let vals: Vec<i32> = series.iter().map(|&(_, v)| v).collect();
        assert_eq!(vals, vec![0, 1, 2, 1, 2]);
    }

    #[test]
    fn sea_battle_timeline_backfills_sides_at_resolution() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line("[02:00:00] Going aboard the War Carp...");
        gs.process_line("[02:00:01] Matetwo has come aboard.");
        gs.process_line(
            "[02:00:05] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[02:01:00] You have been intercepted by the War Frigate 'Modest \
             Sild'!",
        );
        gs.process_line(
            "[02:02:00] Modest Sild has grappled War Carp. A melee breaks out \
             between the crews!",
        );
        // Eliminations in order: enemy, our crewmate, enemy.
        gs.process_line("[02:02:10] Brawny Brigand is eliminated!");
        gs.process_line("[02:02:20] Matetwo is eliminated!");
        gs.process_line("[02:02:30] Grizzled Brigand is eliminated!");
        gs.process_line("[02:03:00] Game over.  Winners: Playerone, Matetwo.");
        let v = gs.current_vessel().unwrap();
        let b = v.current_voyage.as_ref().unwrap().battles.last().unwrap();
        // Sides backfilled from our roster: enemy NPCs Theirs, our Matetwo
        // Ours.
        let sides: Vec<KoSide> =
            b.timeline.events.iter().map(|e| e.side).collect();
        assert_eq!(
            sides,
            vec![KoSide::Theirs, KoSide::Ours, KoSide::Theirs]
        );
        // We won, so every enemy was eliminated → their_start is the enemy-KO
        // count.
        assert_eq!(b.timeline.their_start, Some(2));
        // Time axis: events at +10/+20/+30s from the grapple.
        let series = b.timeline.advantage_series(crate::voyage::AxisMode::Time);
        let xs: Vec<f64> = series.iter().map(|&(x, _)| x).collect();
        assert_eq!(xs, vec![0.0, 10.0, 20.0, 30.0]);
    }

    #[test]
    fn parse_foe_vessel_reads_hull_and_strips_name() {
        let galleon = crate::ships::ship_index("Merchant Galleon");
        assert_eq!(
            parse_foe_vessel("Merchant Galleon 'Foo'"),
            (galleon, "Foo"),
        );
        // A bare brigand/monster name has no hull and is its own name.
        assert_eq!(
            parse_foe_vessel("Modest Sild"),
            (None, "Modest Sild")
        );
        // An unknown hull word isn't mistaken for a ship — kept whole.
        assert_eq!(
            parse_foe_vessel("Rowboat 'Bar'"),
            (None, "Rowboat 'Bar'"),
        );
    }

    #[test]
    fn intercept_named_hull_seeds_foe_ship_and_display_name() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line("[02:00:00] Going aboard the War Carp...");
        gs.process_line(
            "[02:00:05] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[02:01:00] You have been intercepted by the Xebec 'Foo'!",
        );
        // The Damage calculator is told the foe hull for the app to pick up.
        assert_eq!(
            gs.detected_foe_ship,
            crate::ships::ship_index("Xebec")
        );
        let v = gs.current_vessel().unwrap();
        let b = v
            .current_voyage
            .as_ref()
            .unwrap()
            .current_battle
            .as_ref()
            .unwrap();
        assert_eq!(
            b.foe_ship,
            crate::ships::ship_index("Xebec")
        );
        // The stored enemy name drops the hull, keeping just the vessel's name.
        assert_eq!(b.enemy.as_deref(), Some("Foo"));
        // A named-hull foe is not a monkey boat.
        assert_ne!(b.category, BattleCategory::MonkeyBoat);
    }

    #[test]
    fn sea_first_elimination_triggers_fight_jump_once() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line("[02:00:00] Going aboard the War Carp...");
        gs.process_line(
            "[02:00:05] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[02:01:00] You have been intercepted by the War Frigate 'Modest \
             Sild'!",
        );
        gs.process_line(
            "[02:02:00] Modest Sild has grappled War Carp. A melee breaks out \
             between the crews!",
        );
        // The first melee KO of the grappled fight surfaces the sea-battle
        // graph.
        gs.process_line("[02:02:10] Brawny Brigand is eliminated!");
        assert!(gs.take_battle_first_blood());
        // A later KO must not re-fire (one jump per fight).
        gs.process_line("[02:02:20] Grizzled Brigand is eliminated!");
        assert!(!gs.take_battle_first_blood());
    }

    #[test]
    fn cursed_isles_raft_phase_eliminations_not_counted() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Cursed Tuna...");
        gs.process_line(
            "[01:00:03] The crew inhales the noxious fog, and starts to lose \
             fine motor control.",
        );
        let _ = gs.take_cursed_isles_detected();
        // A challenging-zombie duel KO during the raft phase is NOT an island
        // enemy: it isn't recorded onto any timeline and doesn't fire
        // the sea-battle jump (which is reserved for grappled pillage
        // fights).
        gs.process_line("[01:01:00] Challenging Zombie is eliminated!");
        assert!(!gs.take_battle_first_blood());
        assert_eq!(
            gs.current_vessel().unwrap().wave_timeline.events.len(),
            0
        );
        // After landing, island eliminations ARE counted onto the wave
        // timeline.
        gs.process_line(
            "[01:05:00] Ye land on the island, but an angry mob of its \
             inhabitants stands between ye and yer rightful plunderin'!",
        );
        gs.process_line("[01:05:10] Servile Zombie is eliminated!");
        assert_eq!(
            gs.current_vessel().unwrap().wave_timeline.events.len(),
            1
        );
        // The island assault never fires the sea-battle jump.
        assert!(!gs.take_battle_first_blood());
    }

    #[test]
    fn vessels_ordered_latest_boarded_first() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the First Fish...");
        gs.process_line("[02:00:00] Going aboard the Second Fish...");
        gs.process_line("[03:00:00] Going aboard the Third Fish...");
        // Re-boarding the first should float it back to the top.
        gs.process_line("[04:00:00] Going aboard the First Fish...");
        let order = gs.vessels_by_recency();
        let order: Vec<&str> = order.iter().map(|a| a.as_ref()).collect();
        assert_eq!(
            order,
            vec!["First Fish", "Third Fish", "Second Fish"]
        );
    }

    #[test]
    fn records_voyage_battle_and_loot() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line("[02:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[02:00:05] This vessel is now Pillaging, Average to Hard \
             Barbarians.",
        );
        gs.process_line(
            "[02:00:10] Playerone issued an order to set the vessel to sail.",
        );
        // A later move order must NOT start a second voyage.
        gs.process_line(
            "[02:05:00] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[02:06:06] You have been intercepted by the War Frigate 'Modest \
             Sild'!",
        );
        gs.process_line(
            "[02:09:51] Modest Sild has grappled Test Vessel. A melee breaks \
             out between the crews!",
        );
        gs.process_line(
            "[02:14:43] Game over.  Winners: Matetwo, Playerone, A swabbie.",
        );
        gs.process_line(
            "[02:15:02] The victors plundered 7,756 pieces of eight and 9 \
             units of goods from the defeated vessel.",
        );
        gs.process_line(
            "[02:15:02] Ye received 576 pieces of eight as your initial cut \
             of the booty!",
        );
        gs.process_line(
            "[02:20:00] Playerone issued an order to put into port.",
        );

        let v = &gs.vessels["Test Vessel"];
        assert!(v.current_voyage.is_none()); // promoted to completed at port
        assert_eq!(v.voyages.len(), 1);
        let voy = &v.voyages[0];
        assert_eq!(voy.duration_secs(), Some(1190)); // 02:00:10 -> 02:20:00
        assert_eq!(voy.battles.len(), 1);
        let b = &voy.battles[0];
        assert_eq!(b.enemy.as_deref(), Some("Modest Sild"));
        assert_eq!(b.outcome, BattleOutcome::Won);
        assert_eq!(b.sea_secs(), Some(225)); // 02:06:06 -> 02:09:51
        assert_eq!(b.boarding_secs(), Some(292)); // 02:09:51 -> 02:14:43
        assert_eq!(b.total_secs(), Some(517));
        assert_eq!(b.poe, Some(7_756));
        assert_eq!(b.goods, Some(9));
        assert_eq!(b.my_cut, Some(576));
        assert_eq!(b.pirates, 2); // Matetwo + us
        assert_eq!(b.swabbies, 1); // A swabbie
    }

    #[test]
    fn voyages_get_stable_distinct_ids() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line("[02:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[02:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[02:00:10] Playerone issued an order to set the vessel to sail.",
        );
        let live_id = gs.vessels["Test Vessel"]
            .current_voyage
            .as_ref()
            .unwrap()
            .id;
        assert_ne!(
            live_id, 0,
            "a real voyage gets a nonzero id"
        );

        // The id survives promotion from `current_voyage` into `voyages` at
        // port.
        gs.process_line(
            "[02:20:00] Playerone issued an order to put into port.",
        );
        let v = &gs.vessels["Test Vessel"];
        assert!(v.current_voyage.is_none());
        assert_eq!(v.voyages[0].id, live_id);

        // A second run on the same vessel gets a fresh, distinct id.
        gs.process_line(
            "[02:30:00] Playerone issued an order to set the vessel to sail.",
        );
        let second_id = gs.vessels["Test Vessel"]
            .current_voyage
            .as_ref()
            .unwrap()
            .id;
        assert_ne!(second_id, live_id);
    }

    #[test]
    fn manpower_counts_leaver_excludes_disconnect() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[02:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[02:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[02:00:10] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line("[02:00:20] Mateleaver has come aboard.");
        gs.process_line("[02:00:21] Matedrop has come aboard.");
        gs.process_line("[02:00:22] Matefighter has come aboard.");
        gs.process_line(
            "[02:01:00] You intercepted the War Frigate 'Modest Sild'!",
        );
        gs.process_line(
            "[02:02:00] Test Vessel has grappled Modest Sild. A melee breaks \
             out between the crews!",
        );
        // Matedrop's client drops and never returns; Mateleaver bails
        // mid-melee.
        gs.process_line("[02:02:05] Matedrop has disconnected.");
        gs.process_line("[02:02:30] Mateleaver has left the vessel.");
        gs.process_line("[02:03:00] Grim Bart is eliminated!");
        // We win. Matedrop is still rostered (a disconnect isn't a departure),
        // so he's in the winners; Mateleaver isn't (he left).
        gs.process_line(
            "[02:04:00] Game over.  Winners: Playerone, Matefighter, \
             Matedrop, A swabbie.",
        );
        let b = gs.current_voyage().unwrap().battles.last().unwrap();
        assert_eq!(b.outcome, BattleOutcome::Won);
        // Fought = Mateleaver (left, but fought) + Matefighter + us = 3.
        // Matedrop is excluded — disconnected and never reconnected,
        // even though he's a winner.
        assert_eq!(b.pirates, 3);
        assert_eq!(b.swabbies, 1);
    }

    #[test]
    fn manpower_counts_reconnected_dropout() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[02:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[02:00:10] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line("[02:00:20] Matedrop has come aboard.");
        gs.process_line(
            "[02:01:00] You intercepted the War Frigate 'Modest Sild'!",
        );
        gs.process_line(
            "[02:02:00] Modest Sild has grappled Test Vessel. A melee breaks \
             out between the crews!",
        );
        gs.process_line("[02:02:05] Matedrop has disconnected.");
        gs.process_line("[02:02:10] Matedrop has reconnected.");
        gs.process_line("[02:04:00] Game over.  Winners: Playerone, Matedrop.");
        let b = gs.current_voyage().unwrap().battles.last().unwrap();
        // Matedrop came back and fought, so he counts: Matedrop + us = 2.
        assert_eq!(b.pirates, 2);
    }

    #[test]
    fn pvp_detected_from_enemy_player_elimination() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[03:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[03:00:10] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[03:01:00] You intercepted the War Frigate 'Bloody Nightmare'!",
        );
        gs.process_line(
            "[03:02:00] Test Vessel has grappled Bloody Nightmare. A melee \
             breaks out between the crews!",
        );
        gs.process_line("[03:02:30] Enemyone is eliminated!"); // single-word, not our crew
        gs.process_line("[03:02:40] Sea Lawyer is eliminated!"); // NPC mercenary (has a space)
        gs.process_line("[03:04:00] Game over.  Winners: Playerone.");
        let b = gs.current_voyage().unwrap().battles.last().unwrap();
        // an enemy real player was eliminated → PvP, its own category
        assert_eq!(b.category, BattleCategory::Pvp);
    }

    #[test]
    fn pvp_detected_from_winners_on_loss() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[04:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[04:00:10] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[04:01:00] You have been intercepted by the War Frigate 'Bloody \
             Nightmare'!",
        );
        gs.process_line(
            "[04:02:00] Bloody Nightmare has grappled Test Vessel. A melee \
             breaks out between the crews!",
        );
        // We lose with no enemy KO'd — only the winners list reveals the foe
        // players.
        gs.process_line(
            "[04:04:00] Game over.  Winners: Enemyone, Enemytwo, Deck Swab.",
        );
        let b = gs.current_voyage().unwrap().battles.last().unwrap();
        assert_eq!(b.outcome, BattleOutcome::Lost);
        assert_eq!(b.category, BattleCategory::Pvp);
    }

    #[test]
    fn records_loss_with_negative_poe() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[00:20:00] Going aboard the Boring Gar...");
        gs.process_line(
            "[00:20:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[00:20:49] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[00:20:49] You have been intercepted by the War Frigate 'Boring \
             Gar'!",
        );
        gs.process_line(
            "[00:33:03] Game over.  Winners: Nervy Hugh, Insane Yang.",
        );
        gs.process_line(
            "[00:33:11] The victors plundered 27,460 pieces of eight and 350 \
             units of goods from the defeated vessel.",
        );
        let voy = gs.current_voyage().unwrap();
        let b = voy.battles.last().unwrap();
        assert_eq!(b.outcome, BattleOutcome::Lost);
        assert_eq!(b.poe, Some(-27_460)); // we lost it to them
        assert_eq!(b.goods, Some(350));
    }

    #[test]
    fn outcome_unknown_until_identity_confirmed_then_retroactive() {
        use crate::voyage::effective_outcome;
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        // A bare fight with no confirming signal yet (no order/elimination,
        // and we're not among the winners).
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[01:01:00] You have been intercepted by the War Frigate 'Boring \
             Gar'!",
        );
        gs.process_line(
            "[01:04:00] Game over.  Winners: Nervy Hugh, Insane Yang.",
        );
        assert!(!gs.self_confirmed);
        let raw = gs.current_voyage().unwrap().battles.last().unwrap().outcome;
        assert_eq!(raw, BattleOutcome::Lost); // provisional verdict, stored
        assert_eq!(
            effective_outcome(raw, gs.self_confirmed),
            BattleOutcome::Unknown
        );
        // A game-generated order line much later confirms us — the earlier
        // fight is revealed.
        gs.process_line(
            "[01:05:00] Playerone issued an order to set the vessel to sail.",
        );
        assert!(gs.self_confirmed);
        let raw = gs.current_voyage().unwrap().battles.last().unwrap().outcome;
        assert_eq!(
            effective_outcome(raw, gs.self_confirmed),
            BattleOutcome::Lost
        );
    }

    #[test]
    fn no_configured_name_is_always_unknown() {
        let mut gs = GameState::new(); // no player_name
        gs.process_line("[02:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[02:01:00] You have been intercepted by the War Frigate 'Boring \
             Gar'!",
        );
        gs.process_line(
            "[02:04:00] Game over.  Winners: Nervy Hugh, Insane Yang.",
        );
        gs.process_line(
            "[02:04:11] The victors plundered 9,000 pieces of eight and 3 \
             units of goods from the defeated vessel.",
        );
        assert!(!gs.self_confirmed);
        let b = gs.current_voyage().unwrap().battles.last().unwrap();
        assert_eq!(b.outcome, BattleOutcome::Unknown);
        assert_eq!(b.poe, None); // direction unknowable
    }

    #[test]
    fn intercept_then_disengage_is_disengaged() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[23:27:00] Going aboard the Sea Lord...");
        gs.process_line(
            "[23:27:01] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[23:27:02] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[23:27:23] You have been intercepted by the War Frigate 'Lucky \
             Mackerel'!",
        );
        gs.process_line(
            "[23:28:00] Lucky Mackerel disengaged from the battle.",
        );
        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles.len(), 1);
        assert_eq!(
            voy.battles[0].outcome,
            BattleOutcome::Disengaged
        );
        assert!(voy.battles[0].grappled_at.is_none()); // never boarded
        assert!(voy.current_battle.is_none());
    }

    #[test]
    fn stale_pursuit_ended_line_does_not_disengage_active_fight() {
        // A cancelled/expired pursuit ("Arr, ye can no longer pursue ...")
        // names a different vessel than the one we're fighting. It must
        // NOT end the open battle, which then goes on to a clean
        // boarding win.
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/06/23 ======");
        gs.process_line("[02:33:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[02:33:01] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[02:33:02] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[02:33:23] You intercepted the War Frigate 'Hot Barbel'!",
        );
        // Stale pursuit of an unrelated target ends — must be ignored.
        gs.process_line(
            "[02:33:33] Arr, ye can no longer pursue the Thieving \
             Stickleback: That vessel has put into port.",
        );
        gs.process_line(
            "[02:39:29] Test Vessel has grappled Hot Barbel. A melee breaks \
             out between the crews!",
        );
        gs.process_line("[02:42:43] Game over.  Winners: Playerone, Mashtag.");
        gs.process_line(
            "[02:42:58] The victors plundered 3,207 pieces of eight and 15 \
             units of goods from the defeated vessel.",
        );
        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles.len(), 1);
        assert_eq!(
            voy.battles[0].outcome,
            BattleOutcome::Won
        );
        assert_eq!(voy.battles[0].poe, Some(3_207));
        assert!(voy.battles[0].grappled_at.is_some());
        // The chest keeps the rounded-up half of the win.
        assert_eq!(
            gs.current_pillage_poe(),
            (3_207, 0, 1_604)
        );
    }

    #[test]
    fn theft_is_capped_by_the_chest_balance() {
        // Enemies can't plunder more PoE than the chest holds at the time. Here
        // a loss comes first (empty chest → nothing to steal), then a
        // win, then a loss that "plunders" far more than the chest's
        // worth — capped to it.
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[00:00:00] Going aboard the Brave Marlin...");
        gs.process_line(
            "[00:00:01] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[00:00:02] Playerone issued an order to set the vessel to sail.",
        );
        // Lose with an empty chest — they can't take 9,000 from nothing.
        gs.process_line(
            "[00:01:00] You have been intercepted by the War Frigate 'Brigand \
             One'!",
        );
        gs.process_line(
            "[00:01:30] Brigand One has grappled Brave Marlin. A melee breaks \
             out between the crews!",
        );
        gs.process_line("[00:01:40] Game over.  Winners: Raider Onecrew.");
        gs.process_line(
            "[00:01:41] The victors plundered 9,000 pieces of eight and no \
             goods from the defeated vessel.",
        );
        // Win: chest gets ceil(2000/2) = 1,000.
        gs.process_line(
            "[00:02:00] You intercepted the War Frigate 'Brigand Two'!",
        );
        gs.process_line(
            "[00:02:30] Brave Marlin has grappled Brigand Two. A melee breaks \
             out between the crews!",
        );
        gs.process_line("[00:02:40] Game over.  Winners: Playerone.");
        gs.process_line(
            "[00:02:41] The victors plundered 2,000 pieces of eight and no \
             goods from the defeated vessel.",
        );
        // Lose again: they "plunder" 5,000, but the chest only holds 1,000.
        gs.process_line(
            "[00:03:00] You have been intercepted by the War Frigate 'Brigand \
             Three'!",
        );
        gs.process_line(
            "[00:03:30] Brigand Three has grappled Brave Marlin. A melee \
             breaks out between the crews!",
        );
        gs.process_line("[00:03:40] Game over.  Winners: Raider Twocrew.");
        gs.process_line(
            "[00:03:41] The victors plundered 5,000 pieces of eight and no \
             goods from the defeated vessel.",
        );
        // gross 2000, chest 1000, stolen capped at 1000 (the empty-chest loss
        // took 0, the over-cap loss took only the 1000 on hand). Net
        // chest = 0.
        assert_eq!(
            gs.current_pillage_poe(),
            (2_000, 1_000, 1_000)
        );
    }

    #[test]
    fn back_to_back_fights_both_recorded() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[00:20:00] Going aboard the War Frigate...");
        gs.process_line(
            "[00:20:01] This vessel is now Pillaging, Hard Barbarians.",
        );
        gs.process_line(
            "[00:20:02] Playerone issued an order to set the vessel to sail.",
        );
        // Lose to the Boring Gar...
        gs.process_line(
            "[00:20:49] You have been intercepted by the War Frigate 'Boring \
             Gar'!",
        );
        gs.process_line(
            "[00:33:03] Game over.  Winners: Nervy Hugh, Insane Yang.",
        );
        gs.process_line(
            "[00:33:11] The victors plundered 27,460 pieces of eight and no \
             goods from the defeated vessel.",
        );
        // ...then immediately re-engage the same vessel and win.
        gs.process_line(
            "[00:33:25] You intercepted the War Frigate 'Boring Gar'!",
        );
        gs.process_line("[00:44:00] Game over.  Winners: Playerone.");
        gs.process_line(
            "[00:44:01] The victors plundered 5,000 pieces of eight and 2 \
             units of goods from the defeated vessel.",
        );
        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles.len(), 2); // both kept (no confirm-window data loss)
        assert_eq!(
            voy.battles[0].outcome,
            BattleOutcome::Lost
        );
        assert_eq!(voy.battles[0].poe, Some(-27_460));
        assert_eq!(voy.battles[0].goods, Some(0));
        assert_eq!(
            voy.battles[1].outcome,
            BattleOutcome::Won
        );
        assert_eq!(voy.battles[1].poe, Some(5_000));
    }

    #[test]
    fn categorizes_brigand_king_and_defaults_to_brigand() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the War Frigate...");
        gs.process_line(
            "[01:00:01] This vessel is now Pillaging, Hard Barbarians.",
        );
        gs.process_line(
            "[01:00:02] Playerone issued an order to set the vessel to sail.",
        );
        // An ordinary fight -> generic Brigand.
        gs.process_line(
            "[01:00:10] You intercepted the War Frigate 'Fat Mackerel'!",
        );
        gs.process_line("[01:02:00] Game over.  Winners: Playerone.");
        // A king fight: engagement chant tags it, victory line confirms the
        // name.
        gs.process_line(
            "[01:03:00] You have been intercepted by the War Frigate 'Simple \
             Ling'!",
        );
        gs.process_line(
            "[01:03:01] Brace yourself! Vargas the Mad and his barbaric horde \
             are looking for a rumble!",
        );
        gs.process_line("[01:08:00] Game over.  Winners: Playerone.");
        gs.process_line(
            "[01:08:01] Vargas the Mad's ship disappears into the mists.",
        );

        let voy = gs.current_voyage().unwrap();
        assert_eq!(
            voy.battles[0].category,
            BattleCategory::Brigand
        );
        assert_eq!(
            voy.battles[1].category,
            BattleCategory::BrigandKing("Vargas the Mad".to_string())
        );
    }

    #[test]
    fn categorizes_vampirate_and_werewolf_heralds() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the War Frigate...");
        gs.process_line(
            "[01:00:01] This vessel is now Pillaging, Hard Barbarians.",
        );
        gs.process_line(
            "[01:00:02] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:00:10] You intercepted the War Frigate 'Bloodstained \
             Tigerfish'!",
        );
        gs.process_line(
            "[01:00:11] Avast! Yer blood runs cold beneath a gathering gloom \
             and the air is a-flutter with leathern wings! Guard yer throat, \
             a Vampirate vessel closes in!",
        );
        gs.process_line("[01:02:00] Game over.  Winners: Playerone.");
        gs.process_line(
            "[01:03:00] You intercepted the War Frigate 'Snarling Pike'!",
        );
        gs.process_line(
            "[01:03:01] Unearthly howling echos o'er the waves, moonlight \
             glints off curving fangs and hungry eyes watch ye from the dark! \
             Beware! Werewolves have caught yer scent!",
        );
        gs.process_line("[01:05:00] Game over.  Winners: Playerone.");

        let voy = gs.current_voyage().unwrap();
        assert_eq!(
            voy.battles[0].category,
            BattleCategory::Vampirate
        );
        assert_eq!(
            voy.battles[1].category,
            BattleCategory::Werewolf
        );
    }

    #[test]
    fn black_ship_herald_tags_grand_frigate_foe() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the War Frigate...");
        gs.process_line(
            "[01:00:01] This vessel is now Pillaging, Hard Barbarians.",
        );
        gs.process_line(
            "[01:00:02] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:00:10] You intercepted the War Frigate 'Snarling Pike'!",
        );
        // The Black Ship herald replaces the target: the foe becomes a Grand
        // Frigate and the fight is tagged BlackShip. Matched in full — an exact
        // copy of the in-game line (verified against yppedia).
        gs.process_line(
            "[01:00:11] Dark clouds gather as ye bear down upon yer hapless \
             victims, and from the miasma emerges the Black Ship to take the \
             place of yer target in battle! Arrrrgh! Ye be doomed fer sure!",
        );
        let grand = crate::ships::ship_index("Grand Frigate");
        // Seeds the live Damage calculator's foe ship.
        assert_eq!(gs.detected_foe_ship, grand);
        let b = gs
            .current_voyage()
            .unwrap()
            .current_battle
            .as_ref()
            .unwrap();
        assert_eq!(b.category, BattleCategory::BlackShip);
        assert_eq!(b.foe_ship, grand);
    }

    // ---- Mercenary roster + divvy shares (mercs earn none)
    // -------------------------------------

    #[test]
    fn won_roster_splits_mercenaries_from_swabbies() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        // Vocabulary so the classifier knows the swabbie names; a merc's
        // epithet is in neither set, so `[name] [epithet]` falls
        // through to Mercenary.
        gs.name_segments.learn_brigand("Gentle Gayle");
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[01:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[01:00:10] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line(
            "[01:01:00] You intercepted the War Frigate 'Modest Sild'!",
        );
        gs.process_line(
            "[01:02:00] Test Vessel has grappled Modest Sild. A melee breaks \
             out between the crews!",
        );
        // Winners = us + one mercenary + two swabbies (one named, one generic).
        // The generic "A swabbie" never actually appears in a real Game
        // Over roster (production swabbies are always named), but we
        // handle it defensively: its unknown tokens would otherwise
        // default to Mercenary in the classifier.
        gs.process_line(
            "[01:04:00] Game over.  Winners: Playerone, Luka Merciless, \
             Gentle Gayle, A swabbie.",
        );
        let v = gs.current_vessel().unwrap();
        // Vessel keeps the raw lumped NPC tally (all three non-players); the
        // merc roster names just the one `[name][epithet]`.
        assert_eq!(v.swabbies, 3);
        assert_eq!(v.mercenaries.len(), 1);
        assert!(v.mercenaries.contains("Luka Merciless"));
        // The generic swabbie is a swabbie, not a merc — the guard held.
        assert!(!v.mercenaries.contains("A swabbie"));
        assert!(!v.mercenaries.contains("Gentle Gayle"));
        let b = v.current_voyage.as_ref().unwrap().battles.last().unwrap();
        assert_eq!(b.outcome, BattleOutcome::Won);
        assert_eq!(b.pirates, 1); // just us
        // Flat `b.swabbies` is the total NPC crew (for manpower); the roster
        // splits it into disjoint genuine swabbies vs mercenaries.
        assert_eq!(b.swabbies, 3);
        let team = b.our_team.as_ref().unwrap();
        assert_eq!(team.swabbies, 2); // genuine swabbies only (Gentle Gayle + A swabbie)
        assert_eq!(team.mercenaries, 1);
        assert_eq!(team.headcount(), 4); // 1 pirate + 2 swabbies + 1 merc
        // Divvy shares: just us (1 pirate); the merc and the two genuine
        // swabbies earn none.
        assert_eq!(team.shares(), 1);
    }

    #[test]
    fn rum_spice_swap_sheds_one_mercenary() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        {
            let v = gs.current_vessel_mut().unwrap();
            v.swabbies = 5; // 5 NPC bodies, 2 of them mercs => 3 genuine swabbies
            v.mercenaries.insert("Luka Merciless".to_string());
            v.mercenaries.insert("Bree Steeljaw".to_string());
        }
        // The tell arms the swap; the paired single leave/come shift -1 merc /
        // +1 swabbie.
        gs.process_line(
            "[01:00:10] Avast, yer mercenary hirin' is limited by the rum \
             spice in yer hold. Ye need at least 5 spice per mercenary.",
        );
        gs.process_line("[01:00:11] A swabbie has left the vessel.");
        gs.process_line("[01:00:11] A swabbie has come aboard.");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.swabbies, 5); // total unchanged (a merc left, a swabbie replaced it)
        assert_eq!(v.mercenaries.len(), 1); // one merc shed
    }

    #[test]
    fn rum_spice_limit_poisons_voyage() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[01:00:01] This vessel is now Pillaging, Average Barbarians.",
        );
        gs.process_line(
            "[01:00:02] Playerone issued an order to set the vessel to sail.",
        );
        let poisoned = |gs: &GameState| {
            gs.current_vessel()
                .unwrap()
                .current_voyage
                .as_ref()
                .unwrap()
                .poisoned
        };
        assert!(
            !poisoned(&gs),
            "a fresh run isn't poisoned"
        );
        // The hold ran too low on rum spice to sustain the mercenaries.
        gs.process_line(
            "[01:00:10] Avast, yer mercenary hirin' is limited by the rum \
             spice in yer hold. Ye need at least 5 spice per mercenary.",
        );
        assert!(
            poisoned(&gs),
            "the rum-spice hiring-limit tell poisons the underway voyage",
        );
    }

    #[test]
    fn swabbies_leave_before_mercenaries() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        {
            let v = gs.current_vessel_mut().unwrap();
            v.swabbies = 5; // 3 genuine swabbies + 2 mercs
            v.mercenaries.insert("Luka Merciless".to_string());
            v.mercenaries.insert("Bree Steeljaw".to_string());
        }
        // First 2 leave: within the genuine-swabbie pool (3), no merc touched.
        gs.process_line("[01:00:10] 2 swabbies have left the vessel.");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.swabbies, 3);
            assert_eq!(v.mercenaries.len(), 2);
        }
        // Next 2 leave: only 1 genuine swabbie remains, so the overflow sheds 1
        // merc.
        gs.process_line("[01:00:20] 2 swabbies have left the vessel.");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.swabbies, 1);
        assert_eq!(v.mercenaries.len(), 1);
    }

    #[test]
    fn bulk_board_after_tell_does_not_swap() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        {
            let v = gs.current_vessel_mut().unwrap();
            v.swabbies = 5;
            v.mercenaries.insert("Luka Merciless".to_string());
            v.mercenaries.insert("Bree Steeljaw".to_string());
        }
        gs.process_line(
            "[01:00:10] Avast, yer mercenary hirin' is limited by the rum \
             spice in yer hold. Ye need at least 5 spice per mercenary.",
        );
        // A bulk board is re-staffing, not a swap: it disarms without shedding
        // a merc.
        gs.process_line("[01:00:11] 5 swabbies have come aboard.");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.swabbies, 10);
            assert_eq!(v.mercenaries.len(), 2);
        }
        // Proof the arm was consumed: a later single leave is now an ordinary
        // swabbie departure (genuine pool has room), so no merc is
        // shed.
        gs.process_line("[01:00:20] A swabbie has left the vessel.");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.swabbies, 9);
        assert_eq!(v.mercenaries.len(), 2);
    }

    #[test]
    fn winners_roster_backfills_merc_samples_to_ground_truth() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.name_segments.learn_brigand("Gentle Gayle");
        gs.process_line("====== 2026/06/23 ======");
        gs.process_line("[01:00:00] Going aboard the Test Vessel...");
        gs.process_line(
            "[01:00:05] This vessel is now Pillaging, Average Barbarians.",
        );
        // Sail samples the crew (mercs unknown -> 0); a swabbie delta samples
        // again.
        gs.process_line(
            "[01:00:10] Playerone issued an order to set the vessel to sail.",
        );
        gs.process_line("[01:00:20] 3 swabbies have come aboard.");
        {
            let voy = gs
                .current_vessel()
                .unwrap()
                .current_voyage
                .as_ref()
                .unwrap();
            assert!(voy.crew_samples.iter().all(|s| s.mercenaries == 0));
            assert_eq!(voy.merc_checkpoint, 0); // no ground truth yet
        }
        gs.process_line(
            "[01:01:00] You intercepted the War Frigate 'Modest Sild'!",
        );
        gs.process_line(
            "[01:02:00] Test Vessel has grappled Modest Sild. A melee breaks \
             out between the crews!",
        );
        // The win reveals one mercenary — every sample so far is backfilled to
        // it.
        gs.process_line(
            "[01:04:00] Game over.  Winners: Playerone, Luka Merciless, \
             Gentle Gayle, A swabbie.",
        );
        let voy = gs
            .current_vessel()
            .unwrap()
            .current_voyage
            .as_ref()
            .unwrap();
        assert!(!voy.crew_samples.is_empty());
        assert!(voy.crew_samples.iter().all(|s| s.mercenaries == 1));
        // The win backfills every sample of the just-closed stretch and
        // advances the checkpoint over them. The post-resolution
        // `sample_crew` then opens the next stretch with one fresh
        // baseline sample, which stays uncheckpointed until the next
        // win closes it — so the checkpoint sits exactly one behind the
        // sample count.
        assert_eq!(
            voy.merc_checkpoint,
            voy.crew_samples.len() - 1
        );
    }
}
