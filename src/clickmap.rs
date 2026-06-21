use ratatui::prelude::Rect;

use crate::damage::Side;
use crate::jobbers::JobberPane;

#[derive(Clone, Debug)]
pub enum ClickTarget {
    SidebarItem(usize),
    ProfitsInput,
    ProfitsTableCell { row: usize, col: usize },
    ProfitsPanel(usize),
    ProfitsButton,
    ProfitsPopupNo,
    ProfitsPopupYes,
    ProfitsPopupOk,
    DamageCell { row: usize, side: Side },
    DamageIncrement { row: usize, side: Side },
    DamageDecrement { row: usize, side: Side },
    DamageHeadon,
    DamageHeadonIncrement,
    DamageHeadonDecrement,
    DamageButton(usize),
    DamageShipItem(usize),
    JobberVesselButton,
    JobberVesselItem(usize),
    JobberShipType,
    JobberShipItem(usize),
    JobberVoyageType,
    JobberVoyageItem(usize),
    JobberUnpoison,
    /// The Skill Leaderboard panel as a whole (focus it).
    JobberLeaderboard,
    /// A pirate row in the Skill Leaderboard: `col` is the column index, `row` the
    /// rank within that column. Selecting it can open the pirate-stats popup.
    JobberLeaderboardPirate { col: usize, row: usize },
    JobberAboardList,
    JobberGreedyList,
    JobberPlankedList,
    JobberPirate { pane: JobberPane, idx: usize },
    JobberPirateSeeTrophies,
    JobberPirateClose,
    JobberTrophyArea,
    /// The "View Skill Distribution" button (Vampirates).
    JobberSkillDistButton,
    /// A cell in the skill-distribution scatterplot, `(treasure_haul, carpentry)`
    /// standing indices. Hovering or clicking it moves the cursor there.
    JobberSkillDistCell { th: u8, carp: u8 },
    /// The backdrop behind the skill-distribution popup; clicking it closes.
    JobberSkillDistClose,
    /// The "save voyage to history" prompt opener on the Voyage Statistics page.
    VoyageSaveOpen,
    /// Buttons inside the save/discard prompt.
    VoyageSaveConfirm,
    VoyageSaveDiscard,
    /// The backdrop / cancel of the save/discard prompt.
    VoyageSaveCancel,
}

#[derive(Clone)]
pub struct ClickRegion {
    pub rect: Rect,
    pub target: ClickTarget,
}

/// Reverse-iterates so popup regions (pushed last) get priority.
pub fn hit_test(regions: &[ClickRegion], col: u16, row: u16) -> Option<ClickTarget> {
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
