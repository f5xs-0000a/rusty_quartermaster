//! Rendering for the **Voyage Statistics** page (the entire app body).
//!
//! Layout: a centered, scrolling body — header, the Sea Battles table, the stat
//! sections (Timing / Loot / Divvy / Enemies / Advantage / Consumption), then
//! the three charts as full-width bordered boxes — over a pinned footer, with a
//! focus-bound tooltip strip below the whole widget. The stat numbers and the
//! charts form one focus chain (↑/↓): pressing Down off the last number focuses
//! the first chart. The body scrolls to keep the focused item visible; because
//! the chart boxes are bordered, the body is rendered into an offscreen
//! [`Buffer`] and the visible window blitted, so partially-scrolled boxes clip
//! cleanly. Charts (Ship Winrate table/matrix, PoE-per-fight bars, total-value
//! point-vs-box — each this voyage vs historical) enlarge to a popup on Enter.
//! The page shows the current vessel's live or most-recent-completed run. All
//! figures are computed up-front (see [`crate::voyage::stats`]) and handed in
//! via [`VoyageView`]. See the `voyage-statistics-model` memory.

use ratatui::{
    buffer::Buffer,
    prelude::*,
    widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap},
};

use crate::{
    clickmap::{ClickRegion, ClickTarget},
    damage::DamageApp,
    ships::SHIPS,
    utils::offset_title,
    voyage::{
        AxisMode,
        BattleOutcome,
        BattleSnapshot,
        stats::{
            BattleStats,
            BoxPlot,
            CategoryTally,
            ConsumptionStats,
            box_plot,
        },
    },
};

/// The charts, in display order.
pub const CHART_TITLES: [&str; 3] =
    ["Ship Winrate", "PoE per Fight", "Value per Share"];

/// Which charts can enlarge into a popup (parallel to [`CHART_TITLES`]). The
/// "Value per Share" box-plot shows everything in its mini box, so it has no
/// popup — it stays selectable for its tooltip only. "Ship Winrate" enlarges
/// into the full hull-matchup matrix.
pub const CHART_ENLARGEABLE: [bool; 3] = [true, true, false];

/// One-liners shown below the widget when a chart is focused (parallel to
/// [`CHART_TITLES`]).
const CHART_TOOLTIPS: [&str; 3] = [
    "Win rate of our hull against each enemy hull we've fought this voyage \
     (and, once a hull is picked, historically). Enter for the full \
     ship-matchup matrix.",
    "PoE of each concluded fight, newest first (losses negative), with \
     box-plots for this voyage and the rest of the same-hull voyages. Enter \
     to enlarge.",
    "This voyage's value per share — total value ÷ (pirates summed over its \
     fights; mercenaries and swabbies earn no share) — as a point against a \
     historical box of past voyages. Ship- and length-agnostic.",
];

/// Height in rows of each chart's bordered box in the scrolling body (2 borders
/// + 1 padding each side + 5 content rows).
const CHART_H: u16 = 9;

/// Rows a popup's Close button and the blank row above it take from the box's
/// inside.
const CLOSE_H: u16 = 2;

/// Which button the save prompt has focused. Cancel comes first, being the
/// choice that changes nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Cancel,
    Save,
}

/// Which control the Sea Battles popup has focused. The chain runs top→bottom,
/// matching the on-screen order: the fight pager, the record toggle (rendered
/// directly under the pager), then the (always-editable) calculator below it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum BattlesFocus {
    /// "Battle n of m" — ←/→ change fight (wrapping).
    #[default]
    Pager,
    /// The "Recorded / Not Recorded" toggle.
    Record,
    /// The embedded Damage calculator grid.
    Calc,
}

/// Which voyage the Voyage Statistics page is showing. The page can page across
/// the current login's live runs (read-write) and past runs from the voyages
/// file (read-only); `Live` keeps following the newest run as new ones begin.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VoyageSel {
    /// Follow the newest live run of the current login (default). Auto-advances
    /// as new runs begin — the page never gets "stuck" on an old run unless
    /// pinned.
    #[default]
    Live,
    /// A pinned current-login run, by its stable [`crate::voyage::Voyage::id`].
    Session(u64),
    /// A pinned past run, by index into the loaded voyage history — read-only.
    Saved(usize),
}

/// Persistent UI state for the page (mouse/keyboard-driven).
#[derive(Default)]
pub struct VoyageStatsUi {
    /// Which voyage is shown (see [`VoyageSel`]). `Live` by default.
    pub selected: VoyageSel,
    /// Vertical scroll offset, in rows. Driven by [`Self::focus`] — the body
    /// auto-scrolls to keep the focused item visible.
    pub scroll: u16,
    /// Focused item index. Indices `0..n_stats` are the in-body focusables
    /// (the Sea Battles section, then the Timing-onward stat numbers);
    /// `n_stats.. n_stats+CHART_TITLES.len()` are the charts. The focused
    /// item's tooltip shows below the widget.
    pub focus: usize,
    /// Number of non-chart focusables in the last render — the boundary at
    /// which [`Self::focus`] crosses into the charts. Set by `render`.
    pub n_stats: usize,
    /// Stable key per focusable (label / section title / chart title), in
    /// focus order, from the last render. Lets a page turn re-focus the
    /// *same field* by key rather than by raw index — indices shift when
    /// conditional sections (Divvy/Enemies/Advantage/Consumption) appear
    /// or vanish between voyages.
    pub focus_keys: Vec<String>,
    /// Set by a page turn to the outgoing page's focused-field key; the next
    /// render resolves it to an index on the incoming page (falling back
    /// to the clamped index when that field doesn't exist), then clears
    /// it.
    pub pending_focus_key: Option<String>,
    /// When `Some`, the save/discard prompt is open with this button focused.
    pub prompt: Option<SaveChoice>,
    /// When `Some(i)`, chart `i` is enlarged in a popup.
    pub chart_popup: Option<usize>,
    /// Hovered cell in the enlarged Ship Winrate matrix: `(our hull, enemy
    /// hull)` as [`crate::ships::SHIPS`] indices. Drives the cell + header
    /// highlight; set by mouse hover, cleared when the pointer leaves the
    /// grid or the popup closes.
    pub winrate_hover: Option<(usize, usize)>,
    /// When `Some(i)`, the Sea Battles popup is open on page `i` (battle
    /// index).
    pub battles_popup: Option<usize>,
    /// Interactive Damage calculator bound to the open page's fight. Edits
    /// flow back to that battle (when its recording is on). Reloaded on
    /// open/page-turn.
    pub battle_editor: DamageApp,
    /// Our full crew aboard for the open fight — real pirates + swabbies/named
    /// mercenaries (the "Our strength" figure; also feeds the crew advantage).
    pub editor_crew: u32,
    /// The open fight's foe headcount from the melee, if known (`None` falls
    /// back to the foe ship type's pirate capacity). Drives "Their
    /// strength".
    pub editor_their: Option<u32>,
    /// Mirrors the open fight's `recorded` flag (whether it persists to disk)
    /// — drives the toggle label only; the calculator is always editable.
    pub editor_recorded: bool,
    /// Which control inside the Sea Battles popup currently has focus.
    pub battles_focus: BattlesFocus,
}

/// Per-fight series for the charts: current voyage vs persisted history.
#[derive(Default, Clone)]
pub struct ChartData {
    /// Signed PoE of each concluded (won or lost) fight this voyage, in order
    /// — losses are negative. Drives the per-fight bar chart.
    pub cur_fight_poe: Vec<f64>,
    /// Box-and-whiskers rows drawn beneath the per-fight bars, on the bars'
    /// shared scale. The first is styled as "current", the rest as
    /// "historical". Typically `[Voyage, History]` — this voyage vs the
    /// rest of the same-hull voyages — but the renderer takes any number.
    pub fight_boxes: Vec<ChartBox>,
    /// This voyage's **value per share** — total value ÷ Σ(pirates per
    /// fight). The per-head take a pirate earns; mercenaries and swabbies
    /// hold no share and don't dilute it. `0` when no shares were recorded
    /// yet.
    pub cur_per_share: f64,
    /// Value per share of each past voyage (one point each).
    pub hist_per_share: Vec<f64>,
    /// Ship-vs-ship win rates for the Ship Winrate widget (chart 0).
    pub winrate: ShipWinrate,
}

/// One box-and-whiskers row beneath the per-fight bars. `values` is the signed
/// population; when it's empty the row shows `empty_note` (dimmed) in place of
/// a box, or is skipped entirely if that's `None`.
#[derive(Default, Clone)]
pub struct ChartBox {
    pub label: String,
    pub values: Vec<f64>,
    pub empty_note: Option<String>,
}

/// A win/loss tally over a set of decisive (won or lost) fights. Drives the
/// Ship Winrate widget's cells.
#[derive(Default, Clone, Copy)]
pub struct WinCount {
    pub wins: u32,
    /// Total decisive fights (wins + losses); the count shown in parentheses.
    pub decisive: u32,
}

impl WinCount {
    /// Record one decisive fight.
    pub fn add(&mut self, won: bool) {
        self.decisive += 1;
        if won {
            self.wins += 1;
        }
    }

    /// The `"67% (3)"`-style label, or `None` when no decisive fights are
    /// tallied. The percentage is rounded to the nearest whole (2 of 3 →
    /// `67%`).
    pub fn label(&self) -> Option<String> {
        (self.decisive > 0).then(|| {
            let pct = (self.wins as f64 / self.decisive as f64 * 100.0).round()
                as u32;
            format!("{pct}% ({})", self.decisive)
        })
    }
}

/// Win-rate tallies for the Ship Winrate widget (chart 0), bucketed by hull
/// matchup. Keys are [`crate::ships::SHIPS`] indices. The mini widget shows a
/// per-enemy table for the current voyage; the enlarged widget is a full
/// our-hull × enemy-hull matrix.
#[derive(Default, Clone)]
pub struct ShipWinrate {
    /// Our current voyage's hull, or `None` when the ship picker is unset.
    /// When `None`, the mini table hides the Historical column and the
    /// matrix's voyage layer is all dashes.
    pub our_ship: Option<usize>,
    /// Number of decisive (won/lost) fights this voyage, regardless of whether
    /// the enemy hull is known. Distinguishes "haven't fought yet" from
    /// "fought, but the enemy ships weren't identified" for the mini's
    /// empty state.
    pub voyage_fights: usize,
    /// This voyage's tally against each enemy hull index.
    pub voyage: std::collections::BTreeMap<usize, WinCount>,
    /// Historical tally keyed by `(our hull index, enemy hull index)` across
    /// past voyages (excluding the displayed one).
    pub history: std::collections::BTreeMap<(usize, usize), WinCount>,
}

/// One fight's metadata for the Sea Battles popup. The editable
/// Damage-calculator state lives in [`VoyageStatsUi::battle_editor`];
/// `snapshot` here is the stored value used to (re)load that editor and to tell
/// whether the fight is recorded.
#[derive(Clone, Default)]
pub struct BattleRow {
    /// Enemy vessel name, or `None` if the interception line was unparsed.
    pub enemy: Option<String>,
    pub outcome: BattleOutcome,
    /// Display label for what we fought ("Brigands and Barbarians", "King:
    /// …").
    pub category: String,
    pub poe: Option<i64>,
    pub goods: Option<u32>,
    pub boarding_secs: Option<i64>,
    pub pirates: u32,
    pub swabbies: u32,
    /// The Damage-calculator snapshot for this fight (always editable; may be
    /// `None` until anything is captured/entered).
    pub snapshot: Option<BattleSnapshot>,
    /// Whether the fight is recorded (persisted to disk) —
    /// display-independent.
    pub recorded: bool,
    /// Foe headcount computed from the melee (`None` → use the ship-type
    /// estimate).
    pub their_manpower: Option<u32>,
    /// The foe's known hull type ([`crate::ships::SHIPS`] index) when the
    /// encounter announced it (Black Ship, Monkey Boats). Seeds the
    /// editor's foe ship when no snapshot has overridden it. `None` when
    /// the hull is unknown.
    pub foe_ship: Option<usize>,
    /// Side-tagged elimination timeline driving the advantage-over-time graph
    /// in the Sea Battles popup. Empty when the fight recorded no melee
    /// KOs (or a loaded historical fight that didn't persist one).
    pub timeline: crate::voyage::FightTimeline,
}

