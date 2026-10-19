//! `/login-app`, `/login-app-admin`, `/logout` -- see `AccountController@loginApp`,
//! `@loginAppAdmin`, `@logoutApp` (verbatim-read, not just summarized from the doc, given this is
//! the auth entry point). Response shapes are intentionally NOT run through a shared envelope --
//! each matches its controller's literal, ad-hoc JSON exactly.

use crate::auth::{hash_token, mint_token, AuthedTeller};
use crate::extract::JsonOrForm;
use crate::money::Money;
use crate::services::transactions::create_log;
use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "message": message }))).into_response()
}

#[derive(Deserialize)]
pub struct LoginAppRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub phone_uid: String,
}

/// `POST /login-app` -- teller password login. Mirrors `AccountController@loginApp` line for
/// line, including that the device-lock check here only *verifies* `phone_uid` (rejects a
/// mismatch); unlike the QR-login/change-password/register-desktop-uid paths, this endpoint
/// never actually writes `phone_uid` back to the teller row -- confirmed by reading the real
/// source, not assumed from the architecture doc, which described the lock as applying to "the
/// first successful password login" too. It doesn't, in the code as it stands today.
pub async fn login_app(
    State(state): State<Arc<AppState>>,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<LoginAppRequest>,
) -> Response {
    let db = &state.db;

    let event_active = sqlx::query("SELECT 1 FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();

    let acc = sqlx::query("SELECT account_id, is_active FROM accounts WHERE username = ? AND account_type = 'Teller'")
        .bind(&body.username)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(acc_row) = acc else {
        create_log(db, -1, "Teller App", &format!("Login attempt by Username: {}", body.username)).await;
        return fail(StatusCode::UNAUTHORIZED, "Invalid credentials! Please contact administrator");
    };
    let acc_account_id: i32 = acc_row.get("account_id");
    let acc_is_active: i8 = acc_row.get("is_active");
    if acc_is_active == 0 {
        create_log(db, -1, "Teller App", &format!("Login attempt by Username: {}", body.username)).await;
        return fail(StatusCode::UNAUTHORIZED, "Login Failed! Account is not activated, please contact administrator!");
    }

    let row = sqlx::query(
        "SELECT * FROM event_tellers_view WHERE event_status = 'Active' AND username = ?",
    )
    .bind(&body.username)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        create_log(db, -1, "Teller App", &format!("Login attempt by Username: {}", body.username)).await;
        return fail(StatusCode::UNAUTHORIZED, "Invalid credentials! Please contact administrator");
    };

    let password_hash: String = row.get("password");
    let teller_name: String = row.get("teller_name");

    if !crate::auth::verify_password(&body.password, &password_hash) {
        create_log(
            db,
            acc_account_id,
            "Teller App",
            &format!("Failed Login by Teller {teller_name} using device UID: {}, reason incorrect password!", body.phone_uid),
        )
        .await;
        return fail(StatusCode::UNAUTHORIZED, "Incorrect password. Please contact the administrator for assistance.");
    }

    if !event_active {
        create_log(
            db,
            acc_account_id,
            "Teller App",
            &format!("Failed Login by Teller {teller_name} using device UID: {}, reason no available event.", body.phone_uid),
        )
        .await;
        return fail(StatusCode::UNAUTHORIZED, "There is no active event!");
    }

    let existing_phone_uid: String = row.get("phone_uid");
    let teller_id: i32 = row.get("teller_id");

    if !existing_phone_uid.is_empty() {
        if body.phone_uid != existing_phone_uid {
            create_log(
                db,
                acc_account_id,
                "Teller App",
                &format!("Failed Login by Teller {teller_name} using device UID: {}, reason not allowed to login to the phone.", body.phone_uid),
            )
            .await;
            return fail(StatusCode::UNAUTHORIZED, "You are not allowed to login in this phone. Please contact administrator.");
        }
    } else if !body.phone_uid.is_empty() {
        let other = sqlx::query("SELECT teller_name FROM tellers WHERE phone_uid = ? AND teller_id <> ?")
            .bind(&body.phone_uid)
            .bind(teller_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
        if let Some(other) = other {
            let other_name: String = other.get("teller_name");
            create_log(
                db,
                acc_account_id,
                "Teller App",
                &format!("Failed Login by Teller {teller_name} using device UID: {}, reason logging in to other phone already connected to somebody account", body.phone_uid),
            )
            .await;
            return fail(StatusCode::UNAUTHORIZED, &format!("This phone is already linked to {other_name}"));
        }
    }

    create_log(db, acc_account_id, "Teller App", &format!("Login by Teller {teller_name} using device UID: {}", body.phone_uid)).await;
    teller_login_response(&state, row).await
}

async fn teller_login_response(state: &Arc<AppState>, row: sqlx::mysql::MySqlRow) -> Response {
    let db = &state.db;
    let event_teller_id: i32 = row.get("event_teller_id");
    let teller_id: i32 = row.get("teller_id");
    let event_id: i32 = row.get("event_id");

    let token = match mint_token(db, "EventTeller", event_teller_id, state.config.api_token_hours).await {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "message": format!("{e}") }))).into_response(),
    };

    let latest_match = sqlx::query("SELECT match_id, match_number, match_bet_status, meron_bet_status, wala_bet_status FROM matches WHERE event_id = ? ORDER BY match_id DESC LIMIT 1")
        .bind(event_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let match_json: Value = match latest_match {
        Some(m) => json!({
            "match_id": m.get::<i32, _>("match_id"),
            "match_number": m.get::<i32, _>("match_number"),
            "match_bet_status": m.get::<String, _>("match_bet_status"),
            "meron_bet_status": m.get::<i8, _>("meron_bet_status"),
            "wala_bet_status": m.get::<i8, _>("wala_bet_status"),
        }),
        None => Value::Null,
    };

    let teller_balance: Money = row.get("teller_balance");
    let teller_match_balance: Money = row.get("teller_match_balance");

    Json(json!({
        "token": token,
        "teller": {
            "phone_uid": row.get::<String, _>("phone_uid"),
            "event_teller_id": event_teller_id,
            "teller_id": teller_id,
            "teller_name": row.get::<String, _>("teller_name"),
            "teller_balance": teller_balance,
            "teller_match_balance": teller_match_balance,
        },
        "event": {
            "event_id": event_id,
            "event_name": row.get::<String, _>("event_name"),
            "event_status": row.get::<String, _>("event_status"),
        },
        "match": match_json,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct LoginAppAdminRequest {
    pub username: String,
    pub password: String,
}

/// `POST /login-app-admin` -- mirrors `AccountController@loginAppAdmin`. Notably: `admin` in the
/// response is the **entire `admins_view` row**, which includes the bcrypt password hash -- that
/// is what the real app returns, verified against the live `admins_view` definition, not
/// assumed. `cash_in`/`cash_out` are `SUM(...)` over approved cash movements for the Active
/// event; Laravel's query builder `sum()` falls back to the **integer** `0` (not the string
/// `"0.00"`) when there are no matching rows (`$this->aggregate(...) ?: 0`), so those two fields
/// genuinely change JSON type depending on whether any approved cash movement exists yet -- that
/// is replicated here deliberately, not an oversight.
pub async fn login_app_admin(
    State(state): State<Arc<AppState>>,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<LoginAppAdminRequest>,
) -> Response {
    let db = &state.db;

    let event = sqlx::query("SELECT * FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let account = sqlx::query("SELECT * FROM admins_view WHERE username = ? AND is_active = 1")
        .bind(&body.username)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let Some(account) = account else {
        create_log(db, -1, "Admin App", &format!("Login attempt by Username: {}", body.username)).await;
        return fail(StatusCode::UNAUTHORIZED, "Invalid credentials! Please contact administrator");
    };

    let account_id: i32 = account.get("account_id");
    let admin_name: String = account.get("admin_name");

    let Some(event) = event else {
        create_log(db, account_id, "Admin App", &format!("Failed Login by Admin {admin_name}, reason no available event.")).await;
        return fail(StatusCode::UNAUTHORIZED, "No active event available!");
    };

    let password_hash: String = account.get("password");
    if !crate::auth::verify_password(&body.password, &password_hash) {
        create_log(db, account_id, "Admin App", &format!("Failed Login by Admin {admin_name}, reason incorrect password!")).await;
        return fail(StatusCode::UNAUTHORIZED, "Incorrect password. Please contact the administrator for assistance.");
    }

    let admin_id: i32 = account.get("admin_id");
    let event_id: i32 = event.get("event_id");

    let token = match mint_token(db, "EventAdmin", admin_id, state.config.api_token_hours).await {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "message": format!("{e}") }))).into_response(),
    };

    let latest_match = sqlx::query("SELECT match_id, match_number, match_bet_status FROM matches WHERE event_id = ? ORDER BY match_id DESC LIMIT 1")
        .bind(event_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    let match_json: Value = match latest_match {
        Some(m) => json!({
            "match_id": m.get::<i32, _>("match_id"),
            "match_number": m.get::<i32, _>("match_number"),
            "match_bet_status": m.get::<String, _>("match_bet_status"),
        }),
        None => Value::Null,
    };

    let cash_out_sum: Option<Money> = sqlx::query_scalar(
        "SELECT SUM(cash_out_amount) FROM cash_outs_view WHERE event_id = ? AND cash_out_status = 'Approved'",
    )
    .bind(event_id)
    .fetch_one(db)
    .await
    .unwrap_or(None);
    let cash_in_sum: Option<Money> = sqlx::query_scalar(
        "SELECT SUM(cash_in_amount) FROM cash_ins_view WHERE event_id = ? AND cash_in_status = 'Approved'",
    )
    .bind(event_id)
    .fetch_one(db)
    .await
    .unwrap_or(None);

    let cash_out_json = cash_out_sum.map(|m| json!(m)).unwrap_or(json!(0));
    let cash_in_json = cash_in_sum.map(|m| json!(m)).unwrap_or(json!(0));

    let admin_json = admin_row_to_json(&account);
    let event_json = event_row_to_json(&event);

    create_log(db, account_id, "Admin App", &format!("Login by Admin {admin_name}")).await;

    Json(json!({
        "cash_out": cash_out_json,
        "cash_in": cash_in_json,
        "token": token,
        "admin": admin_json,
        "event": event_json,
        "match": match_json,
    }))
    .into_response()
}

fn admin_row_to_json(row: &sqlx::mysql::MySqlRow) -> Value {
    json!({
        "admin_id": row.get::<i32, _>("admin_id"),
        "account_id": row.get::<i32, _>("account_id"),
        "admin_name": row.get::<String, _>("admin_name"),
        "username": row.get::<String, _>("username"),
        "password": row.get::<String, _>("password"),
        "account_type": row.get::<String, _>("account_type"),
        "is_active": row.get::<i8, _>("is_active"),
        "account_date_created": row.get::<crate::types::DbDateTime, _>("account_date_created"),
    })
}

fn event_row_to_json(row: &sqlx::mysql::MySqlRow) -> Value {
    json!({
        "event_id": row.get::<i32, _>("event_id"),
        "event_name": row.get::<String, _>("event_name"),
        "event_description": row.get::<String, _>("event_description"),
        "event_date": row.get::<chrono::NaiveDate, _>("event_date"),
        "event_status": row.get::<String, _>("event_status"),
        "admin_cash_on_hand": row.get::<Money, _>("admin_cash_on_hand"),
        "teller_initial_cash_on_hand": row.get::<Money, _>("teller_initial_cash_on_hand"),
        "prizes": row.get::<Money, _>("prizes"),
        "rd": row.get::<Money, _>("rd"),
        "event_percentage": row.get::<crate::types::Rate, _>("event_percentage"),
        "matching_event_id": row.try_get::<Option<i32>, _>("matching_event_id").unwrap_or(None),
    })
}

/// `POST /logout` -- mirrors `AccountController@logoutApp`: deletes the token row outright
/// (logout is not merely expiry). Note the "not found" case is HTTP 400, not 401 -- that's the
/// real status code in the Laravel source.
pub async fn logout(State(state): State<Arc<AppState>>, headers: axum::http::HeaderMap) -> Response {
    let Some(auth) = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return fail(StatusCode::BAD_REQUEST, "Token not found");
    };
    let Some(token) = auth.strip_prefix("Bearer ") else {
        return fail(StatusCode::BAD_REQUEST, "Token not found");
    };

    let hashed = hash_token(token.trim());
    let existing = sqlx::query("SELECT id FROM personal_access_tokens WHERE token = ?")
        .bind(&hashed)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

    if existing.is_none() {
        return fail(StatusCode::BAD_REQUEST, "Token not found");
    }

    let _ = sqlx::query("DELETE FROM personal_access_tokens WHERE token = ?")
        .bind(&hashed)
        .execute(&state.db)
        .await;

    (StatusCode::OK, Json(json!({ "message": "Logged out" }))).into_response()
}

#[derive(Deserialize)]
pub struct LoginQrRequest {
    pub qr_code: String,
    pub phone_uid: String,
}

/// `POST /login-qr` -- mirrors `AccountController@loginAppQr`. The first scan locks
/// (activates) the phone for this teller; after that it behaves like the password login's
/// device check.
pub async fn login_qr(State(state): State<Arc<AppState>>, JsonOrForm(body): JsonOrForm<LoginQrRequest>) -> Response {
    let db = &state.db;
    if body.qr_code.is_empty() || body.qr_code.chars().count() > 200 || body.phone_uid.is_empty() || body.phone_uid.chars().count() > 100 {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "The given data was invalid.");
    }

    let teller_row = sqlx::query("SELECT teller_id, account_id, teller_name FROM tellers WHERE qr_token_hash = ?")
        .bind(hash_token(&body.qr_code))
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    let Some(teller_row) = teller_row else {
        create_log(db, -1, "Teller App", &format!("QR login attempt with an invalid or replaced QR code, device UID: {}", body.phone_uid)).await;
        return fail(StatusCode::UNAUTHORIZED, "Invalid or outdated QR code. Please ask the administrator for a new one.");
    };
    let teller_id: i32 = teller_row.get("teller_id");
    let account_id: i32 = teller_row.get("account_id");
    let teller_name: String = teller_row.get("teller_name");

    let acc_is_active: Option<i8> = sqlx::query_scalar("SELECT is_active FROM accounts WHERE account_id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    if acc_is_active != Some(1) {
        create_log(db, account_id, "Teller App", &format!("QR login attempt by Teller {teller_name}, reason account not activated.")).await;
        return fail(StatusCode::UNAUTHORIZED, "Login Failed! Account is not activated, please contact administrator!");
    }

    let event_active = sqlx::query("SELECT 1 FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
    if !event_active {
        create_log(db, account_id, "Teller App", &format!("Failed QR Login by Teller {teller_name} using device UID: {}, reason no available event.", body.phone_uid)).await;
        return fail(StatusCode::UNAUTHORIZED, "There is no active event!");
    }

    let username: Option<String> = sqlx::query_scalar("SELECT username FROM accounts WHERE account_id = ?")
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    let Some(username) = username else {
        return fail(StatusCode::UNAUTHORIZED, "Invalid credentials! Please contact administrator");
    };

    let assigned = sqlx::query("SELECT 1 FROM event_tellers_view WHERE event_status = 'Active' AND username = ?")
        .bind(&username)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    if assigned.is_none() {
        create_log(db, account_id, "Teller App", &format!("Failed QR Login by Teller {teller_name}, reason not assigned to the active event.")).await;
        return fail(StatusCode::UNAUTHORIZED, "You are not assigned to the active event. Please contact administrator.");
    }

    // Link the phone on first scan; the teller row is locked so two phones scanning the same
    // QR at once cannot both be linked.
    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    let locked = match sqlx::query("SELECT phone_uid FROM tellers WHERE teller_id = ? FOR UPDATE")
        .bind(teller_id)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(r) => r,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    let existing_phone_uid: String = locked.get("phone_uid");

    if !existing_phone_uid.is_empty() {
        if body.phone_uid != existing_phone_uid {
            let _ = tx.rollback().await;
            create_log(db, account_id, "Teller App", &format!("Failed QR Login by Teller {teller_name} using device UID: {}, reason not allowed to login to the phone.", body.phone_uid)).await;
            return fail(StatusCode::UNAUTHORIZED, "You are not allowed to login in this phone. Please contact administrator.");
        }
    } else {
        let other = sqlx::query("SELECT teller_name FROM tellers WHERE phone_uid = ? AND teller_id <> ?")
            .bind(&body.phone_uid)
            .bind(teller_id)
            .fetch_optional(&mut *tx)
            .await
            .ok()
            .flatten();
        if let Some(other) = other {
            let other_name: String = other.get("teller_name");
            let _ = tx.rollback().await;
            create_log(db, account_id, "Teller App", &format!("Failed QR Login by Teller {teller_name} using device UID: {}, reason logging in to other phone already connected to somebody account", body.phone_uid)).await;
            return fail(StatusCode::UNAUTHORIZED, &format!("This phone is already linked to {other_name}"));
        }
        if let Err(e) = sqlx::query("UPDATE tellers SET phone_uid = ? WHERE teller_id = ?")
            .bind(&body.phone_uid)
            .bind(teller_id)
            .execute(&mut *tx)
            .await
        {
            let _ = tx.rollback().await;
            return fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
        }
        create_log(&mut *tx, account_id, "Teller App", &format!("Device activated by QR for Teller {teller_name}, device UID: {}", body.phone_uid)).await;
    }

    if let Err(e) = tx.commit().await {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }

    create_log(db, account_id, "Teller App", &format!("QR Login by Teller {teller_name} using device UID: {}", body.phone_uid)).await;

    let fresh_row = sqlx::query("SELECT * FROM event_tellers_view WHERE event_status = 'Active' AND username = ?")
        .bind(&username)
        .fetch_one(db)
        .await;
    match fresh_row {
        Ok(row) => teller_login_response(&state, row).await,
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub new_password: String,
    #[serde(default)]
    pub phone_uid: String,
}

/// `POST /change-password` -- mirrors `AccountController@changePasswordApp`. Note this
/// unconditionally overwrites `tellers.phone_uid` from the request, unlike the login paths'
/// conflict-checked device lock -- that's the real behavior, not an oversight on my part.
pub async fn change_password(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    JsonOrForm(body): JsonOrForm<ChangePasswordRequest>,
) -> Response {
    if body.new_password.chars().count() < 4 || body.new_password.chars().count() > 100 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "message": "The new password field must be between 4 and 100 characters." })),
        )
            .into_response();
    }

    let hashed = match bcrypt::hash(&body.new_password, bcrypt::DEFAULT_COST) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "An error occurred while changing the password.", "error": format!("{e}") })),
            )
                .into_response()
        }
    };

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response(),
    };

    let teller_name: Option<String> = sqlx::query_scalar("SELECT teller_name FROM tellers WHERE teller_id = ?")
        .bind(teller.teller_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();

    if let Err(e) = sqlx::query("UPDATE accounts SET password = ? WHERE account_id = ?")
        .bind(&hashed)
        .bind(teller.account_id)
        .execute(&mut *tx)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "An error occurred while changing the password.", "error": format!("{e}") })),
        )
            .into_response();
    }
    if let Err(e) = sqlx::query("UPDATE tellers SET phone_uid = ? WHERE teller_id = ?")
        .bind(&body.phone_uid)
        .bind(teller.teller_id)
        .execute(&mut *tx)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "An error occurred while changing the password.", "error": format!("{e}") })),
        )
            .into_response();
    }

    create_log(
        &mut *tx,
        teller.account_id,
        "Teller App",
        &format!(
            "Change password by Teller #{} ({}), device UID: {}",
            teller.teller_id,
            teller_name.unwrap_or_default(),
            body.phone_uid
        ),
    )
    .await;

    if let Err(e) = tx.commit().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "message": format!("{e}") }))).into_response();
    }

    Json(json!({ "success": true, "message": "Password changed successfully!" })).into_response()
}

