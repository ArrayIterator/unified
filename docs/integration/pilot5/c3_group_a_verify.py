#!/usr/bin/env python3
"""C3 pilot 5 group A verification (task 319): billing, cards,
delivery_notes, entitlements.

For each module, per routed handler (same routed-set rule as the
pilot 4 verifier: names appearing in `.to(name)` in the module):
  - guarded: body contains `require_tenant(&req)` exactly once
  - order: the guard precedes the first identity/capability call
  - kind: per-module write strictness (see KIND below)
  - scope: body contains a module scope marker (reported as a
    count; gate-only handlers legitimately have none)

Adaptations vs the pilot 4 template (documented in
docs/integration/pilot5/group-a-results.md):
  - billing / delivery_notes are mismatch-only, not strict:
    GET and mutation handlers alike resolve `caller_company`
    (never `require_company`); scope is checked with the
    module's `in_company_scope*` / `require_*_in_company_scope` /
    `require_company_match*` / `require_company_change*` helpers.
  - cards keeps the pilot 4 strict rule: GET -> caller_company,
    mutation -> require_company.
  - entitlements is split by file: the tenant surface
    (tenant.rs, handlers_packages.rs, handlers_ai.rs) is strict
    via the pre-existing `TenantScope::resolve` (caller_company +
    TenantScope::resolve), except `ai_capabilities` (GET, no
    TenantScope, caller_company only); the admin surface
    (handlers_admin.rs) is tenant-gate-only (require_tenant only,
    admin acts on body/query company by design).
  - identity calls are per-module (billing adds fee_caller,
    billing_authorize, billing_caller, fee_require_view,
    fee_require_manage; entitlements adds caller_of,
    admin_caller, caller_from, require_capability).

Usage: c3_group_a_verify.py <module> [excluded ...]
Exit code 0 only when missing == 0, order violations == 0 and
kind mismatches == 0.
"""
import os
import re
import sys

ROOT = "/home/hatch/workspace/unified-push/src/modules"

IDENTITY_CALLS = {
    "billing": (
        "fee_caller(&req)",
        "billing_authorize(",
        "billing_caller(",
        "caller(&req)",
        "fee_require_view(",
        "fee_require_manage(",
    ),
    "cards": (
        "caller(&req)",
        "require_permission(",
    ),
    "delivery_notes": (
        "caller(&req)",
        "require_permission(",
    ),
    "entitlements": (
        "caller_of(",
        "admin_caller(",
        "caller_from(",
        "require_capability(",
        "caller(&req)",
    ),
}

# kind rule per module: "strict" (pilot 4), "mismatch", "entitlements".
KIND = {
    "billing": "mismatch",
    "cards": "strict",
    "delivery_notes": "mismatch",
    "entitlements": "entitlements",
}

# Body markers that count a handler as scope-checked (vs gate-only).
SCOPE_MARKERS = {
    "billing": (
        "in_company_scope(",
        "require_record_in_company_scope(",
        "require_company_match(",
        "require_company_match_for(",
        "require_company_change(",
        # Scoped triggers: the handler passes the caller scope into
        # the scan instead of checking a record itself.
        "run_reminder_scan(&state, &_scope)",
        "run_subscription_scan(&_scope)",
    ),
    "cards": (),
    "delivery_notes": (
        "in_company_scope_uuid(",
        "require_note_in_company_scope(",
        "require_company_match_uuid(",
        "require_company_match_uuid_for(",
        "require_company_change_uuid(",
    ),
    "entitlements": (
        "TenantScope::resolve(",
    ),
}

# Entitlements: files that are tenant-gate-only (no caller_company
# requirement); every other routed file is the strict tenant surface.
ENT_GATE_ONLY_FILES = {"handlers_admin.rs"}


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


