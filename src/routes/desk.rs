//! Website actions from `web.php` that are not already on `api.php`.
//!
//! The Blade site uses a session. This service has no session, so these routes require the
//! admin bearer token from `POST /api/login-app-admin`. Bodies are JSON or form fields, and
//! the `1` / `0` / `-1` replies are plain text, the same bytes Laravel returns from `return 1`.
//!
//! Events and the fight controls already live on `/events-admin/*` and `/matches-admin/*`.
//! `MatchController` has no `find` or `delete` method even though `web.php` names those
//! routes, so they are not added here. Reset and restore stay off the API: they wipe or
//! reload the database and are an operator step, described in `docs/SABONG2026.md`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

use crate::auth::{hash_password, AuthedAdmin};
use crate::extract::JsonOrForm;
use crate::money::Money;
use crate::rows::{row_to_json, rows_to_json};
use crate::services::realtime;
use crate::services::transactions::create_log;
use crate::types::DbDateTime;
use crate::AppState;

fn scalar(n: i32) -> Response {
    (StatusCode::OK, n.to_string()).into_response()
}

fn json_error(status: StatusCode, key: &str, message: &str) -> Response {
    (status, Json(json!({ key: message }))).into_response()
}

async fn rows(state: &AppState, sql: &str) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(sql).fetch_all(&state.db).await?;
    Ok(rows_to_json(&rows))
}

async fn one_row(state: &AppState, sql: &str, id: i32) -> Result<Value, sqlx::Error> {
    let row = sqlx::query(sql).bind(id).fetch_optional(&state.db).await?;
    Ok(row.as_ref().map(row_to_json).unwrap_or(Value::Null))
}

