use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table};

use crate::app::{self, SharedState};
use crate::clickmap::{ClickRegion, ClickTarget};
use super::{Focus, PopupKind, ProfitsApp, PANEL_COUNT};

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut ProfitsApp,
    shared: &SharedState,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let vchunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(4), // border(1) + input(1) + suggestion(1) + border(1)
    ])
    .split(area);

    // -- Table --
    let row_data: Vec<(String, String, String, String)> = app
        .rows
        .iter()
        .map(|r| {
            (
                app::commod_name(shared.commodities, r.commod_id).to_owned(),
                r.restock.clone(),
                r.stock.clone(),
                r.booty.clone(),
            )
        })
        .collect();

    let item_width = row_data
        .iter()
        .map(|(name, ..)| name.chars().count())
        .max()
        .unwrap_or(0)
        .max("Item".len()) as u16;

    let header = Row::new(vec!["Item", "Restock", "Stock", "Booty"])
        .style(Style::default().bold())
        .bottom_margin(1);

    let rows: Vec<Row> = row_data
        .iter()
        .map(|(name, restock, stock, booty)| {
            Row::new(vec![
                Cell::new(name.as_str()),
                Cell::new(Line::from(restock.as_str()).right_aligned()),
                Cell::new(Line::from(stock.as_str()).right_aligned()),
                Cell::new(Line::from(booty.as_str()).right_aligned()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(item_width),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(5),
    ];

    // 2 = left + right border, 2 = horizontal padding, 3 = spacing between 4 columns
    let table_width = item_width + 7 + 5 + 5 + 3 + 2 + 2;

    let highlight = if focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::default())
        .cell_highlight_style(highlight)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Inventory "),
        );

    // -- Panel --
    let panel_label_width = app
        .panel
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let panel_inner_width = panel_label_width;
    let panel_width = panel_inner_width + 4; // +2 borders +2 padding

    // -- Horizontal layout: table + gap + panel, centered --
    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(table_width),
        Constraint::Length(1),
        Constraint::Length(panel_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[0]);

    frame.render_stateful_widget(table, hchunks[1], &mut app.table_state);

    // Register table cell click regions
    {
        let table_inner_x = hchunks[1].x + 2; // border + padding
        let table_inner_y = hchunks[1].y + 1; // top border
        // header row + 1 blank bottom_margin = 2 lines before data
        let data_start_y = table_inner_y + 2;
        let scroll_offset = app.table_state.offset();
        let visible_height = hchunks[1].height.saturating_sub(2); // borders
        let visible_rows = visible_height.saturating_sub(2); // header + margin
        let col_xs = [
            table_inner_x,
            table_inner_x + item_width + 1,
            table_inner_x + item_width + 1 + 7 + 1,
            table_inner_x + item_width + 1 + 7 + 1 + 5 + 1,
        ];
        let col_ws = [item_width, 7, 5, 5];
        for vis_row in 0..visible_rows as usize {
            let data_row = scroll_offset + vis_row;
            if data_row >= app.rows.len() {
                break;
            }
            for col in 0..4usize {
                regions.push(ClickRegion {
                    rect: Rect::new(
                        col_xs[col],
                        data_start_y + vis_row as u16,
                        col_ws[col],
                        1,
                    ),
                    target: ClickTarget::ProfitsTableCell { row: data_row, col },
                });
            }
        }
    }

    // Vertically center the panel + stats box
    let panel_content_height = (PANEL_COUNT as u16) * 2 + 2;
    let panel_block_height = panel_content_height + 2;
    let stats_height: u16 = 3; // 1 content line + 2 borders
    let panel_vchunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(panel_block_height),
        Constraint::Length(stats_height),
        Constraint::Fill(1),
    ])
    .split(hchunks[3]);

    let panel_block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title("─── Parameters ");
    let panel_inner = panel_block.inner(panel_vchunks[1]);
    frame.render_widget(panel_block, panel_vchunks[1]);

    let panel_constraints: Vec<Constraint> = (0..PANEL_COUNT)
        .flat_map(|_| [Constraint::Length(1), Constraint::Length(1)])
        .chain([Constraint::Length(1), Constraint::Length(1)])
        .collect();
    let panel_rows = Layout::vertical(panel_constraints).split(panel_inner);

    for (i, field) in app.panel.iter().enumerate() {
        let label_area = panel_rows[i * 2];
        let value_area = panel_rows[i * 2 + 1];

        let label = Paragraph::new(Span::styled(field.label, Style::default().bold()));
        frame.render_widget(label, label_area);

        let is_focused = focused && app.focus == Focus::Panel(i);
        let value_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default()
        };

        if i == 0 {
            render_island_field(frame, field, value_area, is_focused, value_style, shared);
        } else {
            let value = Paragraph::new(Line::from(Span::raw(&field.value)).right_aligned())
                .style(value_style);
            frame.render_widget(value, value_area);

            if is_focused {
                let cx = value_area.x + value_area.width
                    - (field.value.chars().count() - field.value[..field.cursor].chars().count())
                        as u16;
                frame.set_cursor_position((cx, value_area.y));
            }
        }

        // Register click region for this panel field (label + value area)
        regions.push(ClickRegion {
            rect: Rect::new(
                label_area.x,
                label_area.y,
                label_area.width,
                2, // label + value
            ),
            target: ClickTarget::ProfitsPanel(i),
        });
    }

    // "Calculate profits!" button
    let button_area = panel_rows[PANEL_COUNT * 2 + 1];
    let button_focused = focused && app.focus == Focus::Button;
    let button_style = if button_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().bold()
    };
    let button =
        Paragraph::new(Line::from(Span::styled("Calculate profits!", button_style)).centered());
    frame.render_widget(button, button_area);

    regions.push(ClickRegion {
        rect: button_area,
        target: ClickTarget::ProfitsButton,
    });

    // -- Hold Stats box --
    let stats_block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title("─── Hold Stats ");
    let stats_inner = stats_block.inner(panel_vchunks[2]);
    frame.render_widget(stats_block, panel_vchunks[2]);

    let total_alcohol = compute_total_alcohol(app, shared);
    let label = "Total Alcohol";
    let val_str = total_alcohol.to_string();
    let avail = stats_inner.width as usize;
    let pad = avail.saturating_sub(label.len()).saturating_sub(val_str.len());
    let line = format!("{}{:>w$}", label, val_str, w = pad + val_str.len());
    frame.render_widget(Paragraph::new(line), stats_inner);

    // -- Bottom box (input + suggestion) --
    let bottom_width = table_width + 1 + panel_width;
    let bottom_hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(bottom_width),
        Constraint::Fill(1),
    ])
    .split(vchunks[1]);

    let bottom_block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title("─── Search ");
    let bottom_inner = bottom_block.inner(bottom_hchunks[1]);
    frame.render_widget(bottom_block, bottom_hchunks[1]);

    let bottom_rows = Layout::vertical([
        Constraint::Length(1), // input
        Constraint::Length(1), // suggestion / error
    ])
    .split(bottom_inner);

    // -- Input prompt --
    let label = "Add Commodity: ";
    let input_style = if focused && app.focus == Focus::Input {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(label, Style::default().bold()),
        Span::styled(&app.input, input_style),
    ]));
    frame.render_widget(prompt, bottom_rows[0]);

    if focused && app.focus == Focus::Input {
        let cursor_x =
            bottom_rows[0].x + label.len() as u16 + app.input[..app.cursor].chars().count() as u16;
        let cursor_y = bottom_rows[0].y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    regions.push(ClickRegion {
        rect: bottom_rows[0],
        target: ClickTarget::ProfitsInput,
    });

    // -- Status line --
    let status_line = build_status_line(app, shared);
    if let Some(line) = status_line {
        frame.render_widget(Paragraph::new(line), bottom_rows[1]);
    }

    // -- Popup overlay --
    if let Some(ref popup) = app.popup {
        render_popup(frame, popup, regions);
    }
}

