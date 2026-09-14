use ratatui::prelude::Rect;

use crate::{damage::Side, jobbers::JobberPane};

#[derive(Clone, Debug)]
pub enum ClickTarget {
    SidebarItem(usize),
    ProfitsInput,
    ProfitsTableCell {
        row: usize,
        col: usize,
    },
    ProfitsPanel(usize),
    ProfitsButton,
    ProfitsPopupNo,
    ProfitsPopupYes,
    ProfitsPopupOk,
    /// A row in the Profit Breakdown popup: hovering/clicking parks the tooltip
    /// cursor there (the index into
    /// [`crate::profits::ProfitResult::breakdown`]).
    ProfitsBreakdownRow(usize),
    DamageCell {
        row: usize,
        side: Side,
    },
    DamageIncrement {
        row: usize,
        side: Side,
    },
    DamageDecrement {
        row: usize,
        side: Side,
    },
    /// Times Rammed cell / +/- buttons — a single shared counter (one row, not
    /// per-side), so it has no `side` field like the standard damage rows.
    DamageRam,
    DamageRamIncrement,
    DamageRamDecrement,
    DamageShipItem(usize),
    /// Yes / No on the "Reset values?" confirm shown after a ship change.
    DamageResetYes,
    DamageResetNo,
    /// Apply / Keep on the "New battle" prompt shown when a fight begins:
    /// Apply seeds the foe hull and clears the tally, Keep leaves it untouched.
    DamageBattleApply,
    DamageBattleKeep,
    JobberVesselButton,
    JobberVesselItem(usize),
    JobberShipType,
    JobberShipItem(usize),
    JobberVoyageType,
    JobberVoyageItem(usize),
    JobberUnpoison,
    /// The Skill Leaderboard panel as a whole (focus it).
    JobberLeaderboard,
    /// A pirate row in the Skill Leaderboard: `col` is the column index, `row`
    /// the rank within that column. Selecting it can open the pirate-stats
    /// popup.
    JobberLeaderboardPirate {
        col: usize,
        row: usize,
    },
    JobberAboardList,
    JobberGreedyList,
    JobberPlankedList,
    JobberEnthralledList,
    JobberPirate {
        pane: JobberPane,
        idx: usize,
    },
    JobberPirateSeeTrophies,
    JobberPirateClose,
    JobberTrophyArea,
    /// The "View Skill Distribution" button (Vampirates).
    JobberSkillDistButton,
    /// A cell in the skill-distribution scatterplot, `(treasure_haul,
    /// carpentry)` standing indices. Hovering or clicking it moves the
    /// cursor there.
    JobberSkillDistCell {
        th: u8,
        carp: u8,
    },
    /// The backdrop behind the skill-distribution popup; clicking it closes.
    JobberSkillDistClose,
    /// The Cursed Isles "Show Per-Fight Statistics" button — opens the
    /// per-fight advantage-over-time graph popup.
    JobberPerFightButton,
    /// The backdrop behind the per-fight popup; clicking it closes.
    JobberPerFightClose,
    /// Toggle the per-fight graph's X-axis between wall-clock time and KO
    /// sequence.
    JobberPerFightAxisToggle,
    /// Previous / next fight (wave) in the per-fight popup.
    JobberPerFightPrev,
    JobberPerFightNext,
    /// Previous / next selectable voyage in the Voyage Statistics pager.
    VoyagePrev,
    VoyageNext,
    /// The "save voyage to history" prompt opener on the Voyage Statistics
    /// page.
    VoyageSaveOpen,
    /// Buttons inside the save/discard prompt.
    VoyageSaveConfirm,
    VoyageSaveDiscard,
    /// The backdrop / cancel of the save/discard prompt.
    VoyageSaveCancel,
    /// A focusable stat number on the Voyage Statistics page (`idx` into the
    /// page's focusable-stat list); clicking it focuses that stat's tooltip.
    VoyageStat {
        idx: usize,
    },
    /// A selectable mini-chart on the Voyage Statistics page.
    VoyageChart {
        idx: usize,
    },
    /// The backdrop behind an enlarged chart popup; clicking it closes.
    VoyageChartClose,
    /// A cell in the enlarged Ship Winrate matrix: `row` is our hull, `col` the
    /// enemy hull (both [`crate::ships::SHIPS`] indices). Hovering it
    /// highlights the cell and its row/column headers.
    VoyageWinrateCell {
        row: usize,
        col: usize,
    },
    /// The backdrop behind the Sea Battles popup; clicking it closes.
    VoyageBattlesClose,
    /// Previous / next page (fight) buttons in the Sea Battles popup.
    VoyageBattlesPrev,
    VoyageBattlesNext,
    /// The "Record" toggle for the currently-shown fight in the Sea Battles
    /// popup.
    VoyageBattlesRecord,
}

#[derive(Clone)]
pub struct ClickRegion {
    pub rect: Rect,
    pub target: ClickTarget,
}

/// Reverse-iterates so popup regions (pushed last) get priority.
pub fn hit_test(
    regions: &[ClickRegion],
    col: u16,
    row: u16,
) -> Option<ClickTarget> {
    regions.iter().rev().find_map(|r| {
        if r.rect.x <= col
            && col < r.rect.x + r.rect.width
            && r.rect.y <= row
            && row < r.rect.y + r.rect.height
        {
            Some(r.target.clone())
        } else {
            None
        }
    })
}
