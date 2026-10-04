#!/usr/bin/env python3
"""Extract an ocean's map (islands, archipelago labels, leagues) from a saved
yppedia map template page into a `src/data/maps/<ocean>.json` file, which
build.rs compiles into the binary as static data for the Map app.

Download the template page for an ocean, e.g.
https://yppedia.puzzlepirates.com/Template:Map:Emerald_Ocean, with the browser
("save page, complete" is fine - only the HTML is read), then:

    python3 scripts/extract_map.py path/to/page.html Emerald \\
        > src/data/maps/emerald.json

Any number of oceans can be added this way; the file name is free, the
`ocean` field inside is what the app matches against the selected ocean.

The page draws the map as absolutely positioned divs on a 24px grid: an island
or archipelago label is a div at `left:Xpx;top:Ypx` holding a link, and every
league is a div holding one of seven tile images:

    Map_A  solid diagonal, (x,y) down-right to (x+1,y+1)
    Map_B  dotted diagonal, same geometry
    Map_S  solid diagonal, (x,y+1) up-right to (x+1,y)
    Map_T  dotted diagonal, same geometry
    Map_V  solid horizontal, (x,y) to (x+2,y)
    Map_W  dotted horizontal, same geometry
    Map_O  the league-difficulty colour ruler at the top; not part of the sea

Solid tiles come from the wiki's `Chart_league_solid` template and dotted ones
from `Chart_league`: a solid league lies on a route whose chart is sold in
game, a dotted one's chart is not sold and only drops as booty. Each league
is emitted as
`"x,y dir kind"` where `dir` is the heading from the named grid cell (`e`,
`se`, `ne`) and `kind` is `solid` or `dotted`.

The templates only draw the leagues that charted routes follow, so the file
this writes is a transcript of the wiki and not the whole league graph: a pair
of points a single league apart with no chart between them is sailable all the
same, and `build.rs` fills those in from the geometry when it compiles the map.
"""

import argparse
import json
import re
import sys
from html.parser import HTMLParser

GRID = 24

# tile letter -> (heading, kind); the origin cell is the tile's own cell for
# `e` and `se`, and the cell below it for `ne` (the tile's bottom-left corner)
TILES = {
    "A": ("se", "solid"),
    "B": ("se", "dotted"),
    "S": ("ne", "solid"),
    "T": ("ne", "dotted"),
    "V": ("e", "solid"),
    "W": ("e", "dotted"),
}


def far(origin, heading):
    """The point a league away from `origin` along `heading`."""
    x, y = origin
    return {"e": (x + 2, y), "se": (x + 1, y + 1), "ne": (x + 1, y - 1)}[heading]


def style_px(style, key):
    m = re.search(r"(?:^|;)\s*" + key + r"\s*:\s*(-?\d+)(?:px)?", style or "")
    return int(m.group(1)) if m else None


class MapParser(HTMLParser):
    """Collects the top-level positioned divs of the map: a div with a z-index
    whose parent has none is one map element (island label, league, ruler)."""

    def __init__(self):
        super().__init__()
        self.stack = []
        self.islands = []
        self.labels = []
        self.leagues = set()

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "div":
            st = a.get("style", "")
            self.stack.append(
                {
                    "left": style_px(st, "left"),
                    "top": style_px(st, "top"),
                    "z": style_px(st, "z-index"),
                    "tiles": [],
                    "links": [],
                }
            )
        elif tag == "img" and self.stack:
            m = re.search(r"Map_([A-Z])[_.]", a.get("src", ""))
            if m:
                for d in self.stack:
                    d["tiles"].append(m.group(1))
        elif tag == "a" and self.stack:
            # page links carry a title ("Cryo Island (Emerald)"); the image
            # links wrapping the tiles and icons don't
            title = a.get("title")
            if title:
                for d in self.stack:
                    d["links"].append(title)

    def handle_endtag(self, tag):
        if tag != "div" or not self.stack:
            return
        d = self.stack.pop()
        parent = self.stack[-1] if self.stack else None
        if d["z"] is None or (parent is not None and parent["z"] is not None):
            return
        if d["left"] is None or d["top"] is None:
            return
        if d["left"] % GRID or d["top"] % GRID:
            sys.exit(f"element off the {GRID}px grid at {d['left']},{d['top']}")
        x, y = d["left"] // GRID, d["top"] // GRID
        if d["links"]:
            # "Cryo Island (Emerald)" -> "Cryo Island"
            name = re.sub(r"\s*\([^)]*\)\s*$", "", d["links"][0])
            if name.endswith(" Archipelago"):
                self.labels.append({"name": name[: -len(" Archipelago")], "x": x, "y": y})
            elif not name.endswith(" Ocean"):
                # some maps caption themselves with the ocean's own name,
                # which is decoration rather than a place on the water
                self.islands.append({"name": name, "x": x, "y": y})
        for t in d["tiles"]:
            if t not in TILES:
                continue
            heading, kind = TILES[t]
            origin = (x, y + 1) if heading == "ne" else (x, y)
            self.leagues.add((origin, heading, kind))


def extract(html):
    p = MapParser()
    p.feed(html)
    if not p.islands:
        sys.exit("no islands found - is this a yppedia Template:Map page?")
    # every island must sit on a league endpoint, or the cursor can't reach it
    ends = set()
    for origin, heading, _ in p.leagues:
        ends.add(origin)
        ends.add(far(origin, heading))
    stranded = [i["name"] for i in p.islands if (i["x"], i["y"]) not in ends]
    if stranded:
        print(f"warning: islands with no league: {stranded}", file=sys.stderr)
    # a league drawn twice with different tiles (one solid, one dotted) is a
    # wiki authoring slip; keep the solid one, since a chart that exists is the
    # stronger claim, and say so
    by_edge = {}
    for origin, heading, kind in sorted(p.leagues, key=lambda l: l[2] != "solid"):
        if (origin, heading) in by_edge:
            print(f"warning: {origin} {heading} drawn both solid and dotted; keeping solid", file=sys.stderr)
            continue
        by_edge[(origin, heading)] = kind
    p.leagues = {(o, h, k) for (o, h), k in by_edge.items()}
    key = lambda e: (e["y"], e["x"], e["name"])
    return {
        "islands": sorted(p.islands, key=key),
        "labels": sorted(p.labels, key=key),
        "leagues": [
            f"{x},{y} {heading} {kind}"
            for (x, y), heading, kind in sorted(p.leagues, key=lambda l: (l[0][1], l[0][0], l[1]))
        ],
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("html", help="saved Template:Map:<Ocean>_Ocean page")
    ap.add_argument("ocean", help="ocean name as the app spells it, e.g. Emerald")
    args = ap.parse_args()

    with open(args.html, encoding="utf-8", errors="replace") as f:
        ocean_map = extract(f.read())
    ocean_map = {
        "ocean": args.ocean,
        "source": f"https://yppedia.puzzlepirates.com/Template:Map:{args.ocean}_Ocean",
        **ocean_map,
    }
    json.dump(ocean_map, sys.stdout, indent=2, ensure_ascii=False)
    print()
    print(
        f"{args.ocean}: {len(ocean_map['islands'])} islands, "
        f"{len(ocean_map['labels'])} labels, {len(ocean_map['leagues'])} leagues",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
