#!/usr/bin/env python3
"""Generate migration 000127 seed INSERTs from the provider registry seed JSON.

Single source of truth: crates/unified-core/provider_registry_seed.json.
Regenerate with:
  python3 docs/integration/c12_generate_registry_seed_sql.py > <up.sql seed section>
The DDL header/footer in up.sql is hand-written; only the INSERT block below
comes from this script.
"""
import json
import sys

SEED = "crates/unified-core/provider_registry_seed.json"


def sql_str(value):
    if value is None:
        return "NULL"
    return "'" + str(value).replace("'", "''") + "'"


def sql_json(value):
    return sql_str(json.dumps(value, ensure_ascii=False, separators=(",", ":")))


def sql_bool(value):
    return "TRUE" if value else "FALSE"


def main():
    with open(SEED, encoding="utf-8") as handle:
        doc = json.load(handle)
    rows = doc["providers"]
    print(f"-- Seed rows generated from {SEED} (version {doc['version']}, "
          f"{len(rows)} providers). Do not hand-edit: regenerate instead.")
    for provider in rows:
        columns = (
            "family", "provider_id", "display_name", "base_url",
            "sandbox_base_url", "api_style", "auth_scheme",
            "webhook_signature_header", "allowed_hosts", "capabilities",
            "currencies", "credentials_shape", "models", "default_model",
            "adapter_kind", "needs_endpoint", "needs_delegate", "notes",
            "builtin",
        )
        values = [
            sql_str(provider["family"]),
            sql_str(provider["id"]),
            sql_str(provider["display_name"]),
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
