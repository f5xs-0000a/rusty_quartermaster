//! Streaming reader + state machine for the Puzzle Pirates client chat log.
//!
//! The log is a single ever-growing file. A `====== YYYY/MM/DD ======` header
//! line is written on every login/relog. Every other line is `[HH:MM:SS] <body>`.
//!
//! [`GameState`] is the state machine: `process_line` classifies a line and
//! delegates to a specific handler. State is organised per-vessel in a map keyed
//! by ship name, so we can hop between vessels and come back. A vessel is marked
//! `poisoned` if we leave it mid-run (before the booty is divided), since we then
//! miss whatever happens while it keeps sailing.
//!
//! [`spawn_tailer`] does the streaming: it reads bytes appended after a given
//! offset and sends complete lines over a channel. Whole-file ingestion is done
//! synchronously via [`GameState::process_existing`] before the tailer starts,
//! so history is in place before the first frame.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::pirate;
use crate::voyage::{Battle, BattleCategory, BattleOutcome, BattleSnapshot, CrewSample, Voyage};

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
    Trading {
        avg_poe_per_league: u32,
    },
    Exploring {
        monster: String,
    },
    AttackingFlotilla,
    /// Anything not yet modelled — keeps the raw text.
    Other(String),
}

impl JobKind {
    /// Parse the text after `This vessel is now ` (trailing `.` already removed).
    pub fn parse(s: &str) -> JobKind {
        if let Some(rest) = s.strip_prefix("Pillaging, ") {
            if let Some(job) = parse_pillaging(rest) {
                return job;
            }
        } else if s == "Evading" {
            return JobKind::Evading;
        } else if s == "Swabbie Ship Transporting" {
            return JobKind::SwabbieTransport;
        } else if let Some(n) = s
            .strip_prefix("Trading, offering an average of ")
            .and_then(|s| s.strip_suffix(" pieces of eight per league"))
            .and_then(|n| n.parse::<u32>().ok())
        {
            return JobKind::Trading {
                avg_poe_per_league: n,
            };
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
                    write!(f, "Pillaging, {lower} to {upper} {targets}")
                }
            }
            JobKind::Evading => f.write_str("Evading"),
            JobKind::SwabbieTransport => f.write_str("Swabbie Ship Transporting"),
            JobKind::Trading { avg_poe_per_league } => {
                write!(f, "Trading ({avg_poe_per_league} poe/league)")
            }
            JobKind::Exploring { monster } => write!(f, "Exploring the {monster}"),
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
    let split = words
        .iter()
        .position(|w| matches!(*w, "Pirates" | "Brigands" | "Barbarians"))?;
    let diff_text = words[..split].join(" ");
    let target_text = words[split..].join(" ");

