#!/usr/bin/env python3
"""Generate migration 000127 seed INSERTs from the provider registry seed JSON.

Single source of truth: crates/unified-core/provider_registry_seed.json.
Regenerate with:
  python3 docs/integration/c12_generate_registry_seed_sql.py > <up.sql seed section>
  python3 docs/integration/c12_generate_registry_seed_sql.py --ids gmail,yahoo > <subset>
The DDL header/footer in up.sql is hand-written; only the INSERT block below
comes from this script. The --ids filter emits INSERTs for the listed
provider ids only (migration 000131 uses it for the 43 new
email providers).
"""
import argparse
import json

SEED = "crates/unified-core/provider_registry_seed.json"


def sql_str(value):
    if value is None:
        return "NULL"
    return "'" + str(value).replace("'", "''") + "'"


def sql_int(value):
    if value is None:
        return "NULL"
    return str(int(value))


def sql_json(value):
    return sql_str(json.dumps(value, ensure_ascii=False, separators=(",", ":")))


def sql_bool(value):
    return "TRUE" if value else "FALSE"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ids",
        default=None,
        help="Comma-separated provider ids to emit (default: all providers).",
    )
    args = parser.parse_args()
    with open(SEED, encoding="utf-8") as handle:
        doc = json.load(handle)
    rows = doc["providers"]
    if args.ids:
        wanted = [part.strip() for part in args.ids.split(",") if part.strip()]
        selected = []
        for token in wanted:
            if ":" in token:
                family, pid = token.split(":", 1)
                matches = [
                    row
                    for row in doc["providers"]
                    if row["family"] == family and row["id"] == pid
                ]
            else:
                matches = [row for row in doc["providers"] if row["id"] == token]
            if not matches:
                parser.error(f"unknown provider id: {token}")
            if len(matches) > 1:
                parser.error(
                    f"ambiguous provider id {token}: matches "
                    + ", ".join(f"{row['family']}:{row['id']}" for row in matches)
                    + " - qualify it as family:id"
                )
            selected.extend(matches)
        rows = selected
    print(f"-- Seed rows generated from {SEED} (version {doc['version']}, "
          f"{len(rows)} providers). Do not hand-edit: regenerate instead.")
    for provider in rows:
        columns = (
            "family", "provider_id", "display_name", "logo", "base_url",
            "sandbox_base_url", "api_style", "auth_scheme",
            "webhook_signature_header", "allowed_hosts", "capabilities",
            "currencies", "credentials_shape", "models", "default_model",
            "adapter_kind", "smtp_host", "smtp_port", "smtp_tls", "http_api",
            "affiliate_url", "needs_endpoint", "needs_delegate", "notes",
            "builtin",
        )
        values = [
            sql_str(provider["family"]),
            sql_str(provider["id"]),
            sql_str(provider["display_name"]),
            sql_str(provider.get("logo")),
            sql_str(provider.get("base_url")),
            sql_str(provider.get("sandbox_base_url")),
            sql_str(provider.get("api_style")),
            sql_str(provider.get("auth_scheme")),
            sql_str(provider.get("webhook_signature_header")),
            sql_json(provider.get("allowed_hosts", [])),
            sql_json(provider.get("capabilities", {})),
            sql_json(provider.get("currencies", [])),
            sql_json(provider.get("credentials_shape", [])),
            sql_json(provider.get("models", [])),
            sql_str(provider.get("default_model")),
            sql_str(provider["adapter_kind"]),
            sql_str(provider.get("smtp_host")),
            sql_int(provider.get("smtp_port")),
            sql_str(provider.get("smtp_tls")),
            sql_json(provider.get("http_api")) if provider.get("http_api") is not None else "NULL",
            sql_str(provider.get("affiliate_url")),
            sql_bool(provider.get("needs_endpoint", False)),
            sql_bool(provider.get("needs_delegate", False)),
            sql_str(provider.get("notes")),
            "TRUE",
        ]
        print(
            "INSERT INTO provider_registry_definitions ("
            + ", ".join(columns)
            + ") VALUES ("
            + ", ".join(values)
            + ") ON CONFLICT (family, provider_id) DO NOTHING;"
        )


if __name__ == "__main__":
    main()