/// The pager badge for the shown voyage — its status among all selectable runs.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VoyageBadge {
    /// The in-progress run of the current login.
    Live,
    /// A completed current-login run not yet saved to history.
    #[default]
    Unsaved,
    /// A current-login run already persisted to history this session.
    Saved,
    /// A past run loaded from the voyages file — read-only.
    ReadOnly,
}

/// Everything the page needs to draw one voyage, computed by the caller so this
/// module stays free of app-state plumbing.
pub struct VoyageView {
    pub has_voyage: bool,
    /// This is a read-only history page (loaded from disk): no save/discard,
    /// and the Sea Battles calculator can't be edited.
    pub read_only: bool,
    /// 0-based index of the shown voyage among all selectable runs, and the
    /// total count — drives the `Voyage k of n` pager. `page_count <= 1`
    /// hides it.
    pub page: usize,
    pub page_count: usize,
    /// The shown voyage's status badge in the pager.
    pub badge: VoyageBadge,
    /// Vessel name — the centered headline.
    pub vessel: Option<String>,
    /// Ship type (e.g. "War Frigate"), from the vessel's chosen ship.
    pub ship_type: Option<String>,
    /// The run's clock span, e.g. `"12:34 to 13:50"` (end = current time while
    /// still at sea, or the port time once ported). `None` until we've sailed.
    pub period: Option<String>,
    /// Elapsed run time — final duration if ported, else live elapsed.
    pub elapsed_secs: Option<i64>,
    /// The displayed run is finished and not yet saved/dismissed — offer to
    /// save.
    pub saveable: bool,
    pub battle: BattleStats,
    pub consumption: ConsumptionStats,
    pub charts: ChartData,
    /// Per-fight rows for the Sea Battles popup, in chronological order
    /// (resolved fights first, then the in-progress one if any).
    pub battles: Vec<BattleRow>,
    /// The run reached a booty division — gates the Divvy section below.
    pub divvied: bool,
    /// PoE in the booty chest, frozen onto the voyage at its divvy
    /// (user-entered or auto-deduced). Shown in the Divvy section; `None`
    /// if it was never recorded.
    pub booty_chest: Option<u64>,
    /// Goods pillaged this run — `(commodity, quantity)`, from the Profits
    /// Booty column, frozen at the divvy. Shown itemized in the Divvy
    /// section.
    pub booty_goods: Vec<(String, u64)>,
}

pub fn render(
    frame: &mut Frame,
    full: Rect,
    view: &VoyageView,
    ui: &mut VoyageStatsUi,
    focused: bool,
    regions: &mut Vec<ClickRegion>,
) {
    // Nothing to frame until a voyage exists, so the notice saying so stands in
    // for the whole page rather than sitting inside an empty widget.
    if !view.has_voyage {
        crate::utils::render_page_notice(
            frame,
            full,
            &[
                (
                    "No voyage tracked yet.",
                    Style::default().bold(),
                ),
                (
                    "Set sail on a vessel to begin recording stats.",
                    Style::default().fg(Color::DarkGray),
                ),
            ],
        );
        return;
    }

    // Reserve a tooltip strip below the widget (focus-bound, full content
    // width).
    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(2)])
        .split(full);
    let (widget_area, tip_area) = (outer[0], outer[1]);

    // Build the body rows up front (width-agnostic) so the widget can size
    // itself to its content instead of a fixed guess.
    let mut built = build_lines(view);

    // Natural content width: the widest thing we must show without clipping —
    // the longest `label  value` stat / tally row, the pinned header, and the
    // footer hint. The save hint is the widest chrome; reserve room for it
    // always so the width doesn't jump when the save prompt becomes
    // available.
    const FOOTER_W: usize = 26; // "S  save voyage to history"
    let content_w = {
        // The name + parenthesized hull now share one line, so measure them
        // together (the widest header line drives the panel width).
        let title_w = view
            .vessel
            .as_deref()
            .map(|s| s.chars().count())
            .unwrap_or(0)
            + match view.ship_type.as_deref() {
                Some(t) => t.chars().count() + 3, // " ()" around the hull
                None => "Unknown Ship Hull".chars().count() + 3,
            };
        let header_w = title_w.max(
            view.period
                .as_deref()
                .map(|s| s.chars().count())
                .unwrap_or(0),
        );
        built.natural_width().max(header_w).max(FOOTER_W)
    };

    // Size to content (+2 for the borders) and center. A default floor keeps
    // the panel a comfortable width on short content instead of hugging the
    // text; wider content still expands past it. Only the content is a
    // requirement, so the floor gives way to a narrow window while the
    // content refuses one.
    if crate::utils::too_narrow(frame, full, (content_w + 2) as u16) {
        return;
    }
    const DEFAULT_W: u16 = 50; // total width incl. borders
    let width = ((content_w + 2) as u16)
        .max(DEFAULT_W)
        .min(widget_area.width);
    let area = Rect {
        x: widget_area.x + widget_area.width.saturating_sub(width) / 2,
        y: widget_area.y,
        width,
        height: widget_area.height,
    };

    // Pinned header (ship name / type / clock span), kept out of the scroll so
    // it never disappears. The scroll body starts at "Sea Battles". It is built
    // before the box is drawn so its height can be weighed against the room
    // there is, which is also why its width comes from the box's margin rather
    // than from a box not yet made.
    let iw = width.saturating_sub(crate::utils::BOX_MARGIN) as usize;
    // Ship name (bold) with its hull in parentheses on one centered line, e.g.
    // "Test Vessel (Sloop)". The second line is the run's clock span. An
    // unknown hull reads as a dimmed, italic placeholder in the parentheses.
    let mut title = vec![Span::styled(
        view.vessel.clone().unwrap_or_default(),
        Style::default().bold(),
    )];
    match &view.ship_type {
        Some(t) => {
            title.push(Span::styled(
                format!(" ({t})"),
                Style::default().fg(Color::Gray),
            ))
        }
        None => {
            title.push(Span::styled(
                " (Unknown Ship Hull)".to_string(),
                Style::default().fg(Color::DarkGray).italic(),
            ))
        }
    }
    let mut header: Vec<Line<'static>> = vec![centered_spans(title, iw)];
    if let Some(p) = &view.period {
        header.push(centered_line(
            p.clone(),
            iw,
            Style::default().fg(Color::DarkGray),
        ));
    }
    header.push(Line::from("")); // separator from the scrolling body
    // With the pager shown, the status badge sits under the `Voyage k of n` row
    // on its own line, then a blank, then the ship name / type / clock
    // headline. A saved current-login run shows no badge (and no reserved
    // line).
    if view.page_count > 1
        && let Some((label, style)) = voyage_badge_span(view.badge)
    {
        header.insert(0, Line::from("")); // blank between badge and ship name
        header.insert(
            0,
            centered_line(label.to_string(), iw, style),
        );
    }
    let header_h = header.len() as u16;

    // Split: an optional voyage pager, the pinned header, the scrollable body
    // (Sea Battles + stats + charts), then a pinned footer.
    let show_pager = view.page_count > 1;
    let pager_h = if show_pager { 1 } else { 0 };

    // The body is one scroll region, so what the page needs is a scrollable
    // view's worth of rows beneath everything pinned around it: the pager, the
    // header, the footer, the borders, and the tooltip strip already set aside.
    if crate::utils::too_short(
        frame,
        full,
        2 /*borders*/
            + pager_h
            + header_h
            + crate::utils::SCROLL_MIN_ROWS
            + 1 /*footer*/
            + tip_area.height,
    ) {
        return;
    }

    let (title, _) = offset_title("Voyage Statistics");
    let border = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let parts = Layout::vertical([
        Constraint::Length(pager_h),
        Constraint::Length(header_h),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    let (pager_area, header_area, body, footer) =
        (parts[0], parts[1], parts[2], parts[3]);
    if show_pager {
        render_voyage_pager(frame, pager_area, view, regions);
    }
    frame.render_widget(Paragraph::new(header), header_area);

    // Render the body rows into lines now that the width is fixed. The parallel
    // focusable list (Sea Battles, then the stat numbers) is already populated;
    // the charts follow them in the focus order.
    built.finalize(body.width as usize);
    let n_stats = built.focusable.len();
    let n_charts = CHART_TITLES.len();
    let n_focus = n_stats + n_charts;
    ui.n_stats = n_stats;

    // Focus keys in focus order (stats, then charts) — recorded so the next
    // page turn can carry focus to the same field by key. A pending key set
    // by a page turn is resolved here: land on the matching field, or keep
    // the (clamped) index when the incoming voyage lacks that field.
    ui.focus_keys = built
        .focusable
        .iter()
        .map(|f| f.key.clone())
        .chain(CHART_TITLES.iter().map(|t| t.to_string()))
        .collect();
    if let Some(key) = ui.pending_focus_key.take()
        && let Some(idx) = ui.focus_keys.iter().position(|k| *k == key)
    {
        ui.focus = idx;
    }
    ui.focus = ui.focus.min(n_focus.saturating_sub(1));
    let focused_chart = if ui.focus >= n_stats {
        Some(ui.focus - n_stats)
    } else {
        None
    };

    // Content geometry: stat lines, a 1-row gap, then the stacked chart boxes.
    let n_lines = built.lines.len() as u16;
    let charts_top = n_lines + 1;
    let total_h = charts_top + n_charts as u16 * CHART_H;

    // Row + height of the focused item, for auto-scroll.
    let (focus_row, focus_h) = match focused_chart {
        Some(ci) => {
            (
                charts_top + ci as u16 * CHART_H,
                CHART_H,
            )
        }
        None if n_stats > 0 => (built.focusable[ui.focus].line as u16, 1),
        None => (0, 0),
    };
    let max_scroll = total_h.saturating_sub(body.height);
    if focused {
        if focus_row < ui.scroll {
            ui.scroll = focus_row;
        } else if focus_row + focus_h > ui.scroll + body.height {
            ui.scroll = (focus_row + focus_h).saturating_sub(body.height);
        }
    }
    ui.scroll = ui.scroll.min(max_scroll);

    // Highlight the focused stat row (charts highlight via their border below).
    if focused && focused_chart.is_none() && n_stats > 0 {
        let li = built.focusable[ui.focus].line;
        built.lines[li] = built.lines[li]
            .clone()
            .patch_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    }

    // Render the whole scroll content into an offscreen canvas, then blit the
    // visible window — so partially-scrolled chart boxes clip cleanly.
    let mut canvas = Buffer::empty(Rect::new(
        0,
        0,
        body.width,
        total_h.max(1),
    ));
    Paragraph::new(built.lines).render(
        Rect::new(0, 0, body.width, n_lines),
        &mut canvas,
    );
    render_charts(
        &mut canvas,
        Rect::new(
            0,
            charts_top,
            body.width,
            n_charts as u16 * CHART_H,
        ),
        &view.charts,
        if focused { focused_chart } else { None },
    );
    for row in 0 .. body.height {
        let src_y = ui.scroll + row;
        if src_y >= total_h {
            break;
        }
        for col in 0 .. body.width {
            if let Some(src) = canvas.cell(Position::new(col, src_y)).cloned()
                && let Some(dst) = frame.buffer_mut().cell_mut(Position::new(
                    body.x + col,
                    body.y + row,
                ))
            {
                *dst = src;
            }
        }
    }

    // Click regions: stat rows and chart boxes that fall within the body
    // window.
    for (i, f) in built.focusable.iter().enumerate() {
        let line = f.line as u16;
        if line >= ui.scroll && line < ui.scroll + body.height {
            regions.push(ClickRegion {
                rect: Rect::new(
                    body.x,
                    body.y + (line - ui.scroll),
                    body.width,
                    1,
                ),
                target: ClickTarget::VoyageStat {
                    idx: i,
                },
            });
        }
    }
    for i in 0 .. n_charts {
        let top = charts_top + i as u16 * CHART_H;
        let vis_top = top.max(ui.scroll);
        let vis_bot = (top + CHART_H).min(ui.scroll + body.height);
        if vis_top < vis_bot {
            regions.push(ClickRegion {
                rect: Rect::new(
                    body.x,
                    body.y + (vis_top - ui.scroll),
                    body.width,
                    vis_bot - vis_top,
                ),
                target: ClickTarget::VoyageChart {
                    idx: i,
                },
            });
        }
    }

    // Tooltip for the focused item, below the widget.
    if focused {
        let tip = match focused_chart {
            Some(ci) => CHART_TOOLTIPS[ci].to_string(),
            None if n_stats > 0 => built.focusable[ui.focus].tooltip.clone(),
            None => String::new(),
        };
        if !tip.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(tip))
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap {
                        trim: true,
                    })
                    .centered(),
                tip_area,
            );
        }
    }

    // Footer: save hint takes priority; otherwise the nav hint.
    if view.saveable {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "S  save voyage to history",
                Style::default().fg(Color::Cyan),
            )))
            .centered(),
            footer,
        );
        regions.push(ClickRegion {
            rect: footer,
            target: ClickTarget::VoyageSaveOpen,
        });
    } else if view.read_only {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "read-only history · ←/→ change voyage",
                Style::default().fg(Color::DarkGray),
            )))
            .centered(),
            footer,
        );
    } else {
        let hint = if view.page_count > 1 {
            "↑/↓ move · ←/→ voyage · Enter open/enlarge"
        } else {
            "↑/↓ move · Enter open/enlarge"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )))
            .centered(),
            footer,
        );
    }

    // Modal popups. The Sea Battles and chart popups use the full content
    // width, not the narrow body column. Only one modal is ever open at a
    // time.
    if ui.battles_popup.is_some() {
        render_battles_popup(frame, full, view, ui, regions);
    } else if let Some(i) = ui.chart_popup {
        if i == 0 {
            render_winrate_popup(
                frame,
                full,
                &view.charts.winrate,
                ui.winrate_hover,
                regions,
            );
        } else {
            render_chart_popup(frame, full, i, &view.charts, regions);
        }
    } else if let Some(choice) = ui.prompt {
        render_save_prompt(frame, area, choice, regions);
    }
}

