use ratatui::{
    prelude::*,
    widgets::{
        Block,
        Borders,
        Cell,
        Clear,
        Padding,
        Paragraph,
        Row,
        Table,
        Wrap,
    },
};

use super::{
    BreakdownRow,
    FIRST_COL,
    Focus,
    HoldImport,
    InventoryRow,
    P_BOOTY_CHEST,
    P_CO_RATE,
    P_DONATION,
    P_RESTOCK_PLACE,
    P_RESTOCK_RATE,
    P_SELL_PLACE,
    P_STOCKING,
    PopupKind,
    ProfitsApp,
    is_place_field,
};
use crate::{
    api::Commodity,
    app::{self, SharedState},
    clickmap::{ClickRegion, ClickTarget},
    utils::{offset_title, offset_title_width},
};

// Inventory numeric column widths (the Item column flexes).
const RESTOCK_W: u16 = 7; // "Restock"
const STOCK_W: u16 = 5; // "Stock"
const BOOTY_W: u16 = 5; // "Booty"
const SELL_W: u16 = 10; // "Sell Price"
const BUY_W: u16 = 9; // "Buy Price"
const COL_GAP: u16 = 2; // spacing between inventory columns

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
        .map(|r| {
            app::commod_name(shared.commodities, r.commod_id)
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0)
        .max("Item".len()) as u16;

    // The Sell/Buy Price columns are only shown when prices are entered
    // manually (Market unavailable). Otherwise the table is the four
    // base columns.
    let show_prices = !shared.market_supported;
    let (price_w, gaps) = if show_prices {
        (SELL_W + BUY_W, 5)
    } else {
        (0, 3)
    };
    // inter-column gaps + 2 borders + 2 horizontal padding.
    let table_width = item_width
        + RESTOCK_W
        + STOCK_W
        + BOOTY_W
        + price_w
        + gaps * COL_GAP
        + 2
        + 2;

    // Label column shared by the Parameters and Hold Stats tables.
    let label_width = app
        .panel
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        .max("Rum (Hold / Restock)".len()) as u16;
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
    let visible_panels = app.visible_panels(shared.market_supported).len();
    let params_h = visible_panels as u16 + 1 /*blank*/ + 1 /*button*/ + 2 /*borders*/;
    let stats_h = 1 + 2; // 1 row + borders
    let search_h = 2 + 2; // input + suggestion + borders

    // Inventory is the topmost widget and takes the Fill slot so it scrolls;
    // the others stack below it at fixed heights, with the tooltip last.
    let vchunks = Layout::vertical([
        Constraint::Fill(1), // inventory
        Constraint::Length(search_h),
        Constraint::Length(stats_h),
        Constraint::Length(params_h),
        Constraint::Length(2), // tooltip (up to two lines)
    ])
    .split(col);

    render_inventory(
        frame, vchunks[0], app, shared, focused, item_width, regions,
    );
    render_search(
        frame, vchunks[1], app, shared, focused, regions,
    );
    render_hold_stats(
        frame,
        vchunks[2],
        app,
        shared,
        label_width,
    );
    render_parameters(
        frame,
        vchunks[3],
        app,
        shared,
        focused,
        label_width,
        regions,
    );

    // -- Tooltip (focus-bound) --------------------------------------------
    if let Some(text) = build_tooltip(app, shared) {
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap {
                trim: true,
            }),
            vchunks[4],
        );
    }

    // -- Popup overlay -----------------------------------------------------
    let breakdown_cursor = app.breakdown_cursor;
    let show_co = app.show_co_rate;
    let show_donation = app.show_donation;
    if let Some(ref popup) = app.popup {
        render_popup(
            frame,
            popup,
            breakdown_cursor,
            show_co,
            show_donation,
            shared.commodities,
            regions,
        );
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

    let visible = app.visible_panels(shared.market_supported);
    let mut constraints: Vec<Constraint> =
        visible.iter().map(|_| Constraint::Length(1)).collect();
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // button
    let rows = Layout::vertical(constraints).split(inner);

    for (pos, &i) in visible.iter().enumerate() {
        let field = &app.panel[i];
        let cols = Layout::horizontal([
            Constraint::Length(label_width),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .split(rows[pos]);

        frame.render_widget(
            Paragraph::new(Span::styled(
                field.label,
                Style::default().bold(),
            )),
            cols[0],
        );

        let is_focused = focused && app.focus == Focus::Panel(i);
        let value_style = if is_focused {
            Style::default().bg(Color::White).fg(Color::Black)
        } else {
            Style::default()
        };

        if is_place_field(i) {
            render_island_field(
                frame,
                field,
                cols[2],
                is_focused,
                value_style,
                shared,
            );
        } else if i == P_BOOTY_CHEST && field.value.is_empty() {
            // Booty Chest: when blank, show the auto-deduced value as a dim
            // placeholder. The calc uses it unless the user types an override.
            let deduced = app.deduced_chest(shared).to_string();
            let ph_style = if is_focused {
                Style::default()
                    .fg(Color::DarkGray)
                    .bg(Color::White)
                    .italic()
            } else {
                Style::default().fg(Color::DarkGray).italic()
            };
            frame.render_widget(
                Paragraph::new(
                    Line::from(Span::styled(deduced, ph_style)).right_aligned(),
                ),
                cols[2],
            );
            if is_focused {
                frame.set_cursor_position((
                    cols[2].x + cols[2].width,
                    cols[2].y,
                ));
            }
        } else {
            let value = Paragraph::new(
                Line::from(Span::raw(&field.value)).right_aligned(),
            )
            .style(value_style);
            frame.render_widget(value, cols[2]);

            if is_focused {
                let cx = cols[2].x + cols[2].width
                    - (field.value.chars().count()
                        - field.value[.. field.cursor].chars().count())
                        as u16;
                frame.set_cursor_position((cx, cols[2].y));
            }
        }

        regions.push(ClickRegion {
            rect: rows[pos],
            target: ClickTarget::ProfitsPanel(i),
        });
    }

    // "Calculate Profits!" button (last inner row, after the blank spacer).
    let button_area = rows[visible.len() + 1];
    let button_focused = focused && app.focus == Focus::Button;
    let button_style = if button_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default().bold()
    };
    frame.render_widget(
        Paragraph::new(
            Line::from(Span::styled(
                "Calculate Profits!",
                button_style,
            ))
            .centered(),
        ),
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

    let rows = Layout::vertical([Constraint::Length(1)]).split(inner);

    let restock_rum = compute_rum(app, shared, |r| &r.restock);
    let hold_rum = compute_rum(app, shared, |r| &r.stock);

    render_stat_row(
        frame,
        rows[0],
        label_width,
        "Rum (Hold / Restock)",
        format!("{hold_rum} / {restock_rum}"),
    );
}

fn render_stat_row(
    frame: &mut Frame,
    area: Rect,
    label_width: u16,
    label: &str,
    value: String,
) {
    let cols = Layout::horizontal([
        Constraint::Length(label_width),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default().bold(),
        )),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(value).right_aligned()),
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
    // Sell/Buy Price columns are editable only when Market is unavailable;
    // otherwise prices come from Market and the columns are hidden.
    let show_prices = !shared.market_supported;

    let mut header_cells = vec!["Item", "Restock", "Stock", "Booty"];
    if show_prices {
        header_cells.push("Sell Price");
        header_cells.push("Buy Price");
    }
    let header = Row::new(header_cells)
        .style(Style::default().bold())
        .bottom_margin(1);

    let rows: Vec<Row> = app
        .rows
        .iter()
        .map(|r| {
            let name =
                app::commod_name(shared.commodities, r.commod_id).to_owned();
            let mut cells = vec![
                Cell::new(name),
                Cell::new(Line::from(r.restock.clone()).right_aligned()),
                Cell::new(Line::from(r.stock.clone()).right_aligned()),
                Cell::new(Line::from(r.booty.clone()).right_aligned()),
            ];
            if show_prices {
                cells.push(Cell::new(
                    Line::from(r.sell.clone()).right_aligned(),
                ));
                cells.push(Cell::new(
                    Line::from(r.buy.clone()).right_aligned(),
                ));
            }
            Row::new(cells)
        })
        .collect();

    let mut widths = vec![
        Constraint::Length(item_width),
        Constraint::Length(RESTOCK_W),
        Constraint::Length(STOCK_W),
        Constraint::Length(BOOTY_W),
    ];
    if show_prices {
        widths.push(Constraint::Length(SELL_W));
        widths.push(Constraint::Length(BUY_W));
    }

    let highlight = if focused {
        Style::default().bg(Color::White).fg(Color::Black)
    } else {
        Style::default()
    };
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(COL_GAP)
        .row_highlight_style(Style::default())
        .cell_highlight_style(highlight)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title(offset_title("Inventory").0),
        );

    frame.render_stateful_widget(table, area, &mut app.table_state);

    // Register click regions for the name + editable cells. The Sell/Buy
    // columns are only present (and clickable) when prices are entered
    // manually.
    let inner_x = area.x + 2; // border + padding
    let inner_y = area.y + 1; // top border
    let data_start_y = inner_y + 2; // header row + bottom_margin
    let scroll_offset = app.table_state.offset();
    let visible_height = area.height.saturating_sub(2); // borders
    let visible_rows = visible_height.saturating_sub(2); // header + margin
    // Build column x-offsets left-to-right so the price columns (when shown)
    // line up with their click targets.
    let mut col_xs = vec![inner_x];
    let mut col_ws = vec![item_width];
    let mut x = inner_x + item_width + COL_GAP;
    let push_col =
        |xs: &mut Vec<u16>, ws: &mut Vec<u16>, x: &mut u16, w: u16| {
            xs.push(*x);
            ws.push(w);
            *x += w + COL_GAP;
        };
    push_col(
        &mut col_xs,
        &mut col_ws,
        &mut x,
        RESTOCK_W,
    );
    push_col(
        &mut col_xs,
        &mut col_ws,
        &mut x,
        STOCK_W,
    );
    push_col(
        &mut col_xs,
        &mut col_ws,
        &mut x,
        BOOTY_W,
    );
    if show_prices {
        push_col(&mut col_xs, &mut col_ws, &mut x, SELL_W);
        push_col(&mut col_xs, &mut col_ws, &mut x, BUY_W);
    }
    let ncols = col_xs.len();
    for vis_row in 0 .. visible_rows as usize {
        let data_row = scroll_offset + vis_row;
        if data_row >= app.rows.len() {
            break;
        }
        for c in 0 .. ncols {
            regions.push(ClickRegion {
                rect: Rect::new(
                    col_xs[c],
                    data_start_y + vis_row as u16,
                    col_ws[c],
                    1,
                ),
                target: ClickTarget::ProfitsTableCell {
                    row: data_row,
                    col: c,
                },
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
        Paragraph::new(Span::styled(
            label,
            Style::default().bold(),
        )),
        input_cols[0],
    );
    // The input box fills the rest of the row, so the highlight spans the full
    // available width rather than shrinking to the typed text.
    frame.render_widget(
        Paragraph::new(Span::raw(&app.input)).style(input_style),
        input_cols[1],
    );

    if focused && app.focus == Focus::Input {
        let cursor_x =
            input_cols[1].x + app.input[.. app.cursor].chars().count() as u16;
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
                Line::from(Span::styled(
                    "Query Market first",
                    btn_style,
                ))
                .right_aligned(),
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
            Paragraph::new(
                Line::from(Span::styled("Ocean-wide", ph_style))
                    .right_aligned(),
            ),
            area,
        );
        if is_focused {
            frame.set_cursor_position((area.x, area.y));
        }
        return;
    }

    let value =
        Paragraph::new(Line::from(Span::raw(&field.value)).right_aligned())
            .style(value_style);
    frame.render_widget(value, area);
    if is_focused {
        let cx = area.x + area.width
            - (field.value.chars().count()
                - field.value[.. field.cursor].chars().count())
                as u16;
        frame.set_cursor_position((cx, area.y));
    }
}

/// Input-bound commodity suggestion shown under the Search box.
fn build_suggestion_line<'a>(
    app: &'a ProfitsApp,
    shared: &'a SharedState,
) -> Option<Line<'a>> {
    let suggestion = app.suggest(shared.commodities);
    let query_lower = app.input.trim().to_lowercase();
    match suggestion {
        Some(id) => {
            let name = app::commod_name(shared.commodities, id);
            if name.eq_ignore_ascii_case(&query_lower) {
                None
            } else {
                Some(Line::from(vec![
                    Span::styled(
                        "Did you mean \"",
                        Style::default().fg(Color::DarkGray),
                    ),
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
            let query = app.submit_failed.as_ref()?;
            Some(Line::from(vec![
                Span::styled("No \"", Style::default().fg(Color::Red)),
                Span::styled(
                    query.as_str(),
                    Style::default().bold().italic().fg(Color::Red),
                ),
                Span::styled(
                    "\" found",
                    Style::default().fg(Color::Red),
                ),
            ]))
        }
    }
}

/// Focus hint for a market-location field (Restocking/Selling Place): confirms
/// an exact island/archipelago, suggests a fuzzy match, or flags an unknown
/// name. `verb` is the action word in the archipelago note ("restocking" or
/// "selling"). Returns owned text so it outlives the borrowed `value`.
fn place_hint(
    value: &str,
    verb: &str,
    shared: &SharedState,
) -> Option<Text<'static>> {
    let query = value.trim();
    if query.is_empty() {
        return Some(Text::from(Line::from(Span::styled(
            "Leave blank for ocean-wide pricing, or name an island or \
             archipelago."
                .to_owned(),
            Style::default().fg(Color::DarkGray),
        ))));
    }
    // "Did you mean "<name>" (<kind>)? Press enter to accept." in muted text.
    let did_you_mean = |name: &str, kind: &'static str| {
        Some(Text::from(Line::from(vec![
            Span::styled(
                "Did you mean \"",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                name.to_owned(),
                Style::default().bold().italic().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("\" ({kind})? Press enter to accept."),
                Style::default().fg(Color::DarkGray),
            ),
        ])))
    };
    match app::resolve_restock_scope(
        value,
        shared.available_islands,
        shared.ocean_geo,
    ) {
        // Exact island typed — nothing to suggest.
        app::RestockScope::Island(name) if name.eq_ignore_ascii_case(query) => {
            None
        }
        app::RestockScope::Island(name) => did_you_mean(&name, "island"),
        // Exact archipelago — confirm it fans out across its islands.
        app::RestockScope::Archipelago {
            name,
            islands,
        } if name.eq_ignore_ascii_case(query) => {
            Some(Text::from(Line::from(vec![
                Span::styled(
                    "Archipelago \"",
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    name,
                    Style::default().bold().italic().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(
                        "\" — {verb} across {} islands.",
                        islands.len()
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ])))
        }
        app::RestockScope::Archipelago {
            name,
            ..
        } => did_you_mean(&name, "archipelago"),
        app::RestockScope::OceanWide | app::RestockScope::Unknown => {
            Some(Text::from(Line::from(Span::styled(
                "No matching island or archipelago".to_owned(),
                Style::default().fg(Color::Red),
            ))))
        }
    }
}

/// Focus-bound context help (plus loading/error status) shown on the bottom
/// line(s). The inventory table returns a two-line, per-column hint.
fn build_tooltip<'a>(
    app: &'a ProfitsApp,
    shared: &'a SharedState,
) -> Option<Text<'a>> {
    if shared.loading {
        return Some(Text::from(Line::from(Span::styled(
            "Fetching prices...",
            Style::default().fg(Color::Yellow),
        ))));
    }

    if let Some(ref err) = app.calc_error {
        return Some(Text::from(Line::from(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red),
        ))));
    }

    let muted =
        |s: String| Span::styled(s, Style::default().fg(Color::DarkGray));
    let hint = |text: &'static str| {
        Some(Text::from(Line::from(muted(
            text.to_owned(),
        ))))
    };

    match app.focus {
        Focus::Panel(P_RESTOCK_PLACE) if shared.cached_offers.is_empty() => {
            hint("Press Enter to find islands")
        }
        Focus::Panel(P_RESTOCK_PLACE) => {
            place_hint(
                &app.panel[P_RESTOCK_PLACE].value,
                "restocking",
                shared,
            )
        }
        Focus::Panel(P_SELL_PLACE) if shared.cached_offers.is_empty() => {
            hint("Query the market first to pick where to sell.")
        }
        Focus::Panel(P_SELL_PLACE) => {
            place_hint(
                &app.panel[P_SELL_PLACE].value,
                "selling",
                shared,
            )
        }
        Focus::Panel(P_BOOTY_CHEST) => {
            hint(
                "Auto: each fight's retained half, less stolen. Type to \
                 override.",
            )
        }
        Focus::Panel(P_CO_RATE) => {
            hint("Share of total gained the commanding officer keeps.")
        }
        Focus::Panel(P_DONATION) => {
            hint("Share of total gained donated to the crew.")
        }
        Focus::Panel(P_RESTOCK_RATE) => {
            hint("Skimmed from the crew's cut for restocking (not the chest).")
        }
        Focus::Panel(P_STOCKING) => {
            hint(
                "PoE spent stocking up before the voyage; recouped at the \
                 divvy.",
            )
        }
        Focus::Input => hint("Type a commodity and press Enter to add it."),
        Focus::Table => {
            // Per-column action for the selected inventory cell.
            let action =
                match app.table_state.selected_column().unwrap_or(FIRST_COL) {
                    1 => "set how many units to restock to",
                    2 => "set how many units are in the hold",
                    3 => "set how many units are in the booty",
                    4 => "set the shoppe sell price",
                    5 => "set the shoppe buy price",
                    _ => "set the value",
                };
            Some(Text::from(vec![
                Line::from(muted(format!(
                    "Type digits to {action}"
                ))),
                Line::from(muted(
                    "Press Delete to remove commodity".to_owned(),
                )),
            ]))
        }
        Focus::Button => hint("Press Enter to calculate profits."),
        _ => None,
    }
}

