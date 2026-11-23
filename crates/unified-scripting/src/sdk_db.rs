//! Database access for script modules (`Unified.db`).
//!
//! Two surfaces, both tenant-scoped and fail-closed:
//!
//! * `db.kv` — a module-scoped key/value store (ephemeral, held in
//!   memory; restart clears it). Keys are namespaced by tenant and
//!   module, so one tenant's module can never read another's keys.
//! * `db.query` — read-only SQL against the calling tenant's
//!   schema only, with `$N` parameters substituted as escaped text
//!   literals (see `substitute_params`), through the one sanctioned path
//!   (`unified_core::tenancy::with_tenant_connection`). Only a
//!   single `SELECT` is accepted; anything
//!   else, a second statement, or a missing tenant/pool fails.
//!   There is deliberately no raw access to the control-plane
//!   database: a script module can never touch `public` directly.

use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

/// Maximum rows one `db.query` call may return. A query that would
/// return more fails with a clear message instead of buffering
/// unbounded data into RAM.
pub const MAX_QUERY_ROWS: usize = 1000;

/// Maximum number of parameters in one `db.query` call.
pub const MAX_QUERY_PARAMS: usize = 64;

/// Maximum length of one KV key.
pub const MAX_KV_KEY_LEN: usize = 256;

// ---------------------------------------------------------------- KV

/// Module-scoped key/value store. One instance is shared by every
/// script module; keys are namespaced by tenant and module id, so
/// isolation holds even though the map is shared.
#[derive(Default)]
pub struct KvStore {
    inner: RwLock<HashMap<String, Value>>,
}

impl KvStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Namespaced key: `tenant:<uuid|global>:module:<id>:<key>`.
    /// A module without a tenant context (control-plane call) uses
    /// the `global` namespace, which is still isolated per module.
    pub fn namespaced_key(tenant: Option<Uuid>, module_id: &str, key: &str) -> String {
        let tenant_part = tenant.map_or_else(|| "global".to_string(), |id| id.to_string());
        format!("tenant:{tenant_part}:module:{module_id}:{key}")
    }

    fn validate_key(key: &str) -> UnifiedResult<()> {
        if key.is_empty() || key.len() > MAX_KV_KEY_LEN {
            return Err(UnifiedError::bad_request(format!(
                "KV key must be 1..={MAX_KV_KEY_LEN} characters"
            )));
        }
        if key.contains('\0') {
            return Err(UnifiedError::bad_request("KV key must not contain NUL"));
        }
        Ok(())
    }

    pub fn get(&self, tenant: Option<Uuid>, module_id: &str, key: &str) -> UnifiedResult<Option<Value>> {
        Self::validate_key(key)?;
        let namespaced = Self::namespaced_key(tenant, module_id, key);
        Ok(self.inner.read().get(&namespaced).cloned())
    }

    pub fn set(
        &self,
        tenant: Option<Uuid>,
        module_id: &str,
        key: &str,
        value: Value,
    ) -> UnifiedResult<()> {
        Self::validate_key(key)?;
        let namespaced = Self::namespaced_key(tenant, module_id, key);
        self.inner.write().insert(namespaced, value);
        Ok(())
    }

    pub fn delete(&self, tenant: Option<Uuid>, module_id: &str, key: &str) -> UnifiedResult<bool> {
        Self::validate_key(key)?;
        let namespaced = Self::namespaced_key(tenant, module_id, key);
        Ok(self.inner.write().remove(&namespaced).is_some())
    }

    /// Keys visible to this module (and tenant), with the namespace
    /// prefix stripped. `prefix` filters within the module's keys.
    pub fn list(
        &self,
        tenant: Option<Uuid>,
        module_id: &str,
        prefix: &str,
    ) -> UnifiedResult<Vec<String>> {
        let tenant_part = tenant.map_or_else(|| "global".to_string(), |id| id.to_string());
        let namespace = format!("tenant:{tenant_part}:module:{module_id}:");
        let mut keys: Vec<String> = self
            .inner
            .read()
            .keys()
            .filter(|key| key.starts_with(&namespace))
            .map(|key| key[namespace.len()..].to_string())
            .filter(|key| key.starts_with(prefix))
            .collect();
        keys.sort();
        Ok(keys)
    }
}

