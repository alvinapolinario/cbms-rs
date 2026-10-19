//! `/matches-admin/*` -- mirrors `MatchController`'s fight-lifecycle actions (docs/WORKFLOW.md
//! section 7.2/7.3/7.5), written directly against the PHP source. `hold` has no `api.php` route
//! in the real app (web-only there) -- added here as a flagged, deliberate addition so the
//! bridge-linked workflow is reachable over the API; see the plan.

use crate::auth::AuthedAdmin;
use crate::error::AppError;
use crate::models::Fight;
use crate::money::Money;
use crate::services::{bridge_outbox, odds, settlement};
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

async fn active_event_id(state: &Arc<AppState>) -> Option<i32> {
    sqlx::query_scalar("SELECT event_id FROM events WHERE event_status = 'Active' LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
}

async fn fetch_fight(state: &Arc<AppState>, match_id: i32) -> Option<Fight> {
    sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(match_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
}

/// Creates the next empty placeholder fight for the Active event (manual, non-bridge mode) --
/// mirrors the static `MatchController::newStaticMatch()`, including its side effect of
/// resetting every teller's `teller_match_balance` to 0. Must run inside an open transaction.
async fn new_static_match(tx: &mut sqlx::MySqlConnection, event_id: i32) -> Result<Fight, sqlx::Error> {
    sqlx::query("UPDATE event_tellers SET teller_match_balance = 0 WHERE event_id = ?")
        .bind(event_id)
        .execute(&mut *tx)
        .await?;

    let prev_number: Option<i32> = sqlx::query_scalar(
        "SELECT match_number FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let next_number = prev_number.unwrap_or(0) + 1;

    let result = sqlx::query(
        "INSERT INTO matches
            (event_id, match_number, match_winner, meron_total_bet, wala_total_bet, meron_odds, wala_odds,
             meron_bet_status, wala_bet_status, match_status, match_bet_status, match_created_datetime, is_display)
         VALUES (?, ?, '', 0, 0, 0, 0, 0, 0, 'Ongoing', 'Closed', NOW(), 0)",
    )
    .bind(event_id)
    .bind(next_number)
    .execute(&mut *tx)
    .await?;
    let match_id = result.last_insert_id() as i32;

    let fight = sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(match_id)
        .fetch_one(&mut *tx)
        .await?;
    Ok(fight)
}

async fn fight_history(state: &Arc<AppState>) -> Value {
    let Some(event_id) = active_event_id(state).await else {
        return json!({ "success": false, "message": "No active event", "match": null, "matches": [] });
    };

    let latest = sqlx::query_as::<_, Fight>(
        "SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let current_match = if latest.is_none() {
        let mut tx = match state.db.begin().await {
            Ok(tx) => tx,
            Err(_) => return json!({ "success": false, "message": "No active event", "match": null, "matches": [] }),
        };
        let created = new_static_match(&mut tx, event_id).await;
        match created {
            Ok(f) => {
                let _ = tx.commit().await;
                Some(f)
            }
            Err(_) => None,
        }
    } else {
        latest
    };

    let matches = sqlx::query_as::<_, Fight>(
        "SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC",
    )
    .bind(event_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    json!({ "success": true, "match": current_match, "matches": matches })
}

pub async fn list_app(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    Json(fight_history(&state).await).into_response()
}

pub async fn get_started_app(State(state): State<Arc<AppState>>, admin: AuthedAdmin) -> Response {
    if active_event_id(&state).await.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "message": "No active event", "match": null, "matches": [] })),
        )
            .into_response();
    }
    // Mirrors `getStartedApp` -> `getStarted()` then `fightHistory()`. `getStarted()`'s own job
    // (log + broadcast) is folded into this handler; `fightHistory()` already creates the first
    // placeholder fight if none exists yet (manual mode), which is `getStarted()`'s other effect.
    let result = fight_history(&state).await;
    crate::services::transactions::create_log(
        &state.db,
        admin.account_id,
        "Admin App",
        "Matches - Clicked the Get Started button.",
    )
    .await;
    if let Some(event_id) = active_event_id(&state).await {
        crate::services::realtime::latest_match_updated(&state, event_id).await;
        crate::services::realtime::fight_history(&state, event_id).await;
        crate::services::realtime::teller_balances(&state, event_id).await;
    }
    Json(result).into_response()
}

