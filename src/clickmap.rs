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
    /// The ranking in the Tokens and Chests box. Keeps its own window, there
    /// being no cursor in it to follow.
    JobberBoard,
    /// The Boochers box, whose two lists share one window. Keeps its own,
    /// there being no cursor in it to follow.
    JobberBoochers,
    /// The skill tables in the pirate-stats popup. Keeps its own window.
    JobberPirateSkills,
    /// The Duty Timelapse's strip in the same popup, a window on a run of
    /// reports wider than the popup. Counted in reports, and kept at the
    /// newest end unless the user has walked it back. Its bar is what walks
    /// it: the wheel over the popup is the body's, there being one thing a
    /// wheel can mean in a popup that scrolls.
    JobberPirateTimelapse,
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
    /// The Tokens and Chests box as a whole (focus it).
    JobberBoard,
    /// One of its tabs, by place in the strip.
    JobberBoardTab(usize),
    /// The head of one of its figure columns, the sum's counting last; a click
    /// ranks the board on it.
    JobberBoardColumn(usize),
    /// The Boochers box. A click gives it the keys; it has no cursor of its
    /// own for one to land on.
    JobberBoochers,
    JobberPirate {
        pane: JobberPane,
        idx: usize,
    },
    JobberPirateSeeTrophies,
    /// One report's column in the Pirate popup's Duty Timelapse, carrying
    /// which report of the run it is. The pointer over it dates that report,
    /// in the line under the strip.
    JobberPirateTimelapse(usize),
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

/// What a frame drew for the mouse, as the stack of layers that drew it.
///
/// The page draws into the bottom layer, and anything modal over it — a popup,
/// a prompt, a popup over a popup — opens another. Only the topmost layer is
/// live: a modal takes the frame while it is up, so nothing beneath it answers
/// to a click, a hover or the wheel, however much of it is still on show around
/// the edges. That is the same thing the keys already do, each modal eating
/// them until it is dismissed.
#[derive(Default)]
pub struct ClickMap {
    /// Never empty: the page's own layer is the first, in draw order within
    /// it.
    layers: Vec<Vec<ClickRegion>>,
}

impl ClickMap {
    pub fn new() -> Self {
        Self {
            layers: vec![Vec::new()],
        }
    }

    /// Drop every layer and start a fresh frame on the page's own.
    pub fn clear(&mut self) {
        self.layers.clear();
        self.layers.push(Vec::new());
    }

    /// Open a layer over the ones drawn so far: everything pushed from here on
    /// belongs to it, and nothing under it can be reached while it stands.
    pub fn layer(&mut self) {
        self.layers.push(Vec::new());
    }

    pub fn push(&mut self, region: ClickRegion) {
        self.layers
            .last_mut()
            .expect("the page's layer")
            .push(region);
    }

    /// What a click at `col`, `row` lands on: the last region drawn over that
    /// cell in the topmost layer, later regions winning where they overlap
    /// earlier ones.
    pub fn hit(&self, col: u16, row: u16) -> Option<ClickTarget> {
        self.layers
            .last()
            .expect("the page's layer")
            .iter()
            .rev()
            .find(|r| {
                r.rect.x <= col
                    && col < r.rect.x + r.rect.width
                    && r.rect.y <= row
                    && row < r.rect.y + r.rect.height
            })
            .map(|r| r.target.clone())
    }

    /// The regions of the topmost layer, in draw order — what a test reads to
    /// assert what a widget hung on the mouse. Nothing in the app asks: a click
    /// is answered by [`Self::hit`] alone.
    #[cfg(test)]
    pub fn top(&self) -> &[ClickRegion] {
        self.layers.last().expect("the page's layer")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A modal takes the frame: a click lands only on the topmost layer, so
    /// nothing drawn under one answers to the mouse however much of it is still
    /// on show. Later regions within a layer win over earlier ones, which is
    /// what lets a widget hang a target over a region it sits inside.
    #[test]
    fn only_the_topmost_layer_answers_to_a_click() {
        let region = |x, target| {
            ClickRegion {
                rect: Rect::new(x, 0, 4, 1),
                target,
            }
        };
        let mut map = ClickMap::new();
        map.push(region(0, ClickTarget::ProfitsButton));
        map.push(region(0, ClickTarget::ProfitsInput));
        assert!(matches!(
            map.hit(1, 0),
            Some(ClickTarget::ProfitsInput)
        ));

        // A popup over it: its own button answers, and the page under it does
        // not - neither where the popup covers it nor where it does not.
        map.layer();
        map.push(region(8, ClickTarget::ProfitsPopupYes));
        assert!(matches!(
            map.hit(9, 0),
            Some(ClickTarget::ProfitsPopupYes)
        ));
        assert!(map.hit(1, 0).is_none());

        // And a frame starts again on the page's own layer.
        map.clear();
        map.push(region(0, ClickTarget::ProfitsInput));
        assert!(matches!(
            map.hit(1, 0),
            Some(ClickTarget::ProfitsInput)
        ));
    }
}
