use crate::types::DbDateTime;

/// `personal_access_tokens` -- Sanctum's table, reused as a bespoke hashed-token store (Sanctum's
/// own guard is never used). `tokenable_type` is literally the string `'EventAdmin'` or
/// `'EventTeller'` (not an Eloquent model class), and `tokenable_id` is `admin_id` or
/// `event_teller_id` respectively. See docs/WORKFLOW.md section 5.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PersonalAccessToken {
    pub id: i64,
    pub tokenable_type: String,
    pub tokenable_id: i64,
    /// SHA-256 hex hash of the bearer token presented by the client. The plaintext token is
    /// never stored anywhere, only returned to the client once at login.
    pub token: String,
    pub last_used_at: Option<DbDateTime>,
    pub expires_at: Option<DbDateTime>,
}

pub const TOKENABLE_TYPE_ADMIN: &str = "EventAdmin";
pub const TOKENABLE_TYPE_TELLER: &str = "EventTeller";