/// Sum of `field`'s quantity weighted by each commodity's rum multiplier.
fn compute_rum(
    app: &ProfitsApp,
    shared: &SharedState,
    field: impl Fn(&InventoryRow) -> &String,
) -> u64 {
    app.rows
        .iter()
        .map(|r| {
            let name = app::commod_name(shared.commodities, r.commod_id);
            let qty = field(r).parse::<u64>().unwrap_or(0);
            qty * crate::commodities::rum_multiplier(name)
        })
        .sum()
}

/// Greedy word-wrap line count for `text` at `width` columns — used to reserve
/// enough rows for the wrapped tooltip so the popup height is stable.
fn wrapped_line_count(text: &str, width: usize) -> u16 {
    if width == 0 {
        return 1;
    }
    let mut lines: u16 = 1;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if col == 0 {
            col = wlen;
        } else if col + 1 + wlen <= width {
            col += 1 + wlen;
        } else {
            lines += 1;
            col = wlen;
        }
    }
    lines.max(1)
}

/// The "Hold from clipboard" prompt: the recognized goods with their
/// quantities, any names the commodity list doesn't know, and a No / Yes pair.
fn render_hold_import(
    frame: &mut Frame,
    import: &HoldImport,
    commodities: &[Commodity],
    regions: &mut Vec<ClickRegion>,
) {
    const CAP: usize = 8; // goods listed before "...and N more"
    let area = frame.area();

    let shown: Vec<(&str, u64)> = import
        .goods
        .iter()
        .take(CAP)
        .map(|&(id, qty)| (app::commod_name(commodities, id), qty))
        .collect();
    let extra = import.goods.len().saturating_sub(CAP);
    let unknown_line = (!import.unknown.is_empty()).then(|| {
        format!(
            "Not recognized: {}",
            import.unknown.join(", ")
        )
    });

    let list_lines = if import.goods.is_empty() {
        1
    } else {
        shown.len() + usize::from(0 < extra)
    };
    let w: u16 = 48;
    let inner_w = w as usize - 4; // borders + horizontal padding
    let unknown_lines = unknown_line.as_deref().map_or(0, |s| {
        wrapped_line_count(s, inner_w) as usize
    });
    // header, list, unknowns, note, blank, buttons
    let h: u16 = (2 + 1 + list_lines + unknown_lines + 1 + 1 + 1) as u16;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Hold from clipboard").0);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut constraints = vec![Constraint::Length(1)]; // header
    constraints.extend((0 .. list_lines).map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Length(unknown_lines as u16));
    constraints.push(Constraint::Length(1)); // note
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // buttons
    let rows = Layout::vertical(constraints).split(inner);

    frame.render_widget(
        Paragraph::new("Set the Stock column from the copied hold?"),
        rows[0],
    );
    if import.goods.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  (the hold is empty)",
                Style::default().fg(Color::DarkGray),
            )),
            rows[1],
        );
    }
    for (i, (name, qty)) in shown.iter().enumerate() {
        let qty = qty.to_string();
        let pad = inner_w.saturating_sub(4 + name.chars().count() + qty.len());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(format!("  \u{2022} {name}")),
                Span::raw(" ".repeat(pad)),
                Span::styled(qty, Style::default().bold()),
            ])),
            rows[1 + i],
        );
    }
    if 0 < extra {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("  ...and {extra} more"),
                Style::default().fg(Color::DarkGray),
            )),
            rows[1 + shown.len()],
        );
    }
    let unknown_row = rows[1 + list_lines];
    if let Some(text) = unknown_line {
        frame.render_widget(
            Paragraph::new(Span::styled(
                text,
                Style::default().fg(Color::Yellow),
            ))
            .wrap(Wrap {
                trim: true,
            }),
            unknown_row,
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Other rows' Stock is cleared; Booty is left as is.",
            Style::default().fg(Color::DarkGray),
        )),
        rows[2 + list_lines],
    );

    let no_style = if !import.yes_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default()
    };
    let yes_style = if import.yes_focused {
        Style::default().bg(Color::White).fg(Color::Black).bold()
    } else {
        Style::default()
    };
    let buttons = Line::from(vec![
        Span::styled(" No ", no_style),
        Span::raw("  "),
        Span::styled(" Yes ", yes_style),
    ]);
    let btn_row = rows[rows.len() - 1];
    frame.render_widget(
        Paragraph::new(buttons).centered(),
        btn_row,
    );

    let half = btn_row.width / 2;
    regions.push(ClickRegion {
        rect: Rect::new(btn_row.x, btn_row.y, half, 1),
        target: ClickTarget::ProfitsPopupNo,
    });
    regions.push(ClickRegion {
        rect: Rect::new(
            btn_row.x + half,
            btn_row.y,
            btn_row.width - half,
            1,
        ),
        target: ClickTarget::ProfitsPopupYes,
    });
}