/// The voyage pager row: `‹ Prev   Voyage k of n · <badge>   Next ›`. Shown
/// only when more than one run is selectable (`page_count > 1`). Prev/Next are
/// click targets (dimmed and inert at the ends); the keyboard uses ←/→ on the
/// page.
fn render_voyage_pager(
    frame: &mut Frame,
    area: Rect,
    view: &VoyageView,
    regions: &mut Vec<ClickRegion>,
) {
    let nav = Layout::horizontal([
        Constraint::Length(8),
        Constraint::Min(0),
        Constraint::Length(8),
    ])
    .split(area);
    let at_first = view.page == 0;
    let at_last = view.page + 1 >= view.page_count;
    let arrow = |label: &str, dim: bool| {
        let style = if dim {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Cyan)
        };
        Paragraph::new(Span::styled(label.to_string(), style)).centered()
    };
    frame.render_widget(arrow("‹ Prev", at_first), nav[0]);
    frame.render_widget(arrow("Next ›", at_last), nav[2]);
    if !at_first {
        regions.push(ClickRegion {
            rect: nav[0],
            target: ClickTarget::VoyagePrev,
        });
    }
    if !at_last {
        regions.push(ClickRegion {
            rect: nav[2],
            target: ClickTarget::VoyageNext,
        });
    }
    // Just the `Voyage k of n` count here — the status badge is rendered below,
    // on its own line in the header stack.
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(
                "Voyage {} of {}",
                view.page + 1,
                view.page_count
            ),
            Style::default().bold(),
        ))
        .centered(),
        nav[1],
    );
}

/// The label + colour for a voyage's pager status badge, or `None` when no
/// badge should show — an already-saved current-login run needs no marker.
fn voyage_badge_span(badge: VoyageBadge) -> Option<(&'static str, Style)> {
    match badge {
        VoyageBadge::Live => {
            Some((
                "Live",
                Style::default().fg(Color::DarkGray),
            ))
        }
        VoyageBadge::Unsaved => {
            Some((
                "Unsaved",
                Style::default().fg(Color::Red),
            ))
        }
        VoyageBadge::Saved => None,
        VoyageBadge::ReadOnly => {
            Some((
                "Read-only",
                Style::default().fg(Color::DarkGray),
            ))
        }
    }
}

/// Modal: "Persist this run to your history?" with Cancel and Save.
fn render_save_prompt(
    frame: &mut Frame,
    area: Rect,
    choice: SaveChoice,
    regions: &mut Vec<ClickRegion>,
) {
    // Backdrop swallows clicks outside the box (acts as cancel).
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageSaveCancel,
    });

    const PROMPT: &str = "Persist this run to your history?";
    // The prompt, a blank, and the buttons, inside a padded border.
    let w = (PROMPT.len() as u16 + 4).min(area.width);
    let h = (3 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(" Save Voyage? ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::vertical([
        Constraint::Length(1), // prompt
        Constraint::Length(1), // spacer
        Constraint::Length(1), // buttons
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(PROMPT).centered(),
        rows[0],
    );

    for (rect, target) in crate::utils::render_buttons(
        frame,
        rows[2],
        &["Cancel", "Save"],
        Some(usize::from(choice == SaveChoice::Save)),
    )
    .into_iter()
    .zip([
        ClickTarget::VoyageSaveCancel,
        ClickTarget::VoyageSaveConfirm,
    ]) {
        regions.push(ClickRegion {
            rect,
            target,
        });
    }
}

/// Colour for a battle outcome label.
fn outcome_style(o: BattleOutcome) -> Style {
    let c = match o {
        BattleOutcome::Won => Color::Green,
        BattleOutcome::Lost => Color::Red,
        BattleOutcome::Disengaged => Color::Yellow,
        BattleOutcome::Ongoing => Color::Gray,
        BattleOutcome::Unknown => Color::DarkGray,
    };
    Style::default().fg(c).bold()
}

fn outcome_label(o: BattleOutcome) -> &'static str {
    match o {
        BattleOutcome::Won => "Won",
        BattleOutcome::Lost => "Lost",
        BattleOutcome::Disengaged => "Disengaged",
        BattleOutcome::Ongoing => "In progress",
        BattleOutcome::Unknown => "Unknown",
    }
}