def kind_check(mod, fname, name, meth, body, kind_bad):
    rule = KIND[mod]
    has_caller_company = "caller_company(&_tenant, &req)" in body
    has_require_company = "require_company(&_tenant, &req)" in body
    if rule == "strict":
        if meth == "GET":
            if not has_caller_company:
                kind_bad.append(f"{fname}:{name} (GET without caller_company)")
            if has_require_company:
                kind_bad.append(f"{fname}:{name} (GET with require_company)")
        else:
            if not has_require_company:
                kind_bad.append(f"{fname}:{name} ({meth} without require_company)")
            if has_caller_company:
                kind_bad.append(f"{fname}:{name} ({meth} with caller_company)")
    elif rule == "mismatch":
        if not has_caller_company:
            kind_bad.append(f"{fname}:{name} ({meth} without caller_company)")
        if has_require_company:
            kind_bad.append(f"{fname}:{name} ({meth} with require_company)")
    elif rule == "entitlements":
        if fname in ENT_GATE_ONLY_FILES:
            # Tenant-gate-only: the guard is the whole check.
            return
        if not has_caller_company:
            kind_bad.append(f"{fname}:{name} ({meth} without caller_company)")
        if has_require_company:
            kind_bad.append(f"{fname}:{name} ({meth} with require_company)")
        if name == "ai_capabilities":
            return
        if "TenantScope::resolve(" not in body:
            kind_bad.append(f"{fname}:{name} ({meth} without TenantScope::resolve)")


def main():
    mod = sys.argv[1]
    excl = set(sys.argv[2:])
    mdir = os.path.join(ROOT, mod)
    method = {}
    for f in os.listdir(mdir):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        for m in re.finditer(r"web::(get|post|put|patch|delete)\(\)\.to\((\w+)\)", src):
            method.setdefault(m.group(2), m.group(1).upper())
    missing, ok, excluded, order_bad, kind_bad, double = [], [], [], [], [], []
    reads, writes, scoped = 0, 0, 0
    identity_calls = IDENTITY_CALLS[mod]
    for f in sorted(os.listdir(mdir)):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        test_idx = src.find("#[cfg(test)]")
        head = src if test_idx < 0 else src[:test_idx]
        for m in re.finditer(r"pub\((?:crate|super)\) async fn (\w+)", head):
            name = m.group(1)
            if name not in method:
                continue
            j = head.find("{", m.end())
            body = head[j:brace_end(head, j)]
            if name in excl:
                excluded.append(f"{f}:{name}")
                if "require_tenant(&req)" in body:
                    double.append(f"{f}:{name} (excluded but guarded)")
                continue
            count = body.count("require_tenant(&req)")
            if count == 0:
                missing.append(f"{f}:{name}")
                continue
            if count > 1:
                double.append(f"{f}:{name} (x{count})")
            ok.append(f"{f}:{name}")
            guard_at = body.find("require_tenant(&req)")
            first_identity = min(
                (body.find(call) for call in identity_calls if body.find(call) != -1),
                default=-1,
            )
            if first_identity != -1 and first_identity < guard_at:
                order_bad.append(f"{f}:{name}")
            meth = method[name]
            if meth == "GET":
                reads += 1
            else:
                writes += 1
            kind_check(mod, f, name, meth, body, kind_bad)
            if any(marker in body for marker in SCOPE_MARKERS[mod]):
                scoped += 1
    print(
        f"{mod}: guarded={len(ok)} (reads={reads}, writes={writes}) "
        f"scope_checked={scoped} gate_only={len(ok) - scoped} "
        f"missing={len(missing)} excluded={len(excluded)} "
        f"order_violations={len(order_bad)} kind_mismatches={len(kind_bad)} "
        f"double_guards={len(double)}"
    )
    for x in missing:
        print("  MISSING:", x)
    for x in order_bad:
        print("  ORDER:", x)
    for x in kind_bad:
        print("  KIND:", x)
    for x in double:
        print("  DOUBLE:", x)
    for x in excluded:
        print("  EXCL:", x)
    return 1 if missing or order_bad or kind_bad or double else 0


if __name__ == "__main__":
    sys.exit(main())
