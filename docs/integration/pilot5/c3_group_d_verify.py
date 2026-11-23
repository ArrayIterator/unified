#!/usr/bin/env python3
"""C3 pilot 5 group D verification (task 319): plm, referral, rental, sign.

For each module, per routed handler (same routed-set rule as
/tmp/c3_verify.py: names appearing in `.to(name)` in the module):
  - guarded: body contains `require_tenant(&req)` exactly once
  - order: the guard precedes the first identity/capability call
    (`caller(&req)`, `require_permission(`, `authorize(`,
    `check_capability(`, `resolve_identity(`)
  - kind (ADAPTED — documented in group-d-results.md): every guarded
    handler, GET and mutation alike, resolves
    `caller_company(&_tenant, &req)` (mismatch-only company check);
    `require_company` must NOT appear. These four modules' records
    carry a required `tenant_id` and no `company_id`, so writes are
    already fully tenant-partitioned by `tenant_id` and a strict
    "every write must resolve a company" rule would 403 legitimate
    tenant writes whose tenant context / verified caller carries no
    company. The template's GET-vs-mutation kind split therefore
    does not apply here.
  - delegation: sign `cancel_envelope` / `void_envelope` delegate to
    the shared `transition_terminal` helper, which gates first; the
    verifier checks the delegate's body for those two handlers.
  - excluded: names passed on the command line after the module

Usage: c3_group_d_verify.py <module> [excluded ...]
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
)

# Routed handlers guarded via a shared helper that gates first:
# module -> {routed handler: delegate function name}.
DELEGATED = {
    "sign": {
        "cancel_envelope": "transition_terminal",
        "void_envelope": "transition_terminal",
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
    method = {}
    for f in os.listdir(mdir):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        for m in re.finditer(r"web::(get|post|put|patch|delete)\(\)\.to\((\w+)\)", src):
            method.setdefault(m.group(2), m.group(1).upper())
    missing, ok, excluded, order_bad, kind_bad, double = [], [], [], [], [], []
    delegated = []
    reads, writes = 0, 0
    # Bodies of shared delegate helpers (non-routed fns), for the
    # DELEGATED handlers above.
    delegate_bodies = {}
    for f in sorted(os.listdir(mdir)):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        test_idx = src.find("#[cfg(test)]")
        head = src if test_idx < 0 else src[:test_idx]
        for m in re.finditer(r"async fn (\w+)\(", head):
            j = head.find("{", m.end())
            delegate_bodies[m.group(1)] = head[j:brace_end(head, j)]
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
            if count == 0 and name in DELEGATED.get(mod, {}):
                delegate = DELEGATED[mod][name]
                if f"{delegate}(" in body and delegate in delegate_bodies:
                    body = delegate_bodies[delegate]
                    count = body.count("require_tenant(&req)")
                    delegated.append(f"{f}:{name} (via {delegate})")
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
            if meth == "GET":
                reads += 1
            else:
                writes += 1
            # Mismatch-only variant (see docstring): every guarded
            # handler resolves caller_company; require_company is
            # never used in these modules.
            if "caller_company(&_tenant, &req)" not in body:
                kind_bad.append(f"{f}:{name} ({meth} without caller_company)")
            if "require_company(&_tenant, &req)" in body:
                kind_bad.append(f"{f}:{name} ({meth} with require_company)")
    print(
        f"{mod}: guarded={len(ok)} (reads={reads}, writes={writes}) "
        f"missing={len(missing)} excluded={len(excluded)} "
        f"order_violations={len(order_bad)} kind_mismatches={len(kind_bad)} "
        f"double_guards={len(double)}"
    )
    for x in delegated:
        print("  DELEGATED:", x)
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
