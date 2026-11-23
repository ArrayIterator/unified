#!/usr/bin/env python3
"""C3 pilot 4 group B verification (task 316): curriculum, supervision,
community, todos, contracts, departments, email_providers, mail.

Adapted from docs/integration/pilot3/c3_group_a_verify.py:
- handler visibility is pub(crate) OR pub(super) (curriculum uses pub(super))
- routed handlers that delegate auth to a shared helper (supervision
  approve/return_* -> decide_*, todos archive/unarchive_list ->
  set_list_archived) count as guarded when the helper body contains
  the guard (pilot-3 transition_* model)
- order check also covers authorize(&ctx, &req, / require_caller(&req)
  / ai::caller(&req) identity calls

For each module, per routed handler (names appearing in `.to(name)`):
  - guarded: body contains `require_tenant(&req)`
  - order: the guard precedes the first identity/capability call
  - scope-checked: body mentions a scope marker; otherwise gate-only
    (all group-B modules are gate-only: records carry no company field)
  - excluded: names passed on the command line after the module

Usage: c3_group_b_verify.py <module> [excluded ...]
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
)

IDENTITY_CALLS = (
    "caller(&req)",
    "ai::caller(&req)",
    "require_permission(",
    "check_capability(",
    "authorize(&ctx, &req,",
    "require_caller(&req)",
)

# Routed handler -> shared helper carrying the guard.
DELEGATED = {
    "supervision": {
        "approve_inspection": "decide_inspection",
        "return_inspection": "decide_inspection",
        "approve_progress_report": "decide_progress_report",
        "return_progress_report": "decide_progress_report",
    },
    "todos": {
        "archive_list": "set_list_archived",
        "unarchive_list": "set_list_archived",
    },
}


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
    # handler bodies: name -> body (first definition wins)
    bodies = {}
    for f in sorted(os.listdir(mdir)):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        test_idx = src.find("#[cfg(test)]")
        head = src if test_idx < 0 else src[:test_idx]
        for m in re.finditer(r"pub\((?:crate|super)\) async fn (\w+)\s*\(", head):
            name = m.group(1)
            if name in bodies:
                continue
            j = head.find("{", m.end())
            bodies[name] = head[j : brace_end(head, j)]
    missing, ok, excluded, order_bad = [], [], [], []
    scope_checked, gate_only, via_deleg = [], [], []
    for name, body in sorted(bodies.items()):
        if name not in routed:
            continue
        if name in excl:
            excluded.append(name)
            continue
        if name in DELEGATED.get(mod, {}):
            helper = DELEGATED[mod][name]
            if helper in bodies and "require_tenant(&req)" in bodies[helper]:
                ok.append(f"{name} [via {helper}]")
                via_deleg.append(name)
            else:
                missing.append(f"{name} (delegate {helper} unguarded)")
            continue
        if "require_tenant(&req)" not in body:
            missing.append(name)
            continue
        ok.append(name)
        guard_at = body.find("require_tenant(&req)")
        first_identity = min(
            (body.find(call) for call in IDENTITY_CALLS if body.find(call) != -1),
            default=-1,
        )
        if first_identity != -1 and first_identity < guard_at:
            order_bad.append(name)
        if any(marker in body for marker in SCOPE_MARKERS):
            scope_checked.append(name)
        else:
            gate_only.append(name)
    print(
        f"{mod}: guarded={len(ok)} (scope-checked={len(scope_checked)}, "
        f"gate-only={len(gate_only)}, via-delegation={len(via_deleg)}) "
        f"missing={len(missing)} excluded={len(excluded)} "
        f"order_violations={len(order_bad)}"
    )
    for x in missing:
        print("  MISSING:", x)
    for x in order_bad:
        print("  ORDER:", x)
    for x in excluded:
        print("  EXCL:", x)
    return 1 if missing or order_bad else 0


if __name__ == "__main__":
    sys.exit(main())
