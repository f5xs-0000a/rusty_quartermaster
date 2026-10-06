# Interface conventions

The single place that says how this app's interface is built. Every widget
follows these rules, so a new page should look like it belongs without anyone
having to compare it against an existing one by eye.

Each rule states the convention, the helper that implements it (a rule with a
helper is never hand-rolled at the call site), and how to verify it.

To check any rule against the real rendering, dump the interface and read it:

```sh
cargo run --bin gallery -- --out tmp/ui-gallery --prune
```

That writes each state twice. The `.txt` dump is the character grid, which is
what a rule about alignment or spacing is checked against. The `.svg` keeps
the colours and modifiers the text necessarily drops, which is what a rule
about focus or emphasis is checked against; `index.html` shows them all on one
page.

## Rule 1: Widget titles are offset

A widget's title sits **on** its top border, left-aligned, inset from the
corner by a short run of border rule and separated from the rule on both sides
by exactly one space.

Reading left to right along the top border:

1. the top-left corner, `┌`
2. a run of exactly three `─`
3. one space
4. the title text
5. one space
6. the border's own `─` run, continuing to the top-right corner, `┐`

The Inventory widget on the Profits page:

```
┌─── Inventory ──────────────────────────────────────┐
```

The two spaces are the point of the rule: they are what hold the title clear
of the border rule so the text reads as a label rather than as part of the
frame. The leading three-dash run is what keeps the title off the corner.

The title is left-aligned, so widening a widget lengthens only the trailing
run. At its minimum width the two runs are equal and the title reads as
centered:

```
┌─── Inventory ───┐
```

### The title sets a floor on the widget's width

A widget sized to its contents can come out narrower than its own title needs.
The title is not truncated when that happens; the trailing run is eaten
instead, down to nothing, leaving the title jammed against the corner:

```
┌─── Voyage Type ┐
```

So a widget's width is **never** just what its contents need. It is the larger
of the two:

```
width = max(content_width, len(title) + 2 * TITLE_DASHES + 4)
```

which with `TITLE_DASHES = 3` is `len(title) + 10`, accounting for:

| part                      | columns      |
| ------------------------- | ------------ |
| left and right corners    | 2            |
| leading run               | `TITLE_DASHES` |
| trailing run              | `TITLE_DASHES` |
| the spaces flanking it    | 2            |
| the title itself          | `len(title)` |

`Voyage Type` is 11 columns, so its widget may never be narrower than 21. Its
five entries only needed 18, which is how the trailing run vanished.

### Implementation

Take the block and the width from one call, `utils::titled_block`, so sizing a
widget to its contents cannot quietly drop the floor:

```rust
let (block, width) = titled_block("Voyage Type", max_name as u16 + 6);
let list = List::new(items).block(block.padding(Padding::horizontal(1)));
```

The width it hands back already has `max` applied, so it is the width to use.
Both halves matter; a caller that keeps only the block is the bug this rule
exists to prevent.

Never build the title string by hand. The leading run's length lives in one
place, `utils::TITLE_DASHES`.

The two lower-level helpers remain for cases that need them:

- `utils::offset_title` returns the title string and the same floor, for a
  widget that needs an unusual block (different borders, no borders).
- `utils::offset_title_width` is a `const fn`, so a widget can derive a layout
  floor at compile time from the same source the title comes from and the two
  can never drift:

  ```rust
  const MIN_W: u16 = offset_title_width("Inventory");
  ```

Both assume an ASCII title, where byte length equals column count, which all
of ours are.

### Verifying

In any dump, a correct title matches `┌─── <title> ─`, with a trailing run of
at least three. A title touching a corner, missing either space, or with a
leading run of any length other than three is a violation.

Checking it across the whole interface is a scan of the `.txt` dumps for

```
[┌├](─+) (title) (─*)[┐┤]
```

asserting the leading run is exactly three and the trailing run at least
three. At the time of writing the gallery draws 39 distinct titles and all of
them pass.

### The mirror of it: a figure on the bottom border

A figure about the widget as a whole may ride the **bottom** border, the rule
read backwards: right-aligned, one space on each side, and the three-dash run
on its right, between it and the corner.

```
┌─── Map of Emerald Ocean ──────────────┐
│                                       │
└─────────────────────── 2/669 (0.3%) ───┘
```

What belongs there is a figure the widget as a whole answers for — the Map's
memorized tally is the one in the app — not a row of its contents, which
belongs inside. The frame is as much part of the widget as its title is, and a
figure riding it spends no row on itself.

`utils::offset_footer` builds the string; a block takes it as
`.title_bottom(Line::from(offset_footer(&figure)).right_aligned())`. It sets no
floor on the width: a widget narrow enough for the figure to crowd its corner
is one the figure does not belong on.

## Rule 2: Boxed widgets are padded

A boxed widget keeps one blank column between its border and its contents, on
the left and on the right. Contents never touch the frame:

```
┌─── Inventory ──────┐
│ Item  Restock      │
└────────────────────┘
```

not

```
┌─── Inventory ──────┐
│Item  Restock       │
└────────────────────┘
```

This is distinct from the spaces flanking a title in Rule 1. Those sit on the
border row and belong to the title; this padding belongs to the body.

### It raises the floor again

Four columns of every widget are spent before any content is drawn: a border
and a blank on each side. That is `utils::BOX_MARGIN`. So the contents impose
their own floor, and a widget's width is the larger of the two floors:

```
width = max(content_width + BOX_MARGIN, len(title) + 2 * TITLE_DASHES + 4)
```

