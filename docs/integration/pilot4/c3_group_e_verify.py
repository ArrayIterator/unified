#!/usr/bin/env python3
"""C3 pilot 4 group E verification: procurement, vcs,
content, crm, ai_support, maintenance, webhooks, saved_views,
downloader.

For each module, per routed handler (routed = names appearing in
`.to(name)` in the module):
  - guarded: body contains `require_tenant(&req)`
  - order: the guard precedes the first identity/capability call
    (`caller(&req)`, `authorize(`, `require_permission(`, `actor_id(`,
    `authorize_connectors(`, `ai::caller(&req)`)
  - write rule: POST/PUT/PATCH/DELETE handlers must resolve the
    company with `require_caller_company(`; GET handlers must resolve
    it with `caller_company(` (mismatch -> 403)

`require_caller_company` is the pre-315
strict write gate, equivalent to the newer
`require_company_for_write`; these modules already used it, so
the C.3 rename left them untouched and this rule is unchanged.
  - excluded: names passed on the command line after the module

Usage: c3_group_e_verify.py <module> [excluded ...]
Exit code 0 only when missing == 0, order violations == 0 and
write-rule violations == 0.
"""
import os
import re
import sys

ROOT = "/home/hatch/workspace/unified-push/src/modules"

IDENTITY_CALLS = (
    "caller(&req)",
    "authorize(",
    "require_permission(",
    "actor_id(&req)",
    "authorize_connectors(",
    "ai::caller(&req)",
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
    methods = {}
    for f in os.listdir(mdir):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        routed |= set(re.findall(r"\.to\((\w+)\)", src))
        for m in re.finditer(r"web::(get|post|put|patch|delete)\(\)\.to\((\w+)\)", src):
            methods[m.group(2)] = m.group(1)
    missing, ok, excluded, order_bad, write_bad = [], [], [], [], []
    reads, writes = [], []
    for f in sorted(os.listdir(mdir)):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        test_idx = src.find("#[cfg(test)]")
        head = src if test_idx < 0 else src[:test_idx]
        for m in re.finditer(r"pub\((crate|super)\) async fn (\w+)\s*\(", head):
            name = m.group(2)
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
            method = methods.get(name, "?")
            if method in ("post", "put", "patch", "delete"):
                writes.append(f"{f}:{name}")
                if "require_caller_company(" not in body:
                    write_bad.append(f"{f}:{name} ({method}, no require_caller_company)")
            else:
                reads.append(f"{f}:{name}")
                if "caller_company(" not in body:
                    write_bad.append(f"{f}:{name} ({method}, no caller_company)")
    print(
        f"{mod}: guarded={len(ok)} (reads={len(reads)}, writes={len(writes)}) "
        f"missing={len(missing)} excluded={len(excluded)} "
        f"order_violations={len(order_bad)} write_rule_violations={len(write_bad)}"
    )
    for x in missing:
        print("  MISSING:", x)
    for x in order_bad:
        print("  ORDER:", x)
    for x in write_bad:
        print("  WRITE-RULE:", x)
    for x in excluded:
        print("  EXCL:", x)
    return 1 if missing or order_bad or write_bad else 0


if __name__ == "__main__":
    sys.exit(main())
