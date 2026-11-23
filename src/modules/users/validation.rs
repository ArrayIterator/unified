// users module — Validation.
//
// Split from the former single-file module `src/modules/users.rs`.
// The module root (`users.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{NewUser, User};
use unified_core::db::DbPool;
use crate::modules::security::PasswordPolicy;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock, OnceLock};
use uuid::Uuid;

use super::types::EMAIL_PATTERN;

pub(crate) fn validate_email(email: &str) -> UnifiedResult<()> {
    let email = email.trim();
    if email.is_empty() || !EMAIL_PATTERN.is_match(email) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: \"{email}\""
        )));
    }
    Ok(())
}

pub(crate) fn validate_name(name: &str) -> UnifiedResult<()> {
    if name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Name must not be empty"));
    }
    if name.chars().count() > 255 {
        return Err(UnifiedError::bad_request(
            "Name must be at most 255 characters long",
        ));
    }
    Ok(())
}

pub(crate) fn validate_password(password: &str) -> UnifiedResult<()> {
    let problems = PasswordPolicy::default().validate(password);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(UnifiedError::bad_request("Password does not meet the policy")
            .with_metadata("problems", serde_json::json!(problems)))
    }
}

pub(crate) fn hash_password(password: &str) -> UnifiedResult<String> {
    crate::auth::password::hash(password)
        .map_err(|error| UnifiedError::internal(format!("Password hashing failed: {error}")))
}

pub(crate) fn db_pool(ctx: &AppContext) -> Option<DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

/// True when a Diesel error is a unique-constraint violation
/// (PostgreSQL error 23505). User inserts pre-check for a duplicate
/// email, but the check can race with a concurrent insert, so the
/// database unique constraint is the authority: callers map this
/// case to a 409 conflict instead of surfacing a raw Diesel error
/// as a 500.
pub(crate) fn is_unique_violation(error: &diesel::result::Error) -> bool {
    matches!(
        error,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_email_accepts_well_formed_addresses() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("first.last+tag@sub.example.co.id").is_ok());
    }

    #[test]
    fn validate_email_rejects_bad_addresses() {
        for bad in ["plain", "", "a@b", "a@b.c", "a b@c.com", "a@b@c.com"] {
            assert!(validate_email(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn validate_name_accepts_names_and_rejects_empty_or_huge_ones() {
        assert!(validate_name("Ada Lovelace").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(255)).is_ok());
        assert!(validate_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn validate_password_rejects_weak_passwords() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("alllowercase123!").is_err());
        assert!(validate_password("NoDigitsHere!").is_err());
        assert!(validate_password("NoSymbol123456").is_err());
    }

    #[test]
    fn validate_password_accepts_a_strong_password() {
        assert!(validate_password("Str0ng!Passw0rd").is_ok());
    }

    #[test]
    fn hash_password_returns_an_argon2id_hash() {
        let hash = hash_password("Str0ng!Passw0rd").unwrap();
        assert!(!hash.is_empty());
        assert_ne!(hash, "Str0ng!Passw0rd");
        assert!(hash.starts_with("$argon2id$"));
    }

    #[test]
    fn hash_password_uses_a_random_salt() {
        let first = hash_password("Str0ng!Passw0rd").unwrap();
        let second = hash_password("Str0ng!Passw0rd").unwrap();
        assert_ne!(first, second);
    }

    /// Minimal stand-in for a PostgreSQL error payload, so the
    /// unique-violation classifier can be tested without a database.
    struct FakeDbErrorInfo;

    impl diesel::result::DatabaseErrorInformation for FakeDbErrorInfo {
        fn message(&self) -> &str {
            "duplicate key value violates unique constraint \"users_email_key\""
        }

        fn details(&self) -> Option<&str> {
            None
        }

        fn hint(&self) -> Option<&str> {
            None
        }

        fn table_name(&self) -> Option<&str> {
            Some("users")
        }

        fn column_name(&self) -> Option<&str> {
            Some("email")
        }

        fn constraint_name(&self) -> Option<&str> {
            Some("users_email_key")
        }

        fn statement_position(&self) -> Option<i32> {
            None
        }
    }

    fn db_error(kind: diesel::result::DatabaseErrorKind) -> diesel::result::Error {
        diesel::result::Error::DatabaseError(kind, Box::new(FakeDbErrorInfo))
    }

    #[test]
    fn is_unique_violation_matches_only_unique_violations() {
        assert!(is_unique_violation(&db_error(
            diesel::result::DatabaseErrorKind::UniqueViolation
        )));
        assert!(!is_unique_violation(&db_error(
            diesel::result::DatabaseErrorKind::ForeignKeyViolation
        )));
        assert!(!is_unique_violation(&db_error(
            diesel::result::DatabaseErrorKind::CheckViolation
        )));
        assert!(!is_unique_violation(&diesel::result::Error::NotFound));
        assert!(!is_unique_violation(&diesel::result::Error::RollbackTransaction));
    }
}
