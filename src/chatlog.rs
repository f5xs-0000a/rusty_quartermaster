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

    /// Crewmates/hearties currently logged on (global; wiped on relog).
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

        // Battle start: a fresh battle resets the current-battle greedy tally.
        if body.starts_with("You intercepted") || body.starts_with("You have been intercepted") {
            self.on_battle_start();
            return;
        }

        // Battle end: "Game over.  Winners: a, b, Playerone." — if we're in the
        // winning side, it's an authoritative roster of who's aboard.
        if let Some(summary) = body.strip_prefix("Game over.") {
            self.on_battle_end(summary.trim_start());
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
            if let Some(v) = self.current_vessel_mut() {
                v.job_kind = None;
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
            }
            return;
        }
        if let Some(name) = body.strip_suffix(" has left the vessel.") {
            if let Some(v) = self.current_vessel_mut() {
                v.crewmates.remove(name);
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

        // Third-person plank: "<Planker> forced <Victim> to walk the plank."
        if let Some(mid) = body.strip_suffix(" to walk the plank.") {
            if let Some((planker, victim)) = mid.split_once(" forced ") {
                self.on_plank(planker, victim);
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

    /// A new battle began — start a fresh current-battle greedy tally.
    fn on_battle_start(&mut self) {
        if let Some(v) = self.current_vessel_mut() {
            v.greedy_current.clear();
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

    /// Every distinct pirate name we've recorded — the fetch worklist. NPCs and
    /// other unparseable names are filtered out later by name normalization.
    pub fn all_pirate_names(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for v in self.vessels.values() {
            names.extend(v.crewmates.iter().cloned());
            names.extend(v.greedy_by_pirate.keys().cloned());
            names.extend(v.planked_by_us.iter().cloned());
        }
        names.extend(self.online.iter().cloned());
        names
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
}
