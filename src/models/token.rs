use crate::types::DbDateTime;

/// `personal_access_tokens`. Sabong2026 stores the bearer token in plaintext.
/// `tokenable_type` is `'EventAdmin'` or `'EventTeller'`, and `tokenable_id` is
/// `admin_id` or `event_teller_id`. `name` is `admin-token` or `teller-token`.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PersonalAccessToken {
    pub id: i64,
    pub tokenable_type: String,
    pub tokenable_id: i64,
    /// Plaintext bearer token, the same string returned at login.
    pub token: String,
    pub last_used_at: Option<DbDateTime>,
    pub expires_at: Option<DbDateTime>,
}

pub const TOKENABLE_TYPE_ADMIN: &str = "EventAdmin";
pub const TOKENABLE_TYPE_TELLER: &str = "EventTeller";
