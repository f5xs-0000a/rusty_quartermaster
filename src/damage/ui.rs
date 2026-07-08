use ratatui::{
    prelude::*,
    widgets::{
        Block,
        Borders,
        Clear,
        List,
        ListItem,
        ListState,
        Padding,
        Paragraph,
    },
};

use super::{
    CENTER_LABELS,
    DamageApp,
    ROW_COUNT,
    ROW_DAMAGE,
    ROW_GAP,
    ROW_MANPOWER,
    ROW_RAMS,
    ROW_SHIP,
    ROW_SHOTS_LEFT,
    Side,
};
use crate::{
    clickmap::{ClickRegion, ClickTarget},
    ships::SHIPS,
    utils::offset_title,
};

const COL_GAP: u16 = 3;

/// Outer dimensions `(width, height)` of the Damage-calculator box, so callers
/// (the main page and the Sea Battles popup) can lay it out consistently. The
/// Manpower Advantage row is only present on the live page (`show_manpower`);
/// the Sea Battles popup omits it, yielding a box one row shorter.
pub fn calc_box_size(show_manpower: bool) -> (u16, u16) {
    let max_ship_name =
        SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0) as u16;
    let center_width =
        CENTER_LABELS.iter().map(|l| l.len()).max().unwrap_or(0) as u16;
    let inner_width =
        max_ship_name + COL_GAP + center_width + COL_GAP + max_ship_name;
    let rows = if show_manpower {
        ROW_COUNT
    } else {
        ROW_MANPOWER
    };
    (inner_width + 4, rows as u16 + 2) // +2 borders +2 padding ; rows + borders
}

/// Render the bordered Damage-calculator grid into `box_area` (which should be
/// exactly [`calc_box_size`]). This is the lone shared widget — the main page
/// and the Sea Battles popup both call it. `focused` drives the cell highlight.
pub fn render_calculator(
    frame: &mut Frame,
    box_area: Rect,
    app: &DamageApp,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
    show_manpower: bool,
) {
    let max_ship_name =
        SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0) as u16;
    let center_width =
        CENTER_LABELS.iter().map(|l| l.len()).max().unwrap_or(0) as u16;

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Damage Calculator").0);
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let visible_rows = if show_manpower {
        ROW_COUNT
    } else {
        ROW_MANPOWER
    };
    let row_constraints: Vec<Constraint> =
        (0 .. visible_rows).map(|_| Constraint::Length(1)).collect();
    let rows = Layout::vertical(row_constraints).split(inner);

    let cells_active = focused && app.popup.is_none();

    for i in 0 .. visible_rows {
        if i == ROW_GAP {
            continue;
        } else if i == ROW_RAMS {
            render_ram_row(
                frame,
                rows[i],
                app,
                max_ship_name,
                center_width,
                cells_active,
                regions,
            );
        } else if i == ROW_MANPOWER {
            render_manpower_row(
                frame,
                rows[i],
                app,
                max_ship_name,
                center_width,
            );
        } else {
            render_standard_row(
                frame,
                rows[i],
                i,
                app,
                max_ship_name,
                center_width,
                cells_active,
                regions,
            );
        }
    }

    // Ship-select popup (modal over the whole screen) when open.
    if let Some(ref popup) = app.popup {
        render_ship_popup(frame, popup, regions);
    }
    // "Reset values?" confirm (after a ship change) sits over everything.
    if let Some(yes) = app.reset_prompt {
        render_reset_prompt(frame, yes, regions);
    }
}

/// The Damage Calculator page: the shared calculator grid centered in `area`,
/// with a one-line hint below it.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut DamageApp,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let (box_width, box_height) = calc_box_size(true);

    let vchunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(box_height),
        Constraint::Length(1), // hint
        Constraint::Fill(1),
    ])
    .split(area);

    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(box_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[1]);

    render_calculator(
        frame, hchunks[1], app, focused, regions, true,
    );

    // -- Hint --
    if focused && app.popup.is_none() {
        let hint = match app.focus_row {
            ROW_SHIP => Some("Press Enter to select a different ship"),
            ROW_RAMS => {
                Some("Head-on? Increment twice (once for same size class)")
            }
            ROW_DAMAGE => None,
            _ => Some("Space/Enter to increment, Backspace to decrement"),
        };
        if let Some(text) = hint {
            let hint_hchunks = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(box_width),
                Constraint::Fill(1),
            ])
            .split(vchunks[2]);

            frame.render_widget(
                Paragraph::new(Span::styled(
                    text,
                    Style::default().fg(Color::DarkGray),
                )),
                hint_hchunks[1],
            );
        }
    }
}

