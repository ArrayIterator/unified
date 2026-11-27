#!/usr/bin/env python3
"""C3 pilot 5 group C verification: kyc, payments, phone
(guard work) plus library, files, studio (verify-only).

For each module, per routed handler (same routed-set rule as
/tmp/c3_verify.py: names appearing in `.to(name)` in the module):

Guard shapes (adapted per module; see group-c-results.md for why):
  - kyc, phone: the pre-existing fail-closed `resolve_tenant(&req)`
    (helpers.rs: `TenantContext::from_http_request`, 401 without
    resolved context, never falls back to `public`) counts as the
    guard, exactly like `require_tenant(&req)` in the pilot modules.
    Records here carry `tenant_id`, not `company_id`, so there is no
    `caller_company` to resolve; tenant-scoped loaders
    (`load_case`, `load_call`, ...) enforce the partition (404
    cross-tenant).
  - payments: `require_tenant(&req)` (new, adapter.rs) first, plus the
    module's existing verified-caller ownership checks
    (`require_customer` / `list_customer_scope` on `customer_id`,
    `require_payments_admin`, `require_cards_capability`). Records
    carry no `company_id` / `tenant_id` field, so there is no
    `caller_company` to resolve; the tenant gate plus the ownership
    checks are the guard (see the results file).
  - library: `require_tenant(&req)` + `caller_company` (auth.rs),
    the canonical pilot shape. Verify-only: no edits were made.
  - files: handlers resolve the verified caller scope through the
    shared `authorize()` helper (handlers.rs -> types.rs
    `resolve_caller_company`, no header trust) and filter with
    `CallerScope::can_access_record`. Verify-only. NOTE: files has
    NO fail-closed tenant gate - `authorize()` never calls
    `TenantContext::from_http_request` as a gate; a request with no
    resolved tenant context proceeds as Unresolved (global rows
    only) / Platform (control-plane admin). Those handlers are
    reported under NO_TENANT_GATE, not MISSING, and do not affect
    the exit code; the coordinator decides the follow-up.
  - studio: handlers call `authorize()` or `guard()` (auth.rs),
    which both funnel through `caller()` -> `resolve_tenant(req)`
    FIRST (401 without resolved context). Verify-only. The
    `delete_definition` -> `archive_definition` delegation is
    counted as guarded-via-delegation (verified by reading the
    delegating body, as in pilot 3).

Order: the guard precedes the first identity/capability/ownership
call. Scope-checked: body mentions a scope marker for the module
(see MODULES below); otherwise gate-only.

These shapes are unchanged -- kyc/phone
records carry `tenant_id` (not `company_id`), payments guards by
ownership, library uses the `CallerScope` idiom (its
`require_company_match_for` was fixed to refuse unresolved writes
in C.1), and files/studio are verify-only.

Usage: c3_group_c_verify.py <module> [excluded ...]
Exit code 0 only when missing == 0 and order violations == 0 for
the guard modules (kyc, payments, phone). Verify-only modules
(library, files, studio) always exit 0 and report their counts;
their NO_TENANT_GATE / DELEGATED rows are findings, not failures.
"""
import os
import re
import sys

ROOT = "/home/hatch/workspace/unified-push/src/modules"

