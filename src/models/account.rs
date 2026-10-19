use crate::types::DbDateTime;
use serde::Serialize;

/// `accounts` -- the login table for both Admin and Teller accounts.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Account {
    pub account_id: i32,
    pub username: String,
    /// bcrypt hash. Never returned in any Rust response body -- see `AdminRow`/`admins_view`
    /// note below for where Laravel itself leaks this, which we replicate but flag separately.
    #[serde(skip_serializing)]
    pub password: String,
    pub account_type: String,
    pub is_active: i8,
    pub account_date_created: DbDateTime,
}

/// `admins` -- profile row for an Admin account.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Admin {
    pub admin_id: i32,
    pub account_id: i32,
    pub admin_name: String,
}

/// `admins_view` row: admin profile joined with its account. NOTE: the real Laravel view
/// (`admins_view`) selects `accounts.password` (the bcrypt hash) alongside everything else, and
/// `AccountController@loginAppAdmin` returns this *entire row*, hash included, as the `admin`
/// field of its JSON response -- confirmed against the live DB's `SHOW CREATE VIEW admins_view`.
/// This struct replicates that faithfully for parity (see the plan's privacy note), but the
/// field is named `password` here too so it's obvious what's happening; nothing in this
/// implementation should ever log or otherwise use this value beyond returning it verbatim where
/// Laravel does. Consider removing it in a security-hardening pass once parity is confirmed --
/// that's a deliberate, flagged deviation, not an accident.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct AdminViewRow {
    pub admin_id: i32,
    pub account_id: i32,
    pub admin_name: String,
    pub username: String,
    pub password: String,
    pub account_type: String,
    pub is_active: i8,
    pub account_date_created: DbDateTime,
}

/// `tellers` -- profile row for a Teller account.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Teller {
    pub teller_id: i32,
    pub account_id: i32,
    pub teller_name: String,
    pub contact_number: String,
    pub phone_uid: String,
    pub qr_token_hash: Option<String>,
    pub qr_token: Option<String>,
    pub qr_generated_at: Option<DbDateTime>,
}

/// Manual `Serialize` (not derived) because `qr_token_hash`/`qr_token` are `$hidden` on the
/// Eloquent model -- excluded from every `toArray()`/`toJson()`, so they must never appear in
/// any response body built from this struct.
impl Serialize for Teller {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Teller", 5)?;
        s.serialize_field("teller_id", &self.teller_id)?;
        s.serialize_field("account_id", &self.account_id)?;
        s.serialize_field("teller_name", &self.teller_name)?;
        s.serialize_field("contact_number", &self.contact_number)?;
        s.serialize_field("phone_uid", &self.phone_uid)?;
        s.end()
    }
}