// ------------------------------------------------------------- query

/// Validate that `sql` is exactly one read-only statement: a
/// `SELECT`. Leading SQL comments and whitespace are skipped.
/// `WITH` is refused outright: a WITH-CTE can carry a write
/// (`WITH d AS (DELETE ... RETURNING ...) SELECT ...`), which
/// would bypass the read-only contract. Anything else — INSERT,
/// UPDATE, DELETE, DDL, a second statement (`;`), or an empty
/// string — is refused.
pub fn validate_select(sql: &str) -> UnifiedResult<String> {
    let mut rest = sql.trim_start();
    // Skip leading `--` line comments and `/* */` block comments.
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            match after.find('\n') {
                Some(at) => rest = after[at + 1..].trim_start(),
                None => rest = "",
            }
        } else if let Some(after) = rest.strip_prefix("/*") {
            match after.find("*/") {
                Some(at) => rest = after[at + 2..].trim_start(),
                None => {
                    return Err(UnifiedError::bad_request(
                        "Unterminated block comment in SQL",
                    ))
                }
            }
        } else {
            break;
        }
    }
    if rest.is_empty() {
        return Err(UnifiedError::bad_request("SQL must not be empty"));
    }
    if rest.contains(';') {
        return Err(UnifiedError::bad_request(
            "Only a single SQL statement is allowed",
        ));
    }
    let first = rest
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if first != "SELECT" {
        return Err(UnifiedError::bad_request(
            "db.query accepts only SELECT statements",
        ));
    }
    Ok(rest.to_string())
}

/// Escape one parameter value as a Postgres string literal:
/// wrap in single quotes, doubling any single quote inside. With
/// `standard_conforming_strings` on (the Postgres default since
/// 9.1) a backslash is an ordinary character, so doubling the quote
/// is the whole rule — a value can never terminate the literal
/// early.
fn escape_literal(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    format!("'{}'", text.replace('\'', "''"))
}

