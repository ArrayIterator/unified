#!/usr/bin/env python3
"""Build src/modules/email_providers/logos/sprite.svg from the per-provider
logo files in the same directory.

One <symbol id="provider-logo-<stem>" viewBox="0 0 24 24"> per logo,
where <stem> is the logo filename without the extension (for example
`email-gmail`). Logos are fill-only (flat solid fills, no stroke, at
most two colours: var(--icon-primary, #brand) + var(--icon-secondary,
#fff)), so each <symbol> carries only fill="none" — a shape without
an explicit fill renders invisible instead of a black blob (same rule
as scripts/build-module-icons-sprite.py).

Deterministic: symbols sorted by id. No network, no extra deps.
Discovery is by scanning the logos directory (glob), never by a
literal provider list: adding a logo file and re-running this script
is enough to ship it. Set UNIFIED_ICON_ROOT to build from a
different tree (used by tests).

Re-run after adding or changing a logo:
  python3 scripts/build-provider-logos-sprite.py
"""
import glob
import os
import xml.etree.ElementTree as ET
from pathlib import Path

NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)
ROOT = Path(os.environ.get("UNIFIED_ICON_ROOT", "/home/hatch/workspace/unified-push"))
SRC_DIR = ROOT / "src/modules/email_providers/logos"
OUT = SRC_DIR / "sprite.svg"

# Fill-only logos: no stroke anywhere. fill="none" on the symbol means a
# shape that forgot its explicit fill renders invisible (caught by the
# visual render check) instead of a black blob.
SYMBOL_ATTRS = {"fill": "none"}


def collect() -> dict[str, Path]:
    """Map logo stem -> source file, by scanning the logos directory."""
    found: dict[str, Path] = {}
    for path in sorted(glob.glob(str(SRC_DIR / "email-*.svg"))):
        name = Path(path).stem
        found[name] = Path(path)
    return found


def symbol_for(name: str, path: Path) -> ET.Element:
    tree = ET.parse(path)
    root = tree.getroot()
    symbol = ET.Element(f"{{{NS}}}symbol", {"id": f"provider-logo-{name}", "viewBox": "0 0 24 24"})
    symbol.attrib.update(SYMBOL_ATTRS)
    for child in root:
        symbol.append(child)
    return symbol


def main() -> None:
    logos = collect()
    assert logos, f"no provider logos found in {SRC_DIR}"
    sprite = ET.Element(f"{{{NS}}}svg")
    for name in sorted(logos):
        sprite.append(symbol_for(name, logos[name]))
    ET.indent(sprite, space="  ")
    tree = ET.ElementTree(sprite)
    tree.write(OUT, encoding="utf-8", xml_declaration=True)
    print(f"wrote {OUT} ({len(logos)} symbols)")


if __name__ == "__main__":
    main()