/// Modal: the Sea Battles per-fight pager. Each page shows one fight's
/// metadata, an always-editable Damage calculator bound to its snapshot, and
/// the strengths
/// + advantage derived from it. The Recorded/Not Recorded toggle only controls
///   whether the fight persists to disk — it never gates the display.
fn render_battles_popup(
    frame: &mut Frame,
    area: Rect,
    view: &VoyageView,
    ui: &VoyageStatsUi,
    regions: &mut Vec<ClickRegion>,
) {
    // Backdrop swallows outside clicks (acts as close).
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageBattlesClose,
    });

    // Height = 6 fixed single rows (pager, toggle, blank, ship, category,
    // outcome)
    // + an optional advantage chart + the calculator box + the 7-row stats
    //   table +
    // 2 borders.
    let (_, calc_h) = crate::damage::ui::calc_box_size(false);
    let n = view.battles.len();
    let page = ui.battles_popup.unwrap_or(0).min(n.saturating_sub(1));
    // A compact advantage-over-time chart is shown only when the fight logged
    // melee KOs (live fights always; loaded history only if its timeline
    // was persisted).
    const SB_CHART_PLOT_H: usize = 5;
    let want_chart = n > 0 && !view.battles[page].timeline.events.is_empty();
    let chart_h: u16 = if want_chart {
        SB_CHART_PLOT_H as u16 + 2
    } else {
        0
    };
    let w = 60.min(area.width);
    let h = (calc_h + 15 + chart_h).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .border_style(Style::default().fg(Color::White))
        .title(offset_title("Sea Battles").0);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    if n == 0 {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "No sea battles this voyage.",
                    Style::default().bold(),
                )),
            ])
            .centered(),
            inner,
        );
        return;
    }

    let row = &view.battles[page];
    let recorded = ui.editor_recorded;
    let focus = ui.battles_focus;
    let editor_crew = ui.editor_crew;
    let (box_w, box_h) = crate::damage::ui::calc_box_size(false);

    // Allocate rows explicitly so a short popup never shrinks the calculator
    // box: the 6 single header rows, the optional chart, and the calculator
    // get their height first; the stats table takes only the leftover (it
    // clips before the calc does).
    let body = inner.height.saturating_sub(6);
    let chart_rows = chart_h.min(body);
    let rest = body.saturating_sub(chart_rows);
    let calc_rows = box_h.min(rest);
    let table_h = rest.saturating_sub(calc_rows);
    let parts = Layout::vertical([
        Constraint::Length(1),          // Battle n of m (pager)
        Constraint::Length(1),          // (Not) Recorded toggle
        Constraint::Length(1),          // space
        Constraint::Length(1),          // enemy ship (type)
        Constraint::Length(1),          // type of enemy (category)
        Constraint::Length(1),          // won / lost
        Constraint::Length(chart_rows), // optional advantage-over-time chart
        Constraint::Length(calc_rows),  // embedded damage calculator
        Constraint::Length(table_h),    // per-fight stats table
    ])
    .split(inner);

    // -- Pager: ‹ Prev | Battle k of n | Next › (the center is focusable) --
    let nav = Layout::horizontal([
        Constraint::Length(8),
        Constraint::Min(0),
        Constraint::Length(8),
    ])
    .split(parts[0]);
    let arrow = |label: &str| {
        Paragraph::new(Span::styled(
            label.to_string(),
            Style::default().fg(Color::Cyan),
        ))
        .centered()
    };
    frame.render_widget(arrow("‹ Prev"), nav[0]);
    let pager_style = if focus == BattlesFocus::Pager {
        Style::default().fg(Color::Black).bg(Color::Cyan).bold()
    } else {
        Style::default().bold()
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("Battle {} of {}", page + 1, n),
            pager_style,
        ))
        .centered(),
        nav[1],
    );
    frame.render_widget(arrow("Next ›"), nav[2]);
    regions.push(ClickRegion {
        rect: nav[0],
        target: ClickTarget::VoyageBattlesPrev,
    });
    regions.push(ClickRegion {
        rect: nav[2],
        target: ClickTarget::VoyageBattlesNext,
    });

    // -- Record toggle (focusable), directly under the page number. On a
    // read-only    history page there's nothing to persist, so it's a
    // static marker with no    click target. --
    if view.read_only {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "read-only",
                Style::default().fg(Color::DarkGray),
            ))
            .centered(),
            parts[1],
        );
    } else {
        let rec_label = if recorded { "Recorded" } else { "Not Recorded" };
        let rec_style = if focus == BattlesFocus::Record {
            Style::default().fg(Color::Black).bg(Color::Cyan).bold()
        } else if recorded {
            Style::default().fg(Color::Green).bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(rec_label, rec_style)).centered(),
            parts[1],
        );
        regions.push(ClickRegion {
            rect: parts[1],
            target: ClickTarget::VoyageBattlesRecord,
        });
    }

    // -- (blank parts[2]) then enemy ship "(type)", type of enemy, outcome. The
    //    foe ship type always mirrors the calculator's Right column. --
    let enemy = row
        .enemy
        .clone()
        .unwrap_or_else(|| "Unknown vessel".to_string());
    let ship_line = format!(
        "{enemy} ({})",
        SHIPS[ui.battle_editor.right_ship].name
    );
    frame.render_widget(
        Paragraph::new(ship_line).centered(),
        parts[3],
    );
    // PvP is its own category, labelled "Players" and flagged in magenta.
    let is_pvp = row.category == "Players";
    let cat_style = if is_pvp {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::Gray)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            row.category.clone(),
            cat_style,
        ))
        .centered(),
        parts[4],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            outcome_label(row.outcome),
            outcome_style(row.outcome),
        ))
        .centered(),
        parts[5],
    );

    // -- Optional advantage-over-time chart (parts[6]); only when the fight
    // logged    melee KOs. Wall-clock X-axis (the Sea Battles popup keeps
    // no axis toggle).    Each side's headcount is weighted by its ship's
    // morale advantage, read live    from this fight's calculator — so the
    // curve re-weights as the hits/ships are    edited (both weights are
    // 1.0 at full health → plain headcount). --
    if chart_rows > 0 {
        use crate::damage::Side;
        let w_ours = ui.battle_editor.ship_advantage(Side::Left);
        let w_theirs = ui.battle_editor.ship_advantage(Side::Right);
        let series = row.timeline.advantage_series_weighted(
            AxisMode::Time,
            w_ours,
            w_theirs,
        );
        let chart = fight_chart_lines(
            &series,
            parts[6].width as usize,
            SB_CHART_PLOT_H,
            AxisMode::Time,
        );
        frame.render_widget(Paragraph::new(chart), parts[6]);
    }

    // -- Embedded Damage calculator (the shared widget; highlighted only while
    // the    calculator zone holds focus). Always editable. --
    let calc_box = Rect {
        x: parts[7].x + parts[7].width.saturating_sub(box_w) / 2,
        y: parts[7].y,
        width: box_w.min(parts[7].width),
        height: box_h.min(parts[7].height),
    };
    crate::damage::ui::render_calculator(
        frame,
        calc_box,
        &ui.battle_editor,
        focus == BattlesFocus::Calc,
        regions,
        false,
    );

    // -- Per-fight stats table (label-left / value-right). PoE/Goods/Melee come
    //    from the battle log; "Our strength" is our crew aboard; "Their
    // strength"    is the foe headcount from the melee (falling back to the
    // foe ship type's    pirate capacity); advantages come from the
    // calculator. --
    let their = row.their_manpower.unwrap_or_else(|| {
        SHIPS[ui.battle_editor.right_ship].max_pirates as u32
    });
    let table_rows: [(&str, String); 7] = [
        (
            "PoE won",
            row.poe.map(commas).unwrap_or_else(dash),
        ),
        (
            "Goods",
            row.goods.map(|g| commas(g as i64)).unwrap_or_else(dash),
        ),
        (
            "Melee",
            row.boarding_secs.map(dur).unwrap_or_else(dash),
        ),
        ("Our strength", editor_crew.to_string()),
        ("Their strength", their.to_string()),
        (
            "Advantage",
            format!(
                "{:+.0}%",
                ui.battle_editor.advantage_dmg() * 100.0
            ),
        ),
        (
            "Manpower Advantage",
            format!(
                "{:+.1}",
                ui.battle_editor.crew_advantage(editor_crew, their)
            ),
        ),
    ];
    let label_w = table_rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    let val_w = (parts[8].width as usize).saturating_sub(label_w + 2);
    let stat_lines: Vec<Line> = table_rows
        .iter()
        .map(|(label, value)| {
            Line::from(format!(
                "{label:<label_w$}  {value:>val_w$}"
            ))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(stat_lines).style(Style::default().fg(Color::Gray)),
        parts[8],
    );
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

fn cur_style() -> Style {
    Style::default().fg(Color::Cyan)
}
fn hist_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Render the charts as full-width bordered boxes stacked down `area`, into
/// `canvas` (an offscreen buffer the caller blits into the scroll body).
/// `focused_chart` highlights one box's border.
fn render_charts(
    canvas: &mut Buffer,
    area: Rect,
    data: &ChartData,
    focused_chart: Option<usize>,
) {
    for (i, chart_title) in CHART_TITLES.iter().copied().enumerate() {
        let slot = Rect::new(
            area.x,
            area.y + i as u16 * CHART_H,
            area.width,
            CHART_H,
        );
        let border = if focused_chart == Some(i) {
            Style::default().fg(Color::Cyan).bold()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let (title, _) = offset_title(chart_title);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title)
            .padding(Padding::uniform(1));
        let inner = block.inner(slot);
        block.render(slot, canvas);
        Paragraph::new(chart_lines(
            i,
            data,
            inner.width as usize,
            inner.height as usize,
            false,
        ))
        .render(inner, canvas);
    }
}

/// Render the enlarged chart popup over `area`.
fn render_chart_popup(
    frame: &mut Frame,
    area: Rect,
    idx: usize,
    data: &ChartData,
    regions: &mut Vec<ClickRegion>,
) {
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageChartClose,
    });
    // Chart 0, the Ship Winrate matrix, has its own popup
    // (`render_winrate_popup` above) and never arrives here.
    let w = area.width.saturating_sub(2).min(78).max(24);
    // What the frame costs the content: borders, the blank row the padding
    // keeps at the top, and the blank + Close rows at the foot.
    const FRAME_H: u16 = 2 + 1 + CLOSE_H;
    // These charts draw a row per fight and a row per box beneath them, so the
    // box is as tall as it has rows to show. A taller window is spent on more
    // fights, not on blank space, which is why the lines are measured at the
    // tallest the window allows and the box then shrinks to them.
    let max_h = area.height.saturating_sub(2).max(6);
    let lines = chart_lines(
        idx,
        data,
        w.saturating_sub(crate::utils::BOX_MARGIN) as usize,
        max_h.saturating_sub(FRAME_H) as usize,
        true,
    );
    let h = (lines.len() as u16 + FRAME_H).clamp(6, max_h);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let (title, _) = offset_title(CHART_TITLES[idx]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::White))
        .title(title)
        .padding(Padding::new(1, 1, 1, 0));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // The chart, then a blank row and the Close button on the last row.
    let parts = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(lines), parts[0]);
    crate::utils::render_close_button(frame, parts[2]);
    regions.push(ClickRegion {
        rect: parts[2],
        target: ClickTarget::VoyageChartClose,
    });
}

/// Build the lines for chart `idx`, fitting `width`×`height` (used at both
/// mini and enlarged sizes — it scales by the area it's given). `enlarged` is
/// set for the popup, which shows more rows and the full legend.
fn chart_lines(
    idx: usize,
    data: &ChartData,
    width: usize,
    height: usize,
    enlarged: bool,
) -> Vec<Line<'static>> {
    match idx {
        // The Ship Winrate mini is always the compact table; its enlarged form
        // is a dedicated Rect-based matrix popup (see
        // `render_winrate_popup`), not routed through here.
        0 => ship_winrate_table(&data.winrate, width, height),
        1 => poe_bar_lines(data, width, height, enlarged),
        _ => per_share_lines(data, width),
    }
}

/// Width of the row-label column on the Total-value chart (fits "Historical" +
/// gap).
const PBOX_LABEL_W: usize = 12;

/// Mini Ship Winrate table: one row per enemy hull met this voyage, with a
/// Voyage column and — only when our hull is known — a Historical column.
fn ship_winrate_table(
    wr: &ShipWinrate,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let show_hist = wr.our_ship.is_some();
    // Enemy hulls met this voyage, in SHIPS order (BTreeMap keys are sorted).
    let rows: Vec<usize> = wr.voyage.keys().copied().collect();
    if rows.is_empty() {
        // Distinguish "no fights yet" from "fought, but no enemy hull was
        // identified" (the winrate needs the enemy ship type to bucket
        // a fight). Both are centered horizontally and vertically, dim
        // italic — like the PoE chart's empty state.
        let note = if wr.voyage_fights == 0 {
            "Engage in a Sea Battle first"
        } else {
            "Enemy ship types not identified"
        };
        let mut lines = vec![Line::from(""); height.saturating_sub(1) / 2];
        lines.push(centered_line(
            note.to_string(),
            width,
            Style::default().fg(Color::DarkGray).italic(),
        ));
        return lines;
    }
    let voy_of = |e: usize| {
        wr.voyage
            .get(&e)
            .and_then(|w| w.label())
            .unwrap_or_else(|| "—".to_string())
    };
    let hist_of = |e: usize| {
        wr.our_ship
            .and_then(|o| wr.history.get(&(o, e)))
            .and_then(|w| w.label())
            .unwrap_or_else(|| "—".to_string())
    };
    let ship_w = rows
        .iter()
        .map(|&e| SHIPS[e].name.len())
        .chain([4])
        .max()
        .unwrap();
    let voy_w = rows
        .iter()
        .map(|&e| voy_of(e).len())
        .chain(["Voyage".len()])
        .max()
        .unwrap();
    let hist_w = rows
        .iter()
        .map(|&e| hist_of(e).len())
        .chain(["Historical".len()])
        .max()
        .unwrap();

    // Horizontal centering: pad every line to the same offset so columns stay
    // aligned while the whole table sits centered in the widget.
    let table_w = ship_w + 1 + voy_w + if show_hist { 1 + hist_w } else { 0 };
    let left_pad = width.saturating_sub(table_w) / 2;
    let pad = || Span::raw(" ".repeat(left_pad));

    let bold = Style::default().bold();
    let mut lines = Vec::new();
    // Header row — every header centered over its column.
    let mut head = vec![
        pad(),
        Span::styled(format!("{:^ship_w$}", "Ship"), bold),
        Span::raw(" "),
        Span::styled(format!("{:^voy_w$}", "Voyage"), bold),
    ];
    if show_hist {
        head.push(Span::raw(" "));
        head.push(Span::styled(
            format!("{:^hist_w$}", "Historical"),
            bold,
        ));
    }
    lines.push(Line::from(head));
    // One row per enemy hull: ship name left-aligned, the rates centered.
    for &e in &rows {
        let mut spans = vec![
            pad(),
            Span::raw(format!("{:<ship_w$}", SHIPS[e].name)),
            Span::raw(" "),
            Span::styled(
                format!("{:^voy_w$}", voy_of(e)),
                cur_style(),
            ),
        ];
        if show_hist {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{:^hist_w$}", hist_of(e)),
                hist_style(),
            ));
        }
        lines.push(Line::from(spans));
    }
    // Vertical centering: pad the block down to the middle of the widget.
    let top_pad = height.saturating_sub(lines.len()) / 2;
    let mut out = vec![Line::from(""); top_pad];
    out.extend(lines);
    out
}

