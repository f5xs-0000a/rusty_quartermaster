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
instead of wrapping, so padding it moved the cut one word earlier. Rule 4
settles that case.

## Rule 3: Too small a terminal shows a message, not the app

Below the size the app needs, no page is drawn. A page squeezed past its
minimum does not merely look cramped: it drops whole widgets, so it reports
the state of things wrongly. A message saying to enlarge the window, centered
on both axes, is drawn instead.

```
                     Terminal too small
       Enlarge the window to at least 80x24 (it is 70x20).
```

The top bar is held to its own, smaller, minimum. While the bar fits whole it
stays, because it still says what the app is; once a label would be clipped
the bar goes too, a clipped bar reading as broken rather than as small. So
there are three sizes of window:

| terminal                        | drawn                   |
| ------------------------------- | ----------------------- |
| at least the app's minimum      | the page                |
| at least the bar's minimum      | the bar and the message |
| smaller                         | the message alone       |

### The numbers

The app's minimum is **80x24**, the conventional terminal floor.

The top bar's minimum is the sum of every label's width with a blank column
each side — the same padding Rule 2 gives a boxed widget:

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
degenerate case: a widget that cannot shrink to 80 columns is unfinished. The
minimum is not an aspiration to render *something* at 80; it is where the app
must be fully usable.

### Known gaps

The app does not yet satisfy its own minimum:

| page | fault at 80x24 |
| ---- | -------------- |
| Profits | the Inventory table collapses to its header, hiding every row the user entered; the four boxes below it take the height first |
| Profits | `Restocking Place` and `Selling Place` show `Query Ma`, truncated — its block is a fixed 40 columns at every terminal width, too narrow for its own labels |

## Rule 4: An unmet prerequisite is a centered, wrapped notice

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
Map, whose two status rows stay put below it and whose metadata column stays
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
| `Terminal too small` (Rule 3) | the page, unboxed |
| Voyage `No voyage tracked yet.` | the page, unboxed |
| Jobbers `No chat log attached` | the page, unboxed |
| Map `Select an ocean (--ocean)` | the Map box, above its two status rows |
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

## Rule 5: Table headers are centered

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