fn render_standard_row(
    frame: &mut Frame,
    area: Rect,
    row: usize,
    app: &DamageApp,
    max_ship_name: u16,
    center_width: u16,
    cells_active: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let row_cols = Layout::horizontal([
        Constraint::Length(max_ship_name),
        Constraint::Length(COL_GAP),
        Constraint::Length(center_width),
        Constraint::Length(COL_GAP),
        Constraint::Length(max_ship_name),
    ])
    .split(area);

    // Center label
    frame.render_widget(
        Paragraph::new(Span::styled(
            CENTER_LABELS[row],
            Style::default().bold(),
        ))
        .centered(),
        row_cols[2],
    );

    // Focus highlight (Damage row is never focused)
    let left_focused =
        cells_active && app.focus_row == row && app.focus_side == Side::Left;
    let right_focused =
        cells_active && app.focus_row == row && app.focus_side == Side::Right;

    let left_style = if left_focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    let right_style = if right_focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };

    if row == ROW_SHIP {
        frame.render_widget(
            Paragraph::new(SHIPS[app.left_ship].name)
                .centered()
                .style(left_style),
            row_cols[0],
        );
        frame.render_widget(
            Paragraph::new(SHIPS[app.right_ship].name)
                .centered()
                .style(right_style),
            row_cols[4],
        );

        // Ship name cells: click to open ship select popup
        regions.push(ClickRegion {
            rect: row_cols[0],
            target: ClickTarget::DamageCell {
                row,
                side: Side::Left,
            },
        });
        regions.push(ClickRegion {
            rect: row_cols[4],
            target: ClickTarget::DamageCell {
                row,
                side: Side::Right,
            },
        });
    } else if row == ROW_SHOTS_LEFT {
        // Shots Left row: view-only, calculated `N / M` per side (N = shots to
        // max morale damage, M = shots to sink). Shares the Damage
        // row's colored bar, driven by the same morale/hull
        // percentages.
        let (left_n, left_m) = app.shots_left(Side::Left);
        let (right_n, right_m) = app.shots_left(Side::Right);
        let (left_morale, left_hull) = app.calculate_damage(Side::Left);
        let (right_morale, right_hull) = app.calculate_damage(Side::Right);

        frame.render_widget(
            Paragraph::new(format!("{} / {}", left_n, left_m)).centered(),
            row_cols[0],
        );
        apply_damage_bar(
            frame,
            row_cols[0],
            left_morale,
            left_hull,
            false,
        );

        frame.render_widget(
            Paragraph::new(format!("{} / {}", right_n, right_m)).centered(),
            row_cols[4],
        );
        apply_damage_bar(
            frame,
            row_cols[4],
            right_morale,
            right_hull,
            true,
        );
    } else if row < ROW_DAMAGE {
        let li = row - 1;

        // Render left side: [- ] [value] [ +]
        render_value_with_buttons(
            frame,
            row_cols[0],
            &app.left[li].to_string(),
            left_style,
            row,
            Side::Left,
            regions,
        );

        // Render right side: [- ] [value] [ +]
        render_value_with_buttons(
            frame,
            row_cols[4],
            &app.right[li].to_string(),
            right_style,
            row,
            Side::Right,
            regions,
        );
    } else {
        // Damage row: view-only, calculated, with colored bars
        let (left_morale, left_hull) = app.calculate_damage(Side::Left);
        let (right_morale, right_hull) = app.calculate_damage(Side::Right);

        frame.render_widget(
            Paragraph::new(format!(
                "{}% / {}%",
                left_morale, left_hull
            ))
            .centered(),
            row_cols[0],
        );
        apply_damage_bar(
            frame,
            row_cols[0],
            left_morale,
            left_hull,
            false,
        );

        frame.render_widget(
            Paragraph::new(format!(
                "{}% / {}%",
                right_morale, right_hull
            ))
            .centered(),
            row_cols[4],
        );
        apply_damage_bar(
            frame,
            row_cols[4],
            right_morale,
            right_hull,
            true,
        );
    }
}