where `content_width` counts the contents alone, neither border nor padding.
The two floors do not combine: the title's are border-row columns and the
padding's are body columns, so whichever demands more wins.

### Implementation

`utils::titled_block` applies the padding and returns the width with both
floors already taken, so a widget that gets its block from it satisfies both
rules by construction:

```rust
let (block, width) = titled_block("Voyage Type", max_name as u16 + 2);
```

Pass the content width only. Adding the border or the padding yourself
double-counts them.

### Verifying

A violation is a character that is neither a space nor part of the frame
sitting immediately inside a `│`. Scanning the `.txt` dumps for that finds
them, with one caveat: a popup drawn over a page puts the page's own text
immediately *outside* the popup's border, which reads as a hit and is not one.
Only adjacency on the inside of a box counts.

The gallery currently draws no widget whose contents touch its border.

Six did before the rule was written: the Voyage Statistics stat lines (labels
flush left, values flush right), its consumption warning, the Sea Battles
popup rows, the Damage reset prompt, the Map help popup, and the Jobbers
placeholder. The Damage reset prompt was also a clipping bug rather than only
an aesthetic one — its question is 37 columns and its box was 32, so the text
was cut mid-word; sizing the box from the question fixed both faults at once.

One thing padding did not fix: the Jobbers placeholder was pinned to the
top-left of its box and ran off the right edge mid-sentence at 80 columns
instead of wrapping, so padding it moved the cut one word earlier. Rule 5
settles that case.

## Rule 3: Too small a terminal shows a message, not the app

Below the size needed, no page is drawn. A page squeezed past its minimum does
not merely look cramped: it drops whole widgets, so it reports the state of
things wrongly. A message saying to enlarge the window, centered on both axes,
is drawn instead.

```
                     Terminal too small
       Enlarge the window to at least 54 columns (it is 50).
```

### How wide is a page's own business

Each page asks the question for itself, about the content it has in hand right
now, rather than being held to one figure for the whole app. A window that fits
the page the user is on draws it, even where another page would not have fit:
the Damage grid is as wide as two ship names and the labels between them and so
wants 54 columns, while Profits is usable in 48 and the Voyage panel in 44.

The top bar stays above the message, which is the point of putting the question
in the page rather than in the shell — the pages that *do* fit are still one
keypress away.

So the sizes of window are:

| terminal                     | drawn                                     |
| ---------------------------- | ----------------------------------------- |
| at least this page's minimum | the page                                  |
| at least the bar's minimum   | the bar, and the message where the page was|
| smaller                      | the message alone                          |

A page refuses only over what it cannot shrink or scroll out of. The Profits
Inventory is absent from its own minimum for exactly that reason: a table wider
than its box scrolls sideways and still reaches every column, so it raises no
requirement. A `Restocking Place` row has nowhere to scroll to, so it does.

### The numbers

The shell's own floor is the **top bar**, below which no page can be reached at
all. It is the sum of every label's width with a blank column each side — the
same padding Rule 2 gives a boxed widget:

```
topbar_min = Σ (label_width + 2 * TOPBAR_PADDING)
```

which is **49** columns, plus the bar's two rows. Slots are *not* equal width.
Equal slots would size every slot to the widest label, `Statistics`, needing
72 columns before the bar fit; sizing each slot to its own label needs 49.
Keeping the slots equal is not worth clipping a label for, so each slot takes
its label and padding first and any slack is shared out afterwards.

### Everything must work at 80 columns

This is the half of the rule that constrains ordinary work rather than the
degenerate case: **no page may need more than 80 columns** for its own
furniture. The ceiling is not an aspiration to render *something* at 80; it is
where every page must be fully usable, and
`app::topbar_tests::no_page_needs_more_width_than_the_ceiling` holds each page
to it.

What the ceiling does not bound is the data a page is handed — a pirate can
have a longer name than any window — which is why a page that cannot scroll
such content out of the way may still ask for more than 80 and say so.

## Rule 4: A scrollable view keeps four rows, and nothing else is clipped

Height is asked of each page the same way width is, and answered by two tests.

**Where the page has a view the user scrolls through** — a table, a ranked list,
a panning chart — that view keeps at least **four of its own rows**. Fewer than
four and there is too little of it on show to tell that it continues past the
window: a list of thirty reads as a list of two, which misreports what is there
rather than merely cramping it. If the window cannot give four, the terminal is
too small.

The four are the view's *own* rows. A pinned header does not scroll, so it is
not one of them, and the borders are on top of that again:

```
┌─── Inventory ──────────┐
│   Item  Restock  Stock │  <- header, pinned
│                        │  <- its margin
│   Rum       100    250 │  1
│   Iron       50      0 │  2
│   Hemp              80 │  3
│   Sugar cane        12 │  4
│                        │  <- the row kept for its sideways bar
└────────────────────────┘     9 rows for 4 of list
```

**Where it has none** — the Damage calculator is one fixed grid — the test is
simply clipping: if a widget's contents would be cut off, the terminal is too
small. A page with nothing to scroll needs exactly the rows it draws.

### A view that scrolls shows a bar

A view the user scrolls keeps its two rightmost columns: one for the scrollbar,
and the blank one that holds the contents off it, the same blank column Rule 2
keeps between contents and a border. The bar is drawn there only while there is
something to scroll — with every row of the view on show the two columns are the
contents' to use, so a list that fits looks like any other widget and nothing
offers to scroll what cannot.