/// Public read of the active event's current fight and history. The TV pages poll this.
/// Unlike `fight_history`, this does not open a placeholder fight.
pub async fn tv_board(State(state): State<Arc<AppState>>) -> Response {
    let Some(event_id) = active_event_id(&state).await else {
        return Json(json!({
            "success": false,
            "message": "No active event",
            "event": null,
            "match": null,
            "matches": []
        }))
        .into_response();
    };
    let event_name: Option<String> = sqlx::query_scalar("SELECT event_name FROM events WHERE event_id = ?")
        .bind(event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let current = sqlx::query_as::<_, Fight>(
        "SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let matches = sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC")
        .bind(event_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    Json(json!({
        "success": true,
        "event": { "event_id": event_id, "event_name": event_name },
        "match": current,
        "matches": matches,
    }))
    .into_response()
}

/// `GET /tv/get-started`. Public in `web.php`, so this route does not require a token.
pub async fn get_started_web(State(state): State<Arc<AppState>>) -> Response {
    let _ = fight_history(&state).await;
    if let Some(event_id) = active_event_id(&state).await {
        crate::services::realtime::latest_match_updated(&state, event_id).await;
        crate::services::realtime::fight_history(&state, event_id).await;
        crate::services::realtime::teller_balances(&state, event_id).await;
        crate::services::transactions::create_log(
            &state.db,
            -1,
            "Web App",
            "Matches - Clicked the Get Started button.",
        )
        .await;
    }
    StatusCode::OK.into_response()
}

pub async fn get_started_web_admin(State(state): State<Arc<AppState>>, admin: AuthedAdmin) -> Response {
    let _ = fight_history(&state).await;
    if let Some(event_id) = active_event_id(&state).await {
        crate::services::realtime::latest_match_updated(&state, event_id).await;
        crate::services::realtime::fight_history(&state, event_id).await;
        crate::services::realtime::teller_balances(&state, event_id).await;
        crate::services::transactions::create_log(
            &state.db,
            admin.account_id,
            "Web App",
            "Matches - Clicked the Get Started button.",
        )
        .await;
    }
    StatusCode::OK.into_response()
}

/// `MatchController@newMatch`: next fight, betting Open, every teller's match balance reset.
pub async fn new_match(State(state): State<Arc<AppState>>, _admin: AuthedAdmin) -> Response {
    let Some(event_id) = active_event_id(&state).await else {
        return (StatusCode::OK, "0").into_response();
    };
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return (StatusCode::OK, "0").into_response(),
    };
    if sqlx::query("UPDATE event_tellers SET teller_match_balance = 0 WHERE event_id = ?")
        .bind(event_id)
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return (StatusCode::OK, "0").into_response();
    }
    let prev_number: Option<i32> = sqlx::query_scalar(
        "SELECT match_number FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(&mut *tx)
    .await
    .ok()
    .flatten();
    let next_number = prev_number.unwrap_or(0) + 1;
    let inserted = sqlx::query(
        "INSERT INTO matches
            (event_id, match_number, match_winner, meron_total_bet, wala_total_bet, meron_odds, wala_odds,
             meron_bet_status, wala_bet_status, match_bet_status, match_status, match_created_datetime, is_display)
         VALUES (?, ?, '', 0, 0, 0, 0, 0, 0, 'Open', 'Ongoing', NOW(), 0)",
    )
    .bind(event_id)
    .bind(next_number)
    .execute(&mut *tx)
    .await;
    let Ok(inserted) = inserted else {
        return (StatusCode::OK, "0").into_response();
    };
    let match_id = inserted.last_insert_id() as i32;
    if tx.commit().await.is_err() {
        return (StatusCode::OK, "0").into_response();
    }
    crate::services::realtime::match_updated(&state, match_id).await;
    crate::services::realtime::teller_balances(&state, event_id).await;
    (StatusCode::OK, next_number.to_string()).into_response()
}

async fn get_latest_match_inner(state: &Arc<AppState>) -> Response {
    let Some(event_id) = active_event_id(state).await else {
        return Json(json!({ "success": false })).into_response();
    };
    let latest = sqlx::query_as::<_, Fight>(
        "SELECT * FROM matches WHERE event_id = ? ORDER BY match_number DESC LIMIT 1",
    )
    .bind(event_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some(m) = latest else {
        return Json(json!({ "success": false })).into_response();
    };
    Json(json!({
        "success": true,
        "match_id": m.match_id,
        "match_number": m.match_number,
        "match_bet_status": m.match_bet_status,
    }))
    .into_response()
}

/// `POST /matches-admin/latest` (admin token) -- mirrors `MatchController@getLatestMatch`.
pub async fn get_latest_match(State(state): State<Arc<AppState>>, _admin: crate::auth::AuthedAdmin) -> Response {
    get_latest_match_inner(&state).await
}

/// `POST /latest-match` (teller token) -- same underlying method as the admin path above; the
/// real app registers the identical controller method under both a teller- and an admin-token
/// route.
pub async fn get_latest_match_teller(State(state): State<Arc<AppState>>, _teller: crate::auth::AuthedTeller) -> Response {
    get_latest_match_inner(&state).await
}

async fn resum_totals(conn: &mut sqlx::MySqlConnection, match_id: i32) -> Result<(Money, Money), sqlx::Error> {
    let row = sqlx::query(
        "SELECT CAST(COALESCE(SUM(CASE WHEN bet_side = 'Meron' THEN bet_amount END), 0) AS DECIMAL(16,2)) AS meron,
                CAST(COALESCE(SUM(CASE WHEN bet_side = 'Wala' THEN bet_amount END), 0) AS DECIMAL(16,2)) AS wala
         FROM bets WHERE match_id = ? AND bet_status = 1",
    )
    .bind(match_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok((row.try_get("meron")?, row.try_get("wala")?))
}

#[derive(Deserialize)]
pub struct MatchIdRequest {
    pub match_id: i32,
}

enum Side {
    Meron,
    Wala,
}

/// Shared body of `toggleMeron`/`toggleWala` -- identical in the PHP source apart from which
/// bet-status flag flips.
async fn toggle_side(state: &Arc<AppState>, account_id: i32, match_id: i32, side: Side) -> Response {
    let Some(guard) = fetch_fight(state, match_id).await else {
        return fail(StatusCode::NOT_FOUND, "Fight not found.");
    };
    if guard.match_status != "Ongoing" {
        return fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Fight #{} is already settled; betting can no longer be changed.", guard.match_number),
        );
    }
    let event_status: Option<String> = sqlx::query_scalar("SELECT event_status FROM events WHERE event_id = ?")
        .bind(guard.event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if event_status.as_deref() != Some("Active") {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "This event is not active.");
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return AppError::internal(e).into_response(),
    };

    let event_percentage: f64 = sqlx::query_scalar("SELECT event_percentage FROM events WHERE event_id = ?")
        .bind(guard.event_id)
        .fetch_one(&mut *tx)
        .await
        .map(|r: crate::types::Rate| r.value())
        .unwrap_or(0.0);

    let (meron_total, wala_total) = match resum_totals(&mut tx, match_id).await {
        Ok(v) => v,
        Err(e) => return AppError::internal(e).into_response(),
    };

    let (meron_odds, wala_odds) = match odds::recompute_odds(meron_total, wala_total, event_percentage) {
        Some(pair) => pair,
        None => (guard.meron_odds as f64, guard.wala_odds as f64),
    };

    let (new_meron_status, new_wala_status): (i8, i8) = match side {
        Side::Meron => (if guard.meron_bet_status == 0 { 1 } else { 0 }, guard.wala_bet_status),
        Side::Wala => (guard.meron_bet_status, if guard.wala_bet_status == 0 { 1 } else { 0 }),
    };

    let update = sqlx::query(
        "UPDATE matches SET meron_total_bet = ?, wala_total_bet = ?, meron_odds = ?, wala_odds = ?,
            meron_bet_status = ?, wala_bet_status = ? WHERE match_id = ?",
    )
    .bind(meron_total.to_db_string())
    .bind(wala_total.to_db_string())
    .bind(meron_odds as f32)
    .bind(wala_odds as f32)
    .bind(new_meron_status)
    .bind(new_wala_status)
    .bind(match_id)
    .execute(&mut *tx)
    .await;
    if let Err(e) = update {
        return AppError::internal(e).into_response();
    }

    let side_name = match side {
        Side::Meron => "Meron",
        Side::Wala => "Wala",
    };
    crate::services::transactions::create_log(
        &mut *tx,
        account_id,
        "Admin App",
        &format!("Matches - Fight #{} Toggled the Bet Side Status of {side_name}.", guard.match_number),
    )
    .await;

    if let Err(e) = tx.commit().await {
        return AppError::internal(e).into_response();
    }

    // The button already knows the new lock. Publish after the response so this click
    // does not wait to reload every fight.
    let event_id = guard.event_id;
    let broadcast = Arc::clone(state);
    tokio::spawn(async move {
        crate::services::realtime::latest_match_updated(&broadcast, event_id).await;
        crate::services::realtime::fight_history(&broadcast, event_id).await;
    });

    Json(json!({ "message": format!("{side_name} bet toggled!") })).into_response()
}

pub async fn toggle_meron(State(state): State<Arc<AppState>>, admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<MatchIdRequest>) -> Response {
    toggle_side(&state, admin.account_id, body.match_id, Side::Meron).await
}

pub async fn toggle_wala(State(state): State<Arc<AppState>>, admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<MatchIdRequest>) -> Response {
    toggle_side(&state, admin.account_id, body.match_id, Side::Wala).await
}

#[derive(Deserialize)]
pub struct UpdateBetStatusRequest {
    pub match_id: i32,
    pub match_bet_status: String,
    #[serde(default)]
    pub meron_entry: Option<String>,
    #[serde(default)]
    pub wala_entry: Option<String>,
}

pub async fn update_match_bet_status(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<UpdateBetStatusRequest>,
) -> Response {
    if body.match_bet_status != "Open" && body.match_bet_status != "Closed" {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Betting status must be Open or Closed.");
    }
    let Some(guard) = fetch_fight(&state, body.match_id).await else {
        return fail(StatusCode::NOT_FOUND, "Fight not found.");
    };
    if guard.match_status != "Ongoing" {
        return fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Fight #{} is already settled; betting can no longer be changed.", guard.match_number),
        );
    }
    let event_status: Option<String> = sqlx::query_scalar("SELECT event_status FROM events WHERE event_id = ?")
        .bind(guard.event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if event_status.as_deref() != Some("Active") {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "This event is not active.");
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return AppError::internal(e).into_response(),
    };

    if let Err(e) = sqlx::query("UPDATE matches SET is_display = 0").execute(&mut *tx).await {
        return AppError::internal(e).into_response();
    }

    let event_percentage: f64 = sqlx::query_scalar("SELECT event_percentage FROM events WHERE event_id = ?")
        .bind(guard.event_id)
        .fetch_one(&mut *tx)
        .await
        .map(|r: crate::types::Rate| r.value())
        .unwrap_or(0.0);
    let (meron_total, wala_total) = match resum_totals(&mut tx, body.match_id).await {
        Ok(v) => v,
        Err(e) => return AppError::internal(e).into_response(),
    };
    let (meron_odds, wala_odds) = match odds::recompute_odds(meron_total, wala_total, event_percentage) {
        Some(pair) => pair,
        None => (guard.meron_odds as f64, guard.wala_odds as f64),
    };

    if body.match_bet_status == "Open" && guard.hold_reason.is_some() {
        return fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!(
                "Fight #{} is on HOLD ({}). Matching must fix and re-send it first.",
                guard.match_number,
                guard.hold_reason.as_deref().unwrap_or("")
            ),
        );
    }

    let bet_opened_at_now = body.match_bet_status == "Open" && guard.bet_opened_at.is_none();
    let bridge_linked = guard.is_bridge_linked();

    let update_sql = if bet_opened_at_now {
        "UPDATE matches SET meron_total_bet=?, wala_total_bet=?, meron_odds=?, wala_odds=?,
            match_bet_status=?, meron_bet_status=0, wala_bet_status=0,
            meron_entry = COALESCE(?, meron_entry), wala_entry = COALESCE(?, wala_entry),
            bet_opened_at = NOW() WHERE match_id = ?"
    } else {
        "UPDATE matches SET meron_total_bet=?, wala_total_bet=?, meron_odds=?, wala_odds=?,
            match_bet_status=?, meron_bet_status=0, wala_bet_status=0,
            meron_entry = COALESCE(?, meron_entry), wala_entry = COALESCE(?, wala_entry)
            WHERE match_id = ?"
    };
    // Entries of a fight called from matching come from matching, not from this form.
    let (meron_entry, wala_entry) = if bridge_linked { (None, None) } else { (body.meron_entry.clone(), body.wala_entry.clone()) };

    if let Err(e) = sqlx::query(update_sql)
        .bind(meron_total.to_db_string())
        .bind(wala_total.to_db_string())
        .bind(meron_odds as f32)
        .bind(wala_odds as f32)
        .bind(&body.match_bet_status)
        .bind(meron_entry)
        .bind(wala_entry)
        .bind(body.match_id)
        .execute(&mut *tx)
        .await
    {
        return AppError::internal(e).into_response();
    }

    let updated = match sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(body.match_id)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(f) => f,
        Err(e) => return AppError::internal(e).into_response(),
    };

    if let Err(e) = bridge_outbox::queue_status(&mut tx, &updated, &updated.match_bet_status.to_lowercase()).await {
        return AppError::internal(e).into_response();
    }

    crate::services::transactions::create_log(
        &mut *tx,
        admin.account_id,
        "Admin App",
        &format!("Matches - Fight #{} bet status updated to {}!", updated.match_number, updated.match_bet_status),
    )
    .await;

    if let Err(e) = tx.commit().await {
        return AppError::internal(e).into_response();
    }

    crate::services::realtime::match_updated(&state, body.match_id).await;
    crate::services::realtime::tv_display(&state, body.match_id).await;

    Json(json!({ "message": format!("Fight #{} bet status updated to {}!", updated.match_number, updated.match_bet_status) })).into_response()
}

