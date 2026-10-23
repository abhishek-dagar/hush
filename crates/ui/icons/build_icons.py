"""Builds icons.ttf, the chrome's icon font (spec 0003, Icons).

Takes the glyphs named in icons.toml from Lucide's own icon font (built by
Lucide from its SVGs, strokes already outlined), and gives each the
codepoint icons.toml assigns. Run it by hand after changing icons.toml;
the TTF is checked in, so the build never needs Python or the network.

    npm pack lucide-static@1.60.0 && tar xzf lucide-static-1.60.0.tgz
    python3 -m venv venv && venv/bin/pip install fonttools
    venv/bin/python crates/ui/icons/build_icons.py package

Lucide is ISC licensed; its notice (with the MIT notice for the icons it takes
from Feather) is in main/THIRD_PARTY_NOTICES.md.
"""

import json
import sys
import tomllib
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

HERE = Path(__file__).resolve().parent
FAMILY = "Chrome Icons"


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: build_icons.py <unpacked lucide-static package dir>")
    package = Path(sys.argv[1])
    lucide_codepoints = json.loads((package / "font" / "codepoints.json").read_text())
    icons = tomllib.loads((HERE / "icons.toml").read_text())["icons"]

    font = TTFont(package / "font" / "lucide.ttf")
    cmap = font.getBestCmap()
    ours = {}
    for name, icon in icons.items():
        lucide = lucide_codepoints.get(icon["lucide"])
        if lucide is None or lucide not in cmap:
            sys.exit(f"{name}: Lucide has no icon `{icon['lucide']}`")
        ours[icon["codepoint"]] = cmap[lucide]

    options = subset.Options()
    options.layout_features = []
    options.name_IDs = ["*"]
    options.notdef_outline = True
    subsetter = subset.Subsetter(options)
    subsetter.populate(glyphs=sorted(set(ours.values())))
    subsetter.subset(font)

    for table in font["cmap"].tables:
        if table.isUnicode():
            table.cmap = dict(ours)
    for record in font["name"].names:
        if record.nameID in (1, 4, 16):
            record.string = FAMILY
        elif record.nameID == 6:
            record.string = FAMILY.replace(" ", "")

    out = HERE.parent / "assets" / "icons.ttf"
    font.save(out)
    print(f"wrote {out} with {len(ours)} icons")


if __name__ == "__main__":
    main()
