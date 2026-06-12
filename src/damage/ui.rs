use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::ships::SHIPS;
use super::{
    BUTTON_LABELS, CENTER_LABELS, DamageApp, ROW_COUNT, ROW_DAMAGE, ROW_GAP, ROW_HEADON,
    ROW_SHIP, Side,
};

const COL_GAP: u16 = 3;

pub fn render(frame: &mut Frame, area: Rect, app: &mut DamageApp) {
    let max_ship_name = SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0) as u16;
    let center_width = CENTER_LABELS
        .iter()
        .map(|l| l.len())
        .max()
        .unwrap_or(0) as u16;

    let inner_width = max_ship_name + COL_GAP + center_width + COL_GAP + max_ship_name;
    let box_width = inner_width + 4; // +2 borders +2 padding
    let box_height = ROW_COUNT as u16 + 2; // rows + borders
    let button_box_height = BUTTON_LABELS.len() as u16 + 2; // rows + borders

    // Center vertically: box + button box + hint
    let vchunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(box_height),
        Constraint::Length(button_box_height),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);

    // Center horizontally
    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(box_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[1]);

    // -- Main box --
    let box_area = hchunks[1];
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title("─── Damage Calculator ");
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    // -- Rows --
    let row_constraints: Vec<Constraint> =
        (0..ROW_COUNT).map(|_| Constraint::Length(1)).collect();
    let rows = Layout::vertical(row_constraints).split(inner);

    let no_popup = app.popup.is_none();

    for i in 0..ROW_COUNT {
        if i == ROW_GAP {
            continue;
        } else if i == ROW_HEADON {
            render_headon_row(frame, rows[i], app, max_ship_name, center_width, no_popup);
        } else {
            render_standard_row(
                frame,
                rows[i],
                i,
                app,
                max_ship_name,
                center_width,
                no_popup,
            );
        }
    }

    // -- Button box --
    let button_inner_w = BUTTON_LABELS.iter().map(|l| l.len()).max().unwrap_or(0) as u16;
    let button_box_w = button_inner_w + 4; // +2 borders +2 padding
    let button_hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(button_box_w),
        Constraint::Fill(1),
    ])
    .split(vchunks[2]);

    let button_box_area = button_hchunks[1];
    let button_block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1));
    let button_inner = button_block.inner(button_box_area);
    frame.render_widget(button_block, button_box_area);

    for (i, label) in BUTTON_LABELS.iter().enumerate() {
        let btn_rect = Rect::new(button_inner.x, button_inner.y + i as u16, button_inner.width, 1);
        let style = if no_popup && app.button_focused && app.button_index == i {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default()
        };
        frame.render_widget(Paragraph::new(*label).centered().style(style), btn_rect);
    }

    // -- Hint --
    if app.popup.is_none() {
        let hint = if app.button_focused {
            Some("Press Enter to activate")
        } else {
            match app.focus_row {
                ROW_SHIP => Some("Press Enter to select a different ship"),
                ROW_DAMAGE => None,
                _ => Some("Space/Enter to increment, Backspace to decrement"),
            }
        };
        if let Some(text) = hint {
            let hint_hchunks = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(box_width),
                Constraint::Fill(1),
            ])
            .split(vchunks[3]);

            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::default().fg(Color::DarkGray))),
                hint_hchunks[1],
            );
        }
    }

    // -- Ship select popup --
    if let Some(ref popup) = app.popup {
        render_ship_popup(frame, popup);
    }
}

fn render_standard_row(
    frame: &mut Frame,
    area: Rect,
    row: usize,
    app: &DamageApp,
    max_ship_name: u16,
    center_width: u16,
    no_popup: bool,
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
        Paragraph::new(Span::styled(CENTER_LABELS[row], Style::default().bold())).centered(),
        row_cols[2],
    );

    // Focus highlight (Damage row is never focused)
    let left_focused = no_popup && app.focus_row == row && app.focus_side == Side::Left;
    let right_focused = no_popup && app.focus_row == row && app.focus_side == Side::Right;

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
    } else if row < ROW_DAMAGE {
        let li = row - 1;
        frame.render_widget(
            Paragraph::new(app.left[li].to_string())
                .centered()
                .style(left_style),
            row_cols[0],
        );
        frame.render_widget(
            Paragraph::new(app.right[li].to_string())
                .centered()
                .style(right_style),
            row_cols[4],
        );
    } else {
        // Damage row: view-only, calculated, with colored bars
        let (left_morale, left_hull) = app.calculate_damage(Side::Left);
        let (right_morale, right_hull) = app.calculate_damage(Side::Right);

        frame.render_widget(
            Paragraph::new(format!("{}%/{}%", left_morale, left_hull)).centered(),
            row_cols[0],
        );
        apply_damage_bar(frame, row_cols[0], left_morale, left_hull, false);

        frame.render_widget(
            Paragraph::new(format!("{}%/{}%", right_morale, right_hull)).centered(),
            row_cols[4],
        );
        apply_damage_bar(frame, row_cols[4], right_morale, right_hull, true);
    }
}

fn render_headon_row(
    frame: &mut Frame,
    area: Rect,
    app: &DamageApp,
    max_ship_name: u16,
    center_width: u16,
    no_popup: bool,
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
            "Head-on Collisions",
            Style::default().bold(),
        ))
        .centered(),
        row_cols[0],
    );

    // Value
    let focused = no_popup && app.focus_row == ROW_HEADON;
    let style = if focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(app.headon.to_string())
            .centered()
            .style(style),
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
    for x in area.left()..area.right() {
        let rel = if from_right {
            (area.right() - 1 - x) as u32
        } else {
            (x - area.left()) as u32
        };

        if let Some(cell) = buf.cell_mut(Position::new(x, area.y)) {
            if rel < hull_w {
                cell.set_style(Style::default().bg(Color::Rgb(255, 165, 0)).fg(Color::Black));
            } else if rel < morale_w {
                cell.set_style(Style::default().bg(Color::Yellow).fg(Color::Black));
            }
        }
    }
}

fn render_ship_popup(frame: &mut Frame, popup: &super::ShipSelectPopup) {
    let area = frame.area();

    let max_name_len = SHIPS.iter().map(|s| s.name.len()).max().unwrap_or(0);
    // +2 borders +2 padding +2 highlight symbol
    let w = max_name_len as u16 + 6;
    let h = SHIPS.len() as u16 + 2; // +2 borders
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = SHIPS
        .iter()
        .map(|ship| ListItem::new(ship.name))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Select Ship ")
                .title_bottom(" v: View "),
        )
        .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
        .highlight_symbol("> ");

    let mut state = ListState::default().with_selected(Some(popup.selected));
    frame.render_stateful_widget(list, popup_area, &mut state);
}
