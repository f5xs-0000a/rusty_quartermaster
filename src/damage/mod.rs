pub mod ui;

use std::path::PathBuf;
use std::process::Command;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::InputResult;
use crate::clickmap::ClickTarget;
use crate::ships::SHIPS;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Left,
    Right,
}

pub struct ShipSelectPopup {
    pub side: Side,
    pub selected: usize,
}

const ROCK_DAMAGE: u64 = 480;

pub const ROW_SHIP: usize = 0;
pub const ROW_SHOTS: usize = 1;
pub const ROW_ROCKS: usize = 2;
pub const ROW_RAMS: usize = 3;
pub const ROW_HEADON: usize = 4;
pub const ROW_GAP: usize = 5;
pub const ROW_SHOTS_LEFT: usize = 6;
pub const ROW_DAMAGE: usize = 7;
pub const ROW_COUNT: usize = 8;
const LAST_INTERACTIVE_ROW: usize = 4;

/// Labels for the center column. Head-on Collisions is rendered separately.
pub const CENTER_LABELS: &[&str] = &[
    "Ship",
    "Shots Taken",
    "Rocks Banged",
    "Times Rammed",
    "",
    "",
    "Shots Left",
    "Damage",
];

// ---------------------------------------------------------------------------
// DamageApp
// ---------------------------------------------------------------------------

pub struct DamageApp {
    pub left_ship: usize,
    pub right_ship: usize,
    pub left: [u32; 3],
    pub right: [u32; 3],
    pub headon: u32,
    pub focus_row: usize,
    pub focus_side: Side,
    pub popup: Option<ShipSelectPopup>,
    /// When `Some`, the "Reset values?" confirm (shown after a ship change) is
    /// open; the bool is the focused choice (`true` = Yes, the default).
    pub reset_prompt: Option<bool>,
    pub temp_images: Vec<Option<PathBuf>>,
}

impl Default for DamageApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DamageApp {
    pub fn new() -> Self {
        Self {
            left_ship: 0,
            right_ship: 0,
            left: [0; 3],
            right: [0; 3],
            headon: 0,
            focus_row: 0,
            focus_side: Side::Left,
            popup: None,
            reset_prompt: None,
            temp_images: vec![None; SHIPS.len()],
        }
    }

    // -- damage calculation --

    /// Total raw damage taken by `side` from shots + rocks + rams + head-on.
    fn total_damage(&self, side: Side) -> u64 {
        let (values, own_ship, other_ship) = match side {
            Side::Left => (&self.left, &SHIPS[self.left_ship], &SHIPS[self.right_ship]),
            Side::Right => (&self.right, &SHIPS[self.right_ship], &SHIPS[self.left_ship]),
        };

        let shot_dmg = values[0] as u64 * other_ship.cannon_size.damage() as u64;
        let rock_dmg = values[1] as u64 * ROCK_DAMAGE;
        let ram_dmg = values[2] as u64 * other_ship.ram_damage as u64;

        let headon_mult: u64 =
            if own_ship.ship_size_class != other_ship.ship_size_class { 2 } else { 1 };
        let headon_dmg = self.headon as u64 * other_ship.ram_damage as u64 * headon_mult;

        shot_dmg + rock_dmg + ram_dmg + headon_dmg
    }

    /// Returns (morale_percent, hull_percent) for the given side.
    pub fn calculate_damage(&self, side: Side) -> (u32, u32) {
        let own_ship = match side {
            Side::Left => &SHIPS[self.left_ship],
            Side::Right => &SHIPS[self.right_ship],
        };

        let total = self.total_damage(side);

        let morale_pct = (total * 100 / own_ship.morale_hp as u64).min(100) as u32;
        let hull_pct = (total * 100 / own_ship.hull_hp as u64).min(100) as u32;

        (morale_pct, hull_pct)
    }

    // -- Ship-advantage metrics (Left = our ship, Right = the foe) --

    /// A ship's combat advantage from its morale damage: 1.0 (100%) when healthy,
    /// 0.5 (50%) when fully morale-damaged, linear in between.
    pub fn ship_advantage(&self, side: Side) -> f64 {
        let morale_pct = self.calculate_damage(side).0;
        1.0 - morale_pct as f64 / 200.0
    }

