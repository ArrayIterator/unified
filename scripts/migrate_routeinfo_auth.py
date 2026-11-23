#!/usr/bin/env python3
"""Migrate `RouteInfo { ... }` literals to state the new `auth` field.

Adds `auth: RouteAuth::Public` or `auth: RouteAuth::Authenticated`
to every `RouteInfo` struct literal in the tree (inspection C1,
2026-11-22). The value is derived from the literal's own data:

* `Public` when the description starts with `PUBLIC:`,
  `TOKEN-GATED:` or `TOKEN-AUTHENTICATED:` (the modules' own
  public-surface markers), starts with `Public versioned gateway`
  or `Public home page`, contains `webhook receiver` or
  `Receive a provider webhook (signature verified first)`
  (provider webhook receivers verify their own HMAC/shared-secret
  signature instead of a JWT), contains `public storefront` or
  `public when allowed` (public storefront reads), when the path
  contains a `/public/` segment (the tree-wide convention for the
  module-declared public surface, all gated by the API Manager
  `ApiAccessRule`), when the owner is `frontend` (the public
  pages: `/`, `/login`, `/register`, `/forgot-password`,
  `/reset-password`, `/logout`), or when the route is one of the
  auth-bootstrap endpoints below.
* `Authenticated` (fail closed) for everything else, including
  every store product/coupon/tier/order mutation route, mail
  `PUT /settings`, and the audit log reads (inspection C2).

Auth-bootstrap endpoints (no token can exist yet, so no JWT is
possible; each is a credential flow in its own right):
  POST /api/v1/auth-manager/jwt/issue
  POST /api/v1/auth-manager/jwt/verify
  GET  /api/v1/auth-manager/google/authorize
  POST /api/v1/auth-manager/google/callback
  GET  /api/v1/auth-manager/google/sso/status
  POST /api/v1/auth-manager/recovery-codes/verify

Usage:
  migrate_routeinfo_auth.py [--check] [--root DIR] [FILE ...]
  --check   report only, change nothing.
  With no FILE arguments, walks <root>/src and <root>/crates for
  *.rs files. Files modified in the last 60 minutes are skipped
  (another agent may be editing them) and reported; re-run later
  for those — until they are migrated the tree does not compile
  (E0063: missing field `auth`), so the skip list is the follow-up.

The scanner is string-aware (path params like `{id}` sit inside
string literals) and the edit is idempotent: a literal that
already states `auth:` is left alone.
"""

import os
import re
import sys
import time

AUTH_BOOTSTRAP = {
    ("POST", "/api/v1/auth-manager/jwt/issue"),
    ("POST", "/api/v1/auth-manager/jwt/verify"),
    ("GET", "/api/v1/auth-manager/google/authorize"),
    ("POST", "/api/v1/auth-manager/google/callback"),
    ("GET", "/api/v1/auth-manager/google/sso/status"),
    ("POST", "/api/v1/auth-manager/recovery-codes/verify"),
}

FIELD_RE = re.compile(
    r'(method|path|owner|description)\s*:\s*"((?:[^"\\]|\\.)*)"'
)
AUTH_FIELD_RE = re.compile(r'\bauth\s*:')


def is_public(method, path, owner, description):
    """Derive RouteAuth::Public from the literal's own data."""
    if description.startswith(
        ("PUBLIC:", "TOKEN-GATED:", "TOKEN-AUTHENTICATED:")
    ):
        return True
    if description.startswith(
        ("Public versioned gateway", "Public home page")
    ):
        return True
    if owner == "frontend":
        return True
    if "webhook receiver" in description:
        return True
    if "Receive a provider webhook (signature verified first)" in description:
        return True
    if "public storefront" in description or "public when allowed" in description:
        return True
    if "/public/" in path:
        return True
    if (method, path) in AUTH_BOOTSTRAP:
        return True
    return False


def find_literals(src):
    """Yield (start, end) spans of `RouteInfo { ... }` literals.

    String-aware: braces inside string literals (e.g. `{id}` in
    paths) do not affect depth. Escaped quotes inside strings are
    honoured. Returns spans covering `RouteInfo {` through the
    matching `}`.
    """
    spans = []
    needle = "RouteInfo {"
    i = 0
    while True:
        i = src.find(needle, i)
        if i < 0:
            break
        j = src.find("{", i)
        depth = 0
        k = j
        in_str = False
        escaped = False
        while k < len(src):
            ch = src[k]
            if in_str:
                if escaped:
                    escaped = False
                elif ch == "\\":
                    escaped = True
                elif ch == '"':
                    in_str = False
            else:
                if ch == '"':
                    in_str = True
                elif ch == "{":
                    depth += 1
                elif ch == "}":
                    depth -= 1
                    if depth == 0:
                        break
            k += 1
        if depth != 0:
            raise ValueError(f"unbalanced RouteInfo literal at offset {i}")
        spans.append((i, k + 1))
        i = k + 1
    return spans