#[derive(Deserialize)]
pub struct UpdateMatchStatusRequest {
    pub match_id: i32,
    pub match_status: String,
    #[serde(default)]
    pub winner: String,
    #[serde(default)]
    pub dont_create: bool,
}

pub async fn update_match_status(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    crate::extract::JsonOrForm(mut body): crate::extract::JsonOrForm<UpdateMatchStatusRequest>,
) -> Response {
    let settled = ["Completed", "Draw", "Cancelled"];
    if !settled.contains(&body.match_status.as_str()) {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Invalid match status.");
    }
    if body.match_status == "Completed" && !["Meron", "Wala", "Draw", "Cancelled"].contains(&body.winner.as_str()) {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Choose Meron, Wala, Draw or Cancelled.");
    }
    if body.match_status != "Completed" && !["", "-", "Draw", "Cancelled"].contains(&body.winner.as_str()) {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "A draw or cancelled fight has no winner.");
    }

    let Some(target) = fetch_fight(&state, body.match_id).await else {
        return fail(StatusCode::NOT_FOUND, "Fight not found.");
    };
    let event_status: Option<String> = sqlx::query_scalar("SELECT event_status FROM events WHERE event_id = ?")
        .bind(target.event_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if event_status.as_deref() != Some("Active") {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "This event is not active; its results can no longer be changed.");
    }

    let is_correction = settled.contains(&target.match_status.as_str());
    let previous_result = if target.match_winner.is_empty() {
        target.match_status.clone()
    } else {
        format!("{} / {}", target.match_status, target.match_winner)
    };

    if !is_correction && state.config.require_closed_before_result && target.match_bet_status != "Closed" {
        return fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Close betting on Fight #{} before declaring the result.", target.match_number),
        );
    }

    if is_correction {
        body.dont_create = true;
    }

    if state.config.bridge_is_enabled() {
        body.dont_create = true;
    }

    if body.winner == "Draw" || body.winner == "Cancelled" {
        body.match_status = body.winner.clone();
        body.winner = String::new();
    }

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return AppError::internal(e).into_response(),
    };

    let event_percentage: f64 = sqlx::query_scalar("SELECT event_percentage FROM events WHERE event_id = ?")
        .bind(target.event_id)
        .fetch_one(&mut *tx)
        .await
        .map(|r: crate::types::Rate| r.value())
        .unwrap_or(0.0);
    let (meron_total, wala_total) = match resum_totals(&mut tx, body.match_id).await {
        Ok(v) => v,
        Err(e) => return AppError::internal(e).into_response(),
    };
    let (meron_odds, wala_odds) = match odds::recompute_odds(meron_total, wala_total, event_percentage) {
        Some(pair) => pair,
        None => (target.meron_odds as f64, target.wala_odds as f64),
    };

    let match_number = target.match_number;
    let match_winner = if body.match_status == "Completed" { body.winner.clone() } else { "N/A".to_string() };

    let outcome = match body.match_status.as_str() {
        "Completed" if body.winner == "Meron" => settlement::Outcome::Completed { winner: "Meron" },
        "Completed" => settlement::Outcome::Completed { winner: "Wala" },
        "Cancelled" => settlement::Outcome::Cancelled,
        _ => settlement::Outcome::Draw,
    };
    if let Err(e) = settlement::settle_bets(&mut tx, body.match_id, &outcome, meron_odds, wala_odds).await {
        return AppError::internal(e).into_response();
    }

    if let Err(e) = sqlx::query(
        "UPDATE matches SET meron_total_bet=?, wala_total_bet=?, meron_odds=?, wala_odds=?,
            match_status=?, match_winner=?, is_display=1 WHERE match_id=?",
    )
    .bind(meron_total.to_db_string())
    .bind(wala_total.to_db_string())
    .bind(meron_odds as f32)
    .bind(wala_odds as f32)
    .bind(&body.match_status)
    .bind(&match_winner)
    .bind(body.match_id)
    .execute(&mut *tx)
    .await
    {
        return AppError::internal(e).into_response();
    }

    let updated = match sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(body.match_id)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(f) => f,
        Err(e) => return AppError::internal(e).into_response(),
    };

    let bridge_result = if updated.match_status == "Completed" {
        updated.match_winner.to_lowercase()
    } else {
        updated.match_status.to_lowercase()
    };
    let declared_by: Option<String> = sqlx::query_scalar("SELECT username FROM accounts WHERE account_id = ?")
        .bind(admin.account_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();
    if let Err(e) = bridge_outbox::queue_result(
        &mut tx,
        &updated,
        &bridge_result,
        is_correction.then_some(previous_result.as_str()),
        declared_by.as_deref(),
    )
    .await
    {
        return AppError::internal(e).into_response();
    }

    crate::services::transactions::create_log(
        &mut *tx,
        admin.account_id,
        "Admin App",
        &format!("Matches - Fight #{match_number} match status updated to {}!", body.match_status),
    )
    .await;
    if is_correction {
        let winner_suffix = if updated.match_winner != "N/A" && !updated.match_winner.is_empty() {
            format!(" / {}", updated.match_winner)
        } else {
            String::new()
        };
        crate::services::transactions::create_log(
            &mut *tx,
            admin.account_id,
            "Admin App",
            &format!("RESULT CORRECTION: Fight #{match_number} changed from {previous_result} to {}{winner_suffix}", body.match_status),
        )
        .await;
    }

    if let Err(e) = tx.commit().await {
        return AppError::internal(e).into_response();
    }

    crate::services::realtime::tv_display(&state, body.match_id).await;
    if !body.dont_create {
        if let Ok(mut tx2) = state.db.begin().await {
            if new_static_match(&mut tx2, target.event_id).await.is_ok() {
                let _ = tx2.commit().await;
            }
        }
    }
    crate::services::realtime::latest_match_updated(&state, target.event_id).await;
    crate::services::realtime::fight_history(&state, target.event_id).await;
    crate::services::realtime::teller_balances(&state, target.event_id).await;

    Json(json!({ "message": format!("Fight #{match_number} match status updated to {}!", body.match_status) })).into_response()
}