const BTN_WIDTH: u16 = 3;

fn render_value_with_buttons(
    frame: &mut Frame,
    area: Rect,
    value: &str,
    value_style: Style,
    row: usize,
    side: Side,
    regions: &mut Vec<ClickRegion>,
) {
    let sub_cols = Layout::horizontal([
        Constraint::Length(BTN_WIDTH),
        Constraint::Min(0),
        Constraint::Length(BTN_WIDTH),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(" - ")
            .centered()
            .style(Style::default().fg(Color::DarkGray)),
        sub_cols[0],
    );
    frame.render_widget(
        Paragraph::new(value).centered().style(value_style),
        sub_cols[1],
    );
    frame.render_widget(
        Paragraph::new(" + ")
            .centered()
            .style(Style::default().fg(Color::DarkGray)),
        sub_cols[2],
    );

    regions.push(ClickRegion {
        rect: sub_cols[0],
        target: ClickTarget::DamageDecrement {
            row,
            side,
        },
    });
    regions.push(ClickRegion {
        rect: sub_cols[1],
        target: ClickTarget::DamageCell {
            row,
            side,
        },
    });
    regions.push(ClickRegion {
        rect: sub_cols[2],
        target: ClickTarget::DamageIncrement {
            row,
            side,
        },
    });
}

fn render_ram_row(
    frame: &mut Frame,
    area: Rect,
    app: &DamageApp,
    max_ship_name: u16,
    center_width: u16,
    cells_active: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let merged_width = max_ship_name + COL_GAP + center_width;

    let row_cols = Layout::horizontal([
        Constraint::Length(merged_width),
        Constraint::Length(COL_GAP),
        Constraint::Length(max_ship_name),
    ])
    .split(area);

    // Label in merged area, right-aligned
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Times Rammed",
            Style::default().bold(),
        ))
        .centered(),
        row_cols[0],
    );

    // Value with +/- buttons
    let focused = cells_active && app.focus_row == ROW_RAMS;
    let style = if focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };

    let sub_cols = Layout::horizontal([
        Constraint::Length(BTN_WIDTH),
        Constraint::Min(0),
        Constraint::Length(BTN_WIDTH),
    ])
    .split(row_cols[2]);

    frame.render_widget(
        Paragraph::new(" - ")
            .centered()
            .style(Style::default().fg(Color::DarkGray)),
        sub_cols[0],
    );
    frame.render_widget(
        Paragraph::new(app.rams.to_string()).centered().style(style),
        sub_cols[1],
    );
    frame.render_widget(
        Paragraph::new(" + ")
            .centered()
            .style(Style::default().fg(Color::DarkGray)),
        sub_cols[2],
    );

    regions.push(ClickRegion {
        rect: sub_cols[0],
        target: ClickTarget::DamageRamDecrement,
    });
    regions.push(ClickRegion {
        rect: sub_cols[1],
        target: ClickTarget::DamageRam,
    });
    regions.push(ClickRegion {
        rect: sub_cols[2],
        target: ClickTarget::DamageRamIncrement,
    });
}

/// The Manpower Advantage row: same merged layout as the ram row, but
/// view-only. The value cell shows the inferred advantage **range** (both crew
/// counts derived from the two ship types, weighted by each side's morale
/// advantage), to two decimals. Collapses to a single number when the endpoints
/// coincide.
fn render_manpower_row(
    frame: &mut Frame,
    area: Rect,
    app: &DamageApp,
    max_ship_name: u16,
    center_width: u16,
) {
    let merged_width = max_ship_name + COL_GAP + center_width;

    let row_cols = Layout::horizontal([
        Constraint::Length(merged_width),
        Constraint::Length(COL_GAP),
        Constraint::Length(max_ship_name),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Span::styled(
            "Manpower Advantage",
            Style::default().bold(),
        ))
        .centered(),
        row_cols[0],
    );

    let (min, max) = app.manpower_advantage();
    // `+ 0.0` normalises a possible `-0.00` to `0.00`.
    let fmt = |v: f64| format!("{:.2}", v + 0.0);
    let text = if (max - min).abs() < 1e-9 {
        fmt(min)
    } else {
        format!("{} – {}", fmt(min), fmt(max))
    };
    frame.render_widget(
        Paragraph::new(text).centered(),
        row_cols[2],
    );
}

