#!/usr/bin/env python3
"""Build a provider/brand logo sprite from per-logo SVG files.

Default (no arguments) builds the email-provider sprite:
  python3 scripts/build-provider-logos-sprite.py
  -> src/modules/email_providers/logos/sprite.svg

Other collections use the same pattern:
  python3 scripts/build-provider-logos-sprite.py \
      --src src/modules/payments/logos --glob 'payment-*.svg'
  python3 scripts/build-provider-logos-sprite.py \
      --src assets/icons/brands --glob 'brand-*.svg' --prefix '' \
      --out assets/icons/brands-sprite.svg

One <symbol id="<prefix><stem>" viewBox="0 0 24 24"> per logo, where
<stem> is the logo filename without the extension (for example
`email-gmail`, `payment-paypal`, `brand-google`). Logos are fill-only
(flat solid fills, no stroke, at most two colours:
var(--icon-primary, #brand) + var(--icon-secondary, #fff)), so each
<symbol> carries only fill="none" - a shape without an explicit fill
renders invisible instead of a black blob (same rule as
scripts/build-module-icons-sprite.py).

Deterministic: symbols sorted by id. No network, no extra deps.
Discovery is by scanning the logos directory (glob), never by a
literal provider list: adding a logo file and re-running this script
is enough to ship it. Set UNIFIED_ICON_ROOT to build from a
different tree (used by tests).

Re-run after adding or changing a logo.
"""
import argparse
import glob
import os
import xml.etree.ElementTree as ET
from pathlib import Path

NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)
ROOT = Path(os.environ.get("UNIFIED_ICON_ROOT", "/home/hatch/workspace/unified-push"))

# Fill-only logos: no stroke anywhere. fill="none" on the symbol means a
# shape that forgot its explicit fill renders invisible (caught by the
# visual render check) instead of a black blob.
SYMBOL_ATTRS = {"fill": "none"}


def collect(src_dir: Path, pattern: str) -> dict[str, Path]:
    """Map logo stem -> source file, by scanning the logos directory."""
    found: dict[str, Path] = {}
    for path in sorted(glob.glob(str(src_dir / pattern))):
        name = Path(path).stem
        found[name] = Path(path)
    return found


def symbol_for(name: str, path: Path, prefix: str) -> ET.Element:
    tree = ET.parse(path)
    root = tree.getroot()
    symbol = ET.Element(f"{{{NS}}}symbol", {"id": f"{prefix}{name}", "viewBox": "0 0 24 24"})
    symbol.attrib.update(SYMBOL_ATTRS)
    for child in root:
        symbol.append(child)
    return symbol


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--src",
        default="src/modules/email_providers/logos",
        help="directory holding the logo SVGs (default: email provider logos)",
    )
    parser.add_argument(
        "--glob", default="email-*.svg", help="filename glob (default: email-*.svg)"
    )
    parser.add_argument(
        "--prefix",
        default="provider-logo-",
        help="symbol id prefix (default: provider-logo-; use '' for bare stems)",
    )
    parser.add_argument(
        "--out",
        default=None,
        help="output sprite path (default: <src>/sprite.svg)",
    )
    args = parser.parse_args()

    src_dir = ROOT / args.src
    out = Path(args.out) if args.out else src_dir / "sprite.svg"
    if args.out is not None and not Path(args.out).is_absolute():
        out = ROOT / args.out
    logos = collect(src_dir, args.glob)
    assert logos, f"no logos found in {src_dir} matching {args.glob}"
    sprite = ET.Element(f"{{{NS}}}svg")
    for name in sorted(logos):
        sprite.append(symbol_for(name, logos[name], args.prefix))
    ET.indent(sprite, space="  ")
    tree = ET.ElementTree(sprite)
    tree.write(out, encoding="utf-8", xml_declaration=True)
    print(f"wrote {out} ({len(logos)} symbols)")


if __name__ == "__main__":
    main()