    let (lower, upper) = match diff_text.split_once(" to ") {
        Some((lo, hi)) => (Difficulty::parse(lo)?, Difficulty::parse(hi)?),
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
// Vessel
// ---------------------------------------------------------------------------

/// State accumulated for a single vessel we've been aboard.
#[derive(Default)]
pub struct Vessel {
    /// What the vessel is doing now; `None` once the booty has been divided.
    pub job_kind: Option<JobKind>,
    /// Pirates currently aboard with us.
    pub crewmates: HashSet<String>,
    /// Swabbies (NPC crew) aboard. There's no absolute count line, so this is a
    /// running tally from the four "swabbie(s) (has|have) come aboard / left the
    /// vessel" delta lines, snapped to the authoritative roster whenever we win a
    /// fight (see [`on_battle_end`]). Departures saturate at zero so a poisoned
    /// vessel (we missed lines while away) can't underflow.
    pub swabbies: u32,
    /// Lone dragoons currently aboard on an Atlantis run: +1 per "Ye hear a
    /// splash, and the sound of foreign footsteps." line. Reset to zero once the
    /// crew repels all invaders. Always zero on voyage types without dragoons.
    pub dragoons_aboard: u32,
    /// Monster boarding parties currently aboard: +1 per "Dragoons from the
    /// monster took advantage..." line. Each party is 3/4/6 dragoons depending on
    /// the monster — we can't tell which — so we count the parties, not the heads.
    /// Reset alongside [`Self::dragoons_aboard`] when invaders are repelled.
    pub dragoon_boardings: u32,
    /// --- Vampirate lair tracking (Vampirates voyages) ---
    /// Whether we're currently inside a vampire lair (between `Welcome to the
    /// vampire sanctum.` / first `slaps mother` and the first lost `Game over`).
    pub lair_active: bool,
    /// Which wave we're in: 1 on lair entry, +1 per swordfight conclusion (`Game
    /// over` the crew won). Stays at the final wave after the lair ends (a lost
    /// swordfight); `0` if no lair has happened this run.
    pub lair_wave: u32,
    /// Real pirates aboard at lair entry — wave 1's vampire count (the anchor the
    /// per-wave projection grows from at ~+20%).
    pub lair_pirates: u32,
    /// Vampires defeated this lair so far (cumulative `<NPC> is eliminated!`). Note:
    /// undercounts while we're out of the fight — see [`Self::lair_warn`].
    pub vampires_defeated: u32,
    /// Vampires eliminated in the current wave only (reset each wave); checked
    /// against the wave's projected range to detect that we left the fight.
    pub wave_observed: u32,
    /// Projected `[low, high]` vampire count for the *current* wave (wave 1 =
    /// pirates exactly). Set on entry and on each wave advance.
    pub wave_lo: u32,
    pub wave_hi: u32,
    /// Latched once any wave's observed count falls outside its projected range —
    /// i.e. we left the swordfight and miscounted. Drives the on-screen reminder.
    pub lair_warn: bool,
    /// Greedy strikes tallied per attacking pirate, over the whole run.
    pub greedy_by_pirate: HashMap<String, u32>,
    /// Greedy strikes during the current/most-recent battle only. Reset when a
    /// new battle is joined (intercept), so between battles it holds the last
    /// battle's tally. Displayed as `(total - current) + current`.
    pub greedy_current: HashMap<String, u32>,
    /// Jobbers we (the player) planked, in order.
    pub planked_by_us: Vec<String>,
    /// We left this vessel mid-run, so its data has gaps.
    pub poisoned: bool,
    /// Monotonic board sequence; higher = boarded more recently. Updated on
    /// every (re)boarding so the selector can show the latest vessel on top.
    /// Always present, so it drives ordering even when timestamps are missing.
    pub order: u64,
    /// Wall-clock time we last boarded, from the line's `[HH:MM:SS]` combined
    /// with the most recent `====== Y/M/D ======` date header. For display.
    pub boarded_at: Option<NaiveDateTime>,
    /// The voyage currently underway aboard this vessel (sail -> port), if any.
    /// Accumulates per-battle stats; promoted into [`Self::voyages`] at port/divvy.
    pub current_voyage: Option<Voyage>,
    /// Completed sail->port runs, in order. RAM-only this session — nothing is
    /// written to disk until the user is prompted to save or discard (deferred).
    pub voyages: Vec<Voyage>,
}

impl Vessel {
    #[allow(dead_code)] // used in tests / handy accessor
    pub fn total_greedy(&self) -> u32 {
        self.greedy_by_pirate.values().sum()
    }
}

// ---------------------------------------------------------------------------
// GameState — the state machine
// ---------------------------------------------------------------------------

pub struct GameState {
    /// Our own pirate name (from `--user`), used to attribute planks to us.
    pub player_name: Option<Arc<str>>,
    /// True once a chat log has been attached via `--chat-log`.
    pub attached: bool,

    /// Every vessel we've been aboard this session, keyed by ship name.
    pub vessels: HashMap<Arc<str>, Vessel>,
    /// The vessel we're aboard right now, if any.
    pub current: Option<Arc<str>>,

    /// Crewmates/hearties currently logged on (global; wiped on relog). Tracked
    /// from presence lines but not yet surfaced anywhere — kept for a future
    /// "who's online" view. (No longer feeds the fetch worklist, which is now
    /// scoped to the aboard/planked sets.)
    #[allow(dead_code)]
    pub online: HashSet<String>,

    /// Date as we currently believe it to be: the most recent
    /// `====== Y/M/D ======` header, advanced by one day each time the line
    /// clock wraps past midnight without a new header.
    pub current_date: Option<NaiveDate>,
    /// Time of the previous timestamped line; used to detect midnight rollover.
    last_time: Option<NaiveTime>,
    /// Timestamp of the line currently being processed (date + line time).
    now: Option<NaiveDateTime>,
    /// Monotonic counter handing out [`Vessel::order`] values.
    order_counter: u64,
    /// Set for the duration of one line when a sea battle just resolved (`Game
    /// over` / disengage). Lets the app freeze the live Damage calculator onto
    /// that fight. Reset at the top of each [`Self::process_line`].
    battle_just_resolved: bool,
    /// Set for the duration of one line when a special encounter announced the
    /// foe's hull type (e.g. the Black Ship herald). Lets the app seed the live
    /// Damage calculator's foe ship. Reset at the top of each
    /// [`Self::process_line`]; consumed by [`Self::take_detected_foe_ship`].
    detected_foe_ship: Option<usize>,
}

impl GameState {
    pub fn new() -> Self {
        Self {
            player_name: None,
            attached: false,
            vessels: HashMap::new(),
            current: None,
            online: HashSet::new(),
            current_date: None,
            last_time: None,
            now: None,
            order_counter: 0,
            battle_just_resolved: false,
            detected_foe_ship: None,
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
                let line = String::from_utf8_lossy(&data[start..i]);
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
        self.detected_foe_ship = None;
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
        if let Some(prev) = self.last_time {
            if time < prev {
                self.current_date = self.current_date.and_then(|d| d.succ_opt());
            }
        }
        self.last_time = Some(time);
        self.now = self.current_date.map(|d| d.and_time(time));
    }

    fn classify(&mut self, body: &str) {
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
                self.on_greedy_strike(&body[..idx]);
                return;
            }
        }

        // Battle start: a fresh battle resets the current-battle greedy tally and
        // opens a new [`Battle`] record. We capture the enemy vessel name and who
        // initiated. Both "the X" forms end in `!` (live) or `.` (some variants).
        let intercept = body
            .strip_prefix("You intercepted the ")
            .map(|r| (r, true))
            .or_else(|| {
                body.strip_prefix("You have been intercepted by the ")
                    .map(|r| (r, false))
            });
        if let Some((rest, we_intercepted)) = intercept {
            let enemy = rest.trim_end_matches(['!', '.']).trim();
            self.on_battle_start(enemy, we_intercepted);
            return;
        }
        // Bare forms without a vessel name (e.g. "You intercepted the Brigands.").
        if body.starts_with("You intercepted") || body.starts_with("You have been intercepted") {
            let we_intercepted = body.starts_with("You intercepted");
            self.on_battle_start("", we_intercepted);
            return;
        }

        // Set-sail order: starts the voyage on its first occurrence (the order also
        // fires on every subsequent navigation move — those are ignored once a
        // voyage is underway).
        if body.ends_with(" issued an order to set the vessel to sail.") {
            self.on_set_sail();
            return;
        }
        // Put-into-port order: ends the timed sail->port run.
        if body.ends_with(" issued an order to put into port.") {
            self.on_put_into_port();
            return;
        }
        // A grapple begins the boarding melee: "<A> has grappled <B>. A melee
        // breaks out between the crews!" (logged for both sides; we keep the first).
        if body.contains(" has grappled ") && body.ends_with("A melee breaks out between the crews!")
        {
            self.on_grapple();
            return;
        }
        // Disengagements end a battle with no boarding conclusion.
        if body.ends_with(" issued an order to disengage.")
            || body.ends_with(" disengaged from the battle.")
            || body.starts_with("You are no longer being pursued by ")
            || body.starts_with("Arr, ye can no longer pursue")
        {
            self.on_disengage();
            return;
        }
        // Per-fight loot: "The victors plundered N pieces of eight and M units of
        // goods from the defeated vessel." Attaches to the just-resolved battle.
        if let Some(rest) = body.strip_prefix("The victors plundered ") {
            self.on_plunder(rest);
            return;
        }
        // Our cut: "Ye received N pieces of eight as your initial cut of the booty!"
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
        // The Black Ship (El Pollo Diablo) takes the place of our target — a rare
        // special encounter heralded right after interception, like the monster
        // heralds above. Always a Grand Frigate. Matched in full (not a substring)
        // so a player can't trigger it by parroting the line in chat: a chat body
        // is prefixed with `<Name> says, "…`, never exactly the system message.
        if body == "Dark clouds gather as ye bear down upon yer hapless victims, \
                    and from the miasma emerges the Black Ship to take the place \
                    of yer target in battle! Arrrrgh! Ye be doomed fer sure!"
        {
            self.on_black_ship();
            return;
        }

        // Brigand King — the victory line names the king unambiguously ("<King>'s
        // ship disappears into the mists."); the reward chest names them too.
        // Both fire after `Game over`, so they tag the just-resolved battle.
        if let Some(name) = body.strip_suffix("'s ship disappears into the mists.") {
            if BRIGAND_KINGS.contains(&name) {
                self.categorize_recent(BattleCategory::BrigandKing(name.to_string()));
                return;
            }
        }
        if let Some(name) = body
            .strip_prefix("Ye have received one ")
            .and_then(|s| s.strip_suffix(" Chest as part of yer reward!"))
        {
            if BRIGAND_KINGS.contains(&name) {
                self.categorize_recent(BattleCategory::BrigandKing(name.to_string()));
                return;
            }
        }

        // Atlantis: a lone dragoon sneaks aboard.
        if body == "Ye hear a splash, and the sound of foreign footsteps." {
            self.on_dragoon_aboard();
            return;
        }
        // Atlantis: the monster lands a whole boarding party (3/4/6 dragoons).
        if body == "Dragoons from the monster took advantage of their proximity to board yer vessel!"
        {
            self.on_dragoon_boarding();
            return;
        }
        // Atlantis: invaders cleared — the dragoons aboard are gone.
        if body == "Arr! Yer crew has managed to repel all invaders!" {
            self.on_invaders_repelled();
            return;
        }
        // FUTURE (anomaly log): "Yer ship has entered a citadel!" arriving while
        // `dragoons_aboard`/`dragoon_boardings` are non-zero is a noteworthy case
        // worth recording. No state change today — wire it here when that log lands.

        // Vampirates: we entered a lair (wave 1 begins). Waves run from here (and
        // the first slap of Mother) through each swordfight conclusion below — the
        // "rustling in coffins" line is only a "swordfight imminent" herald and does
        // NOT delimit waves, so it isn't parsed.
        if body.starts_with("Welcome to the vampire sanctum") {
            self.on_lair_enter();
            return;
        }
        // Vampirates: someone slapping Mother is the wave-1 fight kickoff and a
        // fallback lair-start signal if we missed the sanctum line (only starts a
        // lair if we aren't already in one).
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
        // winning side, it's an authoritative roster of who's aboard; if the winners
        // are vampires, we lost the board, which ends a lair.
        if let Some(summary) = body.strip_prefix("Game over.") {
            let summary = summary.trim_start();
            self.on_battle_end(summary); // resync crew roster when we won
            self.on_sea_battle_resolve(summary); // record the sea-battle outcome (pillage)
            self.on_lair_gameover(summary); // vampirate wave engine
            self.sample_crew(); // roster may have been resynced
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
            if let Some(v) = self.current_vessel_mut() {
                v.job_kind = None;
                // Finalize the run if it wasn't already closed at port (defensive:
                // some runs end at divvy without a port order we saw).
                if let Some(mut voy) = v.current_voyage.take() {
                    if voy.ported_at.is_none() {
                        voy.ported_at = now;
                    }
                    if let Some(b) = voy.current_battle.take() {
                        voy.battles.push(b);
                    }
                    v.voyages.push(voy);
                }
            }
            return;
        }

        // Swabbie head-count deltas. These are the only board-count signal for
        // NPC crew (there is no absolute "N swabbies aboard" line between
        // fights); a won fight later resyncs the tally from the winners roster.
        if let Some(delta) = parse_swabbie_delta(body) {
            if let Some(v) = self.current_vessel_mut() {
                v.swabbies = if delta >= 0 {
                    v.swabbies.saturating_add(delta as u32)
                } else {
                    v.swabbies.saturating_sub(delta.unsigned_abs() as u32)
                };
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
            }
            self.sample_crew();
            return;
        }

        // We left the vessel, or were planked off it.
        if (body.starts_with("Ye have left '") && body.ends_with("'."))
            || body.starts_with("Ye were made to walk the plank by ")
        {
            self.leave_vessel();
            return;
        }

        // Third-person plank: "<Planker> forced <Victim> to walk the plank."
        if let Some(mid) = body.strip_suffix(" to walk the plank.") {
            if let Some((planker, victim)) = mid.split_once(" forced ") {
                self.on_plank(planker, victim);
                self.sample_crew(); // a crewmate left the roster
                return;
            }
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

        // Catch-all (runs only for otherwise-unhandled lines, so it can't swallow
        // a `Game over` etc.): a Brigand King's engagement / flavour chant names
        // the king. Tag the open battle. Player chatter is skipped so a mention of
        // a king in chat doesn't mislabel a fight.
        if !is_chat_line(body) {
            if let Some(king) = find_brigand_king(body) {
                self.categorize_current(BattleCategory::BrigandKing(king.to_string()));
            }
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
        self.current = Some(key);
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
                v.planked_by_us.push(victim.to_string());
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

    /// A new battle began — start a fresh current-battle greedy tally and open a
    /// new [`Battle`] record on the current voyage (creating the voyage if a fight
    /// somehow starts before we saw a sail order). `enemy` empty => unknown vessel.
    fn on_battle_start(&mut self, enemy: &str, we_intercepted: bool) {
        let now = self.now;
        if let Some(v) = self.current_vessel_mut() {
            v.greedy_current.clear();
        }
        let enemy = (!enemy.is_empty()).then(|| enemy.to_string());
        // A monkey boat is identified by its (fixed) vessel name — which also tells
        // us its hull. The name is from the system interception line, so this can't
        // be spoofed via chat. Also seed the live Damage calculator's foe ship.
        let monkey_ship = enemy.as_deref().and_then(monkey_boat_ship);
        self.detected_foe_ship = monkey_ship;
        if let Some(voy) = self.ensure_voyage() {
            // A still-open previous battle means we never saw its resolution; keep
            // it as a dangling record rather than dropping it.
            if let Some(prev) = voy.current_battle.take() {
                voy.battles.push(prev);
            }
            let mut battle = Battle {
                enemy,
                we_intercepted,
                started_at: now,
                ..Battle::default()
            };
            if let Some(idx) = monkey_ship {
                battle.category = BattleCategory::MonkeyBoat;
                battle.foe_ship = Some(idx);
            }
            voy.current_battle = Some(battle);
        }
    }

    /// First `set the vessel to sail` order of a run starts the voyage; later move
    /// orders during the run just backfill a missing sail time (if the voyage was
    /// lazily created by an early battle).
    fn on_set_sail(&mut self) {
        let now = self.now;
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
                let job_kind = v.job_kind.clone();
                v.current_voyage = Some(Voyage {
                    job_kind,
                    sailed_at: now,
                    ..Voyage::default()
                });
            }
        }
        // Anchor the crew timeline at sail time with the starting headcount.
        self.sample_crew();
    }

    /// A `put into port` order ends the timed run: stamp the port time, fold any
    /// dangling battle in, and promote the voyage into the completed list.
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

    /// The boarding melee started — record the sea/boarding boundary once.
    fn on_grapple(&mut self) {
        let now = self.now;
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.current_battle.as_mut() {
                if b.grappled_at.is_none() {
                    b.grappled_at = now;
                }
            }
        }
    }

    /// A battle ended without a boarding conclusion (someone disengaged / enemy
    /// ported / we shook the pursuit). Resolve the open battle as `Disengaged`.
    fn on_disengage(&mut self) {
        let now = self.now;
        let mut resolved = false;
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(mut b) = voy.current_battle.take() {
                b.outcome = BattleOutcome::Disengaged;
                b.ended_at = now;
                voy.battles.push(b);
                resolved = true;
            }
        }
        self.battle_just_resolved |= resolved;
    }

