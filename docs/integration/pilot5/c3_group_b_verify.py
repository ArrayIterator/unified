#!/usr/bin/env python3
"""C3 pilot 5 group B verification: esg, exams, forums, inbox.

For each module, per routed handler (same routed-set rule as
/tmp/c3_verify.py: names appearing in `.to(name)` in the module):
  - guarded: body contains `require_tenant(&req)` exactly once
  - order: the guard precedes the first identity/capability call
    (`caller(&req)`, `require_permission(`, `authorize(`,
    `check_capability(`, `resolve_identity(`, `actor_id(&req)`)
  - kind: per-module rule (see KIND below)
  - excluded: names passed on the command line after the module

Kind rules (adaptations, documented in group-b-results.md):
  - esg, forums: gate-only (group-C model). Records carry no
    `company_id` field, so no `caller_company` / scope check is
expected; guard + order only. Under carve-out 3
    their writes stay tenant-gate-only until C11 lands.
- exams: scoped, strict under (2026-11-23). GET
    handlers resolve `caller_company` (read gate); mutations
    resolve the strict write gate `require_company_for_write`
    AND apply the scope (a `require_*_in_scope` load,
    `require_company_match`, `in_company_scope` / `exam_in_scope`
    filter, the writable record checks, or the shared
    `change_exam_status` gate). `update_settings` is the one
    documented gate-only mutation (settings carry no company
    field); `get_settings` is a GET and passes the read rule.
    The old mismatch-only rule is superseded: an unresolved
    caller may no longer write a global row.
  - inbox: scope via `tenant_visible`. Every handler must apply
    it directly, through a `load_thread` / `load_canned_reply` /
    `assignments_for` scope-checked load, or by stamping the
    scope company (`scope.company_id`) on create. The
    `tenant_visible` semantics themselves (a company-less scope
    sees any company's threads within the tenant) are a pending
    PRODUCT decision and are deliberately NOT changed here.

Usage: c3_group_b_verify.py <module> [excluded ...]
Exit code 0 only when missing == 0, order violations == 0 and
kind mismatches == 0.
"""
import os
import re
import sys

ROOT = "/home/hatch/workspace/unified-push/src/modules"

IDENTITY_CALLS = (
    "caller(&req)",
    "require_permission(",
    "authorize(",
    "check_capability(",
    "resolve_identity(",
    "actor_id(&req)",
)

EXAMS_SCOPE_MARKERS = (
    "require_company_match",
    "require_company_for_write",
    "require_bank_by_id_in_scope",
    "require_exam_by_id_in_scope",
    "require_question_in_scope",
    "require_attempt_in_scope",
    "require_attempt_writable_in_scope",
    "require_exam_writable_by_id_in_scope",
    "require_rubric_in_scope",
    "require_grade_in_scope",
    "require_result_in_scope",
    "require_record_in_company_scope",
    "require_record_writable_in_company_scope",
    "in_company_scope",
    "exam_in_scope",
    "change_exam_status(",
)

# Gate-only mutations inside the scoped exams module (no company
# field on the record; documented in group-b-results.md).
EXAMS_GATE_ONLY = {"update_settings"}

INBOX_SCOPE_MARKERS = (
    "tenant_visible",
    "load_thread(",
    "load_canned_reply(",
    "assignments_for(",
    "scope.company_id",
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
    method = {}
    for f in os.listdir(mdir):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        for m in re.finditer(r"web::(get|post|put|patch|delete)\(\)\.to\((\w+)\)", src):
            method.setdefault(m.group(2), m.group(1).upper())
    missing, ok, excluded, order_bad, kind_bad, double = [], [], [], [], [], []
    reads, writes = 0, 0
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
                (body.find(call) for call in IDENTITY_CALLS if body.find(call) != -1),
                default=-1,
            )
            if first_identity != -1 and first_identity < guard_at:
                order_bad.append(f"{f}:{name}")
            meth = method[name]
            if mod in ("esg", "forums"):
                # Gate-only: no kind check.
                if meth == "GET":
                    reads += 1
                else:
                    writes += 1
            elif mod == "exams":
                has_caller_company = "caller_company(&_tenant, &req)" in body
                has_write_gate = "require_company_for_write(&_tenant, &req)" in body
                has_scope = any(marker in body for marker in EXAMS_SCOPE_MARKERS)
                if meth == "GET":
                    reads += 1
                    if not has_caller_company:
                        kind_bad.append(f"{f}:{name} (GET without caller_company)")
                    if has_write_gate:
                        kind_bad.append(f"{f}:{name} (GET with require_company_for_write)")
                elif name in EXAMS_GATE_ONLY:
                    writes += 1
                    if not has_caller_company:
                        kind_bad.append(f"{f}:{name} ({meth} carve-out without caller_company)")
                    if has_write_gate:
                        kind_bad.append(f"{f}:{name} ({meth} carve-out with require_company_for_write)")
                else:
                    writes += 1
                    if not has_write_gate:
                        kind_bad.append(f"{f}:{name} ({meth} without require_company_for_write)")
                    if has_caller_company:
                        kind_bad.append(f"{f}:{name} ({meth} with caller_company)")
                    elif not has_scope:
                        kind_bad.append(f"{f}:{name} ({meth} without scope application)")
            elif mod == "inbox":
                if meth == "GET":
                    reads += 1
                else:
                    writes += 1
                if not any(marker in body for marker in INBOX_SCOPE_MARKERS):
                    kind_bad.append(f"{f}:{name} ({meth} without tenant_visible scope)")
            else:
                raise SystemExit(f"unknown module: {mod}")
    print(
        f"{mod}: guarded={len(ok)} (reads={reads}, writes={writes}) "
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
