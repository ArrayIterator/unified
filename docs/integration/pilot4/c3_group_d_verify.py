#!/usr/bin/env python3
"""C3 pilot 4 group D verification: field_service, admissions,
privacy, recruitment, newsletter, notifications, bank_transfer, storage.

For each module, per routed handler (names appearing in `.to(name)` in
the module):
  - guarded: body contains `require_tenant(&req)` AND
    `caller_company(&_tenant, &req)` (gate-only idiom: gate first, then
    verified-caller company resolution for the mismatch 403)

These modules' records carry no company
field, so under carve-out 3 their writes stay tenant-gate-only
(the strict `require_company_for_write` gate does not apply here).
The self-service writes (admissions submit/withdraw, privacy
consent grant/withdraw, own notifications) are carve-out 1 and
also keep `caller_company`.
  - order: the guard precedes the first identity/capability call
  - excluded: names passed on the command line after the module

Usage: c3_group_d_verify.py <module> [excluded ...]
Exit code 0 only when missing == 0 and order violations == 0.
"""
import os
import re
import sys

ROOT = "/home/hatch/workspace/unified-push/src/modules"

IDENTITY_CALLS = (
    "caller(&req)", "authorize(", "require_permission(",
    "check_capability(", "authenticate(", "require_admin(",
    "can_manage(", "require_verification_owner",
    "verification_visible_to_caller", "admin_caller(",
)


def brace_end(src, i):
    depth = 0
    while i < len(src):
        c = src[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return len(src)


def main():
    mod = sys.argv[1]
    excl = set(sys.argv[2:])
    mdir = os.path.join(ROOT, mod)
    routed = set()
    for f in os.listdir(mdir):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        routed |= set(re.findall(r"\.to\((\w+)\)", src))
    root = os.path.join(ROOT, mod + ".rs")
    if os.path.exists(root):
        src = open(root, encoding="utf-8", errors="replace").read()
        routed |= set(re.findall(r"\.to\((\w+)\)", src))
    missing, ok, excluded, order_bad, no_company = [], [], [], [], []
    for f in sorted(os.listdir(mdir)):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        test_idx = src.find("#[cfg(test)]")
        head = src if test_idx < 0 else src[:test_idx]
        for m in re.finditer(r"pub\(crate\) async fn (\w+)\s*\(", head):
            name = m.group(1)
            if name not in routed:
                continue
            j = head.find("{", m.end())
            body = head[j: brace_end(head, j)]
            if name in excl:
                excluded.append(f"{f}:{name}")
                continue
            if "require_tenant(&req)" not in body:
                missing.append(f"{f}:{name}")
                continue
            ok.append(f"{f}:{name}")
            if "caller_company(&_tenant, &req)" not in body:
                no_company.append(f"{f}:{name}")
            guard_at = body.find("require_tenant(&req)")
            first_identity = min(
                (body.find(call) for call in IDENTITY_CALLS if body.find(call) != -1),
                default=-1,
            )
            if first_identity != -1 and first_identity < guard_at:
                order_bad.append(f"{f}:{name}")
    print(
        f"{mod}: routed={len(routed)} guarded={len(ok)} "
        f"missing={len(missing)} excluded={len(excluded)} "
        f"order_violations={len(order_bad)} no_caller_company={len(no_company)}"
    )
    for x in missing:
        print("  MISSING:", x)
    for x in order_bad:
        print("  ORDER:", x)
    for x in no_company:
        print("  NO_COMPANY:", x)
    for x in excluded:
        print("  EXCL:", x)
    return 1 if missing or order_bad or no_company else 0


if __name__ == "__main__":
    sys.exit(main())
