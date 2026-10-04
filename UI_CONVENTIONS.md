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

### Implementation

Use `utils::offset_title`, which returns the title string and the minimum
width that keeps it readable:

```rust
let (title, min_width) = offset_title("Inventory");
let block = Block::default().borders(Borders::ALL).title(title);
```

Never build the string by hand. The leading run's length lives in one place,
`utils::TITLE_DASHES`.

`utils::offset_title_width` is a `const fn`, so a widget can derive its layout
floor at compile time from the same source the title comes from and the two
can never drift:

```rust
const MIN_W: u16 = offset_title_width("Inventory");
```

The width it returns is `title.len() + 2 * TITLE_DASHES + 4`: the title, both
three-dash runs, both flanking spaces, and the two corners. It assumes an
ASCII title, where byte length equals column count, which all of ours are.

### Verifying

In any dump, a correct title matches `┌─── <title> ─` and a correct minimum
width shows equal runs on both sides. A title touching a corner, missing
either space, or with a leading run of any length other than three is a
violation.