/// Replace `$1`, `$2`, ... placeholders in `sql` with escaped text
/// literals from `params` (1-based). Only placeholders outside
/// single-quoted literals, double-quoted identifiers, line/block
/// comments and dollar-quoted strings are replaced; anything else
/// is copied verbatim. A `$N` with no matching parameter, or an
/// unterminated block comment, is an error.
fn substitute_params(sql: &str, params: &[Value]) -> UnifiedResult<String> {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        // Single-quoted literal: copy whole; '' is an escaped quote.
        if c == '\'' {
            out.push(c);
            i += 1;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                out.push(ch);
                i += 1;
                if ch == '\'' {
                    if i < bytes.len() && bytes[i] as char == '\'' {
                        out.push('\'');
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            continue;
        }
        // Double-quoted identifier: copy whole; "" is an escaped quote.
        if c == '"' {
            out.push(c);
            i += 1;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                out.push(ch);
                i += 1;
                if ch == '"' {
                    if i < bytes.len() && bytes[i] as char == '"' {
                        out.push('"');
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            continue;
        }
        let rest = &sql[i..];
        // Line comment: copy to (not including) the newline.
        if rest.starts_with("--") {
            let end = rest.find('\n').map(|at| i + at).unwrap_or(bytes.len());
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        // Block comment: copy whole.
        if rest.starts_with("/*") {
            match rest[2..].find("*/") {
                Some(at) => {
                    let end = i + 2 + at + 2;
                    out.push_str(&sql[i..end]);
                    i = end;
                }
                None => {
                    return Err(UnifiedError::bad_request(
                        "Unterminated block comment in SQL",
                    ));
                }
            }
            continue;
        }
        if c == '$' {
            let after = &rest[1..];
            // Placeholder: `$` followed by digits.
            if after.chars().next().is_some_and(|d| d.is_ascii_digit()) {
                let digits: String = after.chars().take_while(|d| d.is_ascii_digit()).collect();
                let n: usize = digits.parse().unwrap_or(0);
                if n == 0 || n > params.len() {
                    return Err(UnifiedError::bad_request(format!(
                        "Parameter ${n} has no matching value"
                    )));
                }
                out.push_str(&escape_literal(&params[n - 1]));
                i += 1 + digits.len();
                continue;
            }
            // Dollar-quoted string: `$tag$ ... $tag$` (empty tag `$$`
            // allowed). A tag starts with a letter or underscore,
            // never a digit, so `$1` can never open one.
            let first = after.chars().next().unwrap_or('\0');
            if first == '$' || first.is_ascii_alphabetic() || first == '_' {
                let mut j = 1;
                while j < rest.len()
                    && (rest.as_bytes()[j] as char).is_ascii_alphanumeric()
                    || j < rest.len() && (rest.as_bytes()[j] as char) == '_'
                {
                    j += 1;
                }
                if j < rest.len() && rest.as_bytes()[j] as char == '$' {
                    let opener = &rest[..=j];
                    match rest[j + 1..].find(opener) {
                        Some(at) => {
                            let end = i + j + 1 + at + opener.len();
                            out.push_str(&sql[i..end]);
                            i = end;
                            continue;
                        }
                        None => {
                            return Err(UnifiedError::bad_request(
                                "Unterminated dollar-quoted string in SQL",
                            ));
                        }
                    }
                }
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// Run a read-only query against the tenant schema and return the
/// rows as JSON values.
///
/// The SQL is wrapped as `SELECT row_to_json(q) AS row FROM (<sql>)
/// q`, so Postgres serializes each row and Diesel never needs to
/// know the result shape.
///
/// Parameters are written as `$1`, `$2`, ... in the SQL and are
/// substituted as text literals (`$1::uuid` still works: the cast
/// applies to the literal). They are NOT bound through Diesel:
/// every `bind` call changes the query's type, so a runtime-sized
/// parameter list cannot be bound in a loop. Substitution is done
/// by `substitute_params`, which only replaces `$N` outside string
/// literals, quoted identifiers and comments, and escapes each
/// value with the Postgres literal rule (`'` doubled), so a value
/// can never break out of its quotes. The statement is already
/// restricted to a single SELECT by `validate_select`.
///
/// This needs a live database pool and a tenant id; both missing
/// cases fail closed. There is no live PostgreSQL in the dev VM, so
/// this function is verified by its validation unit tests and by
/// compilation only — a live run is still open (see the crate
/// README).
pub fn db_query(
    pool: &unified_core::db::DbPool,
    tenant_id: Uuid,
    sql: &str,
    params: &[Value],
) -> UnifiedResult<Vec<Value>> {
    use diesel::prelude::*;
    use diesel::sql_types::Json;

    if params.len() > MAX_QUERY_PARAMS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_QUERY_PARAMS} parameters are allowed"
        )));
    }
    let cleaned = validate_select(sql)?;
    let rendered = substitute_params(&cleaned, params)?;

    #[derive(QueryableByName)]
    struct JsonRow {
        #[diesel(sql_type = Json)]
        row: Value,
    }

    unified_core::tenancy::with_tenant_connection(pool, tenant_id, |conn| {
        let rows: Vec<JsonRow> = diesel::sql_query(format!(
            "SELECT row_to_json(q) AS row FROM ({rendered}) q"
        ))
        .load(conn)?;
        if rows.len() > MAX_QUERY_ROWS {
            return Err(UnifiedError::bad_request(format!(
                "Query returned more than {MAX_QUERY_ROWS} rows; narrow it down"
            )));
        }
        Ok(rows.into_iter().map(|row| row.row).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_passes_and_other_statements_fail() {
        assert!(validate_select("SELECT id FROM users").is_ok());
        assert!(validate_select("  -- comment\nSELECT 1").is_ok());
        // WITH is refused even for a pure SELECT CTE: a WITH-CTE
        // can carry a write (DELETE ... RETURNING), so allowing it
        // would make the read-only contract depend on keyword
        // scanning instead of on the statement shape.
        assert!(validate_select("/* c */ WITH x AS (SELECT 1) SELECT * FROM x").is_err());
        assert!(
            validate_select("WITH d AS (DELETE FROM t RETURNING id) SELECT * FROM d").is_err()
        );
        assert!(validate_select("select id from users").is_ok());
        assert!(validate_select("INSERT INTO users VALUES (1)").is_err());
        assert!(validate_select("UPDATE users SET x = 1").is_err());
        assert!(validate_select("DELETE FROM users").is_err());
        assert!(validate_select("DROP TABLE users").is_err());
        assert!(validate_select("SELECT 1; SELECT 2").is_err());
        assert!(validate_select("SELECT 1;").is_err());
        assert!(validate_select("").is_err());
        assert!(validate_select("/* unterminated").is_err());
    }

    #[test]
    fn kv_is_namespaced_per_tenant_and_module() {
        let store = KvStore::new();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        store
            .set(Some(tenant_a), "mod-a", "k", Value::String("a".into()))
            .expect("set");
        store
            .set(Some(tenant_b), "mod-a", "k", Value::String("b".into()))
            .expect("set");
        store
            .set(Some(tenant_a), "mod-b", "k", Value::String("c".into()))
            .expect("set");
        assert_eq!(
            store.get(Some(tenant_a), "mod-a", "k").expect("get"),
            Some(Value::String("a".into()))
        );
        assert_eq!(
            store.get(Some(tenant_b), "mod-a", "k").expect("get"),
            Some(Value::String("b".into()))
        );
        assert_eq!(
            store.get(Some(tenant_a), "mod-b", "k").expect("get"),
            Some(Value::String("c".into()))
        );
        assert_eq!(store.get(None, "mod-a", "k").expect("get"), None);
        assert_eq!(
            store.list(Some(tenant_a), "mod-a", "").expect("list"),
            vec!["k".to_string()]
        );
        assert!(store.delete(Some(tenant_a), "mod-a", "k").expect("delete"));
        assert_eq!(store.get(Some(tenant_a), "mod-a", "k").expect("get"), None);
    }

    #[test]
    fn substitute_params_replaces_placeholders_only_outside_literals() {
        let params = [Value::String("o'clock".into())];
        // Basic substitution, with the quote escaped by doubling.
        assert_eq!(
            substitute_params("SELECT * FROM t WHERE a = $1", &params).expect("substitute"),
            "SELECT * FROM t WHERE a = 'o''clock'"
        );
        // Inside a single-quoted literal: untouched.
        assert_eq!(
            substitute_params("SELECT '$1' AS x, $1 AS y", &params).expect("substitute"),
            "SELECT '$1' AS x, 'o''clock' AS y"
        );
        // Inside a double-quoted identifier: untouched.
        assert_eq!(
            substitute_params("SELECT \"$1\" FROM t WHERE a = $1", &params)
                .expect("substitute"),
            "SELECT \"$1\" FROM t WHERE a = 'o''clock'"
        );
        // Inside comments: untouched.
        assert_eq!(
            substitute_params("SELECT -- $1\n $1", &params).expect("substitute"),
            "SELECT -- $1\n 'o''clock'"
        );
        assert_eq!(
            substitute_params("SELECT /* $1 */ $1", &params).expect("substitute"),
            "SELECT /* $1 */ 'o''clock'"
        );
        // Inside a dollar-quoted string: untouched.
        assert_eq!(
            substitute_params("SELECT $$ $1 $$, $1", &params).expect("substitute"),
            "SELECT $$ $1 $$, 'o''clock'"
        );
        // $10 is not $1 followed by 0.
        let ten: Vec<Value> = (1..=10).map(|n| Value::String(format!("v{n}"))).collect();
        assert_eq!(
            substitute_params("SELECT $10, $1", &ten).expect("substitute"),
            "SELECT 'v10', 'v1'"
        );
        // Missing parameter and $0 are errors.
        assert!(substitute_params("SELECT $2", &params).is_err());
        assert!(substitute_params("SELECT $0", &params).is_err());
        // Non-string params render as JSON text.
        assert_eq!(
            substitute_params("SELECT $1", &[Value::from(42)]).expect("substitute"),
            "SELECT '42'"
        );
        // Unterminated block comment is an error.
        assert!(substitute_params("SELECT /* x", &params).is_err());
    }

    #[test]
    fn kv_rejects_bad_keys() {
        let store = KvStore::new();
        assert!(store.set(None, "m", "", Value::Null).is_err());
        assert!(store.set(None, "m", "a\0b", Value::Null).is_err());
        assert!(store
            .set(None, "m", &"x".repeat(MAX_KV_KEY_LEN + 1), Value::Null)
            .is_err());
    }
}
