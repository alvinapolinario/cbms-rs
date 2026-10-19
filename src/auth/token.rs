//! Bearer-token auth for the teller and admin apps.
//!
//! Sabong2026 writes the plaintext `Str::random(60)` value into `personal_access_tokens.token`
//! (`name` is `teller-token` or `admin-token`) and does not set `expires_at`. Its
//! `account.token` middleware is currently a no-op. This API still requires the bearer token,
//! and it looks that plaintext value up, so a token minted by Laravel login works here and a
//! token minted here works against Laravel logout. This API looks the plaintext value up.
//! Sanctum's hashed-token guard is not used.

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{TOKENABLE_TYPE_ADMIN, TOKENABLE_TYPE_TELLER};
use crate::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::Utc;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::sync::Arc;

const TOKEN_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// 60-character alphanumeric string, the same shape as Laravel `Str::random(60)`.
/// The value is stored as plaintext, so either app can look up a token the other minted.
pub fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    (0..60)
        .map(|_| TOKEN_ALPHABET[rng.gen_range(0..TOKEN_ALPHABET.len())] as char)
        .collect()
}

/// Matches PHP's `hash('sha256', $token)`.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Inserts a plaintext token row the way `AccountController@loginApp` / `loginAppAdmin` do,
/// and returns that same string to the client.
pub async fn mint_token(
    pool: &DbPool,
    tokenable_type: &str,
    tokenable_id: i32,
    name: &str,
) -> Result<String, sqlx::Error> {
    let plain = generate_token();
    sqlx::query(
        "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, abilities, created_at, updated_at)
         VALUES (?, ?, ?, ?, '[\"*\"]', NOW(), NOW())",
    )
    .bind(tokenable_type)
    .bind(tokenable_id)
    .bind(name)
    .bind(&plain)
    .execute(pool)
    .await?;
    Ok(plain)
}

/// Deletes a token row outright (matching `AccountController@logoutApp`'s "logout deletes the
/// token, it doesn't merely expire it" behavior).
pub async fn revoke_token(pool: &DbPool, plain_token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM personal_access_tokens WHERE token = ?")
        .bind(plain_token)
        .execute(pool)
        .await?;
    Ok(())
}

struct ResolvedToken {
    id: u64,
    tokenable_type: String,
    /// Stored in a `bigint unsigned` column, but the values are always small ids originally
    /// inserted from an `admin_id`/`event_teller_id` (regular `int(11)`); narrowed to `i32`
    /// wherever it's used to query those tables.
    tokenable_id: i32,
}

fn bearer_token(parts: &Parts) -> Option<String> {
    let header = parts.headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    header.strip_prefix("Bearer ").map(|s| s.trim().to_string())
}

/// Writes a `transactions` row for a denied request -- parity with `CheckAccountToken`'s
/// `logDenied()`. The exact message text isn't guaranteed byte-identical to the PHP version
/// (that's a log string, not API response data covered by the parity goal), but it records the
/// same facts: which token, which path, why it was denied.
async fn log_denied(pool: &DbPool, path: &str, reason: &str, token_id: Option<u64>) {
    let message = match token_id {
        Some(id) => format!("API access DENIED on {path}: {reason} (token #{id})"),
        None => format!("API access DENIED on {path}: {reason}"),
    };
    let _ = sqlx::query(
        "INSERT INTO transactions (account_id, transaction_type, transaction_message, transaction_datetime)
         VALUES (-1, 'API', ?, NOW())",
    )
    .bind(message)
    .execute(pool)
    .await;
}