/// Paint a damage bar onto the buffer after the text has been rendered.
///
/// From the starting edge (`from_right=false` → left, `true` → right):
///   0 … hull%  → orange background (hull damage, higher priority)
///   hull% … morale%  → yellow background (morale-only damage)
///   morale% … 100%  → untouched
///
/// Text on the coloured portion gets a black foreground.
fn apply_damage_bar(
    frame: &mut Frame,
    area: Rect,
    morale_pct: u32,
    hull_pct: u32,
    from_right: bool,
) {
    let w = area.width as u32;
    if w == 0 {
        return;
    }
    let hull_w = hull_pct * w / 100;
    let morale_w = morale_pct * w / 100;

    let buf = frame.buffer_mut();
    for x in area.left() .. area.right() {
        let rel = if from_right {
            (area.right() - 1 - x) as u32
        } else {
            (x - area.left()) as u32
        };

        if let Some(cell) = buf.cell_mut(Position::new(x, area.y)) {
            if rel < hull_w {
                cell.set_style(
                    Style::default()
                        .bg(Color::Rgb(255, 165, 0))
                        .fg(Color::Black),
                );
            } else if rel < morale_w {
                cell.set_style(
                    Style::default().bg(Color::Yellow).fg(Color::Black),
                );
            }
        }
    }
}

/// Modal: "Reset values?" with Yes/No buttons (`yes` = the focused choice).
fn render_reset_prompt(
    frame: &mut Frame,
    yes: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let area = frame.area();
    let (w, h) = (32u16, 5u16);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w.min(area.width),
        h.min(area.height),
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(" Reset values? ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new("Clear the hit tallies for this fight?")
            .style(Style::default().fg(Color::Gray))
            .centered(),
        rows[0],
    );
    let btns = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
        .split(rows[2]);
    let button = |label: &str, focused: bool| {
        let style = if focused {
            Style::default().fg(Color::Black).bg(Color::Cyan).bold()
        } else {
            Style::default().fg(Color::Cyan)
        };
        Paragraph::new(Line::from(Span::styled(
            format!("[ {label} ]"),
            style,
        )))
        .centered()
    };
    frame.render_widget(button("Yes", yes), btns[0]);
    frame.render_widget(button("No", !yes), btns[1]);
    regions.push(ClickRegion {
        rect: btns[0],
        target: ClickTarget::DamageResetYes,
    });
    regions.push(ClickRegion {
        rect: btns[1],
        target: ClickTarget::DamageResetNo,
    });
}

fn render_ship_popup(
    frame: &mut Frame,
    popup: &super::ShipSelectPopup,
    regions: &mut Vec<ClickRegion>,
) {
    let area = frame.area();

    let max_name_len = SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0);
    // +2 borders +2 padding +2 highlight symbol
    let w = max_name_len as u16 + 6;
    let h = SHIPS.len() as u16 + 2; // +2 borders
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> =
        SHIPS.iter().map(|ship| ListItem::new(ship.name)).collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title(offset_title("Select Ship").0)
                .title_bottom(" v: View "),
        )
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol("> ");

    let mut state = ListState::default().with_selected(Some(popup.selected));
    frame.render_stateful_widget(list, popup_area, &mut state);

    // Register click regions for each ship item
    let inner_y = popup_area.y + 1; // top border
    let inner_x = popup_area.x + 1; // left border
    let inner_w = popup_area.width.saturating_sub(2); // borders
    for i in 0 .. SHIPS.len() {
        regions.push(ClickRegion {
            rect: Rect::new(inner_x, inner_y + i as u16, inner_w, 1),
            target: ClickTarget::DamageShipItem(i),
        });
    }
}