The columns are reserved in the widget's **width** whether or not the bar is up,
so a widget sized to its contents does not change width the moment the bar
appears. Content that reflows into the width it is handed — the trophy grid,
three columns of whatever room there is — is laid out a second time once the bar
turns out to be wanted. Laying it out narrower can only lengthen it, so a bar
never un-needs itself and the second pass is the last.

The bar spans the view, and says where in the list the window sits without the
thumb having to be measured:

| part | what it is |
| ---- | ---------- |
| top cell | `▲` while rows remain above, `┬` once the view is at the top |
| bottom cell | `▼` while rows remain below, `┴` once it is at the foot |
| thumb | `█`, as long a part of the track as the rows on show are of the whole |
| track | `│` |

The thumb sits over the cells it can reach: flush with the track's near end at
the first offset, flush with its far end at the last. So the thumb and the two
end glyphs cannot disagree about whether a view has further to travel.

The pirate popup's skill tables in an 80x24 terminal, which holds fifteen of
their seventeen rows, the window at the top of the list:

```
│              Piracy Skills              ┬ │
│ Sailing        Narrow        Able       █ │
│ Carpentry      Broad         Proficient █ │
│ Bilging        Solid         Respected  █ │
...
│ Blacksmithing  Expert        Master     │ │
│                                         │ │
│            Carousing Skills             ▼ │
```

The two arrows are the part worth having: a capped end means there is nothing
that way, so a view can be read as scrolled-to-the-end without comparing the
thumb against the track.

### A view that scrolls sideways shows one along the bottom

The same bar, turned: it lies along the view's bottom row and counts columns.

| part | what it is |
| ---- | ---------- |
| left cell | `◄` while there is more to the left, `├` once the view is at the left edge |
| right cell | `►` while there is more to the right, `┤` once it is at the right |
| thumb | `█`, as long a part of the track as the columns on show are of the whole |
| track | `─` |

It keeps **one row**, where the upright bar keeps two columns. The second column
is there because a bar drawn hard against a word runs into it; a rule under a
line of text already reads clear of it, and rows are the scarcer of the two.

A view that scrolls both ways has one of each, and hands each the room the other
leaves — so neither measures what the other has taken, and the corner where they
would meet stays blank. The Map's chart, four of whose rows and most of whose
columns are on show:

```
│          ◇  Nunataq                                      ○               ○ │ │
│        ╱   ╲                                               ╲               ▼ │
│ ◄─────────────────────────────────██████████─────────────────────────────►   │
│ Messier's Crown (69,2)  not memorized                                Emerald │
```

### The bar answers the mouse

The bar's own column is a click target — the blank one beside it is not, so a
click meant for the text never lands on the bar.

| where | what it does |
| ----- | ------------ |
| an arrow end | one row (or column) that way |
| a capped end | nothing; there is nothing that way, and the glyph says so |
| anywhere on the track | that far along the view: the track's first cell is the start of the content and its last cell the end |
| the wheel, anywhere on the bar | one row, the same as an arrow |

A click on the track is a jump to where it pointed rather than a page-step, so
a long list is crossed in one click. The thumb is not corrected for its own
length, which on the short tracks a four-row view gives would be noise.

An arrow is a *step* of one row, except on the Map, where a row is a quarter of a
league point across and half of one down. There it steps a whole point, which is
the distance the page is drawn in.

The thumb is not dragged. Nothing in the app is.

### A view's window is either its own or its cursor's

The bar reads the same for every view; what a view does with the ask depends on
what moves its window.

- **Its own window.** The two popups keep a scroll offset and nothing else
  decides it, so the bar sets it outright. The Map's chart and the Voyage body
  are the same once scrolled by hand: the bar parts the window from the cursor
  and sets it in rows (canvas cells, on the Map), so it reaches the content's own
  ends and caps there. The parting lasts until the cursor moves — a league point
  selected, a stat or chart focused — which returns the window to following it.
  Until then the cursor may be off the view, which is what lets the Map's bars
  reach open sea.

  The Map's Island column is of this kind too, and the plainest case of it: it
  has no cursor of its own at all, only lines about the point the chart's cursor
  is on. Its window starts at the top of each point selected and is the reader's
  from there, which is why selecting a point clears it along with the chart's
  pan. The wheel over the column scrolls it rather than panning the chart
  behind it: the wheel belongs to whatever it is over.

  It pins a head of its own above that window, and wraps rather than clipping,
  which makes its row count a thing the width decides. Both are the Map's own
  shape rather than anything general, and are documented in `src/map/ui.rs`;
  what the bar has to live with is that the window is whatever rows the head
  leaves, settled before the bar is drawn.

  The Voyage body has to work this way. Its focusable items are not spread
  evenly down it — a stat is one row, a chart is nine — so a bar that moved the
  focus crawled: two thirds of its track scrolled nothing at all, because those
  stats were already on show, and the last two cells leapt thirty rows between
  them. `app::voyage_scroll_tests::the_bar_walks_the_body_evenly_down_its_track`
  holds the body to one cell's worth of rows per cell of track.
- **A cursor's.** The remaining page views have no independent window at all:
  the Inventory, the panes and the Skill Leaderboard scroll to keep their
  selection in sight, recomputed from it every frame. The bar moves the cursor
  and the window follows, which is the only thing it could mean there. The wheel
  over those views already works this way.

  They can work this way because their rows *are* their items: one commodity,
  one pirate, one rank to a row, so moving the cursor a tenth of the way down
  the list moves the window a tenth of the way down the body.

  What the cursor is differs by view, and so does what the bar's two ends mean.
  The Inventory's sideways bar runs from the first editable column to the last,
  since the Item column is not one the cell cursor can rest on.