pub async fn admin_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    match rows(&state, "SELECT * FROM admins_view").await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn admin_find(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(admin_id): Path<i32>) -> Response {
    match one_row(&state, "SELECT * FROM admins_view WHERE admin_id = ?", admin_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

#[derive(Deserialize)]
pub struct AdminStore {
    pub username: String,
    pub password: String,
    pub admin_name: String,
}

pub async fn admin_store(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<AdminStore>,
) -> Response {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE username = ?")
        .bind(&body.username)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    if exists > 0 {
        return scalar(-1);
    }
    let Ok(hash) = hash_password(&body.password) else {
        return scalar(0);
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    let account = sqlx::query(
        "INSERT INTO accounts (username, password, account_type, is_active, account_date_created)
         VALUES (?, ?, 'Admin', 1, NOW())",
    )
    .bind(&body.username)
    .bind(&hash)
    .execute(&mut *tx)
    .await;
    let Ok(account) = account else {
        return scalar(0);
    };
    let created = sqlx::query("INSERT INTO admins (account_id, admin_name) VALUES (?, ?)")
        .bind(account.last_insert_id() as i32)
        .bind(&body.admin_name)
        .execute(&mut *tx)
        .await;
    if created.is_err() {
        return scalar(0);
    }
    create_log(&mut *tx, admin.account_id, "Web App", "Admin - Admin created.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

#[derive(Deserialize)]
pub struct AdminUpdate {
    pub uid: i32,
    #[serde(default, rename = "_password", deserialize_with = "empty_string_as_none")]
    pub password: Option<String>,
    #[serde(rename = "_is_active", deserialize_with = "flex_i8")]
    pub is_active: i8,
    #[serde(rename = "_admin_name")]
    pub admin_name: String,
}

pub async fn admin_update(
    State(state): State<Arc<AppState>>,
    actor: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<AdminUpdate>,
) -> Response {
    let account_id: Option<i32> = sqlx::query_scalar("SELECT account_id FROM admins WHERE admin_id = ?")
        .bind(body.uid)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(account_id) = account_id else {
        return scalar(0);
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    if let Some(password) = body.password.as_deref() {
        let Ok(hash) = hash_password(password) else {
            return scalar(0);
        };
        if sqlx::query("UPDATE accounts SET password = ? WHERE account_id = ?")
            .bind(hash)
            .bind(account_id)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return scalar(0);
        }
    }
    if sqlx::query("UPDATE accounts SET is_active = ? WHERE account_id = ?")
        .bind(body.is_active)
        .bind(account_id)
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return scalar(0);
    }
    if sqlx::query("UPDATE admins SET admin_name = ? WHERE admin_id = ?")
        .bind(&body.admin_name)
        .bind(body.uid)
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return scalar(0);
    }
    create_log(&mut *tx, actor.account_id, "Web App", "Admin - Admin updated.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

pub async fn admin_delete(State(state): State<Arc<AppState>>, actor: AuthedAdmin, Path(admin_id): Path<i32>) -> Response {
    let account_id: Option<i32> = sqlx::query_scalar("SELECT account_id FROM admins WHERE admin_id = ?")
        .bind(admin_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(account_id) = account_id else {
        return scalar(0);
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    if sqlx::query("DELETE FROM admins WHERE admin_id = ?").bind(admin_id).execute(&mut *tx).await.is_err() {
        return scalar(0);
    }
    if sqlx::query("DELETE FROM accounts WHERE account_id = ?").bind(account_id).execute(&mut *tx).await.is_err() {
        return scalar(0);
    }
    create_log(&mut *tx, actor.account_id, "Web App", "Admin - Admin deleted.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

pub async fn teller_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    match rows(&state, "SELECT * FROM tellers_view").await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn teller_find(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(teller_id): Path<i32>) -> Response {
    match one_row(&state, "SELECT * FROM tellers_view WHERE teller_id = ?", teller_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

#[derive(Deserialize)]
pub struct TellerStore {
    pub username: String,
    pub password: String,
    pub teller_name: String,
    pub contact_number: String,
}

pub async fn teller_store(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<TellerStore>,
) -> Response {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE username = ?")
        .bind(&body.username)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    if exists > 0 {
        return scalar(-1);
    }
    let Ok(hash) = hash_password(&body.password) else {
        return scalar(0);
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    let account = sqlx::query(
        "INSERT INTO accounts (username, password, account_type, is_active, account_date_created)
         VALUES (?, ?, 'Teller', 1, NOW())",
    )
    .bind(&body.username)
    .bind(&hash)
    .execute(&mut *tx)
    .await;
    let Ok(account) = account else {
        return scalar(0);
    };
    let created = sqlx::query(
        "INSERT INTO tellers (account_id, teller_name, contact_number) VALUES (?, ?, ?)",
    )
    .bind(account.last_insert_id() as i32)
    .bind(&body.teller_name)
    .bind(&body.contact_number)
    .execute(&mut *tx)
    .await;
    if created.is_err() {
        return scalar(0);
    }
    create_log(&mut *tx, admin.account_id, "Web App", "Teller - Teller created.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

#[derive(Deserialize)]
pub struct TellerUpdate {
    pub uid: i32,
    #[serde(default, rename = "_password", deserialize_with = "empty_string_as_none")]
    pub password: Option<String>,
    #[serde(rename = "_is_active", deserialize_with = "flex_i8")]
    pub is_active: i8,
    #[serde(rename = "_teller_name")]
    pub teller_name: String,
    #[serde(rename = "_contact_number")]
    pub contact_number: String,
}

pub async fn teller_update(
    State(state): State<Arc<AppState>>,
    actor: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<TellerUpdate>,
) -> Response {
    let account_id: Option<i32> = sqlx::query_scalar("SELECT account_id FROM tellers WHERE teller_id = ?")
        .bind(body.uid)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(account_id) = account_id else {
        return scalar(0);
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    if let Some(password) = body.password.as_deref() {
        let Ok(hash) = hash_password(password) else {
            return scalar(0);
        };
        if sqlx::query("UPDATE accounts SET password = ? WHERE account_id = ?")
            .bind(hash)
            .bind(account_id)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return scalar(0);
        }
    }
    if sqlx::query("UPDATE accounts SET is_active = ? WHERE account_id = ?")
        .bind(body.is_active)
        .bind(account_id)
        .execute(&mut *tx)
        .await
        .is_err()
        || sqlx::query("UPDATE tellers SET teller_name = ?, contact_number = ? WHERE teller_id = ?")
            .bind(&body.teller_name)
            .bind(&body.contact_number)
            .bind(body.uid)
            .execute(&mut *tx)
            .await
            .is_err()
    {
        return scalar(0);
    }
    create_log(&mut *tx, actor.account_id, "Web App", "Teller - Teller updated.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

pub async fn teller_clear_phone(State(state): State<Arc<AppState>>, actor: AuthedAdmin, Path(teller_id): Path<i32>) -> Response {
    let updated = sqlx::query("UPDATE tellers SET phone_uid = '' WHERE teller_id = ?")
        .bind(teller_id)
        .execute(&state.db)
        .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, actor.account_id, "Web App", "Teller - Teller phone uid cleared.").await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

pub async fn teller_delete(State(state): State<Arc<AppState>>, actor: AuthedAdmin, Path(teller_id): Path<i32>) -> Response {
    let account_id: Option<i32> = sqlx::query_scalar("SELECT account_id FROM tellers WHERE teller_id = ?")
        .bind(teller_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(account_id) = account_id else {
        return scalar(0);
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    if sqlx::query("DELETE FROM tellers WHERE teller_id = ?").bind(teller_id).execute(&mut *tx).await.is_err()
        || sqlx::query("DELETE FROM accounts WHERE account_id = ?").bind(account_id).execute(&mut *tx).await.is_err()
    {
        return scalar(0);
    }
    create_log(&mut *tx, actor.account_id, "Web App", "Teller - Teller deleted.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

#[derive(Deserialize)]
pub struct WebCashIn {
    pub event_teller_id: i32,
    pub cash_in_amount: Money,
}

/// `CashInController@storeByAdmin`. Returns `1` or `0`.
pub async fn cash_in_store(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<WebCashIn>,
) -> Response {
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    let now = DbDateTime::now();
    let inserted = sqlx::query(
        "INSERT INTO cash_ins (admin_id, event_teller_id, cash_in_amount, cash_in_datetime, cash_in_type, cash_in_status)
         VALUES (?, ?, ?, ?, 'AdminCashIn', 'Approved')",
    )
    .bind(admin.admin_id)
    .bind(body.event_teller_id)
    .bind(body.cash_in_amount.to_db_string())
    .bind(now.0)
    .execute(&mut *tx)
    .await;
    if inserted.is_err()
        || sqlx::query("UPDATE event_tellers SET teller_balance = teller_balance + ? WHERE event_teller_id = ?")
            .bind(body.cash_in_amount.to_db_string())
            .bind(body.event_teller_id)
            .execute(&mut *tx)
            .await
            .is_err()
    {
        return scalar(0);
    }
    create_log(&mut *tx, admin.account_id, "Web App", "Cash Transactions - Cash-in created.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    if let Some(event_id) = realtime::event_id_of_teller(&state.db, body.event_teller_id).await {
        realtime::teller_balances(&state, event_id).await;
        realtime::cash_ins(&state, event_id).await;
    }
    scalar(1)
}

#[derive(Deserialize)]
pub struct CashId {
    pub cash_in_id: i32,
}

#[derive(Deserialize)]
pub struct CashOutId {
    pub cash_out_id: i32,
}

pub async fn cash_in_approve(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashId>,
) -> Response {
    settle_cash(&state, admin, body.cash_in_id, true, true).await
}

pub async fn cash_in_decline(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashId>,
) -> Response {
    settle_cash(&state, admin, body.cash_in_id, true, false).await
}

pub async fn cash_out_approve(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashOutId>,
) -> Response {
    settle_cash(&state, admin, body.cash_out_id, false, true).await
}

pub async fn cash_out_decline(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<CashOutId>,
) -> Response {
    settle_cash(&state, admin, body.cash_out_id, false, false).await
}

async fn settle_cash(state: &AppState, admin: AuthedAdmin, id: i32, is_in: bool, approve: bool) -> Response {
    let table = if is_in { "cash_ins" } else { "cash_outs" };
    let id_col = if is_in { "cash_in_id" } else { "cash_out_id" };
    let status_col = if is_in { "cash_in_status" } else { "cash_out_status" };
    let amount_col = if is_in { "cash_in_amount" } else { "cash_out_amount" };
    let current: Option<String> = sqlx::query_scalar(&format!("SELECT {status_col} FROM {table} WHERE {id_col} = ?"))
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if current.as_deref() != Some("Pending") {
        return json_error(StatusCode::BAD_REQUEST, "error", "Invalid request");
    }
    let status = if approve { "Approved" } else { "Declined" };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "error", "Approval failed. Please try again."),
    };
    let updated = sqlx::query(&format!(
        "UPDATE {table} SET {status_col} = ?, admin_id = ? WHERE {id_col} = ? AND {status_col} = 'Pending'"
    ))
    .bind(status)
    .bind(admin.admin_id)
    .bind(id)
    .execute(&mut *tx)
    .await;
    if !matches!(updated, Ok(r) if r.rows_affected() == 1) {
        return json_error(StatusCode::BAD_REQUEST, "error", "Invalid request");
    }
    let pair: Result<(i32, Money), _> = sqlx::query_as(&format!(
        "SELECT event_teller_id, {amount_col} FROM {table} WHERE {id_col} = ?"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await;
    let Ok((event_teller_id, amount)) = pair else {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "error", "Approval failed. Please try again.");
    };
    if approve {
        let sql = if is_in {
            "UPDATE event_tellers SET teller_balance = teller_balance + ? WHERE event_teller_id = ?"
        } else {
            "UPDATE event_tellers SET teller_balance = teller_balance - ? WHERE event_teller_id = ?"
        };
        if sqlx::query(sql)
            .bind(amount.to_db_string())
            .bind(event_teller_id)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "error", "Approval failed. Please try again.");
        }
    }
    let action = if is_in { "Cash-in" } else { "Cash-out" };
    let verb = if approve { "approved" } else { "declined" };
    create_log(
        &mut *tx,
        admin.account_id,
        "Web App",
        &format!("Cash Transactions - {action} {verb}."),
    )
    .await;
    if tx.commit().await.is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "error", "Approval failed. Please try again.");
    }
    if let Some(event_id) = realtime::event_id_of_teller(&state.db, event_teller_id).await {
        realtime::teller_balances(state, event_id).await;
        if is_in {
            realtime::cash_ins(state, event_id).await;
        } else {
            realtime::cash_outs(state, event_id).await;
        }
    }
    let message = match (is_in, approve) {
        (true, true) => "The cash-in request has been approved.",
        (true, false) => "Cash-in approved successfully.",
        (false, true) => "Cash-out approved successfully.",
        (false, false) => "Cash-out has been approved successfully!",
    };
    Json(json!({ "success": message })).into_response()
}

#[derive(Deserialize)]
pub struct EventIdBody {
    pub event_id: i32,
}

/// Cash desk payload the website used to receive over websockets: tellers on the event,
/// plus that event's cash-in and cash-out rows.
pub async fn cash_board(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<EventIdBody>,
) -> Response {
    let tellers = sqlx::query(
        "SELECT event_teller_id, event_id, teller_id, teller_name, teller_balance, teller_match_balance,
                contact_number, is_active
         FROM event_tellers_view WHERE event_id = ? ORDER BY teller_name ASC",
    )
    .bind(body.event_id)
    .fetch_all(&state.db)
    .await;
    let cash_ins = sqlx::query("SELECT * FROM cash_ins_view WHERE event_id = ? ORDER BY cash_in_id DESC")
        .bind(body.event_id)
        .fetch_all(&state.db)
        .await;
    let cash_outs = sqlx::query("SELECT * FROM cash_outs_view WHERE event_id = ? ORDER BY cash_out_id DESC")
        .bind(body.event_id)
        .fetch_all(&state.db)
        .await;
    match (tellers, cash_ins, cash_outs) {
        (Ok(tellers), Ok(cash_ins), Ok(cash_outs)) => Json(json!({
            "tellers": strip_secrets(rows_to_json(&tellers)),
            "cash_ins": strip_secrets(rows_to_json(&cash_ins)),
            "cash_outs": strip_secrets(rows_to_json(&cash_outs)),
        }))
        .into_response(),
        _ => db_fail_msg(),
    }
}

/// `EventController@getData`, the teller-transactions summary.
pub async fn teller_tx_data(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<EventIdBody>,
) -> Response {
    event_summary(&state, body.event_id).await
}

async fn event_summary(state: &AppState, event_id: i32) -> Response {
    let admin_cash: Option<Money> = sqlx::query_scalar("SELECT admin_cash_on_hand FROM events WHERE event_id = ?")
        .bind(event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(admin_cash) = admin_cash else {
        return json_error(StatusCode::NOT_FOUND, "message", "Event not found");
    };
    let collections = sum_f64(
        state,
        "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND match_status = 'Completed'",
        event_id,
    )
    .await;
    let fights: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM matches WHERE event_id = ?")
        .bind(event_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let cash_in = sum_f64(
        state,
        "SELECT COALESCE(SUM(cash_in_amount), 0) FROM cash_ins_view WHERE event_id = ? AND cash_in_status = 'Approved'",
        event_id,
    )
    .await;
    let cash_out = sum_f64(
        state,
        "SELECT COALESCE(SUM(cash_out_amount), 0) FROM cash_outs_view WHERE event_id = ? AND cash_out_status = 'Approved'",
        event_id,
    )
    .await;
    Json(json!({
        "total_collections": collections,
        "total_fights": fights,
        "cash_on_hand": (admin_cash.value() - cash_in) + cash_out,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct TellerHistoryBody {
    pub event_teller_id: i32,
}

/// `BetController@history`. A bare array, one object per fight, each with its bets.
pub async fn teller_history(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<TellerHistoryBody>,
) -> Response {
    let matches = sqlx::query(
        "SELECT matches.match_id, matches.match_status, matches.match_winner, matches.wala_odds, matches.meron_odds,
                matches.match_number,
                SUM(CASE WHEN bets.bet_side = 'Meron' AND bets.bet_status = 1 AND matches.match_status = 'Completed' THEN bets.bet_amount ELSE 0 END) AS total_meron,
                SUM(CASE WHEN bets.bet_side = 'Wala' AND bets.bet_status = 1 AND matches.match_status = 'Completed' THEN bets.bet_amount ELSE 0 END) AS total_wala,
                SUM(CASE WHEN bets.bet_status = 1 AND matches.match_status = 'Completed' THEN bets.bet_amount ELSE 0 END) AS total_bet
         FROM bets
         JOIN matches ON bets.match_id = matches.match_id
         WHERE bets.event_teller_id = ?
         GROUP BY matches.match_id, matches.match_winner, matches.meron_odds, matches.wala_odds,
                  matches.match_status, matches.match_number
         ORDER BY matches.match_number DESC",
    )
    .bind(body.event_teller_id)
    .fetch_all(&state.db)
    .await;
    let Ok(matches) = matches else {
        return Json(json!([])).into_response();
    };
    let mut detailed = Vec::new();
    for m in matches {
        let match_id: i32 = m.try_get("match_id").unwrap_or(0);
        let bets = sqlx::query(
            "SELECT bet_receipt_code, bet_side, bet_amount, bet_datetime, bet_status, bet_is_winner,
                    bet_win_amount, bet_payout_amount, bet_payout_datetime, bet_payout_by
             FROM bets_view WHERE event_teller_id = ? AND match_id = ? ORDER BY bet_datetime",
        )
        .bind(body.event_teller_id)
        .bind(match_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        let mut bet_json = Vec::new();
        for bet in bets {
            let payout: Money = bet.try_get("bet_payout_amount").unwrap_or(Money::ZERO);
            let payout_by: Option<i32> = bet.try_get("bet_payout_by").ok().flatten();
            let (can_payout, teller_name) = if payout.value() > 0.0 {
                let name: Option<String> = if let Some(by) = payout_by {
                    sqlx::query_scalar("SELECT teller_name FROM event_tellers_view WHERE event_teller_id = ?")
                        .bind(by)
                        .fetch_optional(&state.db)
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                (1, name.unwrap_or_else(|| "Unclaimed".to_string()))
            } else {
                (0, String::new())
            };
            let mut obj = row_to_json(&bet);
            if let Value::Object(map) = &mut obj {
                map.insert("can_payout".into(), json!(can_payout));
                map.insert("teller_name".into(), json!(teller_name));
            }
            bet_json.push(obj);
        }
        detailed.push(json!({
            "match_status": m.try_get::<String, _>("match_status").unwrap_or_default(),
            "match_winner": m.try_get::<String, _>("match_winner").unwrap_or_default(),
            "wala_odds": m.try_get::<f32, _>("wala_odds").unwrap_or(0.0),
            "meron_odds": m.try_get::<f32, _>("meron_odds").unwrap_or(0.0),
            "match_number": m.try_get::<i32, _>("match_number").unwrap_or(0),
            "total_meron": m.try_get::<Money, _>("total_meron").map(|v| v.value()).unwrap_or(0.0),
            "total_wala": m.try_get::<Money, _>("total_wala").map(|v| v.value()).unwrap_or(0.0),
            "total_bet": m.try_get::<Money, _>("total_bet").map(|v| v.value()).unwrap_or(0.0),
            "bets": bet_json,
        }));
    }
    Json(detailed).into_response()
}

pub async fn bet_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(event_id): Path<i32>) -> Response {
    let bets = sqlx::query(
        "SELECT *, get_teller_name(event_teller_id) AS teller_name, get_teller_name(bet_payout_by) AS payout_name
         FROM bets_view WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_all(&state.db)
    .await;
    let Ok(bets) = bets else {
        return db_fail_msg();
    };
    let claimed = sum_f64(&state, "SELECT COALESCE(SUM(bet_payout_amount), 0) FROM bets_view WHERE event_id = ? AND bet_payout_by IS NOT NULL AND bet_status = 1", event_id).await;
    let unclaimed = sum_f64(&state, "SELECT COALESCE(SUM(bet_payout_amount), 0) FROM bets_view WHERE event_id = ? AND bet_payout_by IS NULL AND bet_status = 1", event_id).await;
    let total_bets = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ?", event_id).await;
    let valid_bets = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND match_status = 'Completed'", event_id).await;
    let voided_bets = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 2", event_id).await;
    let draw_bets = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND match_status = 'Draw'", event_id).await;
    let cancelled_bets = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND match_status = 'Cancelled'", event_id).await;
    Json(json!({
        "bets": rows_to_json(&bets),
        "claimed": claimed,
        "unclaimed": unclaimed,
        "total_bets": total_bets,
        "valid_bets": valid_bets,
        "voided_bets": voided_bets,
        "draw_bets": draw_bets,
        "cancelled_bets": cancelled_bets,
        "returned_bets": voided_bets + draw_bets + cancelled_bets,
    }))
    .into_response()
}

pub async fn unclaimed_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(event_id): Path<i32>) -> Response {
    let bets = sqlx::query(
        "SELECT *, get_teller_name(event_teller_id) AS teller_name, get_teller_name(bet_payout_by) AS payout_name
         FROM bets_view
         WHERE event_id = ? AND bet_status = 1 AND bet_payout_amount > 0 AND bet_payout_by IS NULL",
    )
    .bind(event_id)
    .fetch_all(&state.db)
    .await;
    let Ok(bets) = bets else {
        return db_fail_msg();
    };
    let unclaimed = sum_f64(
        &state,
        "SELECT COALESCE(SUM(bet_payout_amount), 0) FROM bets_view
         WHERE event_id = ? AND bet_status = 1 AND bet_payout_amount > 0 AND bet_payout_by IS NULL",
        event_id,
    )
    .await;
    Json(json!({ "bets": rows_to_json(&bets), "unclaimed": unclaimed })).into_response()
}

/// `MatchController@report`.
pub async fn report(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(event_id): Path<i32>) -> Response {
    let event = sqlx::query("SELECT admin_cash_on_hand, prizes, rd, event_percentage FROM events WHERE event_id = ?")
        .bind(event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(event) = event else {
        return json_error(StatusCode::NOT_FOUND, "message", "Event not found");
    };
    let admin_cash: Money = event.try_get("admin_cash_on_hand").unwrap_or(Money::ZERO);
    let prizes: Money = event.try_get("prizes").unwrap_or(Money::ZERO);
    let rd: Money = event.try_get("rd").unwrap_or(Money::ZERO);
    let percentage: f64 = event.try_get::<crate::types::Rate, _>("event_percentage").map(|r| r.value()).unwrap_or(0.0);
    let meron = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND bet_side = 'Meron' AND match_status = 'Completed'", event_id).await;
    let wala = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND bet_side = 'Wala' AND match_status = 'Completed'", event_id).await;
    let total_collection = meron + wala;
    let teller_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_tellers_view WHERE event_id = ?").bind(event_id).fetch_one(&state.db).await.unwrap_or(0);
    let fight_count: i64 = count_status(&state, event_id, None).await;
    let fight_completed: i64 = count_status(&state, event_id, Some("Completed")).await;
    let fight_draw: i64 = count_status(&state, event_id, Some("Draw")).await;
    let fight_cancelled: i64 = count_status(&state, event_id, Some("Cancelled")).await;
    let bet_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bets_view WHERE event_id = ? AND bet_status = 1").bind(event_id).fetch_one(&state.db).await.unwrap_or(0);
    let bet_meron: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND bet_side = 'Meron'").bind(event_id).fetch_one(&state.db).await.unwrap_or(0);
    let bet_wala: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bets_view WHERE event_id = ? AND bet_status = 1 AND bet_side = 'Wala'").bind(event_id).fetch_one(&state.db).await.unwrap_or(0);
    let voided = sum_f64(&state, "SELECT COALESCE(SUM(bet_amount), 0) FROM bets_view WHERE event_id = ? AND bet_status = 2", event_id).await;
    let claimed = sum_f64(&state, "SELECT COALESCE(SUM(bet_payout_amount), 0) FROM bets_view WHERE event_id = ? AND bet_payout_datetime IS NOT NULL AND bet_payout_amount > 0 AND bet_status = 1", event_id).await;
    let unclaimed = sum_f64(&state, "SELECT COALESCE(SUM(bet_payout_amount), 0) FROM bets_view WHERE event_id = ? AND bet_payout_datetime IS NULL AND bet_payout_amount > 0 AND bet_status = 1", event_id).await;
    let salaries = sqlx::query("SELECT daily_rate, overtime_rate, total_overtime, bonus FROM salaries WHERE event_id = ?")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut total_salary = 0.0;
    for row in salaries {
        let daily: Money = row.try_get("daily_rate").unwrap_or(Money::ZERO);
        let ot_rate: Money = row.try_get("overtime_rate").unwrap_or(Money::ZERO);
        let ot: Money = row.try_get("total_overtime").unwrap_or(Money::ZERO);
        let bonus: Money = row.try_get("bonus").unwrap_or(Money::ZERO);
        total_salary += daily.value() + (ot_rate.value() * ot.value()) + bonus.value();
    }
    let cash_in = sum_f64(&state, "SELECT COALESCE(SUM(cash_in_amount), 0) FROM cash_ins_view WHERE event_id = ? AND cash_in_status = 'Approved'", event_id).await;
    let cash_out = sum_f64(&state, "SELECT COALESCE(SUM(cash_out_amount), 0) FROM cash_outs_view WHERE event_id = ? AND cash_out_status = 'Approved'", event_id).await;
    let cash_on_hand = (admin_cash.value() - cash_in) + cash_out;
    let denoms = sqlx::query(
        "SELECT denom_1000, denom_500, denom_200, denom_100, denom_50, denom_20, denom_10, denom_5, denom_1
         FROM admin_remittances WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let actual = denoms.as_ref().map(denom_total).unwrap_or(0.0);
    let organizer = total_collection * percentage;
    let net = organizer - total_salary;
    Json(json!({
        "total_collection": total_collection,
        "organizer_share": organizer,
        "teller_count": teller_count,
        "fight_count": fight_count,
        "fight_count_completed": fight_completed,
        "fight_count_draw": fight_draw,
        "fight_count_cancelled": fight_cancelled,
        "bet_count": bet_count,
        "bet_count_meron": bet_meron,
        "bet_count_wala": bet_wala,
        "voided_bets": voided,
        "claimed_bets": claimed,
        "unclaimed_winnings": unclaimed,
        "salaries": total_salary,
        "net_for_sharing": net,
        "system_share": net * 0.14,
        "operator_share": net * 0.86,
        "revolving_fund": admin_cash.to_db_string(),
        "unclaimed": unclaimed,
        "prizes": prizes.to_db_string(),
        "supposed_cash_on_hand": cash_on_hand,
        "actual_cash_on_hand": actual,
        "difference": cash_on_hand - actual,
        "rd": rd.to_db_string(),
        "comission": percentage * 100.0,
        "short": (cash_on_hand - actual) - rd.value(),
    }))
    .into_response()
}

pub async fn logs(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    let rows = sqlx::query(
        "SELECT *, get_transaction_account_name(account_id, transaction_type) AS account_name
         FROM transactions ORDER BY transaction_datetime DESC",
    )
    .fetch_all(&state.db)
    .await;
    match rows {
        Ok(rows) => Json(json!({ "logs": rows_to_json(&rows) })).into_response(),
        Err(e) => db_fail(e),
    }
}

#[derive(Deserialize)]
pub struct SalaryList {
    pub event_id: i32,
}

pub async fn salary_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, JsonOrForm(body): JsonOrForm<SalaryList>) -> Response {
    match one_list(&state, "SELECT * FROM salaries WHERE event_id = ?", body.event_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn salary_find(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(salary_id): Path<i32>) -> Response {
    match one_row(&state, "SELECT * FROM salaries WHERE salary_id = ?", salary_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

#[derive(Deserialize)]
pub struct SalaryStore {
    pub employee_type: String,
    pub employee_name: String,
    pub daily_rate: Money,
    pub overtime_rate: Money,
    pub total_overtime: Money,
    pub bonus: Money,
    #[serde(default)]
    pub include_all_tellers: Option<Value>,
}

pub async fn salary_store(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    Path(event_id): Path<i32>,
    JsonOrForm(body): JsonOrForm<SalaryStore>,
) -> Response {
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    let names: Vec<String> = if truthy(body.include_all_tellers.as_ref()) {
        sqlx::query_scalar("SELECT teller_name FROM event_tellers_view WHERE event_id = ?")
            .bind(event_id)
            .fetch_all(&mut *tx)
            .await
            .unwrap_or_default()
    } else {
        vec![body.employee_name.clone()]
    };
    for name in names {
        if sqlx::query(
            "INSERT INTO salaries (event_id, employee_type, employee_name, daily_rate, overtime_rate, total_overtime, bonus)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event_id)
        .bind(&body.employee_type)
        .bind(&name)
        .bind(body.daily_rate.to_db_string())
        .bind(body.overtime_rate.to_db_string())
        .bind(body.total_overtime.to_db_string())
        .bind(body.bonus.to_db_string())
        .execute(&mut *tx)
        .await
        .is_err()
        {
            return scalar(0);
        }
        create_log(&mut *tx, admin.account_id, "Web App", "Salaries - Salary created.").await;
    }
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

#[derive(Deserialize)]
pub struct SalaryUpdate {
    pub uid: i32,
    #[serde(rename = "_employee_type")]
    pub employee_type: String,
    #[serde(rename = "_employee_name")]
    pub employee_name: String,
    #[serde(rename = "_daily_rate")]
    pub daily_rate: Money,
    #[serde(rename = "_overtime_rate")]
    pub overtime_rate: Money,
    #[serde(rename = "_total_overtime")]
    pub total_overtime: Money,
    #[serde(rename = "_bonus")]
    pub bonus: Money,
}

pub async fn salary_update(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<SalaryUpdate>,
) -> Response {
    let result = sqlx::query(
        "UPDATE salaries SET employee_type = ?, employee_name = ?, daily_rate = ?, overtime_rate = ?,
            total_overtime = ?, bonus = ? WHERE salary_id = ?",
    )
    .bind(&body.employee_type)
    .bind(&body.employee_name)
    .bind(body.daily_rate.to_db_string())
    .bind(body.overtime_rate.to_db_string())
    .bind(body.total_overtime.to_db_string())
    .bind(body.bonus.to_db_string())
    .bind(body.uid)
    .execute(&state.db)
    .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, admin.account_id, "Web App", "Salaries - Salary updated.").await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

pub async fn salary_delete(State(state): State<Arc<AppState>>, admin: AuthedAdmin, Path(salary_id): Path<i32>) -> Response {
    let result = sqlx::query("DELETE FROM salaries WHERE salary_id = ?").bind(salary_id).execute(&state.db).await;
    match result {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, admin.account_id, "Web App", "Salaries - Salary deleted.").await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

pub async fn remittance_teller_list(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<EventIdBody>,
) -> Response {
    match one_list(&state, "SELECT * FROM teller_remittances_view WHERE event_id = ?", body.event_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn remittance_admin_list(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<EventIdBody>,
) -> Response {
    let admin_cash: Option<Money> = sqlx::query_scalar("SELECT admin_cash_on_hand FROM events WHERE event_id = ?")
        .bind(body.event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let cash_in = sum_f64(&state, "SELECT COALESCE(SUM(cash_in_amount), 0) FROM cash_ins_view WHERE event_id = ? AND cash_in_status = 'Approved'", body.event_id).await;
    let cash_out = sum_f64(&state, "SELECT COALESCE(SUM(cash_out_amount), 0) FROM cash_outs_view WHERE event_id = ? AND cash_out_status = 'Approved'", body.event_id).await;
    let cash_on_hand = (admin_cash.map(|m| m.value()).unwrap_or(0.0) - cash_in) + cash_out;
    let rows = sqlx::query("SELECT * FROM admin_remittances_view WHERE event_id = ?")
        .bind(body.event_id)
        .fetch_all(&state.db)
        .await;
    let Ok(rows) = rows else {
        return db_fail_msg();
    };
    let mut out = rows_to_json(&rows);
    for row in &mut out {
        if let Value::Object(map) = row {
            map.insert("cash_on_hand".into(), json!(cash_on_hand));
        }
    }
    Json(out).into_response()
}

pub async fn remittance_generate(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    Path(event_id): Path<i32>,
) -> Response {
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return scalar(0),
    };
    let tellers: Vec<i32> = sqlx::query_scalar("SELECT event_teller_id FROM event_tellers_view WHERE event_id = ?")
        .bind(event_id)
        .fetch_all(&mut *tx)
        .await
        .unwrap_or_default();
    for id in &tellers {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM teller_remittances WHERE event_teller_id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0);
        if exists == 0
            && sqlx::query(
                "INSERT INTO teller_remittances
                    (event_teller_id, denom_1000, denom_500, denom_200, denom_100, denom_50, denom_20, denom_10, denom_5, denom_1)
                 VALUES (?, 0, 0, 0, 0, 0, 0, 0, 0, 0)",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return scalar(0);
        }
    }
    if !tellers.is_empty() {
        let placeholders = vec!["?"; tellers.len()].join(", ");
        let sql = format!("DELETE FROM teller_remittances WHERE event_teller_id NOT IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in &tellers {
            q = q.bind(id);
        }
        if q.execute(&mut *tx).await.is_err() {
            return scalar(0);
        }
    }
    let admin_exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_remittances WHERE event_id = ?")
        .bind(event_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);
    if admin_exists == 0
        && sqlx::query(
            "INSERT INTO admin_remittances
                (event_id, denom_1000, denom_500, denom_200, denom_100, denom_50, denom_20, denom_10, denom_5, denom_1)
             VALUES (?, 0, 0, 0, 0, 0, 0, 0, 0, 0)",
        )
        .bind(event_id)
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return scalar(0);
    }
    create_log(&mut *tx, admin.account_id, "Web App", "Remittances - Empty Remittance created.").await;
    if tx.commit().await.is_err() {
        return scalar(0);
    }
    scalar(1)
}

#[derive(Deserialize)]
pub struct TellerRemitUpdate {
    pub uid: i32,
    #[serde(rename = "_denom_1000", deserialize_with = "flex_i32")]
    pub denom_1000: i32,
    #[serde(rename = "_denom_500", deserialize_with = "flex_i32")]
    pub denom_500: i32,
    #[serde(rename = "_denom_200", deserialize_with = "flex_i32")]
    pub denom_200: i32,
    #[serde(rename = "_denom_100", deserialize_with = "flex_i32")]
    pub denom_100: i32,
    #[serde(rename = "_denom_50", deserialize_with = "flex_i32")]
    pub denom_50: i32,
    #[serde(rename = "_denom_20", deserialize_with = "flex_i32")]
    pub denom_20: i32,
    #[serde(rename = "_denom_10", deserialize_with = "flex_i32")]
    pub denom_10: i32,
    #[serde(rename = "_denom_5", deserialize_with = "flex_i32")]
    pub denom_5: i32,
    #[serde(rename = "_denom_1", deserialize_with = "flex_i32")]
    pub denom_1: i32,
}

pub async fn remittance_teller_update(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<TellerRemitUpdate>,
) -> Response {
    let result = sqlx::query(
        "UPDATE teller_remittances SET denom_1000=?, denom_500=?, denom_200=?, denom_100=?, denom_50=?,
            denom_20=?, denom_10=?, denom_5=?, denom_1=? WHERE teller_remittance_id = ?",
    )
    .bind(body.denom_1000)
    .bind(body.denom_500)
    .bind(body.denom_200)
    .bind(body.denom_100)
    .bind(body.denom_50)
    .bind(body.denom_20)
    .bind(body.denom_10)
    .bind(body.denom_5)
    .bind(body.denom_1)
    .bind(body.uid)
    .execute(&state.db)
    .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, admin.account_id, "Web App", "Remittances - Remittance updated.").await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

#[derive(Deserialize)]
pub struct AdminRemitUpdate {
    pub admin_uid: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_1000: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_500: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_200: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_100: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_50: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_20: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_10: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_5: i32,
    #[serde(deserialize_with = "flex_i32")]
    pub admin_denom_1: i32,
}

pub async fn remittance_admin_update(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<AdminRemitUpdate>,
) -> Response {
    let result = sqlx::query(
        "UPDATE admin_remittances SET denom_1000=?, denom_500=?, denom_200=?, denom_100=?, denom_50=?,
            denom_20=?, denom_10=?, denom_5=?, denom_1=? WHERE admin_remittance_id = ?",
    )
    .bind(body.admin_denom_1000)
    .bind(body.admin_denom_500)
    .bind(body.admin_denom_200)
    .bind(body.admin_denom_100)
    .bind(body.admin_denom_50)
    .bind(body.admin_denom_20)
    .bind(body.admin_denom_10)
    .bind(body.admin_denom_5)
    .bind(body.admin_denom_1)
    .bind(body.admin_uid)
    .execute(&state.db)
    .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, admin.account_id, "Web App", "Remittances - Remittance updated.").await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

pub async fn remittance_teller_find(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    Path(id): Path<i32>,
) -> Response {
    match one_row(&state, "SELECT * FROM teller_remittances_view WHERE teller_remittance_id = ?", id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn remittance_admin_find(
    State(state): State<Arc<AppState>>,
    _admin: AuthedAdmin,
    Path(id): Path<i32>,
) -> Response {
    match one_row(&state, "SELECT * FROM admin_remittances_view WHERE admin_remittance_id = ?", id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn remittance_delete(State(state): State<Arc<AppState>>, admin: AuthedAdmin, Path(id): Path<i32>) -> Response {
    let result = sqlx::query("DELETE FROM teller_remittances WHERE teller_remittance_id = ?")
        .bind(id)
        .execute(&state.db)
        .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, admin.account_id, "Web App", "Remittances - Remittance deleted.").await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

pub async fn teller_shorts_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(id): Path<i32>) -> Response {
    match one_list(&state, "SELECT * FROM teller_shorts WHERE teller_remittance_id = ?", id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

pub async fn admin_shorts_list(State(state): State<Arc<AppState>>, _admin: AuthedAdmin, Path(id): Path<i32>) -> Response {
    match one_list(&state, "SELECT * FROM admin_shorts WHERE admin_remittance_id = ?", id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_fail(e),
    }
}

#[derive(Deserialize)]
pub struct ShortStore {
    pub teller_remittance_id: Option<i32>,
    pub admin_remittance_id: Option<i32>,
    pub short_amount: Money,
    pub short_remarks: String,
}

pub async fn teller_short_store(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<ShortStore>,
) -> Response {
    let Some(id) = body.teller_remittance_id else {
        return scalar(0);
    };
    short_insert(&state, admin.account_id, "teller_shorts", "teller_remittance_id", id, &body, "Teller").await
}

pub async fn admin_short_store(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    JsonOrForm(body): JsonOrForm<ShortStore>,
) -> Response {
    let Some(id) = body.admin_remittance_id else {
        return scalar(0);
    };
    short_insert(&state, admin.account_id, "admin_shorts", "admin_remittance_id", id, &body, "Admin").await
}

async fn short_insert(
    state: &AppState,
    account_id: i32,
    table: &str,
    col: &str,
    id: i32,
    body: &ShortStore,
    label: &str,
) -> Response {
    let sql = format!("INSERT INTO {table} ({col}, short_amount, short_remarks) VALUES (?, ?, ?)");
    let result = sqlx::query(&sql)
        .bind(id)
        .bind(body.short_amount.to_db_string())
        .bind(&body.short_remarks)
        .execute(&state.db)
        .await;
    match result {
        Ok(_) => {
            create_log(&state.db, account_id, "Web App", &format!("Remittances - {label} Short created.")).await;
            scalar(1)
        }
        Err(_) => scalar(0),
    }
}

pub async fn teller_short_delete(State(state): State<Arc<AppState>>, admin: AuthedAdmin, Path(id): Path<i32>) -> Response {
    short_delete(&state, admin.account_id, "teller_shorts", "teller_short_id", id, "Teller").await
}

pub async fn admin_short_delete(State(state): State<Arc<AppState>>, admin: AuthedAdmin, Path(id): Path<i32>) -> Response {
    short_delete(&state, admin.account_id, "admin_shorts", "admin_short_id", id, "Admin").await
}

async fn short_delete(state: &AppState, account_id: i32, table: &str, col: &str, id: i32, label: &str) -> Response {
    let sql = format!("DELETE FROM {table} WHERE {col} = ?");
    match sqlx::query(&sql).bind(id).execute(&state.db).await {
        Ok(r) if r.rows_affected() == 1 => {
            create_log(&state.db, account_id, "Web App", &format!("Remittances - {label} Short deleted.")).await;
            scalar(1)
        }
        _ => scalar(0),
    }
}

async fn one_list(state: &AppState, sql: &str, id: i32) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(sql).bind(id).fetch_all(&state.db).await?;
    Ok(rows_to_json(&rows))
}

async fn sum_f64(state: &AppState, sql: &str, id: i32) -> f64 {
    sqlx::query_scalar::<_, Money>(sql)
        .bind(id)
        .fetch_one(&state.db)
        .await
        .map(|m| m.value())
        .unwrap_or(0.0)
}

async fn count_status(state: &AppState, event_id: i32, status: Option<&str>) -> i64 {
    match status {
        Some(status) => sqlx::query_scalar("SELECT COUNT(*) FROM matches WHERE event_id = ? AND match_status = ?")
            .bind(event_id)
            .bind(status)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0),
        None => sqlx::query_scalar("SELECT COUNT(*) FROM matches WHERE event_id = ?")
            .bind(event_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0),
    }
}

fn denom_total(row: &sqlx::mysql::MySqlRow) -> f64 {
    let piece = |name: &str, value: f64| -> f64 {
        row.try_get::<i64, _>(name).unwrap_or(0) as f64 * value
    };
    piece("denom_1000", 1000.0)
        + piece("denom_500", 500.0)
        + piece("denom_200", 200.0)
        + piece("denom_100", 100.0)
        + piece("denom_50", 50.0)
        + piece("denom_20", 20.0)
        + piece("denom_10", 10.0)
        + piece("denom_5", 5.0)
        + piece("denom_1", 1.0)
}

fn strip_secrets(mut rows: Vec<Value>) -> Vec<Value> {
    for row in &mut rows {
        if let Value::Object(map) = row {
            map.remove("password");
        }
    }
    rows
}

fn db_fail(err: sqlx::Error) -> Response {
    tracing::error!(error = %err, "website query failed");
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "message", "Unexpected server error.")
}

fn db_fail_msg() -> Response {
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "message", "Unexpected server error.")
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        Some(_) => true,
    }
}

fn flex_i8<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i8, D::Error> {
    Ok(flex_number(deserializer)? as i8)
}

fn flex_i32<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    Ok(flex_number(deserializer)? as i32)
}

fn flex_number<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(n) => Ok(n.as_i64().unwrap_or(0)),
        Value::String(s) => Ok(s.trim().parse().unwrap_or(0)),
        Value::Bool(b) => Ok(if b { 1 } else { 0 }),
        _ => Ok(0),
    }
}

fn empty_string_as_none<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.filter(|s| !s.is_empty()))
}
