#!/usr/bin/env python3
"""C3 pilot 3 group A verification (task 314): pos, pricing_tables,
loyalty, affiliates.

For each module, per routed handler (same routed-set rule as
/tmp/c3_verify.py: names appearing in `.to(name)` in the module):

Task 315/320 (2026-11-23): mutation handlers resolve the strict
write gate `require_company_for_write` and record writes use the
writable check `require_record_writable_in_company_scope*`; both
count as scope markers. Reads use rule A: `in_company_scope` shows
a global (NULL-company) row to any resolved scope.
  - guarded: body contains `require_tenant(&req)`
  - order: the guard precedes the first identity/capability call
    (`caller(&req)`, `require_permission(`, `check_capability(`)
  - scope-checked: body mentions a scope marker (list filter,
    record check, or body verify); otherwise gate-only
  - excluded: names passed on the command line after the module

Usage: c3_group_a_verify.py <module> [excluded ...]
Exit code 0 only when missing == 0 and order violations == 0.
"""
import os
import re
import sys

ROOT = "/home/hatch/workspace/unified-push/src/modules"

SCOPE_MARKERS = (
    "in_company_scope",
    "require_record_in_company_scope",
    "require_company_match",
    "require_sale_in_scope",
    "require_program_in_scope",
    "require_vendor_in_scope",
    "require_affiliate_in_scope",
    "require_commission_in_scope",
    "require_payout_in_scope",
    "require_product_approval_in_scope",
    "require_affiliate_commission_in_scope",
    "require_affiliate_payout_in_scope",
    "vendor_in_scope",
    # Shared loaders that enforce scope inside (not in the handler
    # body itself): pos sale-item handlers go through
    # load_editable_sale, which scope-checks the loaded sale.
    "load_editable_sale(",
    # Task 315/320 (2026-11-23): the strict write gate and the
    # writable record checks count as scope markers too.
    "require_company_for_write",
    "require_record_writable_in_company_scope",
    "require_record_writable_in_company_scope_str",
    "require_record_writable_in_company_scope_uuid",
    "require_note_writable_in_company_scope",
    "require_attempt_writable_in_scope",
    "require_exam_writable_by_id_in_scope",
    "require_risk_writable_in_scope",
)

IDENTITY_CALLS = ("caller(&req)", "require_permission(", "check_capability(")


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
    missing, ok, excluded, order_bad = [], [], [], []
    scope_checked, gate_only = [], []
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
            body = head[j : brace_end(head, j)]
            if name in excl:
                excluded.append(f"{f}:{name}")
                continue
            if "require_tenant(&req)" not in body:
                missing.append(f"{f}:{name}")
                continue
            ok.append(f"{f}:{name}")
            guard_at = body.find("require_tenant(&req)")
            first_identity = min(
                (body.find(call) for call in IDENTITY_CALLS if body.find(call) != -1),
                default=-1,
            )
            if first_identity != -1 and first_identity < guard_at:
                order_bad.append(f"{f}:{name}")
            if any(marker in body for marker in SCOPE_MARKERS):
                scope_checked.append(f"{f}:{name}")
            else:
                gate_only.append(f"{f}:{name}")
    print(
        f"{mod}: guarded={len(ok)} (scope-checked={len(scope_checked)}, "
        f"gate-only={len(gate_only)}) missing={len(missing)} "
        f"excluded={len(excluded)} order_violations={len(order_bad)}"
    )
    for x in missing:
        print("  MISSING:", x)
    for x in order_bad:
        print("  ORDER:", x)
    for x in excluded:
        print("  EXCL:", x)
    print("  GATE-ONLY:")
    for x in gate_only:
        print("   ", x)
    return 1 if missing or order_bad else 0


if __name__ == "__main__":
    sys.exit(main())