/// Enlarged Ship Winrate matrix popup: our hull (rows) × enemy hull (columns),
/// sized to its content with a 1-cell screen margin. A voyage is sailed on a
/// single hull, so only **our current hull's** row carries a Current Voyage
/// rate — rendered as two lines (this voyage's rate on top, the grayed
/// historical rate below, the hull tag on the historical line). Every other row
/// is a single historical-only line. When our hull is unknown, no row shows a
/// voyage rate. Both axes list the full [`SHIPS`] roster. `hover` highlights a
/// cell and its row/column headers. Dash rules: no voyage encounter → a dash
/// where the voyage rate would be; no encounters at all → a lone dash on the
/// historical line.
fn render_winrate_popup(
    frame: &mut Frame,
    area: Rect,
    wr: &ShipWinrate,
    hover: Option<(usize, usize)>,
    regions: &mut Vec<ClickRegion>,
) {
    // Backdrop closes on click; registered first so per-cell hovers win the
    // hit-test.
    regions.push(ClickRegion {
        rect: area,
        target: ClickTarget::VoyageChartClose,
    });

    let ours = wr.our_ship;
    let show_voyage = ours.is_some() && !wr.voyage.is_empty();
    let abbr = |i: usize| SHIPS[i].abbr.iter().collect::<String>();
    let voy = |c: usize| wr.voyage.get(&c).and_then(|w| w.label());
    let hist =
        |r: usize, c: usize| wr.history.get(&(r, c)).and_then(|w| w.label());
    let (title, _) = offset_title(CHART_TITLES[0]);

    // No bottom padding: the Close button is the last row, and a blank row
    // under a button is room reserved for nothing.
    let make_block = |title: &str| {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::White))
            .title(title.to_string())
            .padding(Padding::new(1, 1, 1, 0))
    };

    // Nothing to display → a small centered note.
    if wr.history.is_empty() && !show_voyage {
        // One line to say, so the box is one line tall, its Close row and the
        // frame's own margins aside.
        const NOTE: &str = "No sea battles recorded yet";
        let w = (NOTE.len() as u16 + 4)
            .max(crate::utils::offset_title_width(
                CHART_TITLES[0],
            ))
            .min(area.width.saturating_sub(2));
        let h = (1 + CLOSE_H + 3).min(area.height.saturating_sub(2));
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        frame.render_widget(Clear, rect);
        let block = make_block(&title);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let parts =
            Layout::vertical([Constraint::Min(0), Constraint::Length(CLOSE_H)])
                .split(inner);
        frame.render_widget(
            Paragraph::new(NOTE)
                .style(Style::default().fg(Color::DarkGray).italic())
                .centered(),
            parts[0],
        );
        let close = Rect {
            y: parts[1].y + parts[1].height.saturating_sub(1),
            height: 1,
            ..parts[1]
        };
        crate::utils::render_close_button(frame, close);
        regions.push(ClickRegion {
            rect: close,
            target: ClickTarget::VoyageChartClose,
        });
        return;
    }

    let n = SHIPS.len();
    // Column width = widest label (floored at 2 for the abbreviations / dash).
    let mut cw = 2usize;
    for c in 0 .. n {
        if let Some(s) = voy(c) {
            cw = cw.max(s.chars().count());
        }
        for r in 0 .. n {
            if let Some(s) = hist(r, c) {
                cw = cw.max(s.chars().count());
            }
        }
    }
    let cw = cw as u16;
    let lw = 3u16; // row-label gutter (hull tag)
    let stride = cw + 1; // 1-column gap between columns

    let legend: [&str; 3] = [
        "left = our ship   ·   top = opposing ship",
        "cell = winrate% (total fights)",
        "colored hull: this voyage (upper) / historical (lower)",
    ];

    let grid_w = lw + (n as u16) * cw + (n as u16 - 1); // cells + 1-wide gaps
    let legend_w = legend
        .iter()
        .map(|s| s.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let content_w = grid_w.max(legend_w);
    let body_h = n as u16 + if show_voyage { 1 } else { 0 };
    let content_h = 1 + body_h + 1 + legend.len() as u16; // header + body + blank + legend

    // Size to content, leaving a 1-cell screen margin all around.
    let w = (content_w + 4).min(area.width.saturating_sub(2)).max(12);
    let h = (content_h + CLOSE_H + 3)
        .min(area.height.saturating_sub(2))
        .max(6);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = make_block(&title);
    let outer_inner = block.inner(rect);
    frame.render_widget(block, rect);
    // The grid keeps everything but the Close row at the foot.
    let parts = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(CLOSE_H.min(outer_inner.height)),
    ])
    .split(outer_inner);
    let inner = parts[0];
    let close = Rect {
        y: parts[1].y + parts[1].height.saturating_sub(1),
        height: 1,
        ..parts[1]
    };
    crate::utils::render_close_button(frame, close);
    regions.push(ClickRegion {
        rect: close,
        target: ClickTarget::VoyageChartClose,
    });

    let hl = Style::default().bg(Color::White).fg(Color::Black).bold();
    let bold = Style::default().bold();
    let grid_x = inner.x + lw;
    let right = inner.x + inner.width;
    let bottom = inner.y + inner.height;
    let col_x = |c: u16| grid_x + c * stride;

    // Column headers (enemy hull tags).
    for c in 0 .. n as u16 {
        let x = col_x(c);
        if x + cw > right {
            break;
        }
        let hot = hover.is_some_and(|(_, hc)| hc == c as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                abbr(c as usize),
                if hot { hl } else { bold },
            )))
            .centered(),
            Rect::new(x, inner.y, cw, 1),
        );
    }

    // Data rows.
    let mut y = inner.y + 1;
    for r in 0 .. n {
        if y >= bottom {
            break;
        }
        let is_cur = show_voyage && Some(r) == ours;
        let row_h: u16 = if is_cur { 2 } else { 1 };
        let hist_y = if is_cur { y + 1 } else { y };
        let row_hot = hover.is_some_and(|(hr, _)| hr == r);

        // Hull tag on the historical line — colored for our current hull.
        if hist_y < bottom {
            let base = if is_cur { cur_style().bold() } else { bold };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    abbr(r),
                    if row_hot { hl } else { base },
                ))),
                Rect::new(inner.x, hist_y, lw, 1),
            );
        }

        for c in 0 .. n {
            let x = col_x(c as u16);
            if x + cw > right {
                break;
            }
            let cell_hot = hover == Some((r, c));
            if hist_y < bottom {
                let s = hist(r, c).unwrap_or_else(|| "—".to_string());
                let style = if cell_hot { hl } else { hist_style() };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(s, style)))
                        .centered(),
                    Rect::new(x, hist_y, cw, 1),
                );
            }
            if is_cur && y < bottom {
                let s = voy(c).unwrap_or_else(|| "—".to_string());
                let style = if cell_hot { hl } else { cur_style() };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(s, style)))
                        .centered(),
                    Rect::new(x, y, cw, 1),
                );
            }
            // Hover/click region spans the whole cell (both lines for our
            // hull).
            let top = if is_cur { y } else { hist_y };
            if top < bottom {
                regions.push(ClickRegion {
                    rect: Rect::new(x, top, cw, row_h.min(bottom - top)),
                    target: ClickTarget::VoyageWinrateCell {
                        row: r,
                        col: c,
                    },
                });
            }
        }
        y += row_h;
    }

    // Legend, after a blank line.
    for (ly, line) in (y + 1 ..).zip(legend) {
        if ly >= bottom {
            break;
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )))
            .centered(),
            Rect::new(inner.x, ly, inner.width, 1),
        );
    }
}

/// Chart 1 — signed PoE bars for this voyage's concluded fights (newest at
/// top), with one box-and-whiskers row per [`ChartData::fight_boxes`]
/// population below them, all on one shared scale. See
/// [`signed_bars_with_boxes`].
fn poe_bar_lines(
    data: &ChartData,
    width: usize,
    height: usize,
    enlarged: bool,
) -> Vec<Line<'static>> {
    signed_bars_with_boxes(
        &data.cur_fight_poe,
        &data.fight_boxes,
        width,
        height,
        enlarged,
    )
}

/// Signed horizontal bars — one per value in `bars` (chronological; index `i`
/// is fight `#(i+1)`), newest at the top, with the `#N` label on the left and
/// the value on the right — followed by any number of box-and-whiskers rows
/// from `boxes` (`(label, population)`), drawn beneath on the **same**
/// zero-bracketing scale and the **same** column band so bars and boxes line up
/// glyph-for-glyph.
///
/// General-purpose: a lost fight (negative value) extends left in red; the
/// first box is drawn in the "current" (cyan) style and the rest in the
/// "historical" (gray) style. The mini widget shows at most the latest 5 bars;
/// the enlarged popup shows as many as fit. Empty box populations are skipped.
fn signed_bars_with_boxes(
    bars: &[f64],
    boxes: &[ChartBox],
    width: usize,
    height: usize,
    enlarged: bool,
) -> Vec<Line<'static>> {
    if bars.is_empty() {
        // Centered horizontally and vertically in the plot area.
        let mut lines = vec![Line::from(""); height.saturating_sub(1) / 2];
        lines.push(centered_line(
            "Engage in a Sea Battle first".to_string(),
            width,
            Style::default().fg(Color::DarkGray).italic(),
        ));
        return lines;
    }
    // A box row renders if it has data, or a note to show in place of data.
    let renders =
        |b: &ChartBox| box_plot(&b.values).is_some() || b.empty_note.is_some();
    // Reserve one row per rendering box, then fit bars in the rest.
    let box_rows = boxes.iter().filter(|b| renders(b)).count();
    let fits = height.saturating_sub(box_rows).max(1);
    let cap = if enlarged { fits } else { fits.min(5) };
    let start = bars.len().saturating_sub(cap);
    // Newest first, keeping each value's 1-based index.
    let shown: Vec<(usize, f64)> = bars
        .iter()
        .enumerate()
        .skip(start)
        .map(|(i, &v)| (i + 1, v))
        .rev()
        .collect();

    // The left gutter fits the widest of the "#N" bar labels and the box
    // labels, so the bar band and every box band begin at the same column.
    let idx_w = shown
        .iter()
        .map(|(i, _)| format!("#{i}").chars().count())
        .max()
        .unwrap_or(2);
    let label_w = boxes
        .iter()
        .map(|b| b.label.chars().count())
        .chain(std::iter::once(idx_w))
        .max()
        .unwrap_or(idx_w);
    let vals_s: Vec<String> = shown
        .iter()
        .map(|(_, v)| commas(v.round() as i64))
        .collect();
    let val_w = vals_s.iter().map(|s| s.chars().count()).max().unwrap_or(1);
    let bar_cols = width.saturating_sub(label_w + 2 + 1 + val_w).max(1);

    // One shared scale over the shown bars *and* every box population, always
    // bracketing zero so `signed_bar`'s zero boundary lines up across all rows.
    let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
    for v in shown
        .iter()
        .map(|(_, v)| *v)
        .chain(boxes.iter().flat_map(|b| b.values.iter().copied()))
    {
        lo = lo.min(v);
        hi = hi.max(v);
    }

    let mut lines = Vec::new();
    for ((i, v), vs) in shown.iter().zip(&vals_s) {
        let label = format!("{:>label_w$}", format!("#{i}"));
        let bar = signed_bar(*v, lo, hi, bar_cols);
        let style = if *v < 0.0 {
            Style::default().fg(Color::Red)
        } else {
            cur_style()
        };
        lines.push(Line::from(Span::styled(
            format!("{label}  {bar} {vs:>val_w$}"),
            style,
        )));
    }
    // Box rows — same band (`label_w + 2` prefix, then `bar_cols` cells) and
    // scale as the bars above. First box "current", rest "historical". A
    // box with no data shows its `empty_note` (dimmed) instead, or is
    // skipped.
    for (n, b) in boxes.iter().enumerate() {
        if let Some(bp) = box_plot(&b.values) {
            let style = if n == 0 { cur_style() } else { hist_style() };
            lines.push(Line::from(Span::styled(
                format!(
                    "{:<label_w$}  {}",
                    b.label,
                    signed_box_line(bar_cols, lo, hi, &bp)
                ),
                style,
            )));
        } else if let Some(note) = &b.empty_note {
            // A blank row above it, so the note reads as standing in for the
            // box that is missing rather than as a caption on the
            // row above.
            lines.push(Line::from(""));
            lines.push(centered_line(
                note.clone(),
                width,
                Style::default().fg(Color::DarkGray).italic(),
            ));
        }
    }
    lines
}

