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
    clickmap::{ClickMap, ClickRegion, ClickTarget},
    utils::{offset_title, offset_title_width},
};

// Inventory numeric column widths (the Item column flexes).
const RESTOCK_W: u16 = 7; // "Restock"
const STOCK_W: u16 = 5; // "Stock"
const BOOTY_W: u16 = 5; // "Booty"
const SELL_W: u16 = 10; // "Sell Price"
const BUY_W: u16 = 9; // "Buy Price"
const COL_GAP: u16 = 2; // spacing between inventory columns

/// Rows the page-wide tooltip takes while it has something to say: enough for
/// it to wrap once.
const TOOLTIP_H: u16 = 2;

/// `area` inset to where the boxed widgets of the stack draw their contents. A
/// box spends a border and a blank column on each side, so the tooltip strip
/// beneath them, having no box of its own, keeps both rather than only the
/// blank — otherwise its words sit a column to the left of every other word on
/// the page.
fn strip(area: Rect) -> Rect {
    Rect {
        x: area.x + crate::utils::BOX_MARGIN / 2,
        width: area.width.saturating_sub(crate::utils::BOX_MARGIN),
        ..area
    }
}

/// Columns a No / Yes button row spends. A confirm popup is never narrower than
/// its own buttons.
fn yes_no_width() -> u16 {
    crate::utils::buttons_width(&["No", "Yes"])
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut ProfitsApp,
    shared: &SharedState,
    focused: bool,
    regions: &mut ClickMap,
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
    // inter-column gaps + 2 borders + 2 horizontal padding, and the columns the
    // scrollbar keeps: the commodity list is longer than any window over it, so
    // the bar is all but always up.
    let table_width = item_width
        + RESTOCK_W
        + STOCK_W
        + BOOTY_W
        + price_w
        + gaps * COL_GAP
        + 2
        + 2
        + crate::utils::SCROLLBAR_W;

    // Label column shared by the Parameters and Hold Stats tables, and the
    // widest text the value side of a Parameters row will draw. Both are
    // measured over the rows actually on show, and the value side is measured
    // rather than reserved: a placeholder is the prompt saying what the field
    // wants, so clipping one costs more than widening the panel. The value
    // floor is room to type into a field that is showing nothing.
    let visible = app.visible_panels(shared.market_supported);
    let label_width = visible
        .iter()
        .map(|&i| app.panel[i].label.chars().count())
        .max()
        .unwrap_or(0)
        .max("Rum (Hold / Restock)".len()) as u16;
    let value_width = visible
        .iter()
        .map(|&i| field_text(app, shared, i).chars().count() as u16)
        .max()
        .unwrap_or(0)
        .max(8);
    // label + 2 gap + value + 2 borders + 2 padding.
    let params_width = label_width + 2 + value_width + 2 + 2;

    // What the page cannot do without. The Inventory is absent from it because
    // it scrolls: it may be wider than the window and still show every column,
    // while a Parameters row has nowhere to scroll to.
    let needed_width = params_width
        .max(offset_title_width("Parameters"))
        .max(offset_title_width("Inventory"));
    if crate::utils::too_narrow(frame, area, needed_width) {
        return;
    }

    // One centered column width; every box is widened to it, and the Inventory
    // may still ask for more than the window has.
    let content_width = table_width.max(needed_width).min(area.width);

    let hchunks = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(content_width),
        Constraint::Fill(1),
    ])
    .split(area);
    let col = hchunks[1];

    // -- Vertical stack ----------------------------------------------------
    let params_h = visible.len() as u16 + 1 /*blank*/ + 1 /*button*/ + 2 /*borders*/;
    let stats_h = 1 + 2; // 1 row + borders
    // The tooltip only takes room when it has something to say. The Inventory
    // is the one widget that grows into what the others leave, so a row
    // reserved for nothing is a commodity row it loses. The search's row, which
    // the Inventory keeps at its foot, is not of that kind: it holds the
    // invitation to open one while none is open, so it is never idle.
    //
    // A popup is the exception: it is drawn over the page and takes nothing
    // from it, so the strip keeps its rows for as long as one is up. The page
    // beneath a popup is the page the user left, not a wider one.
    let tooltip_h =
        if build_tooltip(app, shared).is_some() || app.popup.is_some() {
            TOOLTIP_H // up to two lines once it wraps
        } else {
            0
        };

    // Room the page must have, counting the rows the tooltip takes when it has
    // something to say even while it has not: what the page needs cannot move
    // as focus moves, or resting on a field would make the page disappear. The
    // Inventory keeps a scrollable view's worth of commodities under its
    // header, which is what the spare rows go to when the tooltip is empty.
    // The sideways scrollbar's row is counted whether the table is wide enough
    // to want one or not, for the same reason: a window the user widens or
    // narrows must not be able to make the page vanish.
    let inventory_h = 2 /*borders*/
        + 1 /*the header's row*/
        + crate::utils::SCROLL_MIN_ROWS
        + crate::utils::SCROLLBAR_H
        + crate::utils::SEARCH_H;
    let needed_height = inventory_h + stats_h + params_h + TOOLTIP_H;
    if crate::utils::too_short(frame, area, needed_height) {
        return;
    }

    // Inventory is the topmost widget and takes the Fill slot so it scrolls;
    // the others stack below it at fixed heights, with the tooltip last.
    let vchunks = Layout::vertical([
        Constraint::Fill(1), // inventory, the search at its foot
        Constraint::Length(stats_h),
        Constraint::Length(params_h),
        Constraint::Length(tooltip_h),
    ])
    .split(col);

    render_inventory(
        frame, vchunks[0], app, shared, focused, item_width, regions,
    );
    render_hold_stats(
        frame,
        vchunks[1],
        app,
        shared,
        label_width,
    );
    render_parameters(
        frame,
        vchunks[2],
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
            strip(vchunks[3]),
        );
    }

    // -- Popup overlay -----------------------------------------------------
    let breakdown_cursor = app.breakdown_cursor;
    let show_co = app.show_co_rate;
    let show_donation = app.show_donation;
    if let Some(ref popup) = app.popup {
        regions.layer();
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
    regions: &mut ClickMap,
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

        let text = field_text(app, shared, i);
        if is_place_field(i) {
            render_island_field(
                frame,
                field,
                &text,
                cols[2],
                is_focused,
                value_style,
                shared,
            );
        } else if i == P_BOOTY_CHEST && field.value.is_empty() {
            // The deduced chest shows as a dim placeholder; the calc uses it
            // unless the user types an override.
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
                    Line::from(Span::styled(text.as_ref(), ph_style))
                        .right_aligned(),
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
    regions: &mut ClickMap,
) {
    // Sell/Buy Price columns are editable only when Market is unavailable;
    // otherwise prices come from Market and the columns are hidden.
    let show_prices = !shared.market_supported;

    let mut header_cells = vec!["Item", "Restock", "Stock", "Booty"];
    if show_prices {
        header_cells.push("Sell Price");
        header_cells.push("Buy Price");
    }
    // Headers sit centered over their columns. For the numeric columns, whose
    // width is their header's own length, this changes nothing; the Item
    // column is as wide as the longest name, and its header would otherwise
    // drift to the far left of it.
    let header = Row::new(
        header_cells
            .into_iter()
            .map(|cell| Cell::new(Line::from(cell).centered())),
    )
    .style(Style::default().bold());

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
    let table = Table::new(rows, widths.clone())
        .header(header)
        .column_spacing(COL_GAP)
        .row_highlight_style(Style::default())
        .cell_highlight_style(highlight);

    // The box is drawn here rather than by the table, so the table can be
    // placed inside it: centered when it is narrower, scrolled when it is
    // wider.
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Inventory").0);
    let box_inner = block.inner(area);
    frame.render_widget(block, area);

    // The box's last row is the search's. The commodities being added go in
    // here, so the row that puts one in belongs inside the box they land in
    // rather than adrift beneath it.
    let stack = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(crate::utils::SEARCH_H),
    ])
    .split(box_inner);
    let inner = stack[0];
    render_search(
        frame, stack[1], app, shared, focused, regions,
    );

    // Column widths never shrink, so the table has one intrinsic width.
    let col_ws: Vec<u16> = widths
        .iter()
        .map(|c| {
            match c {
                Constraint::Length(w) => *w,
                _ => 0,
            }
        })
        .collect();
    let columns_width = col_ws.iter().sum::<u16>()
        + COL_GAP * col_ws.len().saturating_sub(1) as u16;

    // The table is the one widget that scrolls both ways, and each bar costs
    // the other room: the upright one takes columns, which can only leave
    // the sideways window narrower, and the sideways one takes the bottom
    // row, which can only leave the rows window shorter. Taking room away
    // never un-needs a bar, so one pass over the pair settles both.
    let rows_h = inner.height.saturating_sub(1); // the header's row
    let mut vscrolls = crate::utils::scrolls(rows_h, app.rows.len());
    let hscrolls = crate::utils::scrolls(
        inner.width.saturating_sub(
            if vscrolls {
                crate::utils::SCROLLBAR_W
            } else {
                0
            },
        ),
        columns_width as usize,
    );
    if hscrolls {
        vscrolls = crate::utils::scrolls(
            rows_h.saturating_sub(crate::utils::SCROLLBAR_H),
            app.rows.len(),
        );
    }
    // What the table itself is laid out in, so its columns and rows sit where
    // they would with no bars at all.
    let table_w = inner.width.saturating_sub(
        if vscrolls {
            crate::utils::SCROLLBAR_W
        } else {
            0
        },
    );
    let table_h = inner.height.saturating_sub(
        if hscrolls {
            crate::utils::SCROLLBAR_H
        } else {
            0
        },
    );
    // The window the rows scroll through: what is left under the header, above
    // the sideways bar.
    let body = Rect {
        y: inner.y + 1,
        height: table_h.saturating_sub(1),
        ..inner
    };

    // Where column `i` starts, measured from the table's own left edge.
    let col_offsets: Vec<u16> = col_ws
        .iter()
        .scan(0, |x, w| {
            let at = *x;
            *x += w + COL_GAP;
            Some(at)
        })
        .collect();

    // `columns_x` is where the table's left edge lands on screen, which the
    // click regions below are measured from. It sits left of `inner` while
    // scrolled.
    let columns_x: i32 = if columns_width <= table_w {
        app.hscroll = 0;
        inner.x as i32 + (table_w - columns_width) as i32 / 2
    } else {
        // Too wide to show at once: keep the selected column in view and move
        // the window, never the column widths.
        let max_scroll = columns_width - table_w;
        if let Some(col) = app.table_state.selected_column()
            && col < col_offsets.len()
        {
            let (start, width) = (col_offsets[col], col_ws[col]);
            if start < app.hscroll {
                app.hscroll = start;
            } else if app.hscroll + table_w < start + width {
                app.hscroll = start + width - table_w;
            }
        }
        app.hscroll = app.hscroll.min(max_scroll);
        inner.x as i32 - app.hscroll as i32
    };

    if columns_width <= table_w {
        let rect = Rect {
            x: columns_x as u16,
            y: inner.y,
            width: columns_width,
            height: table_h,
        };
        frame.render_stateful_widget(table, rect, &mut app.table_state);
    } else {
        // Draw at full width offscreen, then blit the visible window, so a
        // partially-scrolled column clips cleanly at the border.
        let mut canvas = Buffer::empty(Rect::new(0, 0, columns_width, table_h));
        StatefulWidget::render(
            table,
            Rect::new(0, 0, columns_width, table_h),
            &mut canvas,
            &mut app.table_state,
        );
        for row in 0 .. table_h {
            for col in 0 .. table_w {
                let src = Position::new(app.hscroll + col, row);
                if let Some(cell) = canvas.cell(src).cloned()
                    && let Some(dst) = frame.buffer_mut().cell_mut(
                        Position::new(inner.x + col, inner.y + row),
                    )
                {
                    *dst = cell;
                }
            }
        }
    }

    // The table settles its own window while it renders, so the bars are drawn
    // from the offsets it left behind rather than the ones it was handed. Each
    // is given the room the other leaves, so neither counts what the other has
    // taken and the corner between them stays blank.
    let scroll_offset = app.table_state.offset();
    crate::utils::render_scrollbar(
        frame,
        regions,
        body,
        crate::clickmap::ScrollView::ProfitsInventory,
        scroll_offset,
        app.rows.len(),
    );
    crate::utils::render_hscrollbar(
        frame,
        regions,
        Rect {
            width: table_w,
            ..inner
        },
        crate::clickmap::ScrollView::ProfitsInventory,
        app.hscroll as usize,
        columns_width as usize,
    );

    // Register click regions for the name + editable cells. The Sell/Buy
    // columns are only present (and clickable) when prices are entered
    // manually.
    let data_start_y = inner.y + 1; // under the header's row
    let visible_rows = body.height;
    for vis_row in 0 .. visible_rows as usize {
        let data_row = scroll_offset + vis_row;
        if data_row >= app.rows.len() {
            break;
        }
        for (c, (at, w)) in col_offsets.iter().zip(&col_ws).enumerate() {
            // A scrolled column can start left of the box; clip it to what is
            // on screen and drop it entirely once nothing is.
            let left = (columns_x + *at as i32).max(inner.x as i32);
            let right =
                (columns_x + (*at + *w) as i32).min((inner.x + table_w) as i32);
            if right <= left {
                continue;
            }
            regions.push(ClickRegion {
                rect: Rect::new(
                    left as u16,
                    data_start_y + vis_row as u16,
                    (right - left) as u16,
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

/// The Inventory's last row: the search field while the page's cursor is on it,
/// and otherwise how to put it there.
fn render_search(
    frame: &mut Frame,
    area: Rect,
    app: &ProfitsApp,
    shared: &SharedState,
    focused: bool,
    regions: &mut ClickMap,
) {
    if app.focus == Focus::Input {
        // The commodity the query resolves to — the fuzzy match is the
        // suggestion, and one that resolves to none says so where the name
        // would be completed.
        let answer = (!app.search.value.trim().is_empty()).then(|| {
            app.suggest(shared.commodities).map_or("no match", |id| {
                app::commod_name(shared.commodities, id)
            })
        });
        crate::utils::render_search(
            frame,
            area,
            &app.search,
            answer.map(crate::utils::SearchAnswer::Resolved),
            focused,
        );
    } else {
        crate::utils::render_search_invite(frame, area, "add a commodity");
    }
    // The row answers to a click either way: on the field it is where the caret
    // goes, and on the invitation it is what the mouse has instead of `/`.
    regions.push(ClickRegion {
        rect: Rect {
            height: crate::utils::SEARCH_H,
            ..area
        },
        target: ClickTarget::ProfitsInput,
    });
}

/// The text a Parameters field draws on its value side: the value it was given,
/// or the placeholder standing in for one it has not been given yet. Sizing the
/// panel and drawing it both read this, so the panel is never sized to
/// something other than what appears in it.
fn field_text<'a>(
    app: &'a ProfitsApp,
    shared: &SharedState,
    i: usize,
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;

    let field = &app.panel[i];
    if is_place_field(i) {
        if shared.cached_offers.is_empty() {
            return Cow::Borrowed("Fetch market first");
        }
        if field.value.is_empty() {
            return Cow::Borrowed("Ocean-wide");
        }
    } else if i == P_BOOTY_CHEST && field.value.is_empty() {
        // Booty Chest: when blank, the auto-deduced value stands in as a dim
        // placeholder. The calc uses it unless the user types an override.
        return Cow::Owned(app.deduced_chest(shared).to_string());
    }
    Cow::Borrowed(&field.value)
}

fn render_island_field(
    frame: &mut Frame,
    field: &crate::utils::PromptField,
    text: &str,
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
                Line::from(Span::styled(text, btn_style)).right_aligned(),
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
                Line::from(Span::styled(text, ph_style)).right_aligned(),
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
    // "Did ye mean "<name>" (<kind>)? Press enter to accept." in muted text.
    let did_you_mean = |name: &str, kind: &'static str| {
        Some(Text::from(Line::from(vec![
            Span::styled(
                "Did ye mean \"",
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
            hint("Fetch the market first to pick where to sell.")
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
        Focus::Input => {
            hint("Type a commodity and press Enter to add it; Esc dismisses.")
        }
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

/// The "Hold From Clipboard" prompt: the recognized goods with their
/// quantities, any names the commodity list doesn't know, and a No / Yes pair.
fn render_hold_import(
    frame: &mut Frame,
    import: &HoldImport,
    commodities: &[Commodity],
    regions: &mut ClickMap,
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
            "No such goods we know of: {}",
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
        crate::utils::wrapped_line_count(s, inner_w) as usize
    });
    // The note says what the Yes costs, so it is reserved the rows it wraps
    // to rather than the one row it used to be cut off inside.
    const NOTE: &str =
        "Every other row's Stock be cleared. Yer Booty stays as it is.";
    let note_lines = crate::utils::wrapped_line_count(NOTE, inner_w) as usize;
    // header, list, unknowns, note, blank, buttons
    let h: u16 =
        (2 + 1 + list_lines + unknown_lines + note_lines + 1 + 1) as u16;
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(offset_title("Hold From Clipboard").0);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut constraints = vec![Constraint::Length(1)]; // header
    constraints.extend((0 .. list_lines).map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Length(unknown_lines as u16));
    constraints.push(Constraint::Length(note_lines as u16));
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Length(1)); // buttons
    let rows = Layout::vertical(constraints).split(inner);

    frame.render_widget(
        Paragraph::new("Fill the Stock column from the copied hold?"),
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
            NOTE,
            Style::default().fg(Color::DarkGray),
        ))
        .wrap(Wrap {
            trim: true,
        }),
        rows[2 + list_lines],
    );

    let buttons = crate::utils::render_buttons(
        frame,
        rows[rows.len() - 1],
        &["No", "Yes"],
        Some(usize::from(import.yes_focused)),
    );
    for (rect, target) in buttons
        .into_iter()
        .zip([ClickTarget::ProfitsPopupNo, ClickTarget::ProfitsPopupYes])
    {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
}

/// Hang the popup's No / Yes click regions on a two-button row, in that order.
/// Every confirm popup on this page answers to the same two targets, whatever
/// its buttons are called.
fn push_yes_no_regions(regions: &mut ClickMap, buttons: Vec<Rect>) {
    for (rect, target) in buttons
        .into_iter()
        .zip([ClickTarget::ProfitsPopupNo, ClickTarget::ProfitsPopupYes])
    {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
}

fn render_popup(
    frame: &mut Frame,
    popup: &PopupKind,
    breakdown_cursor: usize,
    show_co: bool,
    show_donation: bool,
    commodities: &[Commodity],
    regions: &mut ClickMap,
) {
    let area = frame.area();

    match popup {
        PopupKind::HoldImport(import) => {
            render_hold_import(frame, import, commodities, regions);
        }
        PopupKind::ReQueryConfirm {
            yes_focused,
        } => {
            // The caveat belongs to the question, so it shares its line.
            const QUESTION: &str = "Fetch the market prices afresh?";
            const CAVEAT: &str = "This may take a while.";
            let (block, w) = crate::utils::titled_block(
                "Fetch Afresh?",
                ((QUESTION.len() + 1 + CAVEAT.len()) as u16)
                    .max(yes_no_width()),
            );
            let h: u16 = 5;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            let inner = block.inner(popup_area);
            frame.render_widget(block, popup_area);

            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(QUESTION),
                    Span::raw(" "),
                    Span::styled(
                        CAVEAT,
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
                .centered(),
                rows[0],
            );

            push_yes_no_regions(
                regions,
                crate::utils::render_buttons(
                    frame,
                    rows[2],
                    &["No", "Yes"],
                    Some(usize::from(*yes_focused)),
                ),
            );
        }
        PopupKind::DeleteConfirm {
            row_idx: _,
            name,
            yes_focused,
        } => {
            // As wide as the longer of the question and the buttons, and no
            // wider.
            let text_len = "Delete row \"\"?".len() + name.chars().count();
            let (block, w) = crate::utils::titled_block(
                "Delete Row",
                (text_len as u16).max(yes_no_width()),
            );
            let h: u16 = 5;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
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
            frame.render_widget(
                Paragraph::new(prompt).centered(),
                rows[0],
            );

            push_yes_no_regions(
                regions,
                crate::utils::render_buttons(
                    frame,
                    rows[2],
                    &["No", "Yes"],
                    Some(usize::from(*yes_focused)),
                ),
            );
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
            // Widest of the header, the bulleted commodities and the buttons —
            // the box is as wide as that and no wider.
            const HEADER: &str = "Naught to be had on this island:";
            let buttons_w =
                crate::utils::buttons_width(&["Change Island", "Ocean-wide"]);
            let content_w = shown
                .iter()
                .map(|name| name.chars().count() + "  \u{2022} ".len())
                .max()
                .unwrap_or(0)
                .max(HEADER.len()) as u16;
            let (block, w) = crate::utils::titled_block(
                "Restock Warning",
                content_w.max(buttons_w),
            );
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
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

            frame.render_widget(Paragraph::new(HEADER), rows[0]);
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

            // "Change Island" takes the `No` target, being the choice that
            // changes nothing but where the cursor is.
            push_yes_no_regions(
                regions,
                crate::utils::render_buttons(
                    frame,
                    rows[rows.len() - 1],
                    &["Change Island", "Ocean-wide"],
                    Some(usize::from(*ocean_wide_focused)),
                ),
            );
        }
        PopupKind::PriceBlock {
            need_buy,
            need_sell,
        } => {
            const CAP: usize = 6; // per-section list cap before "...and N more"

            let mut lines: Vec<Line> =
                vec![Line::from("Enter the missing prices first:")];

            let section = |lines: &mut Vec<Line>,
                           field: &'static str,
                           whose: &'static str,
                           items: &[String]| {
                lines.push(Line::from(""));
                // The field's own name, underlined, so it reads as the column
                // the user has to go and fill in.
                lines.push(Line::from(vec![
                    Span::styled("Need a ", Style::default().bold()),
                    Span::styled(
                        field,
                        Style::default().bold().underlined(),
                    ),
                    Span::styled(whose, Style::default().bold()),
                ]));
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
                    "Buy Price",
                    " (to restock):",
                    need_buy,
                );
            }
            if !need_sell.is_empty() {
                section(
                    &mut lines,
                    "Sell Price",
                    " (for excess):",
                    need_sell,
                );
            }

            let content_w =
                lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
            let (block, w) =
                crate::utils::titled_block("Prices Needed", content_w);
            let h: u16 = lines.len() as u16 + 1 /*blank*/ + 1 /*button*/ + 2 /*borders*/;
            let x = area.width.saturating_sub(w) / 2;
            let y = area.height.saturating_sub(h) / 2;
            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
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

            // One button, so it is the focused one.
            for rect in crate::utils::render_buttons(
                frame,
                rows[rows.len() - 1],
                &["Ok"],
                Some(0),
            ) {
                regions.push(ClickRegion {
                    rect,
                    target: ClickTarget::ProfitsPopupOk,
                });
            }
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
                .map(|r| crate::utils::wrapped_line_count(r.desc, inner_w))
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

            for rect in crate::utils::render_buttons(
                frame,
                layout[layout.len() - 1],
                &["Ok"],
                Some(0),
            ) {
                regions.push(ClickRegion {
                    rect,
                    target: ClickTarget::ProfitsPopupOk,
                });
            }
        }
    }
}

#[cfg(test)]
mod inventory_tests {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::{
        Focus,
        InventoryRow,
        P_RESTOCK_RATE,
        PopupKind,
        ProfitsApp,
        render,
    };
    use crate::{
        api::Commodity,
        app::SharedState,
        clickmap::{ClickMap, ClickRegion, ClickTarget},
    };

    /// Render the Profits page and hand back the cell click regions, the
    /// area drawn into, and the screen as text.
    fn draw(names: &[&str], width: u16) -> (Vec<ClickRegion>, Rect, String) {
        draw_at(names, width, 40, Focus::Table)
    }

    /// As [`draw`], in a terminal of the given size and with `focus` where the
    /// caller wants it.
    fn draw_at(
        names: &[&str],
        width: u16,
        height: u16,
        focus: Focus,
    ) -> (Vec<ClickRegion>, Rect, String) {
        draw_state(names, width, height, focus, None)
    }

    /// As [`draw_at`], with `popup` raised over the page.
    fn draw_state(
        names: &[&str],
        width: u16,
        height: u16,
        focus: Focus,
        popup: Option<PopupKind>,
    ) -> (Vec<ClickRegion>, Rect, String) {
        let commodities: Vec<Commodity> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Commodity {
                    id: i as u64 + 1,
                    name: (*name).to_owned(),
                }
            })
            .collect();
        let mut app = ProfitsApp::new();
        for i in 0 .. names.len() {
            app.rows.push(InventoryRow::new(i as u64 + 1));
        }
        app.table_state.select(Some(0));
        app.table_state.select_column(Some(0));
        app.focus = focus;
        app.popup = popup;

        let mut regions = ClickMap::new();
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let area = Rect::new(0, 0, width, height);
        terminal
            .draw(|frame| {
                let shared = SharedState {
                    commodities: &commodities,
                    cached_offers: &Default::default(),
                    available_islands: &[],
                    ocean_geo: None,
                    loading: false,
                    market_supported: false,
                    pillage_gross: 0,
                    pillage_stolen: 0,
                    pillage_chest: 0,
                };
                render(
                    frame,
                    area,
                    &mut app,
                    &shared,
                    true,
                    &mut regions,
                );
            })
            .expect("draw");
        let cells = regions
            .top()
            .iter()
            .filter(|r| {
                matches!(
                    r.target,
                    ClickTarget::ProfitsTableCell { .. }
                )
            })
            .cloned()
            .collect();
        (
            cells,
            area,
            format!("{}", terminal.backend()),
        )
    }

    /// Just the cell click regions and the area they were measured in.
    fn cells(names: &[&str], width: u16) -> (Vec<ClickRegion>, Rect) {
        let (cells, area, _) = draw(names, width);
        (cells, area)
    }

    /// Just the rendered screen.
    fn screen_of(names: &[&str], width: u16) -> String {
        draw(names, width).2
    }

    /// What the page needs of the window cannot move as focus moves: resting on
    /// a field raises a tooltip, and a page that only counted those rows while
    /// one was up would disappear under the user's hands.
    ///
    /// With prices entered by hand the panel shows three fields, so the page
    /// needs 22 rows: 8 for the Inventory (its header, four commodity rows, its
    /// sideways scrollbar's row and its borders), 2 more for the search at its
    /// foot and the blank above it, 3 for the Hold Stats, 7 for the Parameters
    /// and 2 for the tooltip.
    #[test]
    fn what_the_page_needs_does_not_move_with_the_focus() {
        for (height, fits) in [(21, false), (22, true)] {
            // The table carries no tooltip; a panel field does.
            for focus in [Focus::Table, Focus::Panel(P_RESTOCK_RATE)] {
                let screen = draw_at(&["Rum"], 80, height, focus).2;
                assert_eq!(
                    !screen.contains(crate::utils::TOO_SMALL),
                    fits,
                    "{height} rows should {} the page",
                    if fits { "draw" } else { "refuse" },
                );
            }
        }
    }

    /// A popup is drawn over the page, so the page keeps the shape it had while
    /// one is up. The tooltip's rows are the page's whether a prompt owns the
    /// keyboard or not, or the Inventory would gain a pair of commodity rows
    /// the moment a prompt opened and lose them again on its way out.
    #[test]
    fn a_popup_does_not_move_the_page_under_it() {
        let names = ["Rum", "Iron", "Hemp", "Cloth", "Swill"];
        let row_of = |screen: &String, text: &str| {
            screen
                .lines()
                .position(|line| line.contains(text))
                .expect("the page draws the Hold Stats box")
        };

        let page = draw_at(&names, 80, 30, Focus::Table).2;
        let prompted = draw_state(
            &names,
            80,
            30,
            Focus::Popup,
            Some(PopupKind::DeleteConfirm {
                row_idx: 0,
                name: "Rum".to_owned(),
                yes_focused: true,
            }),
        )
        .2;

        assert_eq!(
            row_of(&page, "Hold Stats"),
            row_of(&prompted, "Hold Stats"),
            "the boxes under the Inventory must not move as a popup opens",
        );
    }

    /// A cell you cannot see is a cell you must not be able to click, however
    /// far the table is scrolled.
    #[test]
    fn no_cell_region_escapes_the_page() {
        for (names, width) in [
            (vec!["Rum", "Iron"], 120u16),
            (
                vec!["Fine enchanted midnight broadcloth", "Rum"],
                80,
            ),
        ] {
            let (cells, area) = cells(&names, width);
            assert!(!cells.is_empty(), "no cells at {width}");
            for cell in &cells {
                assert!(
                    area.x <= cell.rect.x
                        && cell.rect.x + cell.rect.width <= area.x + area.width,
                    "cell {:?} outside the page at {width}",
                    cell.rect,
                );
                assert!(
                    cell.rect.width > 0,
                    "empty cell at {width}"
                );
            }
        }
    }

    /// A header belongs over its column, not at the far left of it: the Item
    /// column is as wide as the longest name, so a left-aligned header would
    /// drift away from the column it names.
    #[test]
    fn a_header_is_centered_over_its_column() {
        let names = ["Fine enchanted midnight broadcloth", "Rum"];
        let screen = screen_of(&names, 120);
        let header = screen
            .lines()
            .find(|l| l.contains("Item"))
            .expect("a header row");
        let data = screen
            .lines()
            .find(|l| l.contains(names[0]))
            .expect("a data row");
        let head_x = header.find("Item").expect("Item");
        let name_x = data.find(names[0]).expect("the name");
        assert!(
            name_x < head_x,
            "header at {head_x} is not centered over a column starting at \
             {name_x}",
        );
    }

    /// The table is centered when it is narrower than its box, so the first
    /// column does not start hard against the padding.
    #[test]
    fn a_narrow_table_is_centered_in_its_box() {
        let (cells, _) = cells(&["Rum", "Iron"], 120);
        let first = cells
            .iter()
            .find(|c| {
                matches!(
                    c.target,
                    ClickTarget::ProfitsTableCell {
                        col: 0,
                        ..
                    }
                )
            })
            .expect("a first column cell");
        assert!(
            first.rect.x > 2,
            "first column at {} is not centered",
            first.rect.x,
        );
    }
}
