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

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

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
    /// Greedy strikes tallied per attacking pirate.
    pub greedy_by_pirate: HashMap<String, u32>,
    /// Jobbers we (the player) planked, in order.
    pub planked_by_us: Vec<String>,
    /// We left this vessel mid-run, so its data has gaps.
    pub poisoned: bool,
}

impl Vessel {
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
}

impl GameState {
    pub fn new() -> Self {
        Self {
            player_name: None,
            attached: false,
            vessels: HashMap::new(),
            current: None,
            online: HashSet::new(),
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

        // Date header => the player relogged: wipe the whole state.
        if line.starts_with("======") {
            self.on_relog();
            return;
        }

        // Everything else is `[HH:MM:SS] <body>`.
        let Some(body) = strip_timestamp(line) else {
            return;
        };
        self.classify(body);
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

        // Others boarding / leaving the vessel.
        if let Some(name) = body.strip_suffix(" has come aboard.") {
            if let Some(v) = self.current_vessel_mut() {
                v.crewmates.insert(name.to_string());
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
    /// except configuration (player name stays).
    fn on_relog(&mut self) {
        self.vessels.clear();
        self.current = None;
        self.online.clear();
    }

    /// Player went aboard a vessel. Keeps existing data if we're returning to a
    /// vessel we've seen (including its poisoned flag).
    fn on_board_vessel(&mut self, ship: &str) {
        let key: Arc<str> = Arc::from(ship);
        self.vessels.entry(key.clone()).or_default();
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

    /// A greedy strike landed by `attacker`.
    fn on_greedy_strike(&mut self, attacker: &str) {
        if let Some(v) = self.current_vessel_mut() {
            *v.greedy_by_pirate.entry(attacker.to_string()).or_insert(0) += 1;
        }
    }

    /// Mutable access to the vessel we're currently aboard.
    fn current_vessel_mut(&mut self) -> Option<&mut Vessel> {
        let cur = self.current.clone()?;
        self.vessels.get_mut(&cur)
    }

    /// The vessel we're currently aboard, if any.
    pub fn current_vessel(&self) -> Option<&Vessel> {
        let cur = self.current.as_ref()?;
        self.vessels.get(cur)
    }
}

impl Default for GameState {
    fn default() -> Self {
        Self::new()
    }
}

/// Strip the `[HH:MM:SS] ` prefix, returning the message body.
fn strip_timestamp(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(rest[end + 1..].trim_start())
}

// ---------------------------------------------------------------------------
// Streaming tailer
// ---------------------------------------------------------------------------

/// Spawn a background task that tails `path` from `start_offset`, sending each
/// complete (newline-terminated) line over `tx`. Lines are decoded lossily and
/// stripped of trailing CR/LF. A trailing incomplete line is buffered until its
/// newline arrives. Stops when the receiver is dropped.
pub fn spawn_tailer(
    path: String,
    start_offset: u64,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    tokio::task::spawn_blocking(move || {
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
// Rendering (placeholder text wall — interface TBD)
// ---------------------------------------------------------------------------

fn join_sorted(set: &HashSet<String>) -> String {
    if set.is_empty() {
        return "-".to_string();
    }
    let mut v: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
    v.sort_unstable();
    v.join(", ")
}

pub fn render(frame: &mut Frame, area: Rect, state: &GameState, focused: bool) {
    let border_style = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut lines: Vec<Line> = Vec::new();

    if !state.attached {
        lines.push(Line::from(
            "No chat log attached. Pass --chat-log <PATH> to monitor a game log.",
        ));
    } else {
        lines.push(Line::from(format!(
            "Pirate: {}",
            state
                .player_name
                .as_deref()
                .unwrap_or("(unknown — pass --user)")
        )));
        lines.push(Line::from(format!(
            "Vessel: {}",
            state.current.as_deref().unwrap_or("ashore")
        )));

        if let Some(v) = state.current_vessel() {
            lines.push(Line::from(format!(
                "Job:    {}{}",
                v.job_kind
                    .as_ref()
                    .map(|j| j.to_string())
                    .unwrap_or_else(|| "(none)".to_string()),
                if v.poisoned { "   [POISONED]" } else { "" },
            )));

            lines.push(Line::from(""));
            lines.push(Line::from(format!(
                "Crewmates aboard ({}): {}",
                v.crewmates.len(),
                join_sorted(&v.crewmates),
            )));
            lines.push(Line::from(format!(
                "Planked by us ({}): {}",
                v.planked_by_us.len(),
                if v.planked_by_us.is_empty() {
                    "-".to_string()
                } else {
                    v.planked_by_us.join(", ")
                },
            )));

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("Greedy strikes ({} total, top 15)", v.total_greedy()),
                Style::default().bold(),
            )));
            let mut tallies: Vec<(&String, &u32)> = v.greedy_by_pirate.iter().collect();
            tallies.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            if tallies.is_empty() {
                lines.push(Line::from("  -"));
            }
            for (name, count) in tallies.into_iter().take(15) {
                lines.push(Line::from(format!("  {:<18} {:>4}", name, count)));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "Online ({}): {}",
            state.online.len(),
            join_sorted(&state.online),
        )));

        if !state.vessels.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Vessels this session",
                Style::default().bold(),
            )));
            let mut names: Vec<&Arc<str>> = state.vessels.keys().collect();
            names.sort_unstable();
            for name in names {
                let v = &state.vessels[name];
                lines.push(Line::from(format!(
                    "  {}{}",
                    name,
                    if v.poisoned { "  [poisoned]" } else { "" },
                )));
            }
        }
    }

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("─── Chat Log "),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(widget, area);
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
        gs.process_line("[14:56:16] Suryavab has come aboard.");
        gs.process_line("[14:56:24] This vessel is now Pillaging, Average to Hard Barbarians.");
        gs.process_line(
            "[14:56:27] Jayotoa delivers an overwhelming barrage against Hunched Alice, \
             causing some treasure to fall from their grip!",
        );
        gs.process_line(
            "[14:56:28] Jayotoa executes a masterful strike against Demented Carlos, \
             who drops some treasure in surprise!",
        );

        assert_eq!(gs.current.as_deref(), Some("Test Vessel"));
        let v = gs.current_vessel().unwrap();
        assert!(v.crewmates.contains("Suryavab"));
        assert_eq!(v.greedy_by_pirate.get("Jayotoa"), Some(&2));
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
        gs.process_line("[01:01:00] Oopzz has come aboard.");
        gs.process_line("[01:02:00] Playerone forced Oopzz to walk the plank.");
        let v = gs.current_vessel().unwrap();
        assert_eq!(v.planked_by_us, vec!["Oopzz".to_string()]);
        assert!(!v.crewmates.contains("Oopzz"));
    }

    #[test]
    fn relog_wipes_state() {
        let mut gs = GameState::new();
        gs.player_name = Some(Arc::from("Playerone"));
        gs.process_line("[01:00:00] Going aboard the Sugared Bass...");
        gs.process_line("[01:01:00] Yer crew member Akela has logged on.");
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
        gs.process_line("[01:00:00] Yer crew member Akela has logged on.");
        gs.process_line("[01:00:01] Yer hearty, Carolking, has logged on.");
        gs.process_line("[01:00:02] Yer crew member Akela has logged off.");
        assert!(!gs.online.contains("Akela"));
        assert!(gs.online.contains("Carolking"));
    }
}