A view of the second kind is focused before its cursor moves: a cursor that
moves out of sight has not visibly moved at all.

The consequence worth knowing: on a cursor-driven view the thumb lands near
where it was pointed rather than exactly under the pointer, because the rows
the thumb is measured in and the items the cursor counts are not the same
thing — the Voyage body's focusables are a dozen stats spread over fifty rows.

### Implementation

`utils::render_scrollbar` draws the upright bar and `utils::render_hscrollbar`
the sideways one — both over one `render_bar`, since the two differ only in their
glyphs and which edge they take. Each registers its click region and hands back
the rect the contents may use, which is the whole of the area when there is
nothing to scroll.

`render_bar` places the thumb itself rather than handing the job to ratatui's
`Scrollbar`, which rounds the thumb's start and its length apart: the two can sum
past the track, and the far end's glyph is then pushed off the bar — a view at the
end of its travel showing no cap, which is exactly the thing the ends are there to
say. `utils::tests::a_scrollbar_is_two_ends_and_a_thumb_inside_the_track` walks
every shape of window on both axes and checks each bar whole, since the parts are
only right together.

```rust
let body = render_scrollbar(
    frame, regions, area, ScrollView::JobberTrophies, offset, lines.len(),
);
frame.render_widget(
    Paragraph::new(lines.into_iter().skip(offset).take(view_h).collect::<Vec<_>>()),
    body,
);
```

Either is called after whatever region the view claimed for itself, so the bar's
cells answer to the bar rather than to the list behind it. `utils::scrollbar_hit`
reads a click on one and `ScrollHit::resolve` turns the ask into a row or a
column, which `AppShell::scroll_bar` hands to the right view.

A widget that sizes itself to its contents adds `utils::SCROLLBAR_W` — the bar's
column and its blank — to the width it asks for, and `utils::SCROLLBAR_H` to its
height where it scrolls sideways. Where a layout must be settled before a bar can
be drawn, `utils::scrolls` is the one place that answers whether the room is
spent; a view that scrolls both ways asks it twice, once per axis, taking the
other bar's room into account. One pass over the pair settles it, because taking
room away can only make the other bar more wanted, never less.

### Where this applies

Nine views scroll, and each has a bar:

| view | what its window follows | state to read it in |
| ---- | ----------------------- | ------------------- |
| pirate popup, the skill tables | its own offset | `80x24-jobbers-popup-pirate-stats` (`120x40` is tall enough for all of them, and shows no bar) |
| trophies popup, the category grid | its own offset | `80x24-jobbers-popup-trophy-list` |
| Map help popup | its own offset | `120x24-map-help`, and `120x24-map-help-scrolled` for the far end - at `120x40` the whole help fits and shows no bar |
| Profits Inventory, down | the row cursor | `120x40-profits-long-list` |
| Profits Inventory, across | the column cursor | `80x30-profits-wide-table` — at 120 columns the box is as wide as the table and nothing scrolls, so this one needs `--size 80x30` |
| Jobbers panes | that pane's selection | `80x24-jobbers-long-roster` |
| Jobbers Skill Leaderboard | the ranked selection, shared by its columns | `80x24-jobbers-long-roster` |
| Voyage body | its own scroll, until the focus moves | `80x24-voyage-pillage`, and `80x24-voyage-scrolled` for a window with rows on both sides of it |
| Map chart, both ways | its own pan, until a point is selected | `80x24-map-ocean` |
| Map Island column | its own scroll, until a point is selected | `120x24-map-island-exports`, and `120x24-map-island-scrolled` for the far end — the column is only shown from 100 columns out, and at `120x40` what it says about an island fits, so this one needs `--size 120x24` |

### The answer must not move as focus moves

A page counts the rows its tooltip would take whether or not one is showing.
Were it to count them only when one is up, resting on a field would push a tight
window past the page's requirement and the page would vanish under the user's
hands — then come back when focus moved on. The same goes for the Search box's
suggestion row.

The spare rows are not wasted while the transients are empty. Where a page has
one box that takes the slack, they go to it: the Profits Inventory is guaranteed
four commodity rows and shows six when no tooltip is up.
`profits::ui::inventory_tests::what_the_page_needs_does_not_move_with_the_focus`
holds this.

Content the *data* drives is a different matter and does move the requirement —
a pirate boarding lengthens a pane, an understaffed ship adds a warning line —
exactly as it moves the width a page needs under Rule 3. The four-row floor caps
how far that can go: a pane needs its rows up to four of them and no more, since
past four it scrolls.

### A box is as tall as its contents

Four rows is a floor, not an allowance to grow into. On the Jobbers page no box
stretches to fill the window: each takes the rows its contents come to and
whatever is left over stays blank at the foot of the page, so a pane holding
three pirates is three pirates tall. Where they cannot all have that, the boxes
whose lists scroll give way — the panes first, holding the longer rosters, then
the Skill Leaderboard — rather than the page dropping one of them.

The Skill Leaderboard is capped besides, at `--leaderboard-size` (default five):
it is a leaderboard, so it shows its top few and the rest of the ranking
scrolls. It is never the page's filler.

Profits is the one page that still hands its slack to a single box, the
Inventory, rather than leaving it blank.

### What the pages need

In terminal rows, the two-row bar included. The Jobbers figures are for the
rosters in the gallery and grow with them, to the four-row cap:

| page | rows | why |
| ---- | ---- | --- |
| Map | 11 | a four-row viewport, its sideways bar, the search row, its border, the hint under it |
| Damage | 13 | the grid is one fixed block; nothing scrolls |
| Voyage | 14 | pinned header and footer around a four-row body |
| Jobbers, Pillage | 20 | Voyage box, Skill Leaderboard, the panes |
| Jobbers, Atlantis | 23 | the above plus the Atlantis Stats box |
| Jobbers, Vampirates | 26 | the Vampirates Stats box and the distribution button |
| Jobbers, Vikings | 26 | leaderboard beside the panes, under its stats box |
| Jobbers, Cursed Isles | 27 | the Fight Statistics box, the tallest of them |
| Profits | 29 | four boxes stacked under the Inventory's nine rows |

Unlike the 80-column ceiling of Rule 3, **no ceiling is set on height**. The
consequence is deliberate and worth stating plainly: a conventional 80x24
terminal is five rows too short for Profits, and two or three short of Jobbers
on its longest voyage types, and shows the notice there while the rest draws.

## Rule 5: An unmet prerequisite is a centered, wrapped notice

When something cannot be used until a prerequisite is met — an argument not
passed, a window too small, an ocean not chosen — the text saying so is
**centered on both axes** of the room it stands in and **word-wrapped** to that
room's width.

The Jobbers page with no chat log attached, at 80 columns:

```
    Profits       Damage        Jobbers        Voyage          Map      Exit
                                             Statistics


  No chat log attached. Pass --chat-log <PATH> (and --user <NAME>) to monitor a
                                    game log.


```

Both halves earn their place. Centering is what distinguishes the notice from
content: text pinned to the top-left reads as the first row of something, as
though more were to follow, while a block in the middle of an otherwise empty
space reads as the whole of what there is to say. Wrapping is what keeps the
sentence whole; a notice is the one thing on screen that must be read in full, so
it is the worst possible thing to truncate.

### How much room the notice gets

That depends on whether the thing it replaces had a border:

| the notice stands in for | drawn |
| ------------------------ | ----- |
| one widget with visible box bounds | inside those bounds, which are still drawn |
| the whole page | no box, but still a blank column each side |

A widget keeps its frame because the frame is what says *which* widget is
unavailable, while the rest of the page carries on around it; the title stays
legible and the page's layout does not shift. A page-wide notice has no frame to
inherit, so it takes Rule 2's blank column directly against the screen edge.

A page whose whole content is one widget takes the second case, not the first. A
frame there would enclose nothing but the notice — a rectangle sized to its own
message, reading as a stray panel rather than as the page — and its title would
name a widget that is not being shown. Both Jobbers and Voyage Statistics are
such pages, so neither draws a box while its prerequisite is unmet.

That leaves the boxed case for a widget that is genuinely one part of a page: the
Map, whose tally rides the foot of its frame and whose metadata column stays
beside it at width.

### Implementation

`utils::render_notice` does all three things — fold, centre, centre — so a
notice is one call:

```rust
// Page-wide: no border, so the blank columns are taken here.
render_notice(
    frame,
    Rect {
        x: area.x + PADDING.min(area.width),
        width: area.width.saturating_sub(2 * PADDING),
        ..area
    },
    &[(MESSAGE, Style::default())],
);

// Inside a widget: the block's own padding supplies them.
render_notice(frame, block.inner(area), &[(MESSAGE, Style::default())]);
```

It takes the rect the text may fill, not the widget's outer rect, because the
blank columns come from two different places in the two cases above. For a boxed
widget that rect is `block.inner(area)`, whose padding Rule 2 already applied;
for a page-wide notice the caller insets by `utils::PADDING` itself.

Entries are wrapped one at a time, so a heading and the detail under it can be
styled apart:

```rust
render_notice(frame, rect, &[
    ("Terminal too small", Style::default().bold()),
    (&detail, Style::default().fg(Color::DarkGray)),
]);
```

The folding has to happen here rather than in `Wrap`: the notice's height follows
from the text once folded, and the vertical centering needs that height.

### Where this applies

| notice | room |
| ------ | ---- |
| `Terminal too small`, too small for any page (Rule 3) | the screen below the bar, unboxed |
| `Terminal too small`, too small for this page (Rules 3 and 4) | the page, unboxed |
| Voyage `No voyage tracked yet.` | the page, unboxed |
| Jobbers `No chat log attached` | the page, unboxed |
| Map `Select an ocean (--ocean)` | the Map box |
| Map `No map for <ocean> yet` | the same |

Three things that read like notices are deliberately not ones:

- `Fetching island info...` is progress, not a prerequisite — it resolves on its
  own, and moving it would make the metadata column jump.
- `Query Market first` and `Ocean-wide` in the Profits parameter panel are field
  placeholders. They occupy a single row of a laid-out form, where there is no
  second axis to centre on, and they are right-aligned with the values they
  stand in for.
- `Select ship hull first to show historical.` replaces one row of the Voyage
  chart stack. The rest of the chart is still usable, so it is a note inside
  working content rather than a stand-in for it, and it stays on the row where
  its box plot would have been.

## Rule 6: Table headers are centered

A column header sits centered over its column, whatever the column's contents
are aligned to.

The alignment only shows when a column is wider than its own header, which is
the case worth getting right: a column sized to its widest value leaves a
short header stranded at one end, reading as though it belongs to whatever is
beside it rather than to the column it names.

```
          Item                 Restock  Stock  Booty
Fine enchanted midnight broadcloth  …
```

not

```
Item                                 Restock  Stock  Booty
Fine enchanted midnight broadcloth  …
```

Headers are centered independently of the cells below them. The Inventory's
numeric cells are right-aligned under centered headers, which is intended: a
column of figures reads down its right edge, while the header names the whole
column.

