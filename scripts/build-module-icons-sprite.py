#!/usr/bin/env python3
"""Build assets/icons/modules-sprite.svg from per-module icon.svg files.

Icon lookup order (the icon belongs to the module):
  1. module-owned: <module>/icon.svg alongside the module code
     (src/modules/<name>/icon.svg, crates/external-*/icon.svg),
  2. central: assets/icons/modules/<name>.svg (override / fallback store),
  3. placeholder: assets/icons/modules/placeholder.svg, used when a
     module has no icon yet (its symbol is always built).

Discovery is by scanning (glob), never by a literal module list: the
icon set is the union of central filenames and discovered module-owned
icon.svg owners. A module-owned icon overrides the central file of
the same name; the build prints every override so it is visible.

One <symbol id="module-icon-<module>" viewBox="0 0 24 24"> per icon.
Icons are Tabler 2.11.0 outline icons (ColorTicons, MIT — see
assets/icons/LICENSE.colorticons), 24x24 grid, stroke width 2,
single-colour strokes that inherit the surrounding text colour
(`stroke="currentColor" fill="none"`, round caps and joins). Each
<symbol> carries the Tabler root attributes so shapes without an
explicit stroke inherit them; the leading no-op canvas rect
(`M0 0h24v24H0z`) is stripped. Because strokes inherit text colour,
the icons adapt to light and dark themes automatically — there is no
second dark sprite. A per-module data-animation="spin|wiggle|pulse|
bounce" attribute on the icon file's <svg> root is copied onto the
<symbol> (see assets/icons/ANIMATION.md).

Deterministic: symbols sorted by id. No network, no extra deps.
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
SRC_DIR = ROOT / "assets/icons/modules"
OUT = ROOT / "assets/icons/modules-sprite.svg"

# Module-owned icon locations, discovered by scanning (glob patterns,
# never a literal module list).
OWNED_PATTERNS = [
    str(ROOT / "src" / "modules" / "*" / "icon.svg"),
    str(ROOT / "crates" / "external-*" / "icon.svg"),
]

ANIMATIONS = {"spin", "wiggle", "pulse", "bounce"}

# Tabler outline icons: strokes inherit the surrounding text colour.
# The symbol carries the Tabler root attributes so a shape without an
# explicit stroke still renders (caught by the visual render check
# if it renders wrong) instead of a black blob.
SYMBOL_ATTRS = {
    "stroke": "currentColor",
    "fill": "none",
    "stroke-width": "2",
    "stroke-linecap": "round",
    "stroke-linejoin": "round",
}

# The no-op canvas rect every Tabler icon carries as its first path.
CANVAS_PATH_D = "M0 0h24v24H0z"


def collect() -> dict[str, tuple[Path, str]]:
    """Map icon name -> (source file, origin)."""
    found: dict[str, tuple[Path, str]] = {}
    for path in sorted(glob.glob(str(SRC_DIR / "*.svg"))):
        name = Path(path).stem
        found[name] = (Path(path), "central")
    for pattern in OWNED_PATTERNS:
        for path in sorted(glob.glob(pattern)):
            name = Path(path).parent.name
            if name in found:
                print(f"override: {name} uses module-owned {path}")
            found[name] = (Path(path), "module-owned")
    return found


def build() -> None:
    icons = collect()
    symbols = []
    for name in sorted(icons):
        path, _origin = icons[name]
        tree = ET.parse(path)
        root = tree.getroot()
        assert root.get("viewBox") == "0 0 24 24", f"{path}: bad viewBox"
        attrs = {"id": f"module-icon-{name}", "viewBox": "0 0 24 24"}
        attrs.update(SYMBOL_ATTRS)
        anim = root.get("data-animation")
        if anim is not None:
            assert anim in ANIMATIONS, f"{path}: bad data-animation {anim!r}"
            attrs["data-animation"] = anim
        sym = ET.Element(f"{{{NS}}}symbol", attrs)
        for child in list(root):
            # Strip the no-op canvas rect Tabler icons carry.
            if (
                child.tag == f"{{{NS}}}path"
                and child.get("d") == CANVAS_PATH_D
            ):
                continue
            sym.append(child)
        symbols.append(sym)

    assert len(symbols) == len(set(s.get("id") for s in symbols)), "duplicate symbol ids"
    assert all(
        re.fullmatch(r"module-icon-[a-z0-9-]+", s.get("id")) for s in symbols
    ), "bad id pattern"

    svg = ET.Element(f"{{{NS}}}svg")
    svg.extend(symbols)
    ET.ElementTree(svg).write(OUT, encoding="utf-8", xml_declaration=True)
    owned = sum(1 for _, origin in icons.values() if origin == "module-owned")
    print(f"wrote {OUT} with {len(symbols)} symbols ({owned} module-owned)")


if __name__ == "__main__":
    build()