async fn resolve_token(pool: &DbPool, plain: &str) -> Result<ResolvedToken, AppError> {
    // `expires_at` is a Sanctum `timestamp`. sqlx's MySQL driver will not decode that as
    // `NaiveDateTime`, so this column stays `DateTime<Utc>`. Sabong2026 leaves it NULL.
    // A non-null value is still honored so an older row cannot be used past its expiry.
    let row = sqlx::query_as::<_, (u64, String, u64, Option<chrono::DateTime<Utc>>)>(
        "SELECT id, tokenable_type, tokenable_id, expires_at FROM personal_access_tokens WHERE token = ?",
    )
    .bind(plain)
    .fetch_optional(pool)
    .await
    .map_err(AppError::internal)?;

    let (id, tokenable_type, tokenable_id, expires_at) =
        row.ok_or_else(|| AppError::Unauthorized("Unauthenticated.".into()))?;
    let tokenable_id = tokenable_id as i32;

    if let Some(expires_at) = expires_at {
        if expires_at <= Utc::now() {
            return Err(AppError::Unauthorized("Token expired.".into()));
        }
    }

    let _ = sqlx::query("UPDATE personal_access_tokens SET last_used_at = NOW() WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await;

    Ok(ResolvedToken { id, tokenable_type, tokenable_id })
}

/// The authenticated admin, resolved from the bearer token. Use as an Axum extractor:
/// `async fn handler(admin: AuthedAdmin, ...)`.
pub struct AuthedAdmin {
    pub admin_id: i32,
    pub account_id: i32,
    pub token_id: u64,
}

#[axum::async_trait]
impl FromRequestParts<Arc<AppState>> for AuthedAdmin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let path = parts.uri.path().to_string();
        let Some(plain) = bearer_token(parts) else {
            return Err(AppError::Unauthorized("Unauthenticated.".into()));
        };
        let resolved = resolve_token(&state.db, &plain).await?;

        if resolved.tokenable_type != TOKENABLE_TYPE_ADMIN {
            log_denied(&state.db, &path, "role mismatch (admin required)", Some(resolved.id)).await;
            return Err(AppError::Forbidden("Not allowed for this account.".into()));
        }

        let row = sqlx::query_as::<_, (i32, i32, i8)>(
            "SELECT a.admin_id, a.account_id, acc.is_active
             FROM admins a JOIN accounts acc ON acc.account_id = a.account_id
             WHERE a.admin_id = ?",
        )
        .bind(resolved.tokenable_id)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::internal)?;

        let Some((admin_id, account_id, is_active)) = row else {
            return Err(AppError::Unauthorized("Unauthenticated.".into()));
        };
        if is_active == 0 {
            return Err(AppError::Unauthorized("Account is deactivated.".into()));
        }

        Ok(AuthedAdmin { admin_id, account_id, token_id: resolved.id })
    }
}

/// The authenticated teller, resolved from the bearer token, with the event confirmed `Active`.
/// Use as an Axum extractor: `async fn handler(teller: AuthedTeller, ...)`.
pub struct AuthedTeller {
    pub event_teller_id: i32,
    pub teller_id: i32,
    pub account_id: i32,
    pub event_id: i32,
    pub token_id: u64,
}

#[axum::async_trait]
impl FromRequestParts<Arc<AppState>> for AuthedTeller {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let path = parts.uri.path().to_string();
        let Some(plain) = bearer_token(parts) else {
            return Err(AppError::Unauthorized("Unauthenticated.".into()));
        };
        let resolved = resolve_token(&state.db, &plain).await?;

        if resolved.tokenable_type != TOKENABLE_TYPE_TELLER {
            log_denied(&state.db, &path, "role mismatch (teller required)", Some(resolved.id)).await;
            return Err(AppError::Forbidden("Not allowed for this account.".into()));
        }

        // event_tellers -> events -> tellers -> accounts, confirming the event is still Active
        // and the account is still active (docs/WORKFLOW.md section 5).
        let row = sqlx::query_as::<_, (i32, i32, i32, i32, String, i8)>(
            "SELECT et.event_teller_id, et.teller_id, et.event_id, acc.account_id, ev.event_status, acc.is_active
             FROM event_tellers et
             JOIN events ev ON ev.event_id = et.event_id
             JOIN tellers t ON t.teller_id = et.teller_id
             JOIN accounts acc ON acc.account_id = t.account_id
             WHERE et.event_teller_id = ?",
        )
        .bind(resolved.tokenable_id)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::internal)?;

        let Some((event_teller_id, teller_id, event_id, account_id, event_status, is_active)) = row else {
            return Err(AppError::Unauthorized("Unauthenticated.".into()));
        };
        if event_status != "Active" {
            return Err(AppError::Unauthorized("Event is no longer active.".into()));
        }
        if is_active == 0 {
            return Err(AppError::Unauthorized("Account is deactivated.".into()));
        }

        Ok(AuthedTeller { event_teller_id, teller_id, account_id, event_id, token_id: resolved.id })
    }
}

/// Parity with `CheckAccountToken`'s identity-overwrite rule: if a request body claims an
/// `event_teller_id`/`teller_id`/`admin_id` that disagrees with the token's own identity, the
/// request is rejected (403) rather than silently trusting the client-supplied value. Laravel
/// applies this inside its global auth middleware (before the controller ever runs); Axum's
/// extractor model doesn't give the same free access to an arbitrary not-yet-typed request body,
/// so each handler that accepts one of these fields in its request struct calls this explicitly
/// right after extracting its typed body -- same observable 403 behavior, different layer.
pub fn check_identity_claim(claimed: Option<i32>, actual: i32) -> Result<(), AppError> {
    match claimed {
        Some(claimed_id) if claimed_id != actual => {
            Err(AppError::Forbidden("Not allowed for this account.".into()))
        }
        _ => Ok(()),
    }
}
