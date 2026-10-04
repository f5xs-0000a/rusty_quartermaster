// ---------------------------------------------------------------------------
// Field types & validation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum FieldKind {
    PositiveInt,
    Rate,
    Text,
}

pub struct PromptField {
    pub label: &'static str,
    pub kind: FieldKind,
    pub value: String,
    pub cursor: usize,
}

impl PromptField {
    pub fn new(label: &'static str, kind: FieldKind) -> Self {
        Self {
            label,
            kind,
            value: String::new(),
            cursor: 0,
        }
    }

    pub fn accepts(&self, c: char) -> bool {
        match self.kind {
            FieldKind::PositiveInt => c.is_ascii_digit(),
            FieldKind::Rate => {
                if c.is_ascii_digit() {
                    true
                } else if c == '.' {
                    !self.value.contains('.')
                } else if c == '%' {
                    !self.value.contains('%') && self.cursor == self.value.len()
                } else {
                    false
                }
            }
            FieldKind::Text => !c.is_control(),
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.accepts(c) {
            self.value.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    pub fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[.. self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.value.remove(prev);
            self.cursor = prev;
        }
    }

    pub fn delete_char_at(&mut self) {
        if self.cursor < self.value.len() {
            self.value.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.value[.. self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor += self.value[self.cursor ..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }
}

// ---------------------------------------------------------------------------
// General-purpose utilities
// ---------------------------------------------------------------------------

pub fn text_similarity(a: &str, b: &str) -> f64 {
    strsim::jaro_winkler(a, b)
}

/// Sink for best-effort diagnostic lines (save results, load warnings). While
/// the TUI owns the terminal it draws to stdout's alternate screen, but stderr
/// still points at the same terminal — so an `eprintln!` mid-run paints raw
/// bytes over the frame and garbles the render until the next full redraw. Once
/// [`init_diag_log`] points this at a file, [`diag`] appends there instead;
/// before the TUI starts (or if no log file could be opened) it falls back to
/// stderr, so startup progress output is unaffected.
static DIAG_LOG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> =
    std::sync::OnceLock::new();

/// Redirect [`diag`] output to `path` (append, created if absent) for the rest
/// of the process. Call once, just before entering the alternate screen. On
/// failure the sink stays on stderr rather than aborting — diagnostics are
/// best-effort.
pub fn init_diag_log(path: &std::path::Path) {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = DIAG_LOG.set(std::sync::Mutex::new(file));
    }
}

/// Emit a diagnostic line to the log file if one is configured (TUI is up),
/// else to stderr. Prefer the [`diag!`] macro for `eprintln!`-style formatting.
pub fn diag(msg: &str) {
    if let Some(lock) = DIAG_LOG.get()
        && let Ok(mut file) = lock.lock()
    {
        use std::io::Write;
        let _ = writeln!(file, "{msg}");
        return;
    }
    eprintln!("{msg}");
}

/// `eprintln!`-style wrapper over [`diag`]: formats its arguments and routes
/// the line through the TUI-safe sink instead of straight to stderr.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => { $crate::utils::diag(&format!($($arg)*)) };
}

/// Atomically write `value` to `path` as compact JSON.
///
/// The single write path for every JSON file we persist. Serialization is
/// streamed (no intermediate `String`) into a sibling temp file, which is then
/// `rename`d over `path`. A crash or serialization error mid-write leaves the
/// original file untouched rather than truncated, and `rename` on the same
/// filesystem is atomic, so a reader never sees a half-written file.
///
/// `label` names the payload for the log lines (e.g. `"cache"`,
/// `"persisted data"`): a `Saved {label} to {path}` on success, or a
/// `failed to … {label}` on error. Messages go through [`diag`] (the TUI-safe
/// sink) rather than straight to stderr — this runs mid-render from the
/// save/discard prompt, and a raw `eprintln!` would garble the alternate
/// screen. Errors are swallowed beyond that line (save is best-effort).
pub fn write_json_atomic<T: serde::Serialize>(
    path: &std::path::Path,
    value: &T,
    label: &str,
) {
    use std::io::Write;

    // Temp file alongside the target so the final rename stays on one
    // filesystem (a cross-device rename would fail). Tie the name to the
    // target's so concurrent saves of *different* files don't collide.
    let file_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let mut tmp_name = file_name;
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);

    let file = match std::fs::File::create(&tmp) {
        Ok(file) => file,
        Err(e) => {
            crate::diag!(
                "error: failed to open {} for writing: {e}",
                tmp.display()
            );
            return;
        }
    };
    let mut writer = std::io::BufWriter::new(file);
    if let Err(e) = serde_json::to_writer(&mut writer, value) {
        crate::diag!("error: failed to serialize {label}: {e}");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // Flush the BufWriter before the rename, or buffered bytes could be lost.
    if let Err(e) = writer.flush() {
        crate::diag!(
            "error: failed to flush {}: {e}",
            tmp.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    drop(writer);
    if let Err(e) = std::fs::rename(&tmp, path) {
        crate::diag!(
            "error: failed to write {label} to {} (rename from temp failed: \
             {e})",
            path.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    crate::diag!("Saved {label} to {}", path.display());
}

pub fn parse_rate(field: &PromptField) -> f64 {
    let s = field.value.trim();
    if s.is_empty() {
        return 0.0;
    }
    if let Some(stripped) = s.strip_suffix('%') {
        stripped.parse::<f64>().unwrap_or(0.0) / 100.0
    } else {
        s.parse::<f64>().unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Widget titles
// ---------------------------------------------------------------------------

/// Length of the `─` run that leads an offset title. The block's border
/// supplies the matching trailing run.
const TITLE_DASHES: usize = 3;

/// Build a block title in the app's house style (`─── Title `) together with
/// the minimum widget width that keeps it readable. The width comes from
/// [`offset_title_width`] so the two never drift.
///
/// The title is left-aligned on the block; at the returned width it reads
/// `┌─── Title ───┐`, and any extra width simply lengthens the trailing run.
pub fn offset_title(title: &'static str) -> (String, u16) {
    // The `─── …` frame, kept as a tiny macro local to this fn since nothing
    // else needs it. (`concat!` can't be used — `title` isn't a literal.)
    macro_rules! framed {
        ($t:expr) => {
            format!("{} {} ", "─".repeat(TITLE_DASHES), $t)
        };
    }
    (
        framed!(title),
        offset_title_width(title),
    )
}

/// Minimum widget width at which an [`offset_title`] for `title` sits centered
/// — equal `───` runs flank the text. `const` so widgets can derive a `const`
/// minimum width and use it directly as a layout floor. Assumes an ASCII title
/// (byte length == column count), which all of ours are.
pub const fn offset_title_width(title: &'static str) -> u16 {
    // `─── {title} ` spans `title.len() + TITLE_DASHES + 2` columns; the box
    // adds a matching trailing dash run + 2 corners.
    (title.len() + 2 * TITLE_DASHES + 4) as u16
}

/// Columns a boxed widget spends on its frame: a border and a blank column on
/// each side, the blanks holding the contents off the border.
pub const BOX_MARGIN: u16 = 2 * (1 + PADDING);

/// Blank columns between a widget's border and its contents, on each side. A
/// widget with no border keeps the same blank columns at the edges of the room
/// it was given.
pub const PADDING: u16 = 1;

/// A bordered, padded block titled in the house style, together with the width
/// the widget must not go below.
///
/// `content_width` is what the contents alone occupy, counting neither border
/// nor padding. The width answers both of the things that can force a widget
/// wider: the contents inside their margins, and the title needing a trailing
/// rule.
///
/// Taking both from one call is the point: [`offset_title`] hands back the
/// same floor, but a caller that wants only the title tends to drop it.
pub fn titled_block(
    title: &'static str,
    content_width: u16,
) -> (ratatui::widgets::Block<'static>, u16) {
    use ratatui::widgets::{Block, Borders, Padding};

    (
        Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(PADDING))
            .title(offset_title(title).0),
        (content_width + BOX_MARGIN).max(offset_title_width(title)),
    )
}

/// As [`titled_block`], for a popup the user picks a row out of: the labels are
/// centered inside the box *as a block*, without being centered themselves.
///
/// A list is read down its left edge, so the words stay flush with one another
/// and the whole column moves instead. What it is centered against is the slack
/// the box has beyond the labels, which is usually the title's doing — a box
/// held open by `Voyage Type` is wider than `Cursed Isles` needs.
pub fn choice_block(
    title: &'static str,
    label_width: u16,
) -> (ratatui::widgets::Block<'static>, u16) {
    use ratatui::widgets::Padding;

    let (block, width) = titled_block(title, label_width);
    let slack = width.saturating_sub(BOX_MARGIN + label_width);
    (
        block.padding(Padding::new(
            PADDING + slack / 2,
            PADDING,
            0,
            0,
        )),
        width,
    )
}

/// Draw the notice that stands in for something unusable until a prerequisite
/// is met — a missing argument, a window too small — centered on both axes of
/// `area` and word-wrapped to its width. Each entry is wrapped on its own, so a
/// heading and the detail beneath it can carry different styles.
///
/// `area` is the room the text may fill, already holding the blank column per
/// side that the notice keeps at its edges: a boxed widget has it from
/// [`titled_block`]'s padding and passes `block.inner(area)`, while a notice
/// standing in for a whole page has no border to take it from and insets by
/// [`PADDING`] itself.
///
/// Folding the text here rather than leaving it to `Wrap` is what allows the
/// vertical centering: the height follows from the text once it is folded to
/// the width, and is not known before.
pub fn render_notice(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    paragraphs: &[(&str, ratatui::style::Style)],
) {
    use ratatui::{
        text::{Line, Span},
        widgets::Paragraph,
    };

    let lines = paragraphs
        .iter()
        .flat_map(|(text, style)| {
            wrap_words(text, area.width as usize)
                .into_iter()
                .map(|line| Line::from(Span::styled(line, *style)))
        })
        .collect::<Vec<_>>();

    let height = (lines.len() as u16).min(area.height);
    let rect = ratatui::layout::Rect {
        y: area.y + area.height.saturating_sub(height) / 2,
        height,
        ..area
    };
    frame.render_widget(Paragraph::new(lines).centered(), rect);
}

/// Draw a [`render_notice`] that stands in for a whole page rather than for one
/// widget. With no border to take them from, the blank columns it keeps at its
/// edges come from [`PADDING`].
pub fn render_page_notice(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    paragraphs: &[(&str, ratatui::style::Style)],
) {
    render_notice(
        frame,
        ratatui::layout::Rect {
            x: area.x + PADDING.min(area.width),
            width: area.width.saturating_sub(2 * PADDING),
            ..area
        },
        paragraphs,
    );
}

/// Columns a button spends on the brackets that mark it as one, `"[ "` and
/// `" ]"`.
const BUTTON_BRACKETS: u16 = 4;

/// Blank columns kept between two buttons, and at each end of their row.
const BUTTON_GAP: u16 = 2;

/// Columns a row of the given buttons needs: each of them as wide as the widest
/// label, with a [`BUTTON_GAP`] between them and at both ends. A popup takes
/// its width from this so its buttons are never the thing that gets squeezed.
pub fn buttons_width(labels: &[&str]) -> u16 {
    let n = labels.len() as u16;
    if n == 0 {
        return 0;
    }
    let label_w =
        labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    n * (label_w + BUTTON_BRACKETS) + (n + 1) * BUTTON_GAP
}

/// Draw a row of buttons and hand back each one's rect, in order, for the
/// caller to hang a click region on. `focused` is the one the keyboard is
/// resting on.
///
/// Every label is padded to the widest of them, so the buttons in a row are all
/// one width however long their words are; the gaps between them and at both
/// ends of the row are equal, so the row reads as one group rather than as
/// text that happens to be spaced out. A focused button is drawn in reverse,
/// the same mark of "this is where you are" that the top bar and the table
/// cursor use.
pub fn render_buttons(
    frame: &mut ratatui::Frame,
    row: ratatui::layout::Rect,
    labels: &[&str],
    focused: Option<usize>,
) -> Vec<ratatui::layout::Rect> {
    use ratatui::{
        layout::Rect,
        style::{Color, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };

    if labels.is_empty() || row.width == 0 {
        return Vec::new();
    }

    let label_w =
        labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let n = labels.len() as u16;
    // Too little room for the brackets and the words is a layout fault
    // elsewhere; share out what there is rather than overflow the row.
    let button_w = (label_w + BUTTON_BRACKETS).min(row.width / n);
    // One gap more than there are buttons: between each pair, and at each end.
    let free = row.width - button_w * n;
    let gap = free / (n + 1);
    let mut x = row.x + gap + (free - gap * (n + 1)) / 2;

    let mut rects = Vec::with_capacity(labels.len());
    for (i, label) in labels.iter().enumerate() {
        let rect = Rect::new(x, row.y, button_w, 1);
        let style = if focused == Some(i) {
            Style::default().bg(Color::White).fg(Color::Black).bold()
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    "[ {label:^width$} ]",
                    width = label_w as usize
                ),
                style,
            )))
            .centered(),
            rect,
        );
        rects.push(rect);
        x += button_w + gap;
    }
    rects
}

/// Draw the lone Close button a popup carries instead of telling the user that
/// Esc shuts it. Esc still works; what the rule objects to is spending a line
/// saying so, when a button says it and can be clicked besides.
///
/// Returns the button's rect for the caller to register the click on, since
/// only it knows what closing this popup means.
pub fn render_close_button(
    frame: &mut ratatui::Frame,
    row: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    render_buttons(frame, row, &["Close"], None)
        .first()
        .copied()
        .unwrap_or(row)
}

/// Rows a vertically scrollable list keeps before it stops reading as one.
/// Below this there is too little of the list on show to tell that it continues
/// past the window, and a long list looks like a short one — which misreports
/// what is there rather than merely cramping it. The rows are the list's own: a
/// pinned header and the borders are on top of these.
pub const SCROLL_MIN_ROWS: u16 = 4;

/// Columns a view the user scrolls keeps along its right edge for the
/// scrollbar: the single column the bar is drawn in, and the blank one that
/// holds the contents off it — the same blank column [`PADDING`] keeps between
/// contents and a border. They are only spent while there is something to
/// scroll; see [`render_scrollbar`].
pub const SCROLLBAR_W: u16 = 1 + PADDING;

/// Whether a view with `total` rows to show and `rows` of room must scroll, and
/// so spends its [`SCROLLBAR_W`] columns on the bar. [`render_scrollbar`] asks
/// this itself; a caller needs it only when the answer decides a layout it must
/// settle before drawing.
pub fn scrolls(rows: u16, total: usize) -> bool {
    (rows as usize) < total
}

/// Draw the scrollbar of a view the user scrolls and hand back the part of
/// `area` its contents may use. `total` is the rows the view holds in all;
/// `area.height` is how many of them are on show, and `offset` is the first.
///
/// With all of them on show there is nothing to scroll, so no bar is drawn and
/// the whole of `area` comes back: the [`SCROLLBAR_W`] columns are the
/// contents' to use until the moment the bar wants them, and a list that fits
/// looks like any other widget.
///
/// The bar spans the view. Each end carries an arrow while the view can still
/// travel that way and flattens to a cap once it cannot, so the two ends say
/// where in the list the window sits without the thumb having to be read. The
/// thumb is the same part of the track that the rows on show are of the whole,
/// which is what makes a window over thirty rows look unlike one over six.
///
/// The bar is a click target besides, named by `view` so the click can be
/// routed back here; it is registered last, over whatever region the view
/// itself claimed, so the column answers to the bar rather than to the list
/// behind it.
pub fn render_scrollbar(
    frame: &mut ratatui::Frame,
    regions: &mut Vec<crate::clickmap::ClickRegion>,
    area: ratatui::layout::Rect,
    view: crate::clickmap::ScrollView,
    offset: usize,
    total: usize,
) -> ratatui::layout::Rect {
    use ratatui::{
        layout::Rect,
        widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState},
    };

    use crate::clickmap::{ClickRegion, ClickTarget};

    if !scrolls(area.height, total) {
        return area;
    }
    let rows = area.height as usize;
    let bar = Rect {
        x: area.x + area.width.saturating_sub(1),
        width: 1,
        ..area
    };
    regions.push(ClickRegion {
        rect: bar,
        target: ClickTarget::Scrollbar {
            view,
            bar,
            total,
        },
    });
    let max_offset = total - rows;
    let offset = offset.min(max_offset);
    // The state is given the offsets the window can rest at rather than the
    // rows it holds, with the window's own length beside them. That is what
    // makes the thumb `view / total` of the track and lands it against the
    // track's far end at the foot of the list, instead of stopping a thumb's
    // length short of it.
    let mut state = ScrollbarState::new(max_offset + 1)
        .position(offset)
        .viewport_content_length(rows);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some(if 0 < offset { "▲" } else { "┬" }))
            .end_symbol(Some(
                if offset < max_offset { "▼" } else { "┴" },
            ))
            .track_symbol(Some("│"))
            .thumb_symbol("█"),
        area,
        &mut state,
    );
    Rect {
        width: area.width.saturating_sub(SCROLLBAR_W),
        ..area
    }
}

/// What a click on a scrollbar asks of the view it belongs to. Which of the
/// two it is depends only on where in the bar the click landed, so a view that
/// keeps its own window and one whose window follows a cursor read the same
/// click and answer it in their own terms (see [`ScrollHit::resolve`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollHit {
    /// An end cell: one back (`-1`) or one on (`1`). A capped end asks all the
    /// same — there is simply nothing that way to move to, and the clamp in
    /// [`ScrollHit::resolve`] is what makes the ask a no-op.
    Step(i32),
    /// A click on the track, as how far down it the pointer was: `cell` of
    /// `track` cells.
    Jump {
        cell: u16,
        track: u16,
    },
}

/// Read a click at `row` on the bar drawn in `bar` ([`render_scrollbar`] hands
/// the rect to the click region it registers).
pub fn scrollbar_hit(bar: ratatui::layout::Rect, row: u16) -> ScrollHit {
    // The bar's first and last cells are its two arrows; the track is what lies
    // between them.
    let cell = row.saturating_sub(bar.y);
    if cell == 0 {
        return ScrollHit::Step(-1);
    }
    let track = bar.height.saturating_sub(2);
    if track <= cell - 1 {
        return ScrollHit::Step(1);
    }
    ScrollHit::Jump {
        cell: cell - 1,
        track,
    }
}

impl ScrollHit {
    /// Where the click puts a view that is at `current` and can go as far as
    /// `last`. What those two count is the view's own business: a view that
    /// keeps its own window passes its scroll offset and its last offset, while
    /// one whose window follows a cursor passes the cursor and the last row —
    /// the bar means "this far down" either way.
    ///
    /// A click lands the pointed-at part of the list under the pointer, near
    /// enough: the thumb is not corrected for its own length, which on the
    /// short tracks a four-row view gives would be noise.
    pub fn resolve(self, current: usize, last: usize) -> usize {
        match self {
            Self::Step(by) => {
                if by < 0 {
                    current.saturating_sub(1)
                } else {
                    (current + 1).min(last)
                }
            }
            Self::Jump {
                cell,
                track,
            } => {
                // Both ends of the track are reachable: the top cell is the
                // start of the list and the bottom cell its end.
                let steps = track.saturating_sub(1) as usize;
                if steps == 0 {
                    return current.min(last);
                }
                (cell as usize * last + steps / 2) / steps
            }
        }
    }
}

/// Refuse to draw a page in less width than `needed`, drawing the notice saying
/// so in its place. Returns whether it did, so a page that cannot narrow any
/// further returns on `true`.
///
/// How much width is needed is the page's own business and moves with what it
/// has to show, so each page weighs its own content here rather than being
/// held to one figure for the whole app. A page that fits the window is drawn
/// even when another page would not have fit, and the top bar stays above the
/// notice so the pages that do fit are still reachable.
pub fn too_narrow(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    needed: u16,
) -> bool {
    if needed <= area.width {
        return false;
    }
    refuse(
        frame,
        area,
        &format!(
            "Enlarge the window to at least {needed} columns (it is {}).",
            area.width,
        ),
    );
    true
}

/// Refuse to draw a page in less height than `needed`, drawing the notice
/// saying so in its place. Returns whether it did, so a page that cannot
/// shorten any further returns on `true`.
///
/// A page needs the height its widgets' contents need, and
/// [`SCROLL_MIN_ROWS`] for each view the user scrolls through. What `needed`
/// must not do is move as focus moves: a page counts the rows a tooltip would
/// take whether or not one is showing, so that resting on a field cannot make
/// the page it belongs to disappear.
pub fn too_short(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    needed: u16,
) -> bool {
    if needed <= area.height {
        return false;
    }
    refuse(
        frame,
        area,
        &format!(
            "Enlarge the window to at least {needed} rows (it is {}).",
            area.height,
        ),
    );
    true
}

/// The notice a page draws in its own place when the window cannot hold it.
/// Whether the width or the height is lacking, the heading is the one the app
/// uses when the window is too small for anything at all — to the user it is
/// one condition, and which way it is short is the detail line's business.
fn refuse(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    detail: &str,
) {
    use ratatui::style::{Color, Style};

    render_page_notice(
        frame,
        area,
        &[
            (
                "Terminal too small",
                Style::default().bold(),
            ),
            (
                detail,
                Style::default().fg(Color::DarkGray),
            ),
        ],
    );
}

/// Word-wrap `text` to `width` columns, hard-breaking any single word longer
/// than the line so a narrow column never overflows. Returns one `String` per
/// line.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for mut word in text.split_whitespace() {
        // A word that can't fit on its own line is chopped to width.
        while word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(width).collect();
            let consumed = head.len();
            lines.push(head);
            word = &word[consumed ..];
        }
        if word.is_empty() {
            continue;
        }
        let need = if cur.is_empty() {
            word.chars().count()
        } else {
            cur.chars().count() + 1 + word.chars().count()
        };
        if need > width {
            lines.push(std::mem::take(&mut cur));
        } else if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // The width is const-evaluable, so widgets can build `const` floors from
    // it.
    const _: () = assert!(offset_title_width("Ocean") == 15);

    #[test]
    fn wrap_words_breaks_on_spaces() {
        assert_eq!(
            wrap_words("Understaffed. Hire jobbers.", 20),
            vec!["Understaffed. Hire", "jobbers."],
        );
    }

    #[test]
    fn wrap_words_hard_breaks_overlong_words() {
        assert_eq!(
            wrap_words("Understaffed.", 5),
            vec!["Under", "staff", "ed."]
        );
    }

    // A notice sits in the middle of the room it was given on both axes, folded
    // to that room's width.
    #[test]
    fn notice_is_centered_on_both_axes() {
        use ratatui::{
            Terminal,
            backend::TestBackend,
            layout::Rect,
            style::Style,
        };

        let mut terminal =
            Terminal::new(TestBackend::new(14, 5)).expect("terminal");
        terminal
            .draw(|frame| {
                // One blank column each side of a 14-column screen.
                render_notice(
                    frame,
                    Rect::new(PADDING, 0, 14 - 2 * PADDING, 5),
                    &[("one two three four", Style::default())],
                );
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let rows = (0 .. buffer.area.height)
            .map(|y| {
                (0 .. buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            rows,
            [
                "              ",
                // An odd remainder goes to the left of the line.
                "    one two   ",
                "  three four  ",
                "              ",
                "              ",
            ],
        );
    }

    /// Render the scrollbar of a `width`-wide, 6-row view over `total` rows and
    /// return its column, top to bottom, with the width the contents were left.
    #[cfg(test)]
    fn scrollbar_column(
        width: u16,
        offset: usize,
        total: usize,
    ) -> (Vec<String>, u16) {
        use ratatui::{Terminal, backend::TestBackend, layout::Rect};

        use crate::clickmap::ScrollView;

        let mut content_w = width;
        let mut regions = Vec::new();
        let mut terminal =
            Terminal::new(TestBackend::new(width, 6)).expect("terminal");
        terminal
            .draw(|frame| {
                content_w = render_scrollbar(
                    frame,
                    &mut regions,
                    Rect::new(0, 0, width, 6),
                    ScrollView::JobberTrophies,
                    offset,
                    total,
                )
                .width;
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (
            (0 .. 6)
                .map(|y| buffer[(width - 1, y)].symbol().to_string())
                .collect(),
            content_w,
        )
    }

    // Nothing to scroll is nothing to draw, and the column stays the
    // contents'.
    #[test]
    fn a_view_that_fits_keeps_its_scrollbar_column() {
        assert_eq!(
            scrollbar_column(3, 0, 6),
            (
                vec![" "; 6].into_iter().map(String::from).collect(),
                3
            ),
        );
    }

    // Half the rows on show, so the thumb is half the track; the end the view
    // can still travel to carries the arrow and the other end is capped.
    #[test]
    fn a_scrollbar_points_the_way_the_view_can_travel() {
        let thumb_at_top = ["┬", "█", "█", "│", "│", "▼"];
        assert_eq!(
            scrollbar_column(4, 0, 12),
            (
                thumb_at_top.iter().map(|s| s.to_string()).collect(),
                // The bar's column and its blank are no longer the contents'.
                4 - SCROLLBAR_W,
            ),
        );

        let thumb_at_foot = ["▲", "│", "│", "█", "█", "┴"];
        assert_eq!(
            scrollbar_column(4, 6, 12).0,
            thumb_at_foot
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        );
    }

    // The bar answers the mouse on its own column, and nowhere else: the blank
    // column beside it belongs to the contents.
    #[test]
    fn a_scrollbar_is_a_click_region_on_its_own_column() {
        use ratatui::{Terminal, backend::TestBackend, layout::Rect};

        use crate::clickmap::{ClickTarget, ScrollView};

        let mut regions = Vec::new();
        let mut terminal =
            Terminal::new(TestBackend::new(10, 6)).expect("terminal");
        terminal
            .draw(|frame| {
                render_scrollbar(
                    frame,
                    &mut regions,
                    Rect::new(2, 0, 8, 6),
                    ScrollView::JobberLeaderboard,
                    0,
                    12,
                );
            })
            .expect("draw");

        let [region] = regions.as_slice() else {
            panic!(
                "one region for the bar, got {}",
                regions.len()
            );
        };
        assert_eq!(region.rect, Rect::new(9, 0, 1, 6));
        assert!(matches!(
            region.target,
            ClickTarget::Scrollbar {
                view: ScrollView::JobberLeaderboard,
                total: 12,
                ..
            },
        ));
    }

    // A click reads as a step at the two ends and as a jump along the track,
    // and the track's own ends reach the ends of the list.
    #[test]
    fn a_scrollbar_click_steps_at_the_ends_and_jumps_between_them() {
        use ratatui::layout::Rect;

        // Six cells: an arrow at each end, four of track between them.
        let bar = Rect::new(9, 4, 1, 6);
        assert_eq!(
            scrollbar_hit(bar, 4),
            ScrollHit::Step(-1)
        );
        assert_eq!(
            scrollbar_hit(bar, 9),
            ScrollHit::Step(1)
        );
        assert_eq!(
            scrollbar_hit(bar, 5),
            ScrollHit::Jump {
                cell: 0,
                track: 4,
            },
        );

        // A step is one row, and stops at either end of the list.
        assert_eq!(ScrollHit::Step(-1).resolve(3, 9), 2);
        assert_eq!(ScrollHit::Step(-1).resolve(0, 9), 0);
        assert_eq!(ScrollHit::Step(1).resolve(9, 9), 9);

        // The track's first and last cells are the list's first and last rows,
        // and a cell between them lands in proportion.
        assert_eq!(scrollbar_hit(bar, 5).resolve(4, 9), 0);
        assert_eq!(scrollbar_hit(bar, 8).resolve(4, 9), 9);
        assert_eq!(scrollbar_hit(bar, 6).resolve(4, 9), 3);
    }

    #[test]
    fn offset_title_is_centered_at_min_width() {
        let (s, w) = offset_title("Ocean");
        assert_eq!(s, "─── Ocean ");
        // ┌─── Ocean ───┐  → 2 corners + 10 title cols + 3 trailing dashes.
        assert_eq!(w, 15);
        assert_eq!(offset_title_width("Ocean"), 15);
    }
}
