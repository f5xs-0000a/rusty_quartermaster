#!/usr/bin/env python3
"""Read the gem price guide off a saved copy of https://yppedia.puzzlepirates.com/Gem
and write it into the bare cache's islands.

    python3 scripts/extract_gems.py path/to/Gem.html --into src/data/bare_cache.json

The page has one price table per ocean: a row per island with a market and
a column per gem type. `BUY` means the island's palace buys the gem for the
full 1000 PoE apiece (it is the gem's capital destination); the other cells
are spawn prices or unknowns, which are not recorded here. An island that
buys gets a `buys_gems` list of their names; one that buys nothing has no
such key, since an absent purchase is not knowledge. Oceans or islands
missing from the bare cache are reported, not added.
"""

import argparse
import html
import json
import re
import sys


def cells(row):
    return [
        html.unescape(re.sub(r"<[^>]+>", "", c)).strip()
        for c in re.findall(r"<t[hd][^>]*>(.*?)</t[hd]>", row, flags=re.S)
    ]


def price_tables(page):
    """(ocean name, header cells, data rows) for every per-ocean price table.
    Each sits under an `<h3>` with the ocean's name and starts with an
    `Island` header cell."""
    page = re.sub(r"<script.*?</script>|<style.*?</style>", "", page, flags=re.S)
    found = []
    for section in re.split(r"<h3[^>]*>", page)[1:]:
        title = re.sub(r"<[^>]+>", "", section.split("</h3>", 1)[0]).strip()
        for table in re.findall(r"<table.*?</table>", section, flags=re.S):
            rows = [cells(r) for r in re.findall(r"<tr.*?</tr>", table, flags=re.S)]
            rows = [r for r in rows if r]
            if rows and rows[0][0] == "Island":
                found.append((title, rows[0][1:], rows[1:]))
    return found


def bought_gems(header, row):
    return [gem for gem, value in zip(header, row[1:]) if value == "BUY"]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("html", help="saved yppedia Gem page")
    ap.add_argument("--into", required=True, metavar="BARE_JSON")
    args = ap.parse_args()

    with open(args.html, encoding="utf-8", errors="replace") as f:
        tables = price_tables(f.read())
    with open(args.into, encoding="utf-8") as f:
        bare = json.load(f)

    for ocean_name, header, rows in tables:
        ocean = next((o for o in bare["oceans"] if o["name"].lower() == ocean_name.lower()), None)
        if ocean is None:
            print(f"{ocean_name}: not in the bare cache, skipped", file=sys.stderr)
            continue
        islands = {i["name"].lower(): i for a in ocean["archipelagos"] for i in a["islands"]}
        if not islands:
            print(f"{ocean_name}: no geography in the bare cache yet, skipped", file=sys.stderr)
            continue
        written = 0
        for row in rows:
            # the header is repeated as the table's last row
            if row[0] == "Island":
                continue
            island = islands.get(row[0].lower())
            if island is None:
                print(f"{ocean_name}: {row[0]!r} is not in the bare cache", file=sys.stderr)
                continue
            gems = bought_gems(header, row)
            if gems:
                island["buys_gems"] = gems
                written += 1
            else:
                island.pop("buys_gems", None)
        print(f"{ocean_name}: {written} islands buy gems", file=sys.stderr)

    with open(args.into, "w", encoding="utf-8") as f:
        json.dump(bare, f, indent=2, ensure_ascii=False)
        f.write("\n")


if __name__ == "__main__":
    main()