/// A horizontal bar of `cols` cells with zero on the *boundary* between the
/// negative and positive halves — not on a shared cell. The field is split into
/// `neg_w` cells left of zero and `w - neg_w` right of it, proportional to how
/// far `lo`/`hi` reach so both sides share one unit-per-cell. A positive `v`
/// fills rightward from the boundary; a negative `v` fills leftward up to it.
/// Because the two halves are disjoint, a win bar and a loss bar can never
/// collide on the zero column (the bug a single shared zero cell caused).
fn signed_bar(v: f64, lo: f64, hi: f64, cols: usize) -> String {
    let w = cols.max(1);
    let mut cells = vec![' '; w];
    let span = hi - lo; // lo ≤ 0 ≤ hi by construction, so span ≥ 0
    if span <= 0.0 {
        return cells.into_iter().collect();
    }
    // Too narrow to place a boundary — show a single cell for any nonzero
    // fight.
    if w < 2 {
        if v != 0.0 {
            cells[0] = '█';
        }
        return cells.into_iter().collect();
    }
    // Cells left of the zero boundary (index `neg_w` is the first positive
    // cell).
    let neg_w = neg_width(lo, hi, w);
    if v > 0.0 {
        let pos_w = w - neg_w;
        let len = (((v / hi) * pos_w as f64).round() as usize)
            .max(1)
            .min(pos_w);
        for cell in cells.iter_mut().skip(neg_w).take(len) {
            *cell = '█';
        }
    } else if v < 0.0 {
        // v/lo is positive (both negative); fills the cells just left of
        // `neg_w`.
        let len = (((v / lo) * neg_w as f64).round() as usize)
            .max(1)
            .min(neg_w);
        for cell in cells.iter_mut().take(neg_w).skip(neg_w - len) {
            *cell = '█';
        }
    }
    cells.into_iter().collect()
}

/// Cells left of the zero boundary in a `signed_bar`/`signed_box_line` of `w`
/// cells over `[lo, hi]` (index `neg_w` is the first positive cell). When both
/// signs are present, force at least one cell on each side so a small minority
/// value isn't rounded into invisibility. Shared so bars and the reference box
/// place their zero boundary on the exact same column.
fn neg_width(lo: f64, hi: f64, w: usize) -> usize {
    let w = w.max(1);
    let span = hi - lo;
    if span <= 0.0 || w < 2 {
        return 0;
    }
    if lo < 0.0 && hi > 0.0 {
        (((-lo / span) * w as f64).round() as usize).clamp(1, w - 1)
    } else if lo < 0.0 {
        w
    } else {
        0
    }
}

/// Column at which a value `v` lands in a `signed_bar` band — i.e. the cell a
/// bar of that value would reach — so a reference box drawn with these columns
/// lines up with the bars glyph-for-glyph. Positive `v` measures rightward from
/// the zero boundary over `hi`, negative leftward over `lo`, matching
/// `signed_bar`.
fn signed_col(v: f64, lo: f64, hi: f64, w: usize) -> usize {
    let w = w.max(1);
    let neg_w = neg_width(lo, hi, w);
    if v > 0.0 && hi > 0.0 {
        let pos_w = (w - neg_w).max(1);
        let len = (((v / hi) * pos_w as f64).round() as usize)
            .max(1)
            .min(pos_w);
        (neg_w + len).saturating_sub(1).min(w - 1)
    } else if v < 0.0 && lo < 0.0 && neg_w > 0 {
        let len = (((v / lo) * neg_w as f64).round() as usize)
            .max(1)
            .min(neg_w);
        neg_w.saturating_sub(len).min(w - 1)
    } else {
        neg_w.min(w - 1)
    }
}

/// A box-and-whiskers of `w` cells drawn on the *same* scale and column band as
/// [`signed_bar`] (via [`signed_col`]), so it aligns under the PoE-per-fight
/// bars.
fn signed_box_line(w: usize, lo: f64, hi: f64, bp: &BoxPlot) -> String {
    let w = w.max(1);
    let mut cells = vec![' '; w];
    if bp.n == 1 {
        cells[signed_col(bp.median, lo, hi, w)] = '●';
    } else {
        let cmin = signed_col(bp.min, lo, hi, w);
        let cmax = signed_col(bp.max, lo, hi, w);
        let (cmin, cmax) = (cmin.min(cmax), cmin.max(cmax));
        let cq1 = signed_col(bp.q1, lo, hi, w);
        let cq3 = signed_col(bp.q3, lo, hi, w);
        let (cq1, cq3) = (cq1.min(cq3), cq1.max(cq3));
        for cell in cells.iter_mut().take(cmax + 1).skip(cmin) {
            *cell = '─';
        }
        for cell in cells.iter_mut().take(cq3 + 1).skip(cq1) {
            *cell = '█';
        }
        cells[cmin] = '├';
        cells[cmax] = '┤';
        cells[signed_col(bp.median, lo, hi, w)] = '┃';
    }
    cells.into_iter().collect()
}

/// Chart 2 — value per share: this voyage's per-head take (total value ÷
/// Σ(pirates per fight)) as a single point (the Current row) against a
/// historical box & whiskers of past voyages' per-share values, sharing one
/// axis, with a legend below. No popup.
fn per_share_lines(data: &ChartData, width: usize) -> Vec<Line<'static>> {
    let axis = width.saturating_sub(PBOX_LABEL_W);
    let mut all = data.hist_per_share.clone();
    all.push(data.cur_per_share);
    let range = combined_range(&[all.as_slice()]);
    let mut lines = vec![
        box_or_msg(
            "Current",
            PBOX_LABEL_W,
            box_plot(&[data.cur_per_share]),
            range,
            axis,
            None,
            cur_style(),
        ),
        box_or_msg(
            "Historical",
            PBOX_LABEL_W,
            box_plot(&data.hist_per_share),
            range,
            axis,
            None,
            hist_style(),
        ),
    ];
    if let Some((lo, hi)) = range {
        lines.push(axis_line(lo, hi, width, PBOX_LABEL_W));
    }
    lines.push(Line::from(Span::styled(
        "Legend:",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "  ● - This Voyage",
        Style::default().fg(Color::Cyan),
    )));
    lines
}

/// A box-and-whiskers row prefixed by `label` (padded to `label_w` columns), or
/// a dim "no data" message.
fn box_or_msg(
    label: &str,
    label_w: usize,
    bp: Option<BoxPlot>,
    range: Option<(f64, f64)>,
    axis: usize,
    marker: Option<(f64, char)>,
    style: Style,
) -> Line<'static> {
    match (bp, range) {
        (Some(bp), Some((lo, hi))) if axis > 0 => {
            Line::from(Span::styled(
                format!(
                    "{label:<label_w$}{}",
                    box_line(axis, lo, hi, &bp, marker)
                ),
                style,
            ))
        }
        _ => {
            Line::from(Span::styled(
                format!("{label:<label_w$}(no data)"),
                Style::default().fg(Color::DarkGray),
            ))
        }
    }
}