### Where this applies

The app has five column-bearing widgets, and only one is a ratatui `Table`:

| widget | columns built by |
| ------ | ---------------- |
| Profits Inventory | `Table` |
| Jobbers Skill Leaderboard | per-column rects |
| Voyage Ship Winrate matrix | a drawn grid |
| Jobbers skill distribution | a drawn grid |
| Damage calculator | three columns, whose centre column is row labels rather than headers |

All but the Inventory already centered their headers; it is the only one the
rule changed.

## Rule 7: Popups are small, centered, and carry their own way out

A popup takes the screen away from the page under it, so it earns that by being
no bigger than what it has to say, and by saying plainly how to be rid of it.

### As small as its contents

A popup is sized from its contents, not from the window: the widest of its
text, its list and its buttons, plus the frame Rules 1 and 2 ask for.

```
┌─── Restock warning ───────────┐      ┌─── Restock warning ────────────────────────┐
│ No supply on this island for: │      │ No supply on this island for:              │
│   • Hemp                      │      │   • Hemp                                   │
│   • Cloth                     │      │   • Cloth                                  │
│                               │      │                                            │
│  Change Island    Ocean-wide  │      │         Change Island    Ocean-wide        │
└───────────────────────────────┘      └────────────────────────────────────────────┘
          sized to content                     44 columns, because 44 was typed
```

A row nothing is drawn into is not reserved either — the `New battle` prompt has
a line for a noteworthy foe and shows it only when there is one.

### A popup the window cannot hold scrolls, and asks for nothing

Where the window is shorter than what a popup has to say, the popup takes the
rows there are and the rest is read by scrolling, under Rule 4 — it never cuts
its own foot off. So a popup asks for no height beyond what its page already
needs: the Map's help is twenty-odd rows of keys and legend, and is read whole
at the eleven rows the Map page itself draws in
(`map::ui::tests::the_help_scrolls_in_a_window_too_short_for_it`). The three
popups that work this way — the pirate stats, the trophies grid and the Map's
help — each keep their own offset, so the page behind them is untouched by the
reading.

Content with no size of its own still has a *largest useful* size. The Trophies
grid reflows into three columns of whatever width it is given, so it is three
columns of the longest trophy's name and no wider — not the 80 it used to be
typed at. The enlarged `PoE per Fight` chart spends height on one row per fight,
so it is as tall as it has fights to show, where before it took twenty rows and
left the spare ones blank.

Only a view that genuinely uses every column and row it is handed, like the
Ship Winrate matrix, may fill the screen.

### A single-line body is centered

Where the body is one line, it is centered over the buttons beneath it, matching
how they are centered themselves:

```
┌─── Delete row ────┐
│ Delete row "Rum"? │
│                   │
│     No    Yes     │
└───────────────────┘
```

A caveat that belongs to the question shares its line rather than taking
another: `Re-query market prices? This may take some time.` is one body line,
the caveat dimmed, and so it is centered too.

A body of several lines is laid out on its own terms — a bulleted list reads
down a left edge, so it is left-aligned and the rule does not touch it.

### Buttons are bracketed, equal, and evenly spaced

A button is always written `[ Label ]`, brackets included, whether its label is
a word or a letter — `[ Yes ]`, not ` Yes `. Within a row every button is as
wide as the widest label among them, and the gaps between them and at both ends
of the row are equal:

```
│   [ Change Island ]  [  Ocean-wide   ]   │
│         [ Yes ]       [ No  ]            │
```

Equal widths are what make a row read as a set of alternatives rather than as
words of differing importance, and the equal end gaps are what keep the set
centered without a hand-placed offset.

A popup takes its width from `utils::buttons_width`, so the buttons are never
the thing that gets squeezed when the text above them happens to be short.

One look for all of them: the focused button is drawn in reverse, the same mark
the top bar and the table cursor use. Three different looks (reverse here, cyan
there, bold-and-dim elsewhere) used to say the same thing three ways.

Take the row from `utils::render_buttons`, which hands back each button's rect
for the caller to register its click on.

### Nothing sits under the bottom row of buttons

The last row inside a popup is its buttons, with **one blank row above them and
nothing below**. A blank row under a button is room reserved for nothing, which
is what the Ship Winrate popup kept until its frame stopped padding the bottom.

The exception is a tooltip strip, which a popup may reserve beneath its buttons
the way a page does.

### The default is the convenient choice

The highlighted button is the one the user most likely came for. Convenience
decides it; where no choice is clearly the wanted one, the default is the one
that changes least.

| prompt | default | why |
| ------ | ------- | --- |
| `Delete Row` | Yes | the user pressed Delete; Enter finishes what they started |
| `Reset Values?` | Yes | raised by their own ship change, and clearing is the point of it |
| `New Battle` | Apply | a new foe wants its hull seeded; that is why the prompt exists |
| `Hold From Clipboard` | No | nobody asked for it — a clipboard filled elsewhere raised it, and Yes overwrites a hand-typed Stock column |
| `Re-Query?` | No | costs a round trip to the market |
| `Restock Warning` | Change Island | returns the user to the field they were editing |
| `Save Voyage?` | Cancel | the prompt can be raised in passing |

`Save Voyage?` is Cancel and Save; it used to be Save and Discard, with the
default depending on which key opened it. Nothing on screen throws a voyage away
now — a run is discarded by never saving it.

### Esc is not worth a line

No popup spends a line, or a corner of its border, saying that Esc closes it.
Esc still closes it. In place of that, a popup the user can dismiss carries a
**Close button, centered on its own last row**, which can also be clicked:

```
┌─── Ship Winrate ────────────┐
│ No sea battles recorded yet │
│                             │
│          [ Close ]          │
└─────────────────────────────┘
```

Hints the user cannot act on wrongly are not worth a line either. The Save
voyage prompt said `←/→ select · Enter confirm · Esc cancel` under two visible
buttons, which is three facts the buttons already carry.

Take the button from `utils::render_close_button` so every popup's reads the
same; the caller registers its click region, since only it knows what closing
means.

### Two rows of buttons where a popup has its own controls

A popup that does something besides open and close puts those controls on their
own row and keeps Close beneath them, so the way out is always in the same
place:

```
│       0:00                                                      0:15 │
│ [ ← Prev ]                 [ Axis: Time ]                 [ Next → ] │
│                               [ Close ]                              │
└──────────────────────────────────────────────────────────────────────┘
```

Every one of those is clickable, which is why they are buttons in the frame
rather than a hint line below it. A cell holding a button is as wide as the
button's whole label, brackets included — the Axis cell was two columns short
and dropped its closing `]`.

### A choice is shown by highlight alone

A vertical list the user picks from marks the selected row by highlighting it,
with no `> ` prefix:

```
┌─── Voyage Type ───┐          ┌─── Voyage Type ───┐
│ Pillage           │          │   Pillage         │
│ Atlantis          │          │   Atlantis        │
│ Cursed Isles      │ ← hl     │ > Cursed Isles    │ ← hl
│ Vampirates        │          │   Vampirates      │
└───────────────────┘          └───────────────────┘
```

The highlight already says which row it is, and it says so in a way the marker
cannot: it survives the row being read at a glance. Dropping the marker also
narrows the box by the two columns it held.

The choices are centered **as a block**, and are not themselves centered. A list
is read down its left edge, so the words stay flush with one another and the
whole column moves instead — which matters because the box is usually held open
by its title rather than by its longest entry:

```
┌─── Voyage Type ───┐          ┌─── Voyage Type ───┐
│   Pillage         │          │ Pillage           │
│   Atlantis        │          │ Atlantis          │
│   Cursed Isles    │          │ Cursed Isles      │
└───────────────────┘          └───────────────────┘
   block centered                 flush left, box
   in the box's slack             looking lopsided
```

`utils::choice_block` is `titled_block` with that offset applied.

### Nothing to search is nothing to show

A control that cannot do anything is not drawn. The Trophies popup hides its
search box when the pirate has no trophies, or none fetched yet, and shrinks to
the one line it has to say — but it keeps the box when a *filter* matches
nothing, since clearing the filter is what the user needs it for.

## Rule 8: Titles are in title case

Every widget title, popup title and chart title follows **APA title case**:

- the first word is capitalized, whatever it is;
- so is every major word — nouns, verbs, adjectives, adverbs, pronouns — and
  every word of four letters or more, whatever its part of speech;
- minor words of three letters or fewer stay lowercase: the articles `a`, `an`,
  `the`, the short conjunctions (`and`, `but`, `for`, `or`, `nor`, `so`, `yet`)
  and the short prepositions (`at`, `by`, `in`, `of`, `on`, `per`, `to`, `up`,
  `via`);
- both halves of a hyphenated compound are capitalized.

So `PoE per Fight` keeps `per` lowercase at three letters, while
`Hold From Clipboard` capitalizes `From` at four — the length is what decides
it, not whether the word feels important.

A title is a name, which is why it gets this treatment. Anything that is a
*sentence* does not: the body of a popup, a tooltip, a hint, a button label and
a stat row's label are all left as they read.

```
┌─── Delete Row ────┐      ← title, title case
│ Delete row "Rum"? │      ← a question, sentence case
```

### What the sweep changed

| was | is |
| --- | -- |
| `Delete row` | `Delete Row` |
| `Re-query?` | `Re-Query?` |
| `Restock warning` | `Restock Warning` |
| `Prices needed` | `Prices Needed` |
| `Hold from clipboard` | `Hold From Clipboard` |
| `Reset values?` | `Reset Values?` |
| `New battle` | `New Battle` |
| `Save voyage?` | `Save Voyage?` |
| `PoE per fight` | `PoE per Fight` |
| `Value per share` | `Value per Share` |
| `Who are ye?` | `Who Are Ye?` |
| `Choose yer Pirate` | `Choose Yer Pirate` |

The other seventeen titles the gallery draws already complied. The doc comments
that name a popup by its title were carried along with it, so searching the
source for a title still finds the code behind it.


## Rule 9: The app speaks like the game it serves

This is a tool for a pirate game, so its prose reads like one. The register is
not a blanket swap of *you* for *ye*, though: the game splits by what the
sentence is doing, and so do we.

### The record is plain; the address is not

The game writes about you in plain English and talks to you in pirate. Across
a real chat log the split is consistent, and `ye`/`yer` outnumbers
`you`/`your` about four to one:

```
Your standing in Sailing went up and is now Respected in the whole ocean!
Your experience in Carpentry is now 1200!
Your vote has been counted.
You intercepted the War Frigate!
```

```
Ye were paid 10 pieces of eight fer yer foraging.
Ye found an empty basket!
Yer crew member has logged on.
Yer hearty, Foo, has logged off.
```

So a **figure reported about the reader stays plain** — a standing, a rank, an
average, a tally, the definition of a stat. A **sentence addressed to the
reader takes the voice** — a prompt, what answering it will do, a warning, a
refusal, a hint that tells them what will happen.

### A refusal opens with Arr