fn render_island_field(
    frame: &mut Frame,
    field: &crate::utils::PromptField,
    area: Rect,
    is_focused: bool,
    value_style: Style,
    shared: &SharedState,
) {
    if shared.cached_offers.is_empty() {
        let btn_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black).bold()
        } else {
            Style::default().fg(Color::DarkGray).italic()
        };
        frame.render_widget(
            Paragraph::new(Span::styled("Query Market first", btn_style)),
            area,
        );
        return;
    }

    if field.value.is_empty() {
        let ph_style = if is_focused {
            Style::default()
                .fg(Color::DarkGray)
                .bg(Color::White)
                .italic()
        } else {
            Style::default().fg(Color::DarkGray).italic()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("Ocean-wide", ph_style)).right_aligned()),
            area,
        );
        if is_focused {
            frame.set_cursor_position((area.x, area.y));
        }
        return;
    }

    let value =
        Paragraph::new(Line::from(Span::raw(&field.value)).right_aligned()).style(value_style);
    frame.render_widget(value, area);
    if is_focused {
        let cx = area.x + area.width
            - (field.value.chars().count() - field.value[..field.cursor].chars().count()) as u16;
        frame.set_cursor_position((cx, area.y));
    }
}

fn build_status_line<'a>(app: &'a ProfitsApp, shared: &'a SharedState) -> Option<Line<'a>> {
    if shared.loading {
        return Some(Line::from(Span::styled(
            "Fetching prices from market...",
            Style::default().fg(Color::Yellow),
        )));
    }

    if let Some(ref err) = app.calc_error {
        return Some(Line::from(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red),
        )));
    }

    if app.focus == Focus::Panel(0) && !shared.cached_offers.is_empty() {
        let query = app.panel[0].value.trim();
        if query.is_empty() {
            return None;
        }
        return match app::suggest_island(query, shared.available_islands) {
            Some(name) if name.eq_ignore_ascii_case(query) => None,
            Some(name) => Some(Line::from(vec![
                Span::styled("Did you mean \"", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    name,
                    Style::default().bold().italic().fg(Color::DarkGray),
                ),
                Span::styled(
                    "\"? Press enter to accept.",
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            None => Some(Line::from(Span::styled(
                "No matching island",
                Style::default().fg(Color::Red),
            ))),
        };
    }

    let suggestion = app.suggest(shared.commodities);
    let query_lower = app.input.trim().to_lowercase();
    match suggestion {
        Some(id) => {
            let name = app::commod_name(shared.commodities, id);
            if name.eq_ignore_ascii_case(&query_lower) {
                None
            } else {
                Some(Line::from(vec![
                    Span::styled("Did you mean \"", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        name,
                        Style::default().bold().italic().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        "\"? Press enter if yes.",
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            }
        }
        None => {
            let Some(ref query) = app.submit_failed else {
                return None;
            };
            Some(Line::from(vec![
                Span::styled("No \"", Style::default().fg(Color::Red)),
                Span::styled(
                    query.as_str(),
                    Style::default().bold().italic().fg(Color::Red),
                ),
                Span::styled("\" found", Style::default().fg(Color::Red)),
            ]))
        }
    }
}

fn compute_total_alcohol(app: &ProfitsApp, shared: &SharedState) -> u64 {
    let mut total: u64 = 0;
    for row in &app.rows {
        let name = app::commod_name(shared.commodities, row.commod_id);
        let stock = row.stock.parse::<u64>().unwrap_or(0);
        let multiplier = match () {
            _ if name.eq_ignore_ascii_case("swill") => 2,
            _ if name.eq_ignore_ascii_case("grog") => 3,
            _ if name.eq_ignore_ascii_case("fine rum") => 6,
            _ => 0,
        };
        total += stock * multiplier;
    }
    total
}

fn render_popup(frame: &mut Frame, popup: &PopupKind, regions: &mut Vec<ClickRegion>) {
    let area = frame.area();

    match popup {
        PopupKind::ReQueryConfirm { yes_focused } => {
            let w: u16 = 40;
            let h: u16 = 6;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Re-query? ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            frame.render_widget(Paragraph::new("Re-query Market?"), rows[0]);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "This may take some time.",
                    Style::default().fg(Color::DarkGray),
                )),
                rows[1],
            );

            let no_style = if !yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let yes_style = if *yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let buttons = Line::from(vec![
                Span::styled(" No ", no_style),
                Span::raw("  "),
                Span::styled(" Yes ", yes_style),
            ]);
            frame.render_widget(Paragraph::new(buttons).centered(), rows[3]);

            // Register popup button regions (split button row in half)
            let half = rows[3].width / 2;
            regions.push(ClickRegion {
                rect: Rect::new(rows[3].x, rows[3].y, half, 1),
                target: ClickTarget::ProfitsPopupNo,
            });
            regions.push(ClickRegion {
                rect: Rect::new(rows[3].x + half, rows[3].y, rows[3].width - half, 1),
                target: ClickTarget::ProfitsPopupYes,
            });
        }
        PopupKind::DeleteConfirm {
            row_idx: _,
            name,
            yes_focused,
        } => {
            let text_len = "Delete row \"\"?".len() + name.len();
            let w: u16 = (text_len as u16 + 6).max(22);
            let h: u16 = 5;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Delete row ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            let prompt = Line::from(vec![
                Span::raw("Delete row \""),
                Span::styled(name.as_str(), Style::default().bold().italic()),
                Span::raw("\"?"),
            ]);
            frame.render_widget(Paragraph::new(prompt), rows[0]);

            let no_style = if !yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let yes_style = if *yes_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let buttons = Line::from(vec![
                Span::styled(" No ", no_style),
                Span::raw("  "),
                Span::styled(" Yes ", yes_style),
            ]);
            frame.render_widget(Paragraph::new(buttons).centered(), rows[2]);

            let half = rows[2].width / 2;
            regions.push(ClickRegion {
                rect: Rect::new(rows[2].x, rows[2].y, half, 1),
                target: ClickTarget::ProfitsPopupNo,
            });
            regions.push(ClickRegion {
                rect: Rect::new(rows[2].x + half, rows[2].y, rows[2].width - half, 1),
                target: ClickTarget::ProfitsPopupYes,
            });
        }
        PopupKind::RestockWarning {
            missing,
            ocean_wide_focused,
        } => {
            let max_shown = 5;
            let shown: Vec<&str> = missing.iter().take(max_shown).map(|s| s.as_str()).collect();
            let extra = missing.len().saturating_sub(max_shown);

            let list_lines = shown.len() + if 0 < extra { 1 } else { 0 };
            let h: u16 = (3 + list_lines + 2) as u16;
            let w: u16 = 46;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Restock warning ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let mut constraints: Vec<Constraint> = Vec::new();
            constraints.push(Constraint::Length(1)); // header
            for _ in 0..list_lines {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Length(1)); // blank
            constraints.push(Constraint::Length(1)); // buttons

            let rows = Layout::vertical(constraints).split(inner);

            frame.render_widget(
                Paragraph::new("No supply on this island for:"),
                rows[0],
            );
            for (i, name) in shown.iter().enumerate() {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  \u{2022} {}", name),
                        Style::default().fg(Color::Yellow),
                    )),
                    rows[1 + i],
                );
            }
            if 0 < extra {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  ...and {} more", extra),
                        Style::default().fg(Color::DarkGray),
                    )),
                    rows[1 + shown.len()],
                );
            }

            let ocean_style = if *ocean_wide_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let change_style = if !ocean_wide_focused {
                Style::default().bg(Color::White).fg(Color::Black).bold()
            } else {
                Style::default()
            };
            let buttons = Line::from(vec![
                Span::styled(" Change Island ", change_style),
                Span::raw("  "),
                Span::styled(" Ocean-wide ", ocean_style),
            ]);
            let btn_row = rows[rows.len() - 1];
            frame.render_widget(Paragraph::new(buttons).centered(), btn_row);

            let half = btn_row.width / 2;
            regions.push(ClickRegion {
                rect: Rect::new(btn_row.x, btn_row.y, half, 1),
                target: ClickTarget::ProfitsPopupNo,
            });
            regions.push(ClickRegion {
                rect: Rect::new(btn_row.x + half, btn_row.y, btn_row.width - half, 1),
                target: ClickTarget::ProfitsPopupYes,
            });
        }
        PopupKind::ProfitResult(result) => {
            let labels = [
                "Goods Value",
                "Restock Value",
                "C. Officer Cut",
                "Crew Donation",
                "Subtotal",
                "Stocking",
                "Add to Booty",
            ];
            let val_strs: [String; 7] = [
                format!("{}", result.goods_value),
                format!("{}", result.restock_value),
                format!("{}", result.co_cut),
                format!("{}", result.crew_donation),
                format!("{}", result.subtotal),
                format!("{}", result.stocking),
                format!("{}", result.add_to_booty),
            ];
            let max_content = labels
                .iter()
                .zip(val_strs.iter())
                .map(|(l, v)| l.len() + 4 + v.len())
                .max()
                .unwrap_or(0);
            let w: u16 = (max_content as u16 + 4).max(24);
            let h: u16 = 11;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title("─── Profit Breakdown ");
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            let avail = inner.width as usize;

            for (i, (lbl, val_str)) in labels.iter().zip(val_strs.iter()).enumerate() {
                let pad = avail.saturating_sub(lbl.len()).saturating_sub(val_str.len());
                let line = format!("{}{:>w$}", lbl, val_str, w = pad + val_str.len());
                frame.render_widget(Paragraph::new(line), rows[i]);
            }

            let ok_style = Style::default().bg(Color::White).fg(Color::Black).bold();
            let ok_btn = Line::from(Span::styled(" Ok ", ok_style));
            frame.render_widget(Paragraph::new(ok_btn).centered(), rows[8]);

            regions.push(ClickRegion {
                rect: rows[8],
                target: ClickTarget::ProfitsPopupOk,
            });
        }
    }
}