    /// Damage advantage = our advantage − the foe's. Range `[-0.5, +0.5]`.
    pub fn advantage_dmg(&self) -> f64 {
        self.ship_advantage(Side::Left) - self.ship_advantage(Side::Right)
    }

    /// Headcount advantage = our crew × our advantage − their crew × the foe's
    /// advantage, each side's headcount weighted by its ship's combat strength.
    pub fn crew_advantage(&self, ours: u32, theirs: u32) -> f64 {
        ours as f64 * self.ship_advantage(Side::Left)
            - theirs as f64 * self.ship_advantage(Side::Right)
    }

    /// [`Self::crew_advantage`] against the Right ship type's pirate capacity — the
    /// estimate used when the real foe headcount isn't known.
    pub fn advantage_crew(&self, our_pirates: u32) -> f64 {
        self.crew_advantage(our_pirates, SHIPS[self.right_ship].max_pirates as u32)
    }

    /// Capture the current calculator state as a [`BattleSnapshot`] for the Sea
    /// Battles record. `our_pirates` is the live crew count at melee start.
    pub fn snapshot(&self, our_pirates: u32) -> crate::voyage::BattleSnapshot {
        crate::voyage::BattleSnapshot {
            our_ship: self.left_ship,
            foe_ship: self.right_ship,
            our_hits: self.left,
            foe_hits: self.right,
            headon: self.headon,
            our_pirates,
        }
    }

    /// Build a (non-interactive) calculator from a recorded snapshot, so the Sea
    /// Battles popup can reuse the damage/advantage math to render it read-only.
    pub fn from_snapshot(s: &crate::voyage::BattleSnapshot) -> Self {
        let mut app = Self::new();
        app.left_ship = s.our_ship;
        app.right_ship = s.foe_ship;
        app.left = s.our_hits;
        app.right = s.foe_hits;
        app.headon = s.headon;
        app
    }

    /// Whether any hits have been entered (so we only snapshot a fight we tracked).
    pub fn has_input(&self) -> bool {
        self.headon > 0 || self.left.iter().chain(&self.right).any(|&n| n > 0)
    }

    /// Clear the hit counts (keep ship selections) — ready for the next fight.
    pub fn clear_counts(&mut self) {
        self.left = [0; 3];
        self.right = [0; 3];
        self.headon = 0;
    }

    /// Returns (shots-to-max-morale, shots-to-sink) for `side`: how many more
    /// shots from the opposing ship's cannons it would take, given the damage
    /// already entered. Each saturates at 0 once that threshold is reached.
    pub fn shots_left(&self, side: Side) -> (u32, u32) {
        let (own_ship, other_ship) = match side {
            Side::Left => (&SHIPS[self.left_ship], &SHIPS[self.right_ship]),
            Side::Right => (&SHIPS[self.right_ship], &SHIPS[self.left_ship]),
        };

        let cannon_dmg = other_ship.cannon_size.damage() as u64;
        let total = self.total_damage(side);

        let to_morale = (own_ship.morale_hp as u64).saturating_sub(total);
        let to_hull = (own_ship.hull_hp as u64).saturating_sub(total);

        (to_morale.div_ceil(cannon_dmg) as u32, to_hull.div_ceil(cannon_dmg) as u32)
    }

