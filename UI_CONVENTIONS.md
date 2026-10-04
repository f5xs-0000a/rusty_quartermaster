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

One thing padding did not fix: the Jobbers placeholder is pinned to the
top-left of its box and runs off the right edge mid-sentence at 80 columns
instead of wrapping, so padding it moved the cut one word earlier. How
placeholders should behave is not settled here yet.

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
| Jobbers | the placeholder runs off the right edge instead of wrapping |
