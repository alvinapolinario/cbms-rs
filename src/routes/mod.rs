pub mod account;
pub mod admin;
pub mod bet;
pub mod cash_in;
pub mod cash_out;
pub mod claim;
pub mod event;
pub mod event_teller;
pub mod fight;

use crate::AppState;
use axum::routing::post;
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
        .route("/matches/bridge-status", axum::routing::get(fight::bridge_status))
}