    /// Resolve the open sea battle at `Game over`. Won iff our own name is among the
    /// winners; otherwise we lost (and forfeit the plundered PoE). Skipped inside a
    /// vampirate lair, whose per-wave swordfights aren't sea battles. Run *after*
    /// [`Self::on_battle_end`] so the crew snapshot uses the resynced roster.
    fn on_sea_battle_resolve(&mut self, summary: &str) {
        if self.current_vessel().is_some_and(|v| v.lair_active) {
            return;
        }
        let me = self.player_name.clone();
        let me = me.as_deref();
        // The winners roster, parsed once. On a win it's our ship; on a loss it's
        // the foe's crew.
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
        let won = winners
            .iter()
            .any(|n| me.is_some_and(|me| n.eq_ignore_ascii_case(me)));
        let (pirates, swabbies) = self
            .current_vessel()
            .map(|v| (v.crewmates.len() as u32 + 1, v.swabbies))
            .unwrap_or((0, 0));
        // A king on the winning side means we lost to that king — name the fight.
        let king = find_brigand_king(summary);
        // PvP on a loss: a winner who's a real player and not ours = an enemy
        // player. (A win's eliminations already catch enemy players, since all
        // enemies are eliminated.)
        let foe_player = !won
            && winners
                .iter()
                .any(|n| pirate::is_player_name(n) && !self.is_own_crew(n));
        let now = self.now;
        let mut resolved = false;
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(mut b) = voy.current_battle.take() {
                b.outcome = if won {
                    BattleOutcome::Won
                } else {
                    BattleOutcome::Lost
                };
                b.ended_at = now;
                b.pirates = pirates;
                b.swabbies = swabbies;
                if foe_player {
                    b.is_pvp = true;
                }
                // Foe headcount from the melee: on a win, all enemies are
                // eliminated, so it's the KOs that aren't our crew (= not in the
                // winners roster, which is our ship on a win); on a loss it's the
                // size of the winners' (enemy) roster.
                b.their_manpower = Some(if won {
                    b.melee_kos
                        .iter()
                        .filter(|ko| {
                            let ko = ko.as_str();
                            !winners.iter().any(|w| w.eq_ignore_ascii_case(ko))
                        })
                        .count() as u32
                } else {
                    winners.len() as u32
                });
                b.melee_kos.clear();
                if let Some(k) = king {
                    b.category = BattleCategory::BrigandKing(k.to_string());
                }
                voy.battles.push(b);
                resolved = true;
            }
        }
        self.battle_just_resolved |= resolved;
    }

    /// Attach plundered PoE + goods to the most-recently-resolved battle. The
    /// plunder line always follows a `Game over`, so `battles.last_mut()` is it.
    /// PoE is signed by the battle's outcome (negative on a loss — it went to them).
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
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.battles.last_mut() {
                if let Some(poe) = poe {
                    b.poe = Some(if b.outcome == BattleOutcome::Lost {
                        -(poe as i64)
                    } else {
                        poe as i64
                    });
                }
                if goods.is_some() {
                    b.goods = goods;
                }
            }
        }
    }

    /// Attach our personal cut to the most-recently-resolved battle.
    fn on_my_cut(&mut self, poe: u64) {
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.battles.last_mut() {
                b.my_cut = Some(poe);
            }
        }
    }

    /// The Black Ship (El Pollo Diablo) replaced our target. Tag the open fight and
    /// pin its hull to a Grand Frigate; its crew count is left to the usual melee
    /// elimination tally (so it tracks however the devs staff it). Also flags the
    /// foe hull for the live Damage calculator via [`Self::take_detected_foe_ship`].
    fn on_black_ship(&mut self) {
        let grand = crate::ships::ship_index("Grand Frigate");
        self.detected_foe_ship = grand;
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.current_battle.as_mut() {
                b.category = BattleCategory::BlackShip;
                b.foe_ship = grand;
            }
        }
    }

    /// Tag the battle currently in progress (engagement-time signals).
    fn categorize_current(&mut self, cat: BattleCategory) {
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.current_battle.as_mut() {
                b.category = cat;
            }
        }
    }

    /// Tag the open battle, or the just-resolved one if none is open (end-time
    /// signals like the victory/reward lines arrive after `Game over`).
    fn categorize_recent(&mut self, cat: BattleCategory) {
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.current_battle.as_mut().or_else(|| voy.battles.last_mut()) {
                b.category = cat;
            }
        }
    }

    /// The current vessel's active voyage, mutably.
    fn current_voyage_mut(&mut self) -> Option<&mut Voyage> {
        self.current_vessel_mut()?.current_voyage.as_mut()
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
        if let Some(voy) = v.current_voyage.as_mut() {
            voy.crew_samples.push(CrewSample {
                at: now,
                pirates,
                swabbies,
            });
        }
    }

    /// Ensure the current vessel has an active voyage, creating a job-tagged one if
    /// needed. `sailed_at` stays `None` for lazily-created voyages (a battle began
    /// before we saw a sail order); [`Self::on_set_sail`] backfills it.
    fn ensure_voyage(&mut self) -> Option<&mut Voyage> {
        let v = self.current_vessel_mut()?;
        if v.current_voyage.is_none() {
            let job_kind = v.job_kind.clone();
            v.current_voyage = Some(Voyage {
                job_kind,
                ..Voyage::default()
            });
        }
        v.current_voyage.as_mut()
    }

    /// A lone dragoon splashed aboard (Atlantis).
    fn on_dragoon_aboard(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.dragoons_aboard = v.dragoons_aboard.saturating_add(1);
        }
    }

    /// The monster landed a boarding party of dragoons (Atlantis).
    fn on_dragoon_boarding(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.dragoon_boardings = v.dragoon_boardings.saturating_add(1);
        }
    }

    /// The crew repelled all invaders — clear the dragoons aboard.
    fn on_invaders_repelled(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.dragoons_aboard = 0;
            v.dragoon_boardings = 0;
        }
    }

    /// Entered a vampire lair — wave 1 begins with one vampire per pirate aboard.
    /// Resets the lair counters (a re-entry / new lair starts fresh).
    fn on_lair_enter(&mut self) {
        // Real pirates aboard = tracked crewmates + ourselves (NPC swabbies don't
        // count). This is wave 1's vampire count.
        let pirates = self
            .current_vessel()
            .map(|v| v.crewmates.len() as u32 + 1)
            .unwrap_or(0);
        if let Some(v) = self.current_vessel_mut() {
            v.lair_active = true;
            v.lair_wave = 1;
            v.lair_pirates = pirates;
            v.vampires_defeated = 0;
            v.wave_observed = 0;
            v.wave_lo = pirates; // wave 1 is exactly the pirate count
            v.wave_hi = pirates;
            v.lair_warn = false;
        }
    }

    /// A `slaps mother` line: start the lair only if we aren't already in one (so
    /// repeated slaps mid-fight don't reset the counters).
    fn on_lair_slap(&mut self) {
        if !self.current_vessel().is_some_and(|v| v.lair_active) {
            self.on_lair_enter();
        }
    }

    /// A vampire was defeated (only counted while in a lair).
    /// A combatant was knocked out in a melee. In a vampirate lair, NPC names
    /// count as defeated vampires. In a sea battle we record every KO on the open
    /// fight (the basis for the foe's headcount) and flag PvP when an eliminated
    /// real player isn't our own crew.
    fn on_eliminated(&mut self, name: &str) {
        // Vampirate lairs: just tally defeated vampires (NPC names).
        if self.current_vessel().is_some_and(|v| v.lair_active) {
            if !pirate::is_player_name(name) {
                self.on_vampire_defeated();
            }
            return;
        }
        // Sea battle: record the KO and detect an enemy player.
        let enemy_player = pirate::is_player_name(name) && !self.is_own_crew(name);
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.current_battle.as_mut() {
                b.melee_kos.push(name.to_string());
                if enemy_player {
                    b.is_pvp = true;
                }
            }
        }
    }

    /// Whether `name` is us or one of our current vessel's crewmates.
    fn is_own_crew(&self, name: &str) -> bool {
        if self.player_name.as_deref() == Some(name) {
            return true;
        }
        self.current_vessel().is_some_and(|v| v.crewmates.contains(name))
    }

    fn on_vampire_defeated(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            if v.lair_active {
                v.vampires_defeated = v.vampires_defeated.saturating_add(1);
                v.wave_observed = v.wave_observed.saturating_add(1);
            }
        }
    }

    /// A swordfight concluded (`Game over`) — the boundary between lair waves. While
    /// in a lair we close out the current wave (flagging if its observed count fell
    /// outside the projection — i.e. we left the fight), then either:
    ///   * the winners are all vampires => we lost => the lair ends (first loss), or
    ///   * the crew won => advance to the next wave, projecting its range from the
    ///     concluded wave's anchor count (`pirates * growth^(wave-1)`).
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
        if let Some(v) = self.current_vessel_mut() {
            if !v.lair_active {
                return;
            }
            // Close out the wave that just concluded.
            if v.wave_observed < v.wave_lo || v.wave_observed > v.wave_hi {
                v.lair_warn = true;
            }
            if !players_won {
                // First loss ends the lair.
                v.lair_active = false;
                return;
            }
            // Crew won: advance to the next wave and project its range.
            let base = v.lair_pirates as f64 * LAIR_WAVE_GROWTH.powi((v.lair_wave - 1) as i32);
            v.lair_wave += 1;
            v.wave_observed = 0;
            v.wave_lo = (base * LAIR_WAVE_LO).round() as u32;
            v.wave_hi = (base * LAIR_WAVE_HI).round() as u32;
        }
    }

    /// A battle ended with `summary` like `Winners: a, b, Playerone.`. When the
    /// player is among the listed side, that side *is* the crew aboard, so we
    /// overwrite the crewmate set with it (minus ourselves and NPC swabbies).
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
        let player_listed = me.is_some_and(|me| names.iter().any(|n| n.eq_ignore_ascii_case(me)));
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
        let swabbies = names.iter().filter(|n| !pirate::is_player_name(n)).count() as u32;

        if let Some(v) = self.current_vessel_mut() {
            v.crewmates = new_crew;
            v.swabbies = swabbies;
        }
    }

    /// The foe headcount computed for the just-resolved (last) battle, if any.
    pub fn last_resolved_their_manpower(&self) -> Option<u32> {
        self.current_voyage()?.battles.last()?.their_manpower
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

    /// Take the foe-hull index detected from a special encounter this line (once
    /// per detection). The app uses it to seed the live Damage calculator's foe
    /// ship so live tracking — and the captured snapshot — use the right hull.
    pub fn take_detected_foe_ship(&mut self) -> Option<usize> {
        self.detected_foe_ship.take()
    }

    /// Freeze the live Damage-calculator snapshot + advantage onto the just-resolved
    /// (last) battle of the current voyage. Called at Game over / disengage when the
    /// calculator had input, so fights we tracked live land in the history recorded.
    pub fn record_resolved_battle(&mut self, snap: BattleSnapshot, dmg: f64, crew: f64) {
        if let Some(voy) = self.current_voyage_mut() {
            if let Some(b) = voy.battles.last_mut() {
                b.snapshot = Some(snap);
                b.advantage_dmg = Some(dmg);
                b.advantage_crew = Some(crew);
            }
        }
    }

    /// Real pirates aboard the current vessel right now (crewmates + us), or 0.
    pub fn current_pirates(&self) -> u32 {
        self.current_vessel()
            .map(|v| v.crewmates.len() as u32 + 1)
            .unwrap_or(0)
    }

    /// Swabbies (NPC crew, incl. named mercenaries) aboard the current vessel, or 0.
    pub fn current_swabbies(&self) -> u32 {
        self.current_vessel().map(|v| v.swabbies).unwrap_or(0)
    }

    /// The battle at display index `idx` of the displayed voyage on vessel `key`.
    /// The displayed list is the resolved battles, then the in-progress one (if
    /// any), so an index one past the resolved set addresses `current_battle`.
    fn displayed_battle_mut(&mut self, key: &Arc<str>, idx: usize) -> Option<&mut Battle> {
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

    /// Write a fight's Damage-calculator snapshot + recomputed advantage. Driven
    /// by the Sea Battles popup on every edit (the calculator is always editable;
    /// this is independent of whether the fight is recorded).
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
    pub fn set_battle_recorded(&mut self, key: &Arc<str>, idx: usize, recorded: bool) {
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
        if self.current.as_ref() == Some(key) {
            if let Some(me) = self.player_name.as_deref() {
                set.insert(me.to_string());
            }
        }
        set
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
/// line, reward chest, and winners-list entry all carry the full name, so we key
/// categorization off the name rather than per-king chant text — robust across
/// every king without a brittle chant table. (Roster from yppedia.)
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

/// Monkey-boat vessels and the hull each one sails, per yppedia. A monkey boat is
/// identified by its (fixed) vessel name in the interception line, which maps to a
/// known ship type — there are exactly twelve, one per non-niche hull.
const MONKEY_BOATS: &[(&str, &str)] = &[
    ("Petulant Kumquat", "Sloop"),
    ("Itinerant Pomegranate", "Cutter"),
    ("Resplendent Peach", "Dhow"),
    ("Succulent Pear", "Baghlah"),
    ("Appealing Orange", "Longship"),
    ("Adventurous Huckleberry", "Merchant Brig"),
    ("Scrumptious Strawberry", "Junk"),
    ("Dogged Rhubarb", "War Brig"),
    ("Overbearing Pineapple", "Xebec"),
    ("Vainglorious Plum", "Merchant Galleon"),
    ("Determined Pumpkin", "War Frigate"),
    ("Juicy Watermelon", "Grand Frigate"),
];

/// The [`crate::ships::SHIPS`] index of the monkey boat with this exact vessel
/// name, if `name` is one. The name comes from the (system) interception line, so
/// it can't be spoofed via chat.
fn monkey_boat_ship(name: &str) -> Option<usize> {
    MONKEY_BOATS
        .iter()
        .find(|(vessel, _)| *vessel == name)
        .and_then(|(_, ship)| crate::ships::ship_index(ship))
}

/// Whether a line is player chatter (so king names mentioned in chat don't
/// mislabel a fight).
fn is_chat_line(body: &str) -> bool {
    body.contains(" says,") || body.contains(" chats,") || body.contains(" tells ye,")
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
    let time = NaiveTime::parse_from_str(&rest[..end], "%H:%M:%S").ok()?;
    Some((time, rest[end + 1..].trim_start()))
}

/// Parse the date out of a `====== YYYY/MM/DD ======` header line.
fn parse_date_header(line: &str) -> Option<NaiveDate> {
    let inner = line.trim_matches('=').trim();
    NaiveDate::parse_from_str(inner, "%Y/%m/%d").ok()
}

// ---------------------------------------------------------------------------
// Streaming tailer
// ---------------------------------------------------------------------------

/// Spawn a background thread that tails `path` from `start_offset`, sending each
/// complete (newline-terminated) line over `tx`. Lines are decoded lossily and
/// stripped of trailing CR/LF. A trailing incomplete line is buffered until its
/// newline arrives. Stops when the receiver is dropped.
///
/// This is a detached **std** thread, not a Tokio blocking task: a blocking task
/// looping forever would make the runtime's shutdown (on `main` returning) hang
/// waiting for it. A plain thread is simply abandoned when the process exits.
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

            if offset < len {
                if file.seek(SeekFrom::Start(offset)).is_ok() {
                    let mut buf = Vec::new();
                    if let Ok(n) = file.take(len - offset).read_to_end(&mut buf) {
                        offset += n as u64;
                        leftover.extend_from_slice(&buf);

                        // Emit every complete line; keep the remainder.
                        while let Some(pos) = leftover.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = leftover.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes);
                            let line = line.trim_end_matches(['\n', '\r']).to_string();
                            if tx.send(line).is_err() {
                                return; // receiver gone
                            }
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
        let JobKind::Pillaging { lower, upper, .. } = pillage("Pillaging, Average Barbarians")
        else {
            panic!("expected pillaging");
        };
        assert_eq!(lower, Difficulty::Average);
        assert_eq!(upper, Difficulty::Average);
    }

    #[test]
    fn parses_very_hard_and_all_targets() {
        let JobKind::Pillaging {
            upper, targets, ..
        } = pillage("Pillaging, Easy to Very Hard Pirates and Brigands and Barbarians")
        else {
            panic!("expected pillaging");
        };
        assert_eq!(upper, Difficulty::VeryHard);
        assert!(targets.pirates && targets.brigands && targets.barbarians);
    }

    #[test]
    fn parses_other_job_kinds() {
        assert_eq!(pillage("Evading"), JobKind::Evading);
        assert_eq!(pillage("Swabbie Ship Transporting"), JobKind::SwabbieTransport);
        assert_eq!(pillage("Attacking a Flotilla"), JobKind::AttackingFlotilla);
        assert_eq!(
            pillage("Trading, offering an average of 10 pieces of eight per league"),
            JobKind::Trading {
                avg_poe_per_league: 10
            }
        );
        assert_eq!(
            pillage("Exploring the Sucker-bearing Destroyer of the Briny Deep"),
            JobKind::Exploring {
                monster: "Sucker-bearing Destroyer of the Briny Deep".to_string()
            }
        );
    }

    #[test]
    fn tracks_board_crew_and_greedy() {
        let mut gs = GameState::new();
        gs.process_line("[14:55:00] Going aboard the Test Vessel...");
        gs.process_line("[14:56:16] Matetwo has come aboard.");
        gs.process_line("[14:56:24] This vessel is now Pillaging, Average to Hard Barbarians.");
        gs.process_line(
            "[14:56:27] Matethree delivers an overwhelming barrage against Hunched Alice, \
             causing some treasure to fall from their grip!",
        );
        gs.process_line(
            "[14:56:28] Matethree executes a masterful strike against Demented Carlos, \
             who drops some treasure in surprise!",
        );

        assert_eq!(gs.current.as_deref(), Some("Test Vessel"));
        let v = gs.current_vessel().unwrap();
        assert!(v.crewmates.contains("Matetwo"));
        assert_eq!(v.greedy_by_pirate.get("Matethree"), Some(&2));
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
    fn clean_run_then_leave_is_not_poisoned() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
        gs.process_line("[01:00:05] This vessel is now Pillaging, Average Barbarians.");
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
        gs.process_line("[01:00:05] This vessel is now Pillaging, Average Barbarians.");
        gs.process_line("[01:05:00] Ye have left 'Crew'."); // poisoned
        gs.process_line("[01:10:00] Going aboard the Enchanting Pike..."); // return
        assert_eq!(gs.current.as_deref(), Some("Enchanting Pike"));
        assert!(gs.current_vessel().unwrap().poisoned);
    }

    #[test]
    fn counts_dragoons_and_resets_on_repel() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the Abyssal Grunion...");
        gs.process_line("[01:00:05] This vessel is now Atlantis.");
        gs.process_line("[01:01:00] Ye hear a splash, and the sound of foreign footsteps.");
        gs.process_line("[01:01:30] Ye hear a splash, and the sound of foreign footsteps.");
        gs.process_line(
            "[01:02:00] Dragoons from the monster took advantage of their proximity to board yer vessel!",
        );
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.dragoons_aboard, 2);
        assert_eq!(v.dragoon_boardings, 1);

        gs.process_line("[01:03:00] Arr! Yer crew has managed to repel all invaders!");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.dragoons_aboard, 0);
        assert_eq!(v.dragoon_boardings, 0);
    }

    #[test]
    fn attributes_planks_to_us() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Captiviating Mummichog...");
        gs.process_line("[01:01:00] Matefour has come aboard.");
        gs.process_line("[01:02:00] Playerone forced Matefour to walk the plank.");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.planked_by_us, vec!["Matefour".to_string()]);
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
        assert_eq!(gs.player_name.as_deref(), Some("Playerone")); // config survives
    }

    /// Dev tool: ingest a real log via `YPP_LOG=/path cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ingest_real_log() {
        let Ok(path) = std::env::var("YPP_LOG") else {
            eprintln!("set YPP_LOG to a chat log path");
            return;
        };
        let data = std::fs::read(&path).expect("read log");
        let mut gs = GameState::new();
        gs.player_name = std::env::var("YPP_USER").ok().map(|u| Arc::from(u.as_str()));
        gs.process_existing(&data);

        eprintln!("vessels seen: {}", gs.vessels.len());
        let mut names: Vec<&Arc<str>> = gs.vessels.keys().collect();
        names.sort_unstable();
        for n in &names {
            let v = &gs.vessels[*n];
            eprintln!(
                "  {:<28} job={:<48} greedy={:<4} crew={:<3} planked_by_us={} {}",
                n,
                v.job_kind.as_ref().map(|j| j.to_string()).unwrap_or_default(),
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
        assert_eq!(gs.current_date, NaiveDate::from_ymd_opt(2026, 6, 17));
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
        // Underflow (poison-induced over-counting of departures) saturates at 0.
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
            "[01:30:00] Game over.  Winners: Playerone, Tony Ironsides, Master Hogan, A swabbie.",
        );
        assert_eq!(gs.current_vessel().unwrap().swabbies, 3);
    }

    #[test]
    fn greedy_splits_total_and_current_battle() {
        let mut gs = GameState::new();
        gs.process_line("[01:00:00] Going aboard the War Carp...");
        gs.process_line("[01:00:05] You intercepted the Brigands.");
        gs.process_line(
            "[01:00:06] Mateone delivers an overwhelming barrage against X, who drops treasure!",
        );
        gs.process_line(
            "[01:00:07] Mateone executes a masterful strike against Y, who drops treasure!",
        );
        gs.process_line("[01:05:00] Game over.  Winner: Playerone.");
        // Second battle: current tally resets, but Mateone's run total carries.
        gs.process_line("[01:10:00] You have been intercepted by the Barbarians.");
        gs.process_line(
            "[01:10:06] Mateone performs a powerful attack against Z, who drops treasure!",
        );
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.greedy_by_pirate.get("Mateone"), Some(&3)); // total
        assert_eq!(v.greedy_current.get("Mateone"), Some(&1)); // current battle
    }

    #[test]
    fn game_over_winners_override_crew_when_player_listed() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Royal Roughy...");
        gs.process_line("[01:00:01] Mateseven has come aboard.");
        gs.process_line("[01:30:00] Game over.  Winners: Mateone, Playerone, Matesix, A swabbie.");
        let v = gs.current_vessel().unwrap();
        // Crew replaced with the winning side (minus us and the swabbie).
        assert!(v.crewmates.contains("Mateone"));
        assert!(v.crewmates.contains("Matesix"));
        assert!(!v.crewmates.contains("Playerone"));
        assert!(!v.crewmates.contains("A swabbie"));
        assert!(!v.crewmates.contains("Mateseven")); // overwritten
    }

    #[test]
    fn game_over_without_player_leaves_crew_alone() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Royal Roughy...");
        gs.process_line("[01:00:01] Mateseven has come aboard.");
        gs.process_line("[01:30:00] Game over.  Winners: Master Hogan, Brigand Bob.");
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
        // Enter the lair: wave 1's vampires = pirates aboard (Playerone + 2 = 3).
        gs.process_line("[01:01:00] Welcome to the vampire sanctum. ");
        {
            let v = gs.current_vessel().unwrap();
            assert!(v.lair_active);
            assert_eq!(v.lair_wave, 1);
            assert_eq!(v.lair_pirates, 3);
        }
        // Wave 1: 3 vampires defeated (NPCs have a space); a crew KO must not count.
        gs.process_line("[01:01:10] Stygian Lilith is eliminated!");
        gs.process_line("[01:01:11] Matethree is eliminated!"); // crew KO — ignored
        gs.process_line("[01:01:12] Immortal Schreck is eliminated!");
        gs.process_line("[01:01:13] Craving Silvia is eliminated!");
        // Wave 1's swordfight concludes with a crew win -> advance to wave 2. Wave 1
        // hit its target (3 = pirates), so no warning yet.
        gs.process_line("[01:02:00] Game over.  Winners: Playerone, Matetwo.");
        {
            let v = gs.current_vessel().unwrap();
            assert_eq!(v.lair_wave, 2);
            assert_eq!(v.vampires_defeated, 3);
            assert!(!v.lair_warn);
        }
        // Wave 2 should hold ~4 vampires, but we leave the fight and see only 1...
        gs.process_line("[01:02:10] Sunless Collins is eliminated!");
        // ...then lose the next swordfight (winners all vampires), ending the lair.
        gs.process_line("[01:03:00] Game over.  Winners: Revenant Drac, Gloaming Lucy.");
        let v = gs.current_vessel().unwrap();
        assert!(!v.lair_active);
        assert_eq!(v.lair_wave, 2);
        assert_eq!(v.vampires_defeated, 4);
        assert!(v.lair_warn); // wave 2 fell short of its projection -> we left
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
        assert_eq!(order, vec!["First Fish", "Third Fish", "Second Fish"]);
    }

    #[test]
    fn records_voyage_battle_and_loot() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("====== 2026/05/14 ======");
        gs.process_line("[02:00:00] Going aboard the Test Vessel...");
        gs.process_line("[02:00:05] This vessel is now Pillaging, Average to Hard Barbarians.");
        gs.process_line("[02:00:10] Playerone issued an order to set the vessel to sail.");
        // A later move order must NOT start a second voyage.
        gs.process_line("[02:05:00] Playerone issued an order to set the vessel to sail.");
        gs.process_line("[02:06:06] You have been intercepted by the Modest Sild!");
        gs.process_line(
            "[02:09:51] Modest Sild has grappled Test Vessel. A melee breaks out between the crews!",
        );
        gs.process_line("[02:14:43] Game over.  Winners: Matetwo, Playerone, A swabbie.");
        gs.process_line(
            "[02:15:02] The victors plundered 7,756 pieces of eight and 9 units of goods from the defeated vessel.",
        );
        gs.process_line("[02:15:02] Ye received 576 pieces of eight as your initial cut of the booty!");
        gs.process_line("[02:20:00] Playerone issued an order to put into port.");

        let v = &gs.vessels["Test Vessel"];
        assert!(v.current_voyage.is_none()); // promoted to completed at port
        assert_eq!(v.voyages.len(), 1);
        let voy = &v.voyages[0];
        assert_eq!(voy.duration_secs(), Some(1190)); // 02:00:10 -> 02:20:00
        assert_eq!(voy.battles.len(), 1);
        let b = &voy.battles[0];
        assert_eq!(b.enemy.as_deref(), Some("Modest Sild"));
        assert!(!b.we_intercepted);
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
    fn records_loss_with_negative_poe() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[00:20:00] Going aboard the Boring Gar...");
        gs.process_line("[00:20:05] This vessel is now Pillaging, Average Barbarians.");
        gs.process_line("[00:20:49] Playerone issued an order to set the vessel to sail.");
        gs.process_line("[00:20:49] You have been intercepted by the Boring Gar!");
        gs.process_line("[00:33:03] Game over.  Winners: Nervy Hugh, Insane Yang.");
        gs.process_line(
            "[00:33:11] The victors plundered 27,460 pieces of eight and 350 units of goods from the defeated vessel.",
        );
        let voy = gs.current_voyage().unwrap();
        let b = voy.battles.last().unwrap();
        assert_eq!(b.outcome, BattleOutcome::Lost);
        assert_eq!(b.poe, Some(-27_460)); // we lost it to them
        assert_eq!(b.goods, Some(350));
    }

    #[test]
    fn intercept_then_disengage_is_disengaged() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[23:27:00] Going aboard the Sea Lord...");
        gs.process_line("[23:27:01] This vessel is now Pillaging, Average Barbarians.");
        gs.process_line("[23:27:02] Playerone issued an order to set the vessel to sail.");
        gs.process_line("[23:27:23] You have been intercepted by the Lucky Mackerel!");
        gs.process_line("[23:28:00] Lucky Mackerel disengaged from the battle.");
        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles.len(), 1);
        assert_eq!(voy.battles[0].outcome, BattleOutcome::Disengaged);
        assert!(voy.battles[0].grappled_at.is_none()); // never boarded
        assert!(voy.current_battle.is_none());
    }

    #[test]
    fn back_to_back_fights_both_recorded() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[00:20:00] Going aboard the War Frigate...");
        gs.process_line("[00:20:01] This vessel is now Pillaging, Hard Barbarians.");
        gs.process_line("[00:20:02] Playerone issued an order to set the vessel to sail.");
        // Lose to the Boring Gar...
        gs.process_line("[00:20:49] You have been intercepted by the Boring Gar!");
        gs.process_line("[00:33:03] Game over.  Winners: Nervy Hugh, Insane Yang.");
        gs.process_line(
            "[00:33:11] The victors plundered 27,460 pieces of eight and no goods from the defeated vessel.",
        );
        // ...then immediately re-engage the same vessel and win.
        gs.process_line("[00:33:25] You intercepted the Boring Gar!");
        gs.process_line("[00:44:00] Game over.  Winners: Playerone.");
        gs.process_line(
            "[00:44:01] The victors plundered 5,000 pieces of eight and 2 units of goods from the defeated vessel.",
        );
        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles.len(), 2); // both kept (no confirm-window data loss)
        assert_eq!(voy.battles[0].outcome, BattleOutcome::Lost);
        assert_eq!(voy.battles[0].poe, Some(-27_460));
        assert_eq!(voy.battles[0].goods, Some(0));
        assert_eq!(voy.battles[1].outcome, BattleOutcome::Won);
        assert!(voy.battles[1].we_intercepted);
        assert_eq!(voy.battles[1].poe, Some(5_000));
    }

    #[test]
    fn categorizes_brigand_king_and_defaults_to_brigand() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the War Frigate...");
        gs.process_line("[01:00:01] This vessel is now Pillaging, Hard Barbarians.");
        gs.process_line("[01:00:02] Playerone issued an order to set the vessel to sail.");
        // An ordinary fight -> generic Brigand.
        gs.process_line("[01:00:10] You intercepted the Fat Mackerel!");
        gs.process_line("[01:02:00] Game over.  Winners: Playerone.");
        // A king fight: engagement chant tags it, victory line confirms the name.
        gs.process_line("[01:03:00] You have been intercepted by the Simple Ling!");
        gs.process_line(
            "[01:03:01] Brace yourself! Vargas the Mad and his barbaric horde are looking for a rumble!",
        );
        gs.process_line("[01:08:00] Game over.  Winners: Playerone.");
        gs.process_line("[01:08:01] Vargas the Mad's ship disappears into the mists.");

        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles[0].category, BattleCategory::Brigand);
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
        gs.process_line("[01:00:01] This vessel is now Pillaging, Hard Barbarians.");
        gs.process_line("[01:00:02] Playerone issued an order to set the vessel to sail.");
        gs.process_line("[01:00:10] You intercepted the Bloodstained Tigerfish!");
        gs.process_line(
            "[01:00:11] Avast! Yer blood runs cold beneath a gathering gloom and the air is a-flutter with leathern wings! Guard yer throat, a Vampirate vessel closes in!",
        );
        gs.process_line("[01:02:00] Game over.  Winners: Playerone.");
        gs.process_line("[01:03:00] You intercepted the Snarling Pike!");
        gs.process_line(
            "[01:03:01] Unearthly howling echos o'er the waves, moonlight glints off curving fangs and hungry eyes watch ye from the dark! Beware! Werewolves have caught yer scent!",
        );
        gs.process_line("[01:05:00] Game over.  Winners: Playerone.");

        let voy = gs.current_voyage().unwrap();
        assert_eq!(voy.battles[0].category, BattleCategory::Vampirate);
        assert_eq!(voy.battles[1].category, BattleCategory::Werewolf);
    }
}
