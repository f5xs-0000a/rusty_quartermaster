use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table};

use crate::app::{self, SharedState};
use crate::clickmap::{ClickRegion, ClickTarget};
use crate::utils::{offset_title, offset_title_width};
use super::{Focus, InventoryRow, PopupKind, ProfitsApp, PANEL_COUNT};

// Inventory numeric/placeholder column widths (the Item column flexes).
const RESTOCK_W: u16 = 7; // "Restock"
const STOCK_W: u16 = 5; // "Stock"
const BOOTY_W: u16 = 5; // "Booty"
const SELL_W: u16 = 10; // "Sell Price"
const BUY_W: u16 = 9; // "Buy Price"

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut ProfitsApp,
    shared: &SharedState,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    // -- Widths ------------------------------------------------------------
    let item_width = app
        .rows
        .iter()
        .map(|r| app::commod_name(shared.commodities, r.commod_id).chars().count())
        .max()
        .unwrap_or(0)
        .max("Item".len()) as u16;

    // 5 single-column gaps + 2 borders + 2 horizontal padding.
    let table_width =
        item_width + RESTOCK_W + STOCK_W + BOOTY_W + SELL_W + BUY_W + 5 + 2 + 2;

    // Label column shared by the Parameters and Hold Stats tables.
    let label_width = app
        .panel
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        .max("Ship Hold Alcohol".len()) as u16;
    // label + 2 gap + min input(8) + 2 borders + 2 padding.
    let params_width = label_width + 2 + 8 + 2 + 2;

    // One centered column width; every box is widened to it.
    let content_width = table_width
        .max(params_width)
        .max(offset_title_width("Parameters"))
        .max(offset_title_width("Inventory"))
        .min(area.width.max(1));

    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(content_width),
        Constraint::Fill(1),
    ])
    .split(area);
    let col = hchunks[1];

    // -- Vertical stack ----------------------------------------------------
    let params_h = PANEL_COUNT as u16 + 1 /*blank*/ + 1 /*button*/ + 2 /*borders*/;
    let stats_h = 2 + 2; // 2 rows + borders
    let search_h = 2 + 2; // input + suggestion + borders

    let vchunks = Layout::vertical([
        Constraint::Length(params_h),
        Constraint::Length(stats_h),
        Constraint::Fill(1), // inventory
        Constraint::Length(search_h),
        Constraint::Length(1), // tooltip
    ])
    .split(col);

    render_parameters(frame, vchunks[0], app, shared, focused, label_width, regions);
    render_hold_stats(frame, vchunks[1], app, shared, label_width);
    render_inventory(frame, vchunks[2], app, shared, focused, item_width, regions);
    render_search(frame, vchunks[3], app, shared, focused, regions);

    // -- Tooltip (focus-bound) --------------------------------------------
    if let Some(line) = build_tooltip_line(app, shared) {
        frame.render_widget(Paragraph::new(line), vchunks[4]);
    }

    // -- Popup overlay -----------------------------------------------------
    if let Some(ref popup) = app.popup {
        render_popup(frame, popup, regions);
    }
}

fn render_parameters(
    frame: &mut Frame,
    area: Rect,
    app: &ProfitsApp,
    shared: &SharedState,
    focused: bool,
    label_width: u16,
    regions: &mut Vec<ClickRegion>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Parameters").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut constraints: Vec<Constraint> =
        (0..PANEL_COUNT).map(|_| Constraint::Length(1)).collect();
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // button
    let rows = Layout::vertical(constraints).split(inner);

    for (i, field) in app.panel.iter().enumerate() {
        let cols = Layout::horizontal([
            Constraint::Length(label_width),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(rows[i]);

        frame.render_widget(
            Paragraph::new(Span::styled(field.label, Style::default().bold())),
            cols[0],
        );

        let is_focused = focused && app.focus == Focus::Panel(i);
        let value_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default()
        };

        if i == 0 {
            render_island_field(frame, field, cols[2], is_focused, value_style, shared);
        } else {
            let value = Paragraph::new(Line::from(Span::raw(&field.value)).right_aligned())
                .style(value_style);
            frame.render_widget(value, cols[2]);

            if is_focused {
                let cx = cols[2].x + cols[2].width
                    - (field.value.chars().count()
                        - field.value[..field.cursor].chars().count())
                        as u16;
                frame.set_cursor_position((cx, cols[2].y));
            }
        }

        regions.push(ClickRegion {
            rect: rows[i],
            target: ClickTarget::ProfitsPanel(i),
        });
    }

    // "Calculate Profits!" button (last inner row).
    let button_area = rows[PANEL_COUNT + 1];
    let button_focused = focused && app.focus == Focus::Button;
    let button_style = if button_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().bold()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("Calculate Profits!", button_style)).centered()),
        button_area,
    );
    regions.push(ClickRegion {
        rect: button_area,
        target: ClickTarget::ProfitsButton,
    });
}