#[derive(Deserialize)]
pub struct HoldRequest {
    pub match_id: i32,
    pub reason: String,
}

#[derive(Deserialize)]
pub struct UpdateDisplayRequest {
    pub match_id: i32,
    #[serde(default)]
    pub is_display: i8,
}

pub async fn hold(State(state): State<Arc<AppState>>, admin: AuthedAdmin, crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<HoldRequest>) -> Response {
    let reason = body.reason.trim();
    if reason.is_empty() || reason.chars().count() > 200 {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Give a short reason for the hold (max 200 characters).");
    }
    let Some(target) = fetch_fight(&state, body.match_id).await else {
        return fail(StatusCode::NOT_FOUND, "Fight not found.");
    };
    if !target.is_bridge_linked() {
        return fail(StatusCode::UNPROCESSABLE_ENTITY, "Only fights called from the matching system can be put on hold.");
    }
    if target.match_status != "Ongoing" || target.bet_opened_at.is_some() {
        return fail(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Fight #{}: betting already opened. Cancel the fight instead (bets are refunded).", target.match_number),
        );
    }

    let reason: String = reason.chars().take(200).collect();
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return AppError::internal(e).into_response(),
    };
    if let Err(e) = sqlx::query("UPDATE matches SET hold_reason = ? WHERE match_id = ?")
        .bind(&reason)
        .bind(body.match_id)
        .execute(&mut *tx)
        .await
    {
        return AppError::internal(e).into_response();
    }
    let updated = match sqlx::query_as::<_, Fight>("SELECT * FROM matches WHERE match_id = ?")
        .bind(body.match_id)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(f) => f,
        Err(e) => return AppError::internal(e).into_response(),
    };
    if let Err(e) = bridge_outbox::queue_status(&mut tx, &updated, "held").await {
        return AppError::internal(e).into_response();
    }
    crate::services::transactions::create_log(
        &mut *tx,
        admin.account_id,
        "Admin App",
        &format!("Matches - Fight #{} put on HOLD: {reason}", target.match_number),
    )
    .await;
    if let Err(e) = tx.commit().await {
        return AppError::internal(e).into_response();
    }

    Json(json!({ "message": format!("Fight #{} is on hold and was sent back to matching.", target.match_number) })).into_response()
}