The game's refusals have one shape: `Arr,` or `Arr!`, then the plain statement,
then optionally a second sentence giving the rule behind it.

```
Arr, ye cannot view that info right now.
Arr, ye can't do that til yer done with yer current task.
Arr, ye must be charted to sea monsters first.
Arr! Ye can't stand there.
Arr, ye can't whisk to any island matching 'beaufort'. Ye can only whisk
to islands ye've visited or that appear on a map ye carry.
```

Not everything gets one. A plain requirement stays plain: the game's own
`Sea charts must be put on the table before their courses may be charted.`

### The vocabulary

| plain | ours |
| --- | --- |
| you, your (addressing) | ye, yer |
| you're | yer |
| until | til |
| for | fer |
| ship | vessel |
| crew members | pirates, or hearties for our own |
| money | pieces of eight, PoE |
| none, nothing | naught |

And no software vocabulary where a plainer word exists. Nothing the reader
sees is *invalid*, *parsed*, *queried*, *synced*, *persisted* or *data*: a run
is **kept**, a name is one **we can't find**, prices are **fetched**.

An ocean's name renders bare, so prose supplies the rest: `the Emerald ocean`,
never `Emerald` on its own.

### It does not license vagueness

The voice is the wrapping, never the content. A prompt still says exactly what
answering it will do, a figure is still the figure, and a warning still names
what is at risk. Where the two conflict the fact wins — which is why the
window-too-small notices are terse (`Arr! Too narrow.`) rather than fuller:
they are drawn in a window too small by definition, and there the shorter
line is both the better pirate and the better engineering.

Say only what is true, too. A failed lookup means we could not find a pirate,
not that none exists, so the notice says `no 'Foo' to be found` rather than
`no pirate 'Foo'`.

### Out of scope

- **Titles**, which are names and belong to
  [Rule 8](#rule-8-titles-are-in-title-case) — except one that is itself a
  question put to the reader, which is why `Who Are Ye?` reads as it does.
- **Key hints**: `Press Enter to pick a vessel.` There is no *you* to convert
  and plainness is the whole job.
- **Figure definitions**: `Average cannonballs fired per sea battle.` These
  are the record, exactly where the game writes `Your standing…`.
- **Diagnostics**, which are for us and not the reader.

### Verifying

Read the prompts and notices in the gallery's `.txt` dumps and ask whether the
game would have said it that way. The screens outside the page model have
their own entries: `startup-setup*` for the first thing anyone sees, and
`window-*` for the refusals, one per form they take.

### What the sweep changed

| was | is |
| --- | -- |
| `Terminal too small` | `Yer Window Be Too Small` |
| `Enlarge the window to at least {n} columns (it is {}).` | `Arr! Too narrow. Make it {n} columns (it is {}).` |
| `No pirate '{}' on {}. Check spelling, or Esc to skip.` | `Arr, no '{}' to be found on the {} ocean. Check yer spelling, or Esc to skip.` |
| `Couldn't verify: {e} (Esc to skip)` | `Arr, couldn't look ye up: {e} (Esc to skip)` |
| `Verifying {name} on {o}…` | `Looking for {name} on the {o} ocean…` |
| `Select this to enable market querying.` | `Pick this to fetch market prices.` |
| `Press Enter to not identify yourself.` | `Press Enter to stay nameless.` |
| `Jobber functionality will be reduced as a result.` | `Ye'll see less of yer jobbers that way.` |
| `Voyage win/loss will also be indeterminate without a name.` | `And without a name, we can't tell a win from a loss.` |
| `You left the ship and you might have missed logs that were important.` | `Ye left the ship, so we might have missed something important.` |
| `Press Enter to ignore the warnings.` | `Press Enter to pay it no mind.` |
| `Please do not leave the Swordfight even if you lose.` | `Don't leave the fray, even if ye lose.` |
| `Invalid ship selected.` | `Arr, too many aboard for that ship.` |
| `Not queried yet` | `Not looked up yet` |
| `Did you mean "…"` | `Did ye mean "…"` |
| `Query Market first` | `Fetch market first` |
| `Query the market first to pick where to sell.` | `Fetch the market first to pick where to sell.` |
| `Not recognized: {}` | `No such goods we know of: {}` |
| `Set the Stock column from the copied hold?` | `Fill the Stock column from the copied hold?` |
| `Other rows' Stock is cleared; Booty is left as is.` | `Every other row's Stock be cleared. Yer Booty stays as it is.` |
| `Re-query market prices?` | `Fetch the market prices afresh?` |
| `Re-Query?` | `Fetch Afresh?` |
| `This may take some time.` | `This may take a while.` |
| `No supply on this island for:` | `Naught to be had on this island:` |
| `Enter the missing prices before calculating:` | `Enter the missing prices first:` |
| `Current tally is not saved.` | `Ye'll lose the tally as it stands.` |
| `No geography data for this island.` | `We know naught of this island.` |
| `Select an ocean (--ocean) to see its map.` | `Pick an ocean (--ocean) to see its map.` |
| `Loaded persisted data from {}` | `Read yer records from {}` |
| `Fetching market data for {} missing commodities...` | `Fetching prices for {} missing commodities...` |

Two of those were not only out of voice. `Invalid ship selected.` described the
wrong condition: the check fires when more crew are aboard than the chosen
ship can hold, which is not an unknown ship. And the hold prompt's note was
being **clipped**, 50 characters rendered in a 44-column row with no wrapping;
it now reserves the rows it wraps to, as [Rule 4](#rule-4-a-scrollable-view-keeps-four-rows-and-nothing-else-is-clipped)
requires.
