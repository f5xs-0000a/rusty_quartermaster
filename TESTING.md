# Testing

## Unit tests

```sh
cargo test
```

Runs the whole suite — chat-log parser, damage calculator, voyage stats, and
profit logic. No network or game client needed.

## Driving the live tailer by hand

When a chat log is attached, the app reads it in full at startup and then
**tails it live**, watching for appended lines. The live path powers Voyage
Statistics, the Jobbers live crew view, the fight-driven auto-navigation, and
the Damage calculator's **New battle** prompt.

Those live behaviors only fire for lines that arrive *after* startup (through
`GameState::feed_chat_line`), never during the initial whole-file read
(`process_existing`). To exercise them without playing the game, replay a log
into a target file one line at a time with `scripts/feed_log.py`.

### Why a regular file, not a FIFO

The tailer reads by byte offset + file length: it seeks past what it has already
read and slurps the newly-appended bytes (see `spawn_tailer` in
`src/chatlog.rs`). A named pipe (`mkfifo`) always reports length 0, so the
offset tailer would never see any data, and the startup `std::fs::read` would
block until EOF. Appending to a plain file behaves exactly like a live game log
growing, which is what the app expects.

### Picking a section of a real log

Feed a slice of your own chat log rather than a hand-written one. `scripts/`
holds two composable tools:

- `section.sh <log> <start-line> <end-line>` — prints that line range with the
  chatter stripped out (only game-system lines survive), so the feed is short
  and relevant.
- `feed_log.py <source> <target>` — feeds `source` into `target` one line per
  keypress.

In the commands below replace the placeholders with your own: `CHATLOG` (path
to your client chat log), `START`/`END` (line numbers), `OCEAN` and `PIRATE`.

Find the range you want first, e.g. a login boundary or a specific encounter:

```sh
grep -n '^======' CHATLOG                 # login lines
grep -n 'intercepted\|Black Ship' CHATLOG
```

### Steps

1. **Terminal A — start the feeder** on a chat-stripped slice (process
   substitution hands the filtered slice to `feed_log.py` as its source). It
   truncates the target and waits at a prompt.

   ```sh
   python3 scripts/feed_log.py \
     <(bash scripts/section.sh CHATLOG START END) \
     /tmp/ypp_live.log
   ```

2. **Terminal B — start the app on the target.** Pass `--ocean`/`--user`
   explicitly so the startup popup is skipped; no `--query-market`, so it
   runs offline.

   ```sh
   cargo run -- --ocean OCEAN --user PIRATE /tmp/ypp_live.log
   ```

3. **Back in Terminal A — feed lines:**

   | input        | effect                |
   | ------------ | --------------------- |
   | `<Enter>`    | feed the next line    |
   | `N <Enter>`  | feed the next N lines |
   | `a <Enter>`  | feed all remaining    |
   | `q <Enter>`  | quit                  |

   Feed up to an interception, watch the **New battle** prompt appear in the
   app, answer it (Apply / Keep), then continue.

For a quick, non-interactive check you can skip `feed_log.py` and dump a slice
straight into the target the app tails:

```sh
bash scripts/section.sh CHATLOG START END > /tmp/ypp_live.log
```

## Whole-file parse check (no UI)

To dump the parsed vessel/crew state for an entire log without the TUI — useful
for confirming a log parses at all — use the ignored dev test:

```sh
YPP_LOG=/path/to/chat.log YPP_USER=Yourpirate \
  cargo test ingest_real_log -- --ignored --nocapture
```

This uses `process_existing` (whole-file), so it does **not** exercise the live
prompt or auto-navigation — only the resulting parse state.