    fn values_mut(&mut self, side: Side) -> &mut [u32; 3] {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    pub fn increment(&mut self) {
        if self.focus_row == ROW_HEADON {
            self.headon = self.headon.saturating_add(1);
            return;
        }
        let idx = self.focus_row - 1;
        let vals = self.values_mut(self.focus_side);
        vals[idx] = vals[idx].saturating_add(1);
    }

    pub fn decrement(&mut self) {
        if self.focus_row == ROW_HEADON {
            self.headon = self.headon.saturating_sub(1);
            return;
        }
        let idx = self.focus_row - 1;
        let vals = self.values_mut(self.focus_side);
        vals[idx] = vals[idx].saturating_sub(1);
    }

    // -- ship image viewing --

    fn view_ship(&mut self, idx: usize) {
        let path = if let Some(ref path) = self.temp_images[idx] {
            path.clone()
        } else {
            let name = SHIPS[idx].name.to_lowercase().replace(' ', "_");
            let path = std::env::temp_dir().join(format!("ratatui-ship-{}.png", name));
            if std::fs::write(&path, SHIPS[idx].image_data).is_err() {
                return;
            }
            self.temp_images[idx] = Some(path.clone());
            path
        };

        Command::new("xdg-open")
            .arg(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
    }

    pub fn cleanup_temp_images(&mut self) {
        for slot in &mut self.temp_images {
            if let Some(path) = slot.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    // -- key handling --

    pub fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        if self.reset_prompt.is_some() {
            return self.handle_reset_prompt_key(key);
        }

        if key.code == KeyCode::Esc {
            return self.handle_esc();
        }

        if self.popup.is_some() {
            return self.handle_popup_key(key);
        }

        match key.code {
            KeyCode::Up => {
                if 0 < self.focus_row {
                    self.focus_row -= 1;
                } else {
                    // Already at the top row — hand focus back to the top bar.
                    return InputResult::Exit;
                }
            }
            KeyCode::Down => {
                if self.focus_row < LAST_INTERACTIVE_ROW {
                    self.focus_row += 1;
                }
            }
            KeyCode::Left => {
                if self.focus_side == Side::Right && self.focus_row != ROW_HEADON {
                    self.focus_side = Side::Left;
                }
            }
            KeyCode::Right => {
                if self.focus_side == Side::Left && self.focus_row != ROW_HEADON {
                    self.focus_side = Side::Right;
                }
            }
            KeyCode::Enter if self.focus_row == ROW_SHIP => {
                let current = match self.focus_side {
                    Side::Left => self.left_ship,
                    Side::Right => self.right_ship,
                };
                self.popup = Some(ShipSelectPopup {
                    side: self.focus_side,
                    selected: current,
                });
            }
            KeyCode::Char(' ') if self.focus_row == ROW_SHIP => {
                let current = match self.focus_side {
                    Side::Left => self.left_ship,
                    Side::Right => self.right_ship,
                };
                self.popup = Some(ShipSelectPopup {
                    side: self.focus_side,
                    selected: current,
                });
            }
            KeyCode::Enter | KeyCode::Char(' ') if 0 < self.focus_row => {
                self.increment();
            }
            KeyCode::Backspace if 0 < self.focus_row => {
                self.decrement();
            }
            _ => {}
        }
        InputResult::Consumed
    }

    fn handle_esc(&mut self) -> InputResult {
        if self.popup.is_some() {
            self.popup = None;
            return InputResult::Consumed;
        }
        InputResult::Exit
    }

    /// Keys for the "Reset values?" confirm (default Yes). Enter/Space/Y on Yes
    /// clears the hit counts; No / N / Esc dismisses it keeping the values.
    fn handle_reset_prompt_key(&mut self, key: KeyEvent) -> InputResult {
        match key.code {
            KeyCode::Left | KeyCode::Right => {
                if let Some(yes) = self.reset_prompt.as_mut() {
                    *yes = !*yes;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.reset_prompt == Some(true) {
                    self.clear_counts();
                }
                self.reset_prompt = None;
            }
            KeyCode::Char('y' | 'Y') => {
                self.clear_counts();
                self.reset_prompt = None;
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.reset_prompt = None;
            }
            _ => {}
        }
        InputResult::Consumed
    }

    fn handle_popup_key(&mut self, key: KeyEvent) -> InputResult {
        let Some(ref mut popup) = self.popup else {
            return InputResult::Consumed;
        };

        match key.code {
            KeyCode::Up => {
                if 0 < popup.selected {
                    popup.selected -= 1;
                }
            }
            KeyCode::Down => {
                if popup.selected + 1 < SHIPS.len() {
                    popup.selected += 1;
                }
            }
            KeyCode::Enter => {
                let idx = popup.selected;
                match popup.side {
                    Side::Left => self.left_ship = idx,
                    Side::Right => self.right_ship = idx,
                }
                self.popup = None;
                // Changing ship invalidates the tallies — offer to reset (Yes default).
                self.reset_prompt = Some(true);
            }
            KeyCode::Char('v') => {
                let idx = popup.selected;
                self.view_ship(idx);
            }
            _ => {}
        }
        InputResult::Consumed
    }
}

/// Apply a Damage-calculator click `target` to `app`, returning true iff it was
/// a `Damage*` variant (so the caller knows it was consumed). Mirrors the live
/// page's click arms but owns no global focus — used by the Sea Battles editor.
pub fn apply_click(app: &mut DamageApp, target: &ClickTarget) -> bool {
    match *target {
        ClickTarget::DamageCell { row, side } => {
            app.popup = None;
            if row == ROW_SHIP {
                let current = match side {
                    Side::Left => app.left_ship,
                    Side::Right => app.right_ship,
                };
                app.popup = Some(ShipSelectPopup { side, selected: current });
            } else {
                app.focus_row = row;
                app.focus_side = side;
            }
        }
        ClickTarget::DamageIncrement { row, side } => {
            app.popup = None;
            app.focus_row = row;
            app.focus_side = side;
            app.increment();
        }
        ClickTarget::DamageDecrement { row, side } => {
            app.popup = None;
            app.focus_row = row;
            app.focus_side = side;
            app.decrement();
        }
        ClickTarget::DamageHeadon => {
            app.popup = None;
            app.focus_row = ROW_HEADON;
        }
        ClickTarget::DamageHeadonIncrement => {
            app.popup = None;
            app.focus_row = ROW_HEADON;
            app.increment();
        }
        ClickTarget::DamageHeadonDecrement => {
            app.popup = None;
            app.focus_row = ROW_HEADON;
            app.decrement();
        }
        ClickTarget::DamageShipItem(i) => {
            if let Some(ref popup) = app.popup {
                match popup.side {
                    Side::Left => app.left_ship = i,
                    Side::Right => app.right_ship = i,
                }
                app.popup = None;
                app.reset_prompt = Some(true);
            }
        }
        ClickTarget::DamageResetYes => {
            app.clear_counts();
            app.reset_prompt = None;
        }
        ClickTarget::DamageResetNo => {
            app.reset_prompt = None;
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // `DamageApp::new` starts with both sides as the first ship (Sloop): Small
    // cannons (960 dmg), morale_hp 5760, hull_hp 9600.
    const CANNON: u32 = 960;
    const MORALE_HP: u32 = 5760;
    const HULL_HP: u32 = 9600;

    #[test]
    fn advantage_metrics() {
        let mut app = DamageApp::new(); // both Sloops, no damage
        assert!(!app.has_input());
        // No damage: both ships fully healthy, so damage advantage is zero.
        assert!((app.advantage_dmg() - 0.0).abs() < 1e-9);
        // Crew: our pirates at full advantage minus the foe Sloop's complement.
        let foe = SHIPS[app.right_ship].max_pirates as f64;
        assert!((app.advantage_crew(5) - (5.0 - foe)).abs() < 1e-9);

        // Land 3 shots on the foe (Right): 3*960 = 2880 = 50% of 5760 morale.
        app.right[0] = 3;
        assert!(app.has_input());
        assert_eq!(app.calculate_damage(Side::Right).0, 50);
        // Their advantage drops to 0.75, ours stays 1.0 -> +0.25.
        assert!((app.advantage_dmg() - 0.25).abs() < 1e-9);

        app.clear_counts();
        assert!(!app.has_input());
        assert!((app.advantage_dmg() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn shots_left_counts_down_from_full_health() {
        let app = DamageApp::new();
        // ceil(5760/960)=6 to max morale, ceil(9600/960)=10 to sink.
        assert_eq!(app.shots_left(Side::Left), (6, 10));
        assert_eq!(MORALE_HP.div_ceil(CANNON), 6);
        assert_eq!(HULL_HP.div_ceil(CANNON), 10);
    }

    #[test]
    fn shots_left_accounts_for_damage_already_taken() {
        let mut app = DamageApp::new();
        app.left[0] = 1; // one shot taken: 960 damage
        // remaining morale 4800 -> 5 shots, remaining hull 8640 -> 9 shots.
        assert_eq!(app.shots_left(Side::Left), (5, 9));
    }

    #[test]
    fn shots_left_saturates_at_zero_once_maxed() {
        let mut app = DamageApp::new();
        app.left[0] = 6; // 5760 damage = morale capped
        // morale exhausted -> 0; hull has 3840 left -> 4 shots.
        assert_eq!(app.shots_left(Side::Left), (0, 4));
    }
}