#[derive(Deserialize)]
pub struct RegisterDesktopUidRequest {
    #[serde(default)]
    pub phone_uid: String,
}

/// `POST /register-desktop-uid` -- mirrors `AccountController@registerDesktopUID`.
pub async fn register_desktop_uid(
    State(state): State<Arc<AppState>>,
    teller: AuthedTeller,
    JsonOrForm(body): JsonOrForm<RegisterDesktopUidRequest>,
) -> Response {
    let teller_name: Option<String> = sqlx::query_scalar("SELECT teller_name FROM tellers WHERE teller_id = ?")
        .bind(teller.teller_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(teller_name) = teller_name else {
        return (StatusCode::NOT_FOUND, Json(json!({ "success": false, "message": "Teller not found" }))).into_response();
    };

    if let Err(e) = sqlx::query("UPDATE tellers SET phone_uid = ? WHERE teller_id = ?")
        .bind(&body.phone_uid)
        .bind(teller.teller_id)
        .execute(&state.db)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "An error occurred.", "error": format!("{e}") })),
        )
            .into_response();
    }

    create_log(&state.db, teller.account_id, "Desktop App", &format!("Registered desktop UID: {} for teller {teller_name}", body.phone_uid)).await;

    Json(json!({ "success": true, "message": "Desktop UID registered successfully" })).into_response()
}
