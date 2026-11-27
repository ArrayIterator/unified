#!/usr/bin/env python3
"""Build assets/icons/submodules-sprite.svg from per-sub-module icon
files in assets/icons/submodules/.

Sub-modules are the Rust `mod` declarations inside the module roots
(`src/modules/<module>.rs`, plus a few deeper declarations) - 582
distinct names, one icon each, keyed by the sub-module name
(`submodules/<name>.svg`). Names that share a concept share an icon;
see docs/integration/SUBMODULE_ICONS.md for the definition and the
full name -> Tabler icon mapping.

One <symbol id="submodule-icon-<name>" viewBox="0 0 24 24"> per icon.
Icons are Tabler 2.11.0 outline icons (MIT - see
assets/icons/LICENSE.tabler), 24x24 grid, stroke width 2,
single-colour strokes that inherit the surrounding text colour
(`stroke="currentColor" fill="none"`, round caps and joins). Each
<symbol> carries the Tabler root attributes so shapes without an
explicit stroke inherit them; the leading no-op canvas rect
(`M0 0h24v24H0z`) is stripped.

Deterministic: symbols sorted by id. No network, no extra deps.
Discovery is by scanning (glob), never by a literal name list.
Set UNIFIED_ICON_ROOT to build from a different tree (used by tests).
"""
import glob
import os
import re
import xml.etree.ElementTree as ET
from pathlib import Path

NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)
ROOT = Path(os.environ.get("UNIFIED_ICON_ROOT", "/home/hatch/workspace/unified-push"))
SRC_DIR = ROOT / "assets/icons/submodules"
OUT = ROOT / "assets/icons/submodules-sprite.svg"

# Tabler outline icons: strokes inherit the surrounding text colour.
SYMBOL_ATTRS = {
    "stroke": "currentColor",
    "fill": "none",
    "stroke-width": "2",
    "stroke-linecap": "round",
    "stroke-linejoin": "round",
}

# The no-op canvas rect every Tabler icon carries as its first path.
CANVAS_PATH_D = "M0 0h24v24H0z"


def build() -> None:
    symbols = []
    for path in sorted(glob.glob(str(SRC_DIR / "*.svg"))):
        name = Path(path).stem
        tree = ET.parse(path)
        root = tree.getroot()
        assert root.get("viewBox") == "0 0 24 24", f"{path}: bad viewBox"
        attrs = {"id": f"submodule-icon-{name}", "viewBox": "0 0 24 24"}
        attrs.update(SYMBOL_ATTRS)
        sym = ET.Element(f"{{{NS}}}symbol", attrs)
        for child in list(root):
            if (
                child.tag == f"{{{NS}}}path"
                and child.get("d") == CANVAS_PATH_D
            ):
                continue
            sym.append(child)
        symbols.append(sym)

    assert len(symbols) == len(set(s.get("id") for s in symbols)), "duplicate symbol ids"
    assert all(
        re.fullmatch(r"submodule-icon-[a-z0-9_]+", s.get("id")) for s in symbols
    ), "bad id pattern"

    svg = ET.Element(f"{{{NS}}}svg")
    svg.extend(symbols)
    ET.ElementTree(svg).write(OUT, encoding="utf-8", xml_declaration=True)
    print(f"wrote {OUT} with {len(symbols)} symbols")


if __name__ == "__main__":
    build()