# guard: substrings that count as the tenant gate in a handler body.
# identity: calls the guard must precede. scope: markers that make a
# handler scope-checked rather than gate-only.
MODULES = {
    "kyc": {
        "guard": ("require_tenant(&req)", "resolve_tenant(&req)"),
        "identity": ("caller(&req)", "require_permission(", "ai::caller(&req)"),
        "scope": ("load_case", "load_document", "load_visible_case",
                  "load_subject_type_by_code", "load_context",
                  "load_instruction", "load_risk_level",
                  "load_document_type_by_code", "tenant_id"),
        "verify_only": False,
    },
    "phone": {
        "guard": ("require_tenant(&req)", "resolve_tenant(&req)"),
        "identity": ("caller(&req)", "require_permission(", "ai::caller(&req)"),
        "scope": ("load_call", "load_visible_call", "load_queue",
                  "load_menu", "load_node", "load_script", "load_rule",
                  "load_member", "load_waiter", "load_recording",
                  "load_participant", "tenant_id"),
        "verify_only": False,
    },
    "payments": {
        "guard": ("require_tenant(&req)",),
        "identity": ("require_customer(", "require_customer_any(",
                     "require_payments_admin(", "list_customer_scope(",
                     "require_cards_capability("),
        "scope": ("require_customer(", "require_customer_any(",
                  "list_customer_scope(", "require_payments_admin(",
                  "require_cards_capability("),
        "verify_only": False,
    },
    "library": {
        "guard": ("require_tenant(&req)",),
        "identity": ("caller(&req)", "require_permission(",
                     "require_circulate(", "require_catalog("),
        "scope": ("caller_company", "in_company_scope",
                  "require_record_in_company_scope",
                  "require_company_match"),
        "verify_only": True,
    },
    "files": {
        # authorize() IS the guard shape here (scope resolution);
        # tenant-gate absence is reported separately below.
        "guard": ("authorize(",),
        "identity": (),
        "scope": ("can_access_record", "caller_company"),
        "verify_only": True,
    },
    "studio": {
        "guard": ("authorize(", "guard(&ctx, &req", "guard(ctx, &req"),
        "identity": (),
        "scope": ("can_access_record", "require_company_scope",
                  "scoped_read_company", "caller_company"),
        "verify_only": True,
        "delegated": {"delete_definition": "archive_definition"},
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


def check_body(body, cfg):
    """Return (guarded, order_ok, scope_checked)."""
    guard_at = min(
        (body.find(g) for g in cfg["guard"] if body.find(g) != -1),
        default=-1,
    )
    if guard_at == -1:
        return False, True, False
    first_identity = min(
        (body.find(call) for call in cfg["identity"] if body.find(call) != -1),
        default=-1,
    )
    order_ok = first_identity == -1 or first_identity > guard_at
    scope_checked = any(marker in body for marker in cfg["scope"])
    return True, order_ok, scope_checked


def main():
    mod = sys.argv[1]
    excl = set(sys.argv[2:])
    cfg = MODULES[mod]
    mdir = os.path.join(ROOT, mod)
    routed = set()
    for f in os.listdir(mdir):
        if not f.endswith(".rs"):
            continue
        src = open(os.path.join(mdir, f), encoding="utf-8", errors="replace").read()
        routed |= set(re.findall(r"\.to\((\w+)\)", src))
    missing, ok, excluded, order_bad = [], [], [], []
    scope_checked, gate_only, no_tenant_gate, delegated = [], [], [], []
    # files routes every handler through the shared authorize() funnel
    # in handlers.rs; the tenant gate lives there, not in each body.
    authorize_has_gate = False
    if mod == "files":
        auth_src = open(
            os.path.join(mdir, "handlers.rs"), encoding="utf-8", errors="replace"
        ).read()
        am = re.search(r"fn authorize\(", auth_src)
        if am:
            aj = auth_src.find("{", am.end())
            abody = auth_src[aj:brace_end(auth_src, aj)]
            # authorize() takes req: &HttpRequest, so the call is
            # require_tenant(req); accept either call form.
            authorize_has_gate = (
                "require_tenant(req)" in abody or "require_tenant(&req)" in abody
            )
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
            body = head[j:brace_end(head, j)]
            if name in excl:
                excluded.append(f"{f}:{name}")
                continue
            if name in cfg.get("delegated", {}):
                target = cfg["delegated"][name]
                # The delegating body must name the guarded target.
                if target in body:
                    delegated.append(f"{f}:{name} -> {target}")
                    ok.append(f"{f}:{name}")
                else:
                    missing.append(f"{f}:{name} (delegation to {target} not found)")
                continue
            guarded, order_ok, is_scope = check_body(body, cfg)
            if not guarded:
                missing.append(f"{f}:{name}")
                continue
            ok.append(f"{f}:{name}")
            if not order_ok:
                order_bad.append(f"{f}:{name}")
            (scope_checked if is_scope else gate_only).append(f"{f}:{name}")
            if mod == "files" and "require_tenant(&req)" not in body \
                    and "resolve_tenant(&req)" not in body \
                    and not authorize_has_gate:
                no_tenant_gate.append(f"{f}:{name}")
    print(
        f"{mod}: guarded={len(ok)} (scope-checked={len(scope_checked)}, "
        f"gate-only={len(gate_only)}, delegated={len(delegated)}) "
        f"missing={len(missing)} excluded={len(excluded)} "
        f"order_violations={len(order_bad)}"
        + (f" no_tenant_gate={len(no_tenant_gate)}" if mod == "files" else "")
    )
    for x in missing:
        print("  MISSING:", x)
    for x in order_bad:
        print("  ORDER:", x)
    for x in excluded:
        print("  EXCL:", x)
    for x in delegated:
        print("  DELEGATED:", x)
    for x in no_tenant_gate:
        print("  NO_TENANT_GATE:", x)
    if cfg["verify_only"]:
        return 0
    return 1 if missing or order_bad else 0


if __name__ == "__main__":
    sys.exit(main())