pub async fn update_display(
    State(state): State<Arc<AppState>>,
    admin: AuthedAdmin,
    crate::extract::JsonOrForm(body): crate::extract::JsonOrForm<UpdateDisplayRequest>,
) -> Response {
    let match_id = body.match_id;
    let is_display = body.is_display;

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return AppError::internal(e).into_response(),
    };
    if let Err(e) = sqlx::query("UPDATE matches SET is_display = 0").execute(&mut *tx).await {
        return AppError::internal(e).into_response();
    }
    let Some(match_number): Option<i32> = (sqlx::query_scalar("SELECT match_number FROM matches WHERE match_id = ?")
        .bind(match_id)
        .fetch_optional(&mut *tx)
        .await
        .unwrap_or(None))
    else {
        return fail(StatusCode::NOT_FOUND, "Fight not found.");
    };
    if let Err(e) = sqlx::query("UPDATE matches SET is_display = ? WHERE match_id = ?")
        .bind(is_display)
        .bind(match_id)
        .execute(&mut *tx)
        .await
    {
        return AppError::internal(e).into_response();
    }
    crate::services::transactions::create_log(
        &mut *tx,
        admin.account_id,
        "Admin App",
        &format!("Matches - Fight #{match_number} TV display status updated successfully.!"),
    )
    .await;
    if let Err(e) = tx.commit().await {
        return AppError::internal(e).into_response();
    }

    crate::services::realtime::tv_display(&state, match_id).await;

    Json(json!({ "message": "TV display status updated successfully." })).into_response()
}

/// `bridgeStatus` -- stub until the outgoing bridge client/worker exist (next phase). Matches
/// the disabled shape `MatchingBridge::status()` returns when the bridge isn't configured.
pub async fn bridge_status(State(state): State<Arc<AppState>>) -> Response {
    if !state.config.bridge_is_enabled() {
        return Json(json!({ "enabled": false })).into_response();
    }
    let pending: i64 = sqlx::query("SELECT COUNT(*) AS c FROM bridge_outbox WHERE sent_at IS NULL")
        .fetch_one(&state.db)
        .await
        .map(|r| r.get::<i64, _>("c"))
        .unwrap_or(0);
    Json(json!({ "enabled": true, "pending": pending })).into_response()
}
