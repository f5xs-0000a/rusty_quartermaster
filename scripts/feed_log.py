#!/usr/bin/env python3
"""Interactive chat-log feeder for ypp_quartermaster.

Replays a source chat-log into a *target* file one line at a time, on each
Enter press, so you can drive the app's live tailer by hand and watch the
fight-driven auto-navigation react line by line.

Why a regular file and not a FIFO: the app tails the target by byte offset +
file length (see `spawn_tailer` in src/chatlog.rs) — it seeks past what it has
already read and slurps the new bytes. A real named pipe (mkfifo) always reports
length 0, so the offset tailer would never see any data, and the startup
whole-file read (`std::fs::read`) would block until EOF. Appending to a regular
file behaves exactly like a live game log growing, which is what the app expects.

Usage:
    scripts/feed_log.py SOURCE TARGET

Then point the app at TARGET (launch it before or after — either works):
    cargo run -- --ocean Emerald --user Playerone TARGET

At the prompt:
    <Enter>   feed the next line
    N <Enter> feed the next N lines
    a <Enter> feed all remaining lines
    q <Enter> quit
"""
import sys


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(1)
    source, target = sys.argv[1], sys.argv[2]

    with open(source, "r", encoding="utf-8", errors="replace") as f:
        lines = [line.rstrip("\r\n") for line in f]

    # Start the target empty so the app tails from a clean slate. If the app is
    # already running and pointed here, the tailer detects the shrink and resets
    # its offset to 0 (it's defensive about replaced/truncated files).
    open(target, "w").close()

    total = len(lines)
    print(f"Loaded {total} line(s) from {source}")
    print(f"Feeding into {target} (truncated). Point the app at it.")
    print("Enter=next, N=next N, a=all, q=quit\n")

    i = 0
    while i < total:
        try:
            cmd = input(f"[{i}/{total}] > ").strip()
        except EOFError:
            print()
            break

        if cmd in ("q", "quit"):
            break

        if cmd in ("a", "all"):
            count = total - i
        elif cmd == "":
            count = 1
        elif cmd.isdigit():
            count = max(1, int(cmd))
        else:
            print("  ? Enter=next, N=next N, a=all, q=quit")
            continue

        count = min(count, total - i)
        # One append + flush per line so the 300ms-poll tailer can pick each up.
        with open(target, "a", encoding="utf-8") as out:
            for _ in range(count):
                out.write(lines[i] + "\n")
                out.flush()
                print(f"  fed [{i}] {lines[i]}")
                i += 1

    print(f"\nDone: fed {i}/{total} line(s).")


if __name__ == "__main__":
    main()
