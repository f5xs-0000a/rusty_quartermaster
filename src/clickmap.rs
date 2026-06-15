use ratatui::prelude::Rect;

use crate::damage::Side;

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
    JobberVessel(usize),
    JobberUnpoison,
    JobberAboardList,
    JobberGreedyList,
    JobberPlankedList,
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