/// A "lo …… hi" axis label line, its numbers aligned under a `label_w`-indented
/// box row.
fn axis_line(lo: f64, hi: f64, width: usize, label_w: usize) -> Line<'static> {
    let lo_s = commas(lo.round() as i64);
    let hi_s = commas(hi.round() as i64);
    let gap = width
        .saturating_sub(label_w + lo_s.len() + hi_s.len())
        .max(1);
    Line::from(Span::styled(
        format!(
            "{}{lo_s}{}{hi_s}",
            " ".repeat(label_w),
            " ".repeat(gap)
        ),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Column for value `v` within `[lo, hi]` mapped onto `width` cells.
fn val_col(v: f64, lo: f64, hi: f64, width: usize) -> usize {
    if width <= 1 || hi <= lo {
        return 0;
    }
    (((v - lo) / (hi - lo)) * (width - 1) as f64)
        .round()
        .clamp(0.0, (width - 1) as f64) as usize
}

/// Build a horizontal box-and-whiskers string of `width` cells over `[lo, hi]`.
/// `n == 1` renders a single point; `marker` overlays its char at its value.
fn box_line(
    width: usize,
    lo: f64,
    hi: f64,
    bp: &BoxPlot,
    marker: Option<(f64, char)>,
) -> String {
    let w = width.max(1);
    let mut cells = vec![' '; w];
    if bp.n == 1 {
        cells[val_col(bp.median, lo, hi, w)] = '●';
    } else {
        let cmin = val_col(bp.min, lo, hi, w);
        let cmax = val_col(bp.max, lo, hi, w);
        let cq1 = val_col(bp.q1, lo, hi, w);
        let cq3 = val_col(bp.q3, lo, hi, w);
        for cell in cells.iter_mut().take(cmax + 1).skip(cmin) {
            *cell = '─';
        }
        for cell in cells.iter_mut().take(cq3 + 1).skip(cq1) {
            *cell = '█';
        }
        cells[cmin] = '├';
        cells[cmax] = '┤';
        cells[val_col(bp.median, lo, hi, w)] = '┃';
    }
    if let Some((m, ch)) = marker {
        cells[val_col(m, lo, hi, w)] = ch;
    }
    cells.into_iter().collect()
}

/// Format a signed advantage value for the Y-axis gutter, to one decimal
/// (`+6.0`, `0.0`, `-3.5`). `+ 0.0` normalises a possible `-0.0` to `0.0`.
fn fmt_adv(v: f64) -> String {
    let v = v + 0.0;
    if v > 0.0 {
        format!("+{v:.1}")
    } else {
        format!("{v:.1}")
    }
}

/// Format a duration in seconds as `M:SS` for the time axis.
fn fmt_mmss(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Render a per-fight **advantage-over-time** line graph: a single signed line
/// over a zero baseline, `height` plot rows tall plus two axis rows. `series`
/// is `(x, advantage)` — the advantage is a float so the Sea Battles caller can
/// pass a morale-weighted curve ([`FightTimeline::advantage_series_weighted`]);
/// the wave charts pass their raw `our_alive − their_alive` headcount as
/// floats. `x` is seconds under [`AxisMode::Time`], else the event index. Drawn
/// in the same text/braille style as the other charts (per-cell sign colouring:
/// cyan when we're ahead, red when behind). Y labels are one-decimal. Shared by
/// the jobbers per-fight popup and the Sea Battles popup.
pub fn fight_chart_lines(
    series: &[(f64, f64)],
    width: usize,
    height: usize,
    axis: AxisMode,
) -> Vec<Line<'static>> {
    let rows = height.max(3);
    if series.is_empty() {
        return vec![Line::from(Span::styled(
            "(no fight data)",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    // Y-range, always spanning zero (the baseline), with a 1-unit minimum span.
    let (mut ymin, mut ymax) =
        series.iter().fold((0f64, 0f64), |(lo, hi), &(_, v)| {
            (lo.min(v), hi.max(v))
        });
    if (ymax - ymin) < 1.0 {
        ymin -= 0.5;
        ymax += 0.5;
    }
    let span = ymax - ymin;
    let row_of = |v: f64| -> usize {
        (((ymax - v) / span) * (rows - 1) as f64)
            .round()
            .clamp(0.0, (rows - 1) as f64) as usize
    };
    let zero_row = row_of(0.0);
    let xmax = series.last().map(|&(x, _)| x).unwrap_or(0.0).max(1.0);

    // Gutter sized to the widest Y label (one-decimal, signed), so a fractional
    // or large-magnitude label never shoves the axis column out of alignment.
    let ymax_lbl = fmt_adv(ymax);
    let ymin_lbl = fmt_adv(ymin);
    let label_w = ymax_lbl.len().max(ymin_lbl.len()).max(3); // >= "0.0"
    let gutter = label_w + 1; // label field + a space
    let plot_w = width.saturating_sub(gutter + 1).max(2); // +1 for the axis column

    // Rasterize the step function into a (char, colour) grid.
    let mut cells = vec![vec![(' ', Color::Reset); plot_w]; rows];
    for cell in cells[zero_row].iter_mut() {
        *cell = ('┄', Color::DarkGray);
    }
    let mut idx = 0usize;
    let mut prev_row: Option<usize> = None;
    let denom = (plot_w - 1).max(1) as f64;
    for (c, x) in (0 .. plot_w).map(|c| (c, (c as f64 / denom) * xmax)) {
        while idx + 1 < series.len() && series[idx + 1].0 <= x {
            idx += 1;
        }
        let v = series[idx].1;
        let r = row_of(v);
        let color = if v > 0.0 {
            Color::Cyan
        } else if v < 0.0 {
            Color::Red
        } else {
            Color::Gray
        };
        match prev_row {
            // A level change: draw the riser in this column with rounded
            // corners. The top of the screen is the *higher*
            // advantage, so a rising value (r < pr) turns up out of
            // the old level and into the new; a falling value (r >
            // pr) turns down. The old level keeps its incoming `─` (from
            // column c-1) and gets the elbow here; the new level's `─`
            // continues at c+1.
            Some(pr) if pr != r => {
                for row_cells in
                    cells.iter_mut().take(pr.max(r)).skip(pr.min(r) + 1)
                {
                    row_cells[c] = ('│', color);
                }
                let (old_corner, new_corner) = if r < pr {
                    ('╯', '╭') // rising: ─╯ leaves the old level upward, ╭─ joins the new
                } else {
                    ('╮', '╰') // falling: ─╮ leaves downward, ╰─ joins below
                };
                cells[pr][c] = (old_corner, color);
                cells[r][c] = (new_corner, color);
            }
            // Same level (or the first column): a flat horizontal run.
            _ => {
                cells[r][c] = ('─', color);
            }
        }
        prev_row = Some(r);
    }

    // Plot rows, each prefixed by the gutter label + axis tick.
    let mut lines = Vec::with_capacity(rows + 2);
    for (r, row_cells) in cells.iter().enumerate() {
        let label = if r == 0 {
            ymax_lbl.clone()
        } else if r == zero_row {
            "0.0".to_string()
        } else if r == rows - 1 {
            ymin_lbl.clone()
        } else {
            String::new()
        };
        let axis_char = if r == zero_row { '┼' } else { '┤' };
        let mut spans = vec![
            Span::styled(
                format!("{label:>label_w$} "),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                axis_char.to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        let mut i = 0;
        while i < row_cells.len() {
            let col = row_cells[i].1;
            let mut s = String::new();
            while i < row_cells.len() && row_cells[i].1 == col {
                s.push(row_cells[i].0);
                i += 1;
            }
            spans.push(Span::styled(
                s,
                Style::default().fg(col),
            ));
        }
        lines.push(Line::from(spans));
    }

    // X-axis: a baseline tick row, then start/end labels.
    lines.push(Line::from(Span::styled(
        format!(
            "{}└{}",
            " ".repeat(gutter),
            "─".repeat(plot_w)
        ),
        Style::default().fg(Color::DarkGray),
    )));
    let (start_lbl, end_lbl) = match axis {
        AxisMode::Time => ("0:00".to_string(), fmt_mmss(xmax)),
        AxisMode::Event => {
            (
                "#0".to_string(),
                format!("#{}", series.len() - 1),
            )
        }
    };
    let gap = plot_w
        .saturating_sub(start_lbl.len() + end_lbl.len())
        .max(1);
    lines.push(Line::from(Span::styled(
        format!(
            "{}{start_lbl}{}{end_lbl}",
            " ".repeat(gutter + 1),
            " ".repeat(gap)
        ),
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// Min/max across several value slices, or `None` if all empty.
fn combined_range(slices: &[&[f64]]) -> Option<(f64, f64)> {
    let (mut lo, mut hi, mut any) = (f64::INFINITY, f64::NEG_INFINITY, false);
    for s in slices {
        for &v in *s {
            any = true;
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    any.then_some((lo, hi))
}

/// A focusable stat: which built line it lives on, the tooltip to show below
/// the widget when it's focused, and a stable `key` (the label / section title)
/// used to carry focus onto the same field across a voyage page turn.
struct Focusable {
    line: usize,
    tooltip: String,
    key: String,
}

/// One body row kept in raw form until the widget's width is known. The width
/// is derived from these rows ([`Built::natural_width`]); each then renders to
/// a `Line` at that width ([`Built::finalize`]).
enum Row {
    /// An empty spacer line.
    Blank,
    /// A centered yellow section header.
    Section(String),
    /// A `label … value` row: label flush-left, value flush-right.
    Stat {
        label: String,
        value: String,
    },
    /// Three centered, individually-styled columns spanning the width.
    ThreeCol([(String, Style); 3]),
    /// A pre-rendered, width-independent line (its own width is fixed).
    Raw(Line<'static>),
    /// Free text word-wrapped to the resolved width — expands to as many lines
    /// as needed instead of forcing the widget wider. Must be the *last*
    /// row (it breaks the row↔line 1:1 mapping the focusables rely on).
    Wrap {
        text: String,
        style: Style,
    },
}

/// The page body (everything below the pinned header): the rows in focus order
/// — the Sea Battles section first, then every Timing-onward stat number — plus
/// the parallel list of focusables. Rows are width-agnostic until
/// [`Self::finalize`] renders them into `lines` once the dynamic widget width
/// is known.
#[derive(Default)]
struct Built {
    rows: Vec<Row>,
    /// Filled by [`Self::finalize`]; empty until then.
    lines: Vec<Line<'static>>,
    focusable: Vec<Focusable>,
}

impl Built {
    fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    fn line(&mut self, l: Line<'static>) {
        self.push(Row::Raw(l));
    }

    /// Push free text that word-wraps to the resolved width. Must be the last
    /// row.
    fn wrap(&mut self, text: &str, style: Style) {
        self.push(Row::Wrap {
            text: text.to_string(),
            style,
        });
    }

    fn blank(&mut self) {
        self.push(Row::Blank);
    }

    fn section(&mut self, title: &str) {
        self.push(Row::Section(title.to_string()));
    }

    fn three_col(&mut self, cells: [(String, Style); 3]) {
        self.push(Row::ThreeCol(cells));
    }

    /// Push a section header that is itself focusable (its header line carries
    /// the `tooltip`). Used for sections you can "enter", like Sea Battles.
    fn focus_section(&mut self, title: &str, tooltip: &str) {
        self.focusable.push(Focusable {
            line: self.rows.len(),
            tooltip: tooltip.to_string(),
            key: title.to_string(),
        });
        self.push(Row::Section(title.to_string()));
    }

    /// Push a focusable `label .... value` stat row tied to `tooltip`. The
    /// `label` doubles as the focus key (unique across the body).
    fn stat(&mut self, label: &str, value: String, tooltip: &str) {
        self.focusable.push(Focusable {
            line: self.rows.len(),
            tooltip: tooltip.to_string(),
            key: label.to_string(),
        });
        self.push(Row::Stat {
            label: label.to_string(),
            value,
        });
    }

    /// The narrowest content width that shows every row without clipping: the
    /// widest `label + 2 spaces + value` stat and the three-column tally (each
    /// of whose cells must fit a third of the width), plus any pre-rendered
    /// line. Section headers just center, so they only need their own text
    /// width.
    fn natural_width(&self) -> usize {
        self.rows
            .iter()
            .map(|r| {
                match r {
                    Row::Blank => 0,
                    Row::Section(t) => t.chars().count(),
                    Row::Stat {
                        label,
                        value,
                    } => label.chars().count() + 2 + value.chars().count(),
                    Row::ThreeCol(cells) => {
                        cells
                            .iter()
                            .map(|(s, _)| s.chars().count())
                            .max()
                            .unwrap_or(0)
                            * 3
                    }
                    Row::Raw(l) => l.width(),
                    // Wrapped text never forces the widget wider than the
                    // sentence — it just needs room for its
                    // longest single word (which can't wrap).
                    Row::Wrap {
                        text,
                        ..
                    } => {
                        text.split_whitespace()
                            .map(|w| w.chars().count())
                            .max()
                            .unwrap_or(0)
                    }
                }
            })
            .max()
            .unwrap_or(0)
    }

    /// Render every row into `self.lines` at the resolved `width`. A
    /// [`Row::Wrap`] expands to several lines, so this isn't a 1:1 row→line
    /// map — safe only because wraps are always last (see [`Row::Wrap`]).
    fn finalize(&mut self, width: usize) {
        let mut lines = Vec::with_capacity(self.rows.len());
        for r in &self.rows {
            match r {
                Row::Blank => lines.push(Line::from("")),
                Row::Section(t) => lines.push(section(t, width)),
                Row::Stat {
                    label,
                    value,
                } => lines.push(stat(label, value.clone(), width)),
                Row::ThreeCol(cells) => {
                    lines.push(three_col(width, cells.clone()))
                }
                Row::Raw(l) => lines.push(l.clone()),
                Row::Wrap {
                    text,
                    style,
                } => {
                    for piece in crate::utils::wrap_words(text, width) {
                        lines.push(Line::from(Span::styled(piece, *style)));
                    }
                }
            }
        }
        self.lines = lines;
    }
}

fn build_lines(view: &VoyageView) -> Built {
    let b = &view.battle;
    let c = &view.consumption;
    let mut out = Built::default();

    // Sea Battles — a focusable section over a full-width three-column table
    // (labels over counts). The header (ship name/type/period) is pinned above
    // the scroll, so the body starts here.
    out.focus_section(
        "Sea Battles",
        "Win / loss / disengage tally. Enter to open the per-fight log.",
    );
    let head = Style::default().fg(Color::DarkGray);
    out.three_col([
        ("Wins".to_string(), head),
        ("Losses".to_string(), head),
        ("Disengages".to_string(), head),
    ]);
    out.three_col([
        (
            b.wins.to_string(),
            Style::default().fg(Color::Green).bold(),
        ),
        (
            b.losses.to_string(),
            Style::default().fg(Color::Red).bold(),
        ),
        (
            b.disengages.to_string(),
            Style::default().fg(Color::Yellow).bold(),
        ),
    ]);
    out.blank();

    // Timing — every number from here on is focusable with a tooltip.
    out.section("Timing");
    out.stat(
        "At Sea",
        opt_dur(view.elapsed_secs.map(|s| s as f64)),
        "Total time from setting sail to putting into port.",
    );
    out.stat(
        "Time Sailing",
        b.time_at_sea_secs.map(dur).unwrap_or_else(dash),
        "Time searching and sailing, not fighting — total time minus time in \
         battle.",
    );
    out.stat(
        "Time in Battle",
        dur(b.time_in_battle_secs),
        "Total fighting time this voyage — every sea engagement's length \
         summed.",
    );
    out.stat(
        "Avg. Sea Battle",
        with_sd(
            opt_dur(b.avg_battle_secs),
            b.avg_battle_sd,
            sd_secs,
        ),
        "Average length of a sea engagement, interception to resolution.",
    );
    out.stat(
        "  Battle Navigation",
        with_sd(
            b.avg_naval_secs.map(turns).unwrap_or_else(dash),
            b.avg_naval_sd,
            sd_turns,
        ),
        "Average turns spent navigating to grapple (35s per turn) — the naval \
         phase.",
    );
    out.stat(
        "  Swordfight/Rumble",
        with_sd(
            opt_dur(b.avg_boarding_secs),
            b.avg_boarding_sd,
            sd_secs,
        ),
        "Average time from grapple to Game Over — the boarding melee.",
    );
    out.blank();

    // Loot.
    out.section("Loot");
    out.stat(
        "PoE per win",
        with_sd(
            opt_commas(b.poe_per_fight_won),
            b.poe_per_fight_won_sd,
            sd_commas,
        ),
        "Average pieces of eight plundered per won fight.",
    );
    out.stat(
        "PoE per engagement",
        with_sd(
            opt_commas(b.poe_per_fight_net),
            b.poe_per_fight_net_sd,
            sd_commas,
        ),
        "Average net PoE per decisive fight, counting losses as negative.",
    );
    out.stat(
        "Net PoE gained",
        commas(b.poe_net_total),
        "Net pieces of eight across every fight (losses subtracted).",
    );
    out.stat(
        "PoE per crew",
        opt_commas(b.poe_per_crew),
        "Net PoE divided by the time-weighted average crew aboard.",
    );
    out.stat(
        "PoE per crew per win",
        opt_commas(b.poe_per_crew_per_fight_won),
        "Won PoE per crew member per won fight.",
    );
    out.stat(
        "PoE per crew per engagement",
        opt_commas(b.poe_per_crew_per_fight_all),
        "Net PoE per crew member per decisive fight, wins and losses together.",
    );
    out.stat(
        "Goods per win",
        with_sd(
            opt1(b.goods_per_fight),
            b.goods_per_fight_sd,
            sd_one,
        ),
        "Average units of goods won per won fight (a count — the log never \
         itemizes).",
    );
    out.stat(
        "Goods per engagement",
        with_sd(
            opt1(b.goods_per_engagement),
            b.goods_per_engagement_sd,
            sd_one,
        ),
        "Average net goods per decisive fight — goods lost in defeats \
         subtract.",
    );
    out.blank();

    // Divvy — only for a run that reached a booty division. The PoE earned
    // (gross and net across fights, plus what stayed in the booty chest)
    // and the goods pillaged, itemized from the Profits Booty column.
    if view.divvied {
        out.section("Divvy");
        out.stat(
            "Gross PoE won",
            commas(b.poe_won_total),
            "Total pieces of eight plundered across won fights (before \
             losses).",
        );
        out.stat(
            "Net PoE earned",
            commas(b.poe_net_total),
            "Net pieces of eight across every fight (losses subtracted).",
        );
        out.stat(
            "In Booty Chest",
            view.booty_chest
                .map(|c| commas(c as i64))
                .unwrap_or_else(dash),
            "PoE left in the booty chest for the divvy — your entered figure, \
             else auto-deduced.",
        );
        if !view.booty_goods.is_empty() {
            out.line(Line::from(Span::styled(
                "Goods pillaged".to_string(),
                Style::default().fg(Color::Gray),
            )));
            for (name, qty) in &view.booty_goods {
                out.stat(
                    &format!("  {name}"),
                    commas(*qty as i64),
                    "Units of this good won this voyage (from the Profits \
                     Booty column).",
                );
            }
        }
        out.blank();
    }

    // Enemies by category — only categories that occurred (no zero rows). The
    // value is `total (wins+losses+disengages)`. Named brigand kings are pulled
    // out of the flat list and grouped, indented, under a "Brigand King"
    // header.
    if !b.categories.is_empty() {
        out.section("Enemies");
        let mut kings: Vec<(&str, &CategoryTally)> = Vec::new();
        for (label, t) in &b.categories {
            match label.strip_prefix("King: ") {
                Some(name) => kings.push((name, t)),
                None => {
                    out.stat(
                        label,
                        enemy_value(t),
                        &enemy_tip(label, t),
                    )
                }
            }
        }
        if !kings.is_empty() {
            out.line(Line::from(Span::styled(
                "Brigand King".to_string(),
                Style::default().fg(Color::Gray),
            )));
            for (name, t) in kings {
                out.stat(
                    &format!("  {name}"),
                    enemy_value(t),
                    &enemy_tip(&format!("King {name}"), t),
                );
            }
        }
        out.blank();
    }

    // Advantage (only when damage was tracked for at least one fight).
    if b.avg_advantage_dmg.is_some() || b.avg_advantage_crew.is_some() {
        out.section("Advantage (avg)");
        out.stat(
            "Damage advantage",
            with_sd(
                b.avg_advantage_dmg
                    .map(|a| format!("{:+.0}%", a * 100.0))
                    .unwrap_or_else(dash),
                b.avg_advantage_dmg_sd,
                sd_pct,
            ),
            "Advantage is the difference of the working melee space between \
             one member of our crew against the opposing member of their crew.",
        );
        out.stat(
            "Crew advantage",
            with_sd(
                b.avg_advantage_crew
                    .map(|a| format!("{a:+.1}"))
                    .unwrap_or_else(dash),
                b.avg_advantage_crew_sd,
                sd_one,
            ),
            "Manpower advantage is the total sum of working melee space for \
             our crew, against the total working melee space of the opposing \
             crew.",
        );
        out.blank();
    }

    // Consumption.
    out.section("Consumption");
    out.stat(
        "Cannon Balls",
        commas(c.balls as i64),
        "Cannon balls fired this voyage (Restock minus Stock, summed across \
         all sizes — a ship burns only its own).",
    );
    out.stat(
        "  per battle",
        opt1(c.balls_per_battle),
        "Average cannonballs fired per sea battle.",
    );
    out.stat(
        "Rum",
        commas(c.rum.weighted() as i64),
        "Rum consumed, weighted by potency (swill 2 / grog 3 / fine rum 6).",
    );
    out.stat(
        "  swill",
        commas(c.rum.swill as i64),
        "Swill drained this voyage (Restock minus Stock).",
    );
    out.stat(
        "  grog",
        commas(c.rum.grog as i64),
        "Grog drained this voyage (Restock minus Stock).",
    );
    out.stat(
        "  fine rum",
        commas(c.rum.fine_rum as i64),
        "Fine rum drained this voyage (Restock minus Stock).",
    );
    out.stat(
        "  per crew",
        opt1(c.rum_per_crew),
        "Rum per crew member aboard.",
    );
    out.stat(
        "  per crew / min",
        opt2(c.rum_per_crew_per_min),
        "Rum per crew member per minute of the run.",
    );
    out.stat(
        "Rum spice",
        commas(c.rum_spice as i64),
        "Rum spice consumed this voyage.",
    );
    out.stat(
        "  per merc",
        opt1(c.rum_spice_per_mercenary),
        "Rum spice per mercenary — spice fuels mercenaries, not swabbies.",
    );
    out.stat(
        "  per merc / min",
        opt2(c.rum_spice_per_mercenary_per_min),
        "Rum spice per mercenary per minute (approximate — see the note \
         below).",
    );
    let warn = if c.rum_spice_unreliable {
        "Due to losing battles, these statistics are inaccurate measures of \
         consumption."
    } else {
        "These statistics are not accurate measures of consumption."
    };
    out.wrap(
        warn,
        Style::default().fg(Color::DarkGray).italic(),
    );

    out
}

/// A single line of `text` centered within `width` cells, styled.
fn centered_line(text: String, width: usize, style: Style) -> Line<'static> {
    let w = width.max(1);
    Line::from(Span::styled(
        format!("{text:^w$}"),
        style,
    ))
}

/// Center a run of individually-styled spans as one unit, padding both sides so
/// the combined text sits centered while each span keeps its own style.
fn centered_spans(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let w = width.max(1);
    let text_w: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = w.saturating_sub(text_w);
    let (left, right) = (pad / 2, pad - pad / 2);
    let mut out = Vec::with_capacity(spans.len() + 2);
    if left > 0 {
        out.push(Span::raw(" ".repeat(left)));
    }
    out.extend(spans);
    if right > 0 {
        out.push(Span::raw(" ".repeat(right)));
    }
    Line::from(out)
}

/// A full-width row of three centered, individually-styled columns.
fn three_col(width: usize, cells: [(String, Style); 3]) -> Line<'static> {
    let col = (width / 3).max(1);
    let spans: Vec<Span<'static>> = cells
        .into_iter()
        .map(|(s, style)| Span::styled(format!("{s:^col$}"), style))
        .collect();
    Line::from(spans)
}

/// A centered yellow section header line.
fn section(title: &str, width: usize) -> Line<'static> {
    let w = width.max(1);
    Line::from(Span::styled(
        format!("{title:^w$}"),
        Style::default().fg(Color::Yellow).bold(),
    ))
}

/// A stat row spanning the whole `width`: label flush-left, value flush-right.
/// When the two can't both fit (a narrow widget in a wider terminal), they fall
/// back to a plain two-space separator (`label  value`).
fn stat(label: &str, value: String, width: usize) -> Line<'static> {
    let lw = label.chars().count();
    let vw = value.chars().count();
    let text = if width >= lw + 2 + vw {
        format!(
            "{label}{}{value}",
            " ".repeat(width - lw - vw)
        )
    } else {
        format!("{label}  {value}")
    };
    Line::from(text)
}

// -- value formatters --

/// The em-dash placeholder for a missing value.
fn dash() -> String {
    "—".to_string()
}

/// Append a `", σ = …"` suffix to `value` when a standard deviation is present
/// (the stats layer leaves it `None` for fewer than three data points, so this
/// naturally shows nothing then). `fmt` renders the σ in the stat's own units.
fn with_sd(value: String, sd: Option<f64>, fmt: fn(f64) -> String) -> String {
    match sd {
        Some(s) => format!("{value}, σ = {}", fmt(s)),
        None => value,
    }
}

/// A duration in seconds → `"4.2 turns"` (35 seconds per battle-navigation
/// turn).
fn turns(secs: f64) -> String {
    format!("{:.1} turns", secs / 35.0)
}

/// An enemy row's value: `total (wins+losses+disengages)`.
fn enemy_value(t: &CategoryTally) -> String {
    let total = t.wins + t.losses + t.disengages;
    format!(
        "{total} ({}+{}+{})",
        t.wins, t.losses, t.disengages
    )
}

/// Tooltip for an enemy row, spelling out the W/L/D breakdown.
fn enemy_tip(label: &str, t: &CategoryTally) -> String {
    format!(
        "Vs {label}: {} won, {} lost, {} disengaged.",
        t.wins, t.losses, t.disengages
    )
}

/// σ formatters, one per stat unit. For time, σ is shown plainly in seconds.
fn sd_secs(s: f64) -> String {
    format!("{}s", s.round() as i64)
}
fn sd_turns(s: f64) -> String {
    format!("{:.1}", s / 35.0)
}
fn sd_commas(s: f64) -> String {
    commas(s.round() as i64)
}
fn sd_one(s: f64) -> String {
    format!("{s:.1}")
}
fn sd_pct(s: f64) -> String {
    format!("{:.0}%", s * 100.0)
}

/// `12,345`-style thousands grouping for a signed integer.
fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (i, ch) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch as char);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// `1h 04m` / `3m 45s` / `45s` from a second count.
fn dur(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {sec:02}s")
    } else {
        format!("{sec}s")
    }
}

fn opt_dur(x: Option<f64>) -> String {
    x.map(|v| dur(v.round() as i64))
        .unwrap_or_else(|| "—".to_string())
}

fn opt_commas(x: Option<f64>) -> String {
    x.map(|v| commas(v.round() as i64))
        .unwrap_or_else(|| "—".to_string())
}

fn opt1(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "—".to_string())
}

fn opt2(x: Option<f64>) -> String {
    x.map(|v| format!("{v:.2}"))
        .unwrap_or_else(|| "—".to_string())
}
