use ratatui::prelude::Rect;

use crate::{damage::Side, jobbers::JobberPane};

/// A view the user scrolls, named so that a click on its scrollbar can be
/// routed back to whatever the view scrolls with — its own window for some, a
/// cursor the window follows for others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollView {
    /// The Profits Inventory table. Follows the cell cursor.
    ProfitsInventory,
    /// One of the Jobbers pirate panes. Follows that pane's selection.
    JobberPane(JobberPane),
    /// The Jobbers Skill Leaderboard, whose columns share one window. Follows
    /// the ranked selection.
    JobberLeaderboard,
    /// The skill tables in the pirate-stats popup. Keeps its own window.
    JobberPirateSkills,
    /// The category grid in the trophies popup. Keeps its own window.
    JobberTrophies,
    /// The text in the note editor, for a note longer than the box it grew to.
    /// Follows the caret as it is typed.
    JobberNoteText,
    /// Everything under the Voyage Statistics page's pinned header. Keeps its
    /// own window, which the focused stat or chart also nudges.
    VoyageBody,
    /// The save prompt's boxes and the figures under them. Keeps its own
    /// window, which the focused box also nudges.
    VoyageSavePrompt,
    /// The Map page's chart, a viewport on a canvas larger than it both ways.
    /// Follows the sailing cursor.
    MapCanvas,
    /// The Map page's Island column: what is known about the point under the
    /// cursor. Keeps its own window, which starts at the top of each island
    /// the cursor is put on.
    MapIslandInfo,
    /// The Map page's help popup, in a window too short for all of it. Keeps
    /// its own offset, which opens at the top of the help.
    MapHelp,
}

/// Which way a scrollbar runs. A view that scrolls both ways has one of each,
/// and a click says which it landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

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
    /// Yes / No on the "Reset Values?" confirm shown after a ship change.
    DamageResetYes,
    DamageResetNo,
    /// Apply / Keep on the "New Battle" prompt shown when a fight begins:
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
    /// The Pirate popup's Add / Edit Note button, which opens the note editor.
    JobberPirateNote,
    JobberPirateClose,
    /// The note editor's buttons: Save writes the note down, Cancel leaves what
    /// was written before it alone.
    JobberNoteSave,
    JobberNoteCancel,
    JobberTrophyArea,
    /// The Trophies popup's Close button.
    JobberTrophyClose,
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
    /// No / Yes on the prompt asking whether a copied duty report's pirates
    /// should join the roster.
    JobberRosterNo,
    JobberRosterYes,
    /// Previous / next selectable voyage in the Voyage Statistics pager.
    VoyagePrev,
    VoyageNext,
    /// The "save voyage to history" prompt opener on the Voyage Statistics
    /// page.
    VoyageSaveOpen,
    /// The Save button inside the save prompt.
    VoyageSaveConfirm,
    /// Its Cancel button, and the backdrop behind the prompt.
    VoyageSaveCancel,
    /// One of the save prompt's checkboxes (`idx` into
    /// [`crate::voyage::ui::SAVE_PARTS`]); clicking it flips that part.
    VoyageSavePart {
        idx: usize,
    },
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
    /// A league point or island drawn on the Map page; clicking it moves the
    /// cursor there.
    MapPoint {
        x: u16,
        y: u16,
    },
    /// The backdrop behind the Map page's help popup; clicking it closes.
    MapHelpClose,
    /// The Map page's Island column, so the wheel over it scrolls what it says
    /// instead of panning the chart.
    MapIslandInfo,
    /// A view's scrollbar. The hit test hands back no geometry of its own, so
    /// the bar carries the rect it was drawn into — the click's row within it
    /// is the whole of what the click says — and `total`, the rows the view
    /// held when it was drawn, which is what its last offset is counted
    /// from.
    Scrollbar {
        view: ScrollView,
        axis: ScrollAxis,
        bar: Rect,
        total: usize,
    },
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