fn render_hold_stats(
    frame: &mut Frame,
    area: Rect,
    app: &ProfitsApp,
    shared: &SharedState,
    label_width: u16,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Hold Stats").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    let restock_alcohol = compute_alcohol(app, shared, |r| &r.restock);
    let hold_alcohol = compute_alcohol(app, shared, |r| &r.stock);

    render_stat_row(frame, rows[0], label_width, "Restock Alcohol", restock_alcohol);
    render_stat_row(frame, rows[1], label_width, "Ship Hold Alcohol", hold_alcohol);
}

fn render_stat_row(frame: &mut Frame, area: Rect, label_width: u16, label: &str, value: u64) {
    let cols = Layout::horizontal([
        Constraint::Length(label_width),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Span::styled(label, Style::default().bold())),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(value.to_string()).right_aligned()),
        cols[2],
    );
}

fn render_inventory(
    frame: &mut Frame,
    area: Rect,
    app: &mut ProfitsApp,
    shared: &SharedState,
    focused: bool,
    item_width: u16,
    regions: &mut Vec<ClickRegion>,
) {
    let header = Row::new(vec!["Item", "Restock", "Stock", "Booty", "Sell Price", "Buy Price"])
        .style(Style::default().bold())
        .bottom_margin(1);

    let placeholder = || {
        Cell::new(Line::from(Span::styled("—", Style::default().fg(Color::DarkGray))).right_aligned())
    };
    let rows: Vec<Row> = app
        .rows
        .iter()
        .map(|r| {
            let name = app::commod_name(shared.commodities, r.commod_id).to_owned();
            Row::new(vec![
                Cell::new(name),
                Cell::new(Line::from(r.restock.clone()).right_aligned()),
                Cell::new(Line::from(r.stock.clone()).right_aligned()),
                Cell::new(Line::from(r.booty.clone()).right_aligned()),
                placeholder(), // Sell Price (inert placeholder)
                placeholder(), // Buy Price (inert placeholder)
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(item_width),
        Constraint::Length(RESTOCK_W),
        Constraint::Length(STOCK_W),
        Constraint::Length(BOOTY_W),
        Constraint::Length(SELL_W),
        Constraint::Length(BUY_W),
    ];

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
                .title(offset_title("Inventory").0),
        );

    frame.render_stateful_widget(table, area, &mut app.table_state);

    // Register click regions for the name + editable cells (cols 0..=3 only;
    // the Sell/Buy placeholder columns are not interactive).
    let inner_x = area.x + 2; // border + padding
    let inner_y = area.y + 1; // top border
    let data_start_y = inner_y + 2; // header row + bottom_margin
    let scroll_offset = app.table_state.offset();
    let visible_height = area.height.saturating_sub(2); // borders
    let visible_rows = visible_height.saturating_sub(2); // header + margin
    let col_xs = [
        inner_x,
        inner_x + item_width + 1,
        inner_x + item_width + 1 + RESTOCK_W + 1,
        inner_x + item_width + 1 + RESTOCK_W + 1 + STOCK_W + 1,
    ];
    let col_ws = [item_width, RESTOCK_W, STOCK_W, BOOTY_W];
    for vis_row in 0..visible_rows as usize {
        let data_row = scroll_offset + vis_row;
        if data_row >= app.rows.len() {
            break;
        }
        for c in 0..4usize {
            regions.push(ClickRegion {
                rect: Rect::new(col_xs[c], data_start_y + vis_row as u16, col_ws[c], 1),
                target: ClickTarget::ProfitsTableCell { row: data_row, col: c },
            });
        }
    }
}

fn render_search(
    frame: &mut Frame,
    area: Rect,
    app: &ProfitsApp,
    shared: &SharedState,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Search").0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // input
        Constraint::Length(1), // suggestion (input-bound)
    ])
    .split(inner);

    let label = "Add Commodity: ";
    let input_style = if focused && app.focus == Focus::Input {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    let input_cols = Layout::horizontal([
        Constraint::Length(label.len() as u16),
        Constraint::Fill(1),
    ])
    .split(rows[0]);

    frame.render_widget(
        Paragraph::new(Span::styled(label, Style::default().bold())),
        input_cols[0],
    );
    // The input box fills the rest of the row, so the highlight spans the full
    // available width rather than shrinking to the typed text.
    frame.render_widget(
        Paragraph::new(Span::raw(&app.input)).style(input_style),
        input_cols[1],
    );

    if focused && app.focus == Focus::Input {
        let cursor_x = input_cols[1].x + app.input[..app.cursor].chars().count() as u16;
        frame.set_cursor_position((cursor_x, input_cols[1].y));
    }

    regions.push(ClickRegion {
        rect: rows[0],
        target: ClickTarget::ProfitsInput,
    });

    if let Some(line) = build_suggestion_line(app, shared) {
        frame.render_widget(Paragraph::new(line), rows[1]);
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
            Paragraph::new(
                Line::from(Span::styled("Query Market first", btn_style)).right_aligned(),
            ),
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

/// Input-bound commodity suggestion shown under the Search box.
fn build_suggestion_line<'a>(app: &'a ProfitsApp, shared: &'a SharedState) -> Option<Line<'a>> {
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
                    Span::styled(name, Style::default().bold().italic().fg(Color::DarkGray)),
                    Span::styled("\"? Press enter if yes.", Style::default().fg(Color::DarkGray)),
                ]))
            }
        }
        None => {
            let query = app.submit_failed.as_ref()?;
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

/// Focus-bound context help (plus loading/error status) shown on the bottom line.
fn build_tooltip_line<'a>(app: &'a ProfitsApp, shared: &'a SharedState) -> Option<Line<'a>> {
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

    let hint = |text: &'static str| {
        Some(Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        )))
    };

    match app.focus {
        Focus::Panel(0) if shared.cached_offers.is_empty() => hint("Press Enter to find islands"),
        Focus::Panel(0) => {
            let query = app.panel[0].value.trim();
            if query.is_empty() {
                return hint("Leave blank for ocean-wide pricing, or name a restocking island.");
            }
            match app::suggest_island(query, shared.available_islands) {
                Some(name) if name.eq_ignore_ascii_case(query) => None,
                Some(name) => Some(Line::from(vec![
                    Span::styled("Did you mean \"", Style::default().fg(Color::DarkGray)),
                    Span::styled(name, Style::default().bold().italic().fg(Color::DarkGray)),
                    Span::styled(
                        "\"? Press enter to accept.",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])),
                None => Some(Line::from(Span::styled(
                    "No matching island",
                    Style::default().fg(Color::Red),
                ))),
            }
        }
        Focus::Panel(4) => hint("Restocking rate imposed by your crew"),
        Focus::Panel(5) => hint("Amount spent before voyage to stock up"),
        Focus::Input => Some(Line::from(Span::styled(
            "Type a commodity and press Enter to add it.",
            Style::default().fg(Color::DarkGray),
        ))),
        Focus::Table => Some(Line::from(Span::styled(
            "Type digits to set quantities; Delete removes the commodity.",
            Style::default().fg(Color::DarkGray),
        ))),
        Focus::Button => Some(Line::from(Span::styled(
            "Press Enter to calculate profits.",
            Style::default().fg(Color::DarkGray),
        ))),
        _ => None,
    }
}

fn alcohol_multiplier(name: &str) -> u64 {
    match () {
        _ if name.eq_ignore_ascii_case("swill") => 2,
        _ if name.eq_ignore_ascii_case("grog") => 3,
        _ if name.eq_ignore_ascii_case("fine rum") => 6,
        _ => 0,
    }
}

/// Sum of `field`'s quantity weighted by each commodity's alcohol multiplier.
fn compute_alcohol(
    app: &ProfitsApp,
    shared: &SharedState,
    field: impl Fn(&InventoryRow) -> &String,
) -> u64 {
    app.rows
        .iter()
        .map(|r| {
            let name = app::commod_name(shared.commodities, r.commod_id);
            let qty = field(r).parse::<u64>().unwrap_or(0);
            qty * alcohol_multiplier(name)
        })
        .sum()
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
                .title(offset_title("Re-query?").0);
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
                .title(offset_title("Delete row").0);
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
                .title(offset_title("Restock warning").0);
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
            // Floor at 24 for readability, but also never below the title's width.
            let w: u16 = (max_content as u16 + 4)
                .max(24)
                .max(offset_title_width("Profit Breakdown"));
            let h: u16 = 11;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title(offset_title("Profit Breakdown").0);
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