def migrate_literal(text):
    """Return (new_text, auth_value, reason) or None when already migrated."""
    if AUTH_FIELD_RE.search(text):
        return None
    fields = dict(FIELD_RE.findall(text))
    if set(fields) != {"method", "path", "owner", "description"}:
        raise ValueError(f"cannot parse fields in literal: {text[:120]!r}")
    public = is_public(
        fields["method"], fields["path"], fields["owner"], fields["description"]
    )
    value = "unified_core::auth::RouteAuth::Public" if public else "unified_core::auth::RouteAuth::Authenticated"
    inner = text[len("RouteInfo {") : -1]  # between braces
    if "\n" in inner:
        # Multi-line: fields end with a comma; insert a new field
        # line before the closing brace, matching field indent.
        m_close = re.search(r"\n([ \t]*)$", inner)
        close_indent = m_close.group(1) if m_close else ""
        m_field = re.search(r"\n([ \t]*)\w+\s*:", inner)
        field_indent = m_field.group(1) if m_field else close_indent
        stripped = inner.rstrip()
        if not stripped.endswith(","):
            stripped += ","
        new_inner = (
            stripped
            + "\n"
            + field_indent
            + f"auth: {value},"
            + "\n"
            + close_indent
        )
    else:
        stripped = inner.rstrip()
        if stripped.endswith(","):
            new_inner = stripped + f" auth: {value}"
        else:
            new_inner = stripped + f", auth: {value}"
    return "RouteInfo {" + new_inner + "}", value, (
        "public" if public else "authenticated"
    )


def migrate_file(path, check=False):
    """Migrate one file. Returns (literals, inserted, public_count, skipped_reason)."""
    with open(path, encoding="utf-8") as f:
        src = f.read()
    spans = find_literals(src)
    if not spans:
        return 0, 0, 0, None
    out = []
    pos = 0
    inserted = 0
    public_count = 0
    for start, end in spans:
        result = migrate_literal(src[start:end])
        out.append(src[pos:start])
        if result is None:
            out.append(src[start:end])
        else:
            new_text, value, _ = result
            out.append(new_text)
            inserted += 1
            if value == "unified_core::auth::RouteAuth::Public":
                public_count += 1
        pos = end
    out.append(src[pos:])
    if inserted and not check:
        with open(path, "w", encoding="utf-8") as f:
            f.write("".join(out))
    return len(spans), inserted, public_count, None


def iter_rs_files(root):
    for base in ("src", "crates"):
        for dirpath, _dirnames, filenames in os.walk(os.path.join(root, base)):
            if "target" in dirpath.split(os.sep):
                continue
            for name in filenames:
                if name.endswith(".rs"):
                    yield os.path.join(dirpath, name)


def main(argv):
    check = "--check" in argv
    args = [a for a in argv if not a.startswith("--")]
    if args and os.path.isdir(args[0]):
        root = args[0]
        files = [a for a in args[1:] if a.endswith(".rs")]
    else:
        root = "."
        files = [a for a in args if a.endswith(".rs")]
    if not files:
        files = sorted(iter_rs_files(root))
    now = time.time()
    total_literals = total_inserted = total_public = 0
    skipped_hot = []
    changed = []
    errors = []
    for path in files:
        try:
            age_min = (now - os.path.getmtime(path)) / 60.0
        except OSError:
            continue
        if age_min < 60:
            skipped_hot.append((path, age_min))
            continue
        try:
            literals, inserted, public_count, _ = migrate_file(path, check=check)
        except ValueError as exc:
            errors.append((path, str(exc)))
            continue
        total_literals += literals
        total_inserted += inserted
        total_public += public_count
        if inserted:
            changed.append((path, inserted, public_count))
    print(f"files scanned: {len(files)}")
    print(f"RouteInfo literals: {total_literals}")
    print(f"auth fields inserted: {total_inserted} (public: {total_public})")
    print(f"files changed: {len(changed)}")
    if errors:
        print(f"ERRORS: {len(errors)}")
        for path, exc in errors:
            print(f"  ERROR {path}: {exc}", file=sys.stderr)
    if skipped_hot:
        print(f"SKIPPED (modified <60 min ago): {len(skipped_hot)}")
        for path, age in sorted(skipped_hot):
            print(f"  {age:.0f}m ago  {path}")
    if check:
        print("(check mode: nothing written)")
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