fn render_popup(
    frame: &mut Frame,
    popup: &PopupKind,
    breakdown_cursor: usize,
    show_co: bool,
    show_donation: bool,
    commodities: &[Commodity],
    regions: &mut Vec<ClickRegion>,
) {
    let area = frame.area();

    match popup {
        PopupKind::HoldImport(import) => {
            render_hold_import(frame, import, commodities, regions);
        }
        PopupKind::ReQueryConfirm {
            yes_focused,
        } => {
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

            frame.render_widget(
                Paragraph::new("Re-query market prices?"),
                rows[0],
            );
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
            frame.render_widget(
                Paragraph::new(buttons).centered(),
                rows[3],
            );

            // Register popup button regions (split button row in half)
            let half = rows[3].width / 2;
            regions.push(ClickRegion {
                rect: Rect::new(rows[3].x, rows[3].y, half, 1),
                target: ClickTarget::ProfitsPopupNo,
            });
            regions.push(ClickRegion {
                rect: Rect::new(
                    rows[3].x + half,
                    rows[3].y,
                    rows[3].width - half,
                    1,
                ),
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
                Span::styled(
                    name.as_str(),
                    Style::default().bold().italic(),
                ),
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
            frame.render_widget(
                Paragraph::new(buttons).centered(),
                rows[2],
            );

            let half = rows[2].width / 2;
            regions.push(ClickRegion {
                rect: Rect::new(rows[2].x, rows[2].y, half, 1),
                target: ClickTarget::ProfitsPopupNo,
            });
            regions.push(ClickRegion {
                rect: Rect::new(
                    rows[2].x + half,
                    rows[2].y,
                    rows[2].width - half,
                    1,
                ),
                target: ClickTarget::ProfitsPopupYes,
            });
        }
        PopupKind::RestockWarning {
            missing,
            ocean_wide_focused,
        } => {
            let max_shown = 5;
            let shown: Vec<&str> =
                missing.iter().take(max_shown).map(|s| s.as_str()).collect();
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
            for _ in 0 .. list_lines {
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
            frame.render_widget(
                Paragraph::new(buttons).centered(),
                btn_row,
            );

            let half = btn_row.width / 2;
            regions.push(ClickRegion {
                rect: Rect::new(btn_row.x, btn_row.y, half, 1),
                target: ClickTarget::ProfitsPopupNo,
            });
            regions.push(ClickRegion {
                rect: Rect::new(
                    btn_row.x + half,
                    btn_row.y,
                    btn_row.width - half,
                    1,
                ),
                target: ClickTarget::ProfitsPopupYes,
            });
        }
        PopupKind::PriceBlock {
            need_buy,
            need_sell,
        } => {
            const CAP: usize = 6; // per-section list cap before "...and N more"

            let mut lines: Vec<Line> = vec![Line::from(
                "Enter the missing prices before calculating:",
            )];

            let section = |lines: &mut Vec<Line>,
                           title: &'static str,
                           items: &[String]| {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    title,
                    Style::default().bold(),
                )));
                for name in items.iter().take(CAP) {
                    lines.push(Line::from(Span::styled(
                        format!("  \u{2022} {name}"),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                let extra = items.len().saturating_sub(CAP);
                if 0 < extra {
                    lines.push(Line::from(Span::styled(
                        format!("  ...and {extra} more"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            };
            if !need_buy.is_empty() {
                section(
                    &mut lines,
                    "Need a Buy Price (to restock):",
                    need_buy,
                );
            }
            if !need_sell.is_empty() {
                section(
                    &mut lines,
                    "Need a Sell Price (for excess):",
                    need_sell,
                );
            }

            let content_w =
                lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
            let w: u16 = (content_w + 4)
                .max(offset_title_width("Prices needed"))
                .max(30);
            let h: u16 = lines.len() as u16 + 1 /*blank*/ + 1 /*button*/ + 2 /*borders*/;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .title(offset_title("Prices needed").0);
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let mut constraints: Vec<Constraint> =
                lines.iter().map(|_| Constraint::Length(1)).collect();
            constraints.push(Constraint::Length(1)); // blank
            constraints.push(Constraint::Length(1)); // button
            let rows = Layout::vertical(constraints).split(inner);

            for (i, line) in lines.into_iter().enumerate() {
                frame.render_widget(Paragraph::new(line), rows[i]);
            }

            let ok_style =
                Style::default().bg(Color::White).fg(Color::Black).bold();
            let btn_row = rows[rows.len() - 1];
            frame.render_widget(
                Paragraph::new(
                    Line::from(Span::styled(" Ok ", ok_style)).centered(),
                ),
                btn_row,
            );
            regions.push(ClickRegion {
                rect: btn_row,
                target: ClickTarget::ProfitsPopupOk,
            });
        }
        PopupKind::ProfitResult(result) => {
            let bd = result.breakdown(show_co, show_donation);
            // Most rows are non-negative magnitudes; only "Total gained in
            // pillage" ever shows a "-" (a loss).
            let valstr = |r: &BreakdownRow| -> String { r.value.to_string() };

            // Lay out one line per breakdown row, plus a blank separator after
            // any row flagged `gap_after`. `placed` maps each
            // layout slot to its breakdown index (or None for a
            // separator).
            let mut placed: Vec<Option<usize>> = Vec::new();
            for (i, r) in bd.iter().enumerate() {
                placed.push(Some(i));
                if r.gap_after {
                    placed.push(None);
                }
            }

            // Sized to the widest row (label + value) only — the tooltip wraps
            // rather than stretching the popup.
            let max_content = bd
                .iter()
                .map(|r| r.label.len() + 4 + valstr(r).len())
                .max()
                .unwrap_or(0);
            let w: u16 = (max_content as u16 + 4)
                .max(24)
                .max(offset_title_width("Profit Breakdown"));
            // Reserve enough lines for the tallest tooltip once wrapped to
            // width.
            let inner_w = w.saturating_sub(4) as usize; // − borders − padding
            let desc_lines = bd
                .iter()
                .map(|r| wrapped_line_count(r.desc, inner_w))
                .max()
                .unwrap_or(1)
                .max(1);
            // rows/separators + blank + wrapped description + Ok button + 2
            // borders
            let h: u16 = placed.len() as u16 + 1 + desc_lines + 1 + 2;
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

            let mut constraints: Vec<Constraint> =
                placed.iter().map(|_| Constraint::Length(1)).collect();
            constraints.push(Constraint::Length(1)); // blank
            constraints.push(Constraint::Length(desc_lines)); // description (wraps)
            constraints.push(Constraint::Length(1)); // button
            let layout = Layout::vertical(constraints).split(inner);

            let avail = inner.width as usize;

            for (slot, place) in placed.iter().enumerate() {
                let Some(i) = *place else { continue }; // blank separator
                let r = &bd[i];
                let v = valstr(r);
                let pad =
                    avail.saturating_sub(r.label.len()).saturating_sub(v.len());
                let line = format!(
                    "{}{:>w$}",
                    r.label,
                    v,
                    w = pad + v.len()
                );

                // Headlines (Total gained, Add to Booty) are bold; any negative
                // value (only Total gained can be) is red — a loss.
                let negative = r.value < 0;
                let mut style = Style::default();
                if r.headline {
                    style = style.bold();
                }
                if negative {
                    style = style.fg(Color::Red);
                }
                // Highlight the row the tooltip is describing.
                if i == breakdown_cursor {
                    style = style.bg(Color::White);
                    if !negative {
                        style = style.fg(Color::Black);
                    }
                }
                frame.render_widget(
                    Paragraph::new(Span::styled(line, style)),
                    layout[slot],
                );

                // Hover region: moving the mouse over a row parks the cursor
                // there.
                regions.push(ClickRegion {
                    rect: layout[slot],
                    target: ClickTarget::ProfitsBreakdownRow(i),
                });
            }

            // The selected row's explanation, on the dedicated tooltip line(s);
            // wraps within the (narrow) popup rather than widening it.
            let desc = bd.get(breakdown_cursor).map(|r| r.desc).unwrap_or("");
            frame.render_widget(
                Paragraph::new(Span::styled(
                    desc,
                    Style::default().fg(Color::DarkGray).italic(),
                ))
                .wrap(Wrap {
                    trim: true,
                }),
                layout[layout.len() - 2],
            );

            let ok_style =
                Style::default().bg(Color::White).fg(Color::Black).bold();
            let ok_btn = Line::from(Span::styled(" Ok ", ok_style));
            let ok_row = layout[layout.len() - 1];
            frame.render_widget(
                Paragraph::new(ok_btn).centered(),
                ok_row,
            );

            regions.push(ClickRegion {
                rect: ok_row,
                target: ClickTarget::ProfitsPopupOk,
            });
        }
    }
}
