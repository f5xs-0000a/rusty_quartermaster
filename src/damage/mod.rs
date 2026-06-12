pub mod ui;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::InputResult;
use crate::ships::SHIPS;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
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
pub const ROW_DAMAGE: usize = 5;
pub const ROW_COUNT: usize = 6;
const LAST_INTERACTIVE_ROW: usize = 4;

/// Labels for the center column. Head-on Collisions is rendered separately.
pub const CENTER_LABELS: &[&str] = &[
    "Ship",
    "Shots Taken",
    "Rocks Banged",
    "Times Rammed",
    "",
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
        }
    }

    // -- damage calculation --

    /// Returns (morale_percent, hull_percent) for the given side.
    pub fn calculate_damage(&self, side: Side) -> (u32, u32) {
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

        let total = shot_dmg + rock_dmg + ram_dmg + headon_dmg;

        let morale_pct = (total * 100 / own_ship.morale_hp as u64).min(100) as u32;
        let hull_pct = (total * 100 / own_ship.hull_hp as u64).min(100) as u32;

        (morale_pct, hull_pct)
    }

    fn values_mut(&mut self, side: Side) -> &mut [u32; 3] {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn increment(&mut self) {
        if self.focus_row == ROW_HEADON {
            self.headon = self.headon.saturating_add(1);
            return;
        }
        let idx = self.focus_row - 1;
        let vals = self.values_mut(self.focus_side);
        vals[idx] = vals[idx].saturating_add(1);
    }

    fn decrement(&mut self) {
        if self.focus_row == ROW_HEADON {
            self.headon = self.headon.saturating_sub(1);
            return;
        }
        let idx = self.focus_row - 1;
        let vals = self.values_mut(self.focus_side);
        vals[idx] = vals[idx].saturating_sub(1);
    }

    // -- key handling --

    pub fn handle_key(&mut self, key: KeyEvent) -> InputResult {
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
                } else {
                    return InputResult::Exit;
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
            }
            _ => {}
        }
        InputResult::Consumed
    }
}
