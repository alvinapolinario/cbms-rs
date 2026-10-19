pub mod account;
pub mod admin;
pub mod bet;
pub mod cash_in;
pub mod cash_out;
pub mod claim;
pub mod desk;
pub mod event;
pub mod event_teller;
pub mod fight;
pub mod print_api;

use crate::AppState;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // Public (throttle:10,1 in the real app -- rate limiting is a later hardening pass).
        .route("/login-app", post(account::login_app))
        .route("/login-qr", post(account::login_qr))
        .route("/login-app-admin", post(account::login_app_admin))
        // account.token:any
        .route("/logout", post(account::logout))
        // account.token:teller
        .route("/change-password", post(account::change_password))
        .route("/register-desktop-uid", post(account::register_desktop_uid))
        .route("/create-bet", post(bet::create_bet))
        .route("/teller-data", post(event_teller::get_data_teller))
        .route("/reprint-last-receipt", post(bet::reprint))
        .route("/reprint-teller", post(bet::reprint_teller))
        .route("/latest-match", post(fight::get_latest_match_teller))
        .route("/bet-history", post(bet::bet_history))
        .route("/bet-history-ongoing", post(bet::bet_history_ongoing))
        .route("/cash-in", post(cash_in::store_by_teller))
        .route("/cash-in/history", post(cash_in::cash_in_history))
        .route("/cash-out", post(cash_out::store_by_teller))
        .route("/cash-out/history", post(cash_out::cash_out_history))
        .route("/claim-winning", post(claim::claim_winning))
        .route("/claim-winning-reprint", post(claim::claim_winning_reprint))
        .route("/claimed-receipts", post(claim::claimed_receipts))
        .route("/unclaimed-receipts", post(bet::unclaimed_receipts))
        .route("/void-bet", post(bet::void_bet))
        .route("/voided-bets", post(bet::voided_bets))
        // account.token:admin
        .route("/cash-in-admin", post(cash_in::store_by_admin_via_app))
        .route("/cash-in-admin/history", post(cash_in::cash_in_history_admin))
        .route("/cash-in-admin/approval", post(cash_in::approval_via_app))
        .route("/admin-data", post(admin::get_data))
        .route("/cash-out-admin", post(cash_out::store_by_admin_via_app))
        .route("/cash-out-admin/history", post(cash_out::cash_out_history_admin))
        .route("/cash-out-admin/approval", post(cash_out::approval_via_app))
        .route("/event-tellers-admin", post(event_teller::list_tellers_admin))
        .route("/reprint-admin", post(bet::reprint_admin))
        .route("/matches-admin/list", post(fight::list_app))
        .route("/matches-admin/get-started", post(fight::get_started_app))
        .route("/matches-admin/latest", post(fight::get_latest_match))
        .route("/matches-admin/toggle-meron", post(fight::toggle_meron))
        .route("/matches-admin/toggle-wala", post(fight::toggle_wala))
        .route("/matches-admin/update-bet-status", post(fight::update_match_bet_status))
        .route("/matches-admin/update-match-status", post(fight::update_match_status))
        .route("/matches-admin/update-display", post(fight::update_display))
        // Flagged deviation: Laravel only exposes `hold` on the web (session) routes -- added
        // here as an admin-token API route so the bridge-linked workflow is reachable at all
        // through this API-only port. See docs/WORKFLOW.md section 7.2 step 5 / the project plan.
        .route("/matches-admin/hold", post(fight::hold))
        .route("/events-admin/list", post(event::list))
        .route("/events-admin/find", post(event::find))
        .route("/events-admin/store", post(event::store))
        .route("/events-admin/update", post(event::update))
        .route("/events-admin/delete", post(event::delete))
        .route("/events-admin/tellers", post(event_teller::list_by_event_app))
        .route("/events-admin/update-tellers", post(event_teller::update_tellers_app))
        // Not a real api.php route (it's web-only, `/matches/bridge-status`) -- kept here,
        // unauthenticated, purely as an operator convenience; no real client depends on this
        // exact path.
        .route("/matches/bridge-status", get(fight::bridge_status))
        .route("/matches/new-match", get(fight::new_match))
        .route("/matches/get-started", get(fight::get_started_web_admin))
        .route("/tv/get-started", get(fight::get_started_web))
        .route("/tv/board", get(fight::tv_board))
        // Print service. Header X-Print-Key, same paths the FastAPI client calls.
        .route("/print-service/events", get(print_api::events))
        .route("/print-service/tellers", get(print_api::tellers))
        .route("/print-service/history/{event_teller_id}", get(print_api::history))
        .route("/print-service/event-totals", get(print_api::event_totals))
        .route("/print-service/cash-transactions", get(print_api::cash_transactions))
        .route("/print-service/cash-transactions/{event_teller_id}", get(print_api::teller_cash_summary))
        .route("/print-service/analytics/event/{event_id}", get(print_api::analytics_event))
        // Website actions. Admin bearer token stands in for the Blade session.
        .route("/accounts/admins/list", post(desk::admin_list))
        .route("/accounts/admins/store", post(desk::admin_store))
        .route("/accounts/admins/update", post(desk::admin_update))
        .route("/accounts/admins/find/{admin_id}", post(desk::admin_find))
        .route("/accounts/admins/{admin_id}", axum::routing::delete(desk::admin_delete))
        .route("/accounts/tellers/list", post(desk::teller_list))
        .route("/accounts/tellers/store", post(desk::teller_store))
        .route("/accounts/tellers/update", post(desk::teller_update))
        .route("/accounts/tellers/find/{teller_id}", post(desk::teller_find))
        .route("/accounts/tellers/{teller_id}/clear-phone-uid", post(desk::teller_clear_phone))
        .route("/accounts/tellers/{teller_id}", axum::routing::delete(desk::teller_delete))
        .route("/cash-transactions/board", post(desk::cash_board))
        .route("/cash-transactions/cash-in", post(desk::cash_in_store))
        .route("/cash-transactions/cash-in/approve", post(desk::cash_in_approve))
        .route("/cash-transactions/cash-in/decline", post(desk::cash_in_decline))
        .route("/cash-transactions/cash-out/approve", post(desk::cash_out_approve))
        .route("/cash-transactions/cash-out/decline", post(desk::cash_out_decline))
        .route("/teller-transactions/get-data", post(desk::teller_tx_data))
        .route("/teller-transactions/history", post(desk::teller_history))
        .route("/bets/list/{event_id}", post(desk::bet_list))
        .route("/unclaimed/list/{event_id}", post(desk::unclaimed_list))
        .route("/reports/report/{event_id}", post(desk::report))
        .route("/logs/list", post(desk::logs))
        .route("/salaries/list", post(desk::salary_list))
        .route("/salaries/store/{event_id}", post(desk::salary_store))
        .route("/salaries/update", post(desk::salary_update))
        .route("/salaries/find/{salary_id}", post(desk::salary_find))
        .route("/salaries/{salary_id}", axum::routing::delete(desk::salary_delete))
        .route("/remittances/tellers/list", post(desk::remittance_teller_list))
        .route("/remittances/admins/list", post(desk::remittance_admin_list))
        .route("/remittances/generate/{event_id}", post(desk::remittance_generate))
        .route("/remittances/teller/update", post(desk::remittance_teller_update))
        .route("/remittances/admin/update", post(desk::remittance_admin_update))
        .route("/remittances/teller/find/{teller_remittance_id}", post(desk::remittance_teller_find))
        .route("/remittances/admin/find/{admin_remittance_id}", post(desk::remittance_admin_find))
        .route("/remittances/{teller_remittance_id}", axum::routing::delete(desk::remittance_delete))
        .route("/remittances/teller-shorts/list/{teller_remittance_id}", get(desk::teller_shorts_list))
        .route("/remittances/teller-shorts/store", post(desk::teller_short_store))
        .route("/remittances/teller-shorts/delete/{teller_short_id}", axum::routing::delete(desk::teller_short_delete))
        .route("/remittances/admin-shorts/list/{admin_remittance_id}", get(desk::admin_shorts_list))
        .route("/remittances/admin-shorts/store", post(desk::admin_short_store))
        .route("/remittances/admin-shorts/delete/{admin_short_id}", axum::routing::delete(desk::admin_short_delete))
}
