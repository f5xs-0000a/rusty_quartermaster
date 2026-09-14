#!/usr/bin/env bash
# Pipe a slice of a Puzzle Pirates chat log with the chatter stripped out, so
# only game-system lines remain -- handy for feeding the app's live tailer
# without wading through conversation.
#
#   usage: scripts/section.sh <log> <start-line> <end-line>
#   e.g.:  scripts/section.sh path/to/your_ypp_log.txt 12000 12200
#
# Straight to a file the app tails (dumps the whole slice at once):
#   scripts/section.sh <log> <start> <end> > /tmp/ypp_live.log
#
# Or feed it live, one line per Enter, via feed_log.py + process substitution:
#   python3 scripts/feed_log.py \
#     <(scripts/section.sh <log> <start> <end>) /tmp/ypp_live.log
set -u

LOG="${1:?usage: section.sh <log> <start-line> <end-line>}"
START="${2:?usage: section.sh <log> <start-line> <end-line>}"
END="${3:?usage: section.sh <log> <start-line> <end-line>}"

# Drop chat: any line carrying a speech verb token (says / <scope> chats /
# tells ye / shouts / broadcasts) or a quoted-message line (first or trailing),
# which always ends in a double quote. System lines end in `!` or `.`, so they
# survive.
sed -n "${START},${END}p" "$LOG" \
    | grep -vE ' (says|chats|tells ye|shouts|broadcasts),|"$' || true
