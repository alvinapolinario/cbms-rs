# Sabong2026 Laravel reference

Source of truth for the Rust port in this repo.

- Laravel app: `D:\2026_Projects\Sabong2026\api`
- Print service: `D:\2026_Projects\Sabong2026\print-service`
- Scanned: 2026-10-19

Laravel 10 on PHP 8.1. API routes are prefixed with `/api` by `RouteServiceProvider`. The admin website is session-based Blade. A separate FastAPI app prints receipts and keeps analytics in its own database.

This Rust crate already exposes most of the phone JSON routes. It does not yet match the website, the print API, or every behavior detail below. The checklist at the end is the integration task list.

## Runtime

Controllers talk to MySQL directly. There is no service layer and no Form Request classes. Each action can write an audit row through `Controller::createLog` into `transactions`. Live screens use `ShouldBroadcastNow` (the queue driver is `sync`).

Domain tables are not created by migrations in this repo. The app expects an existing MySQL schema plus these views:

- `event_tellers_view`
- `admins_view`
- `bets_view`
- `claims_view`
- `cash_ins_view`
- `cash_outs_view`

Sanctum uses `personal_access_tokens`. `beyondcode/laravel-websockets` uses `websockets_statistics_entries`.

Other pieces that are present and not part of the fight flow: Vuexy layout helpers in `app/Helpers/Helpers.php`, theme settings in `config/custom.php`, the stock `inspire` command, and the framework test skeleton. Octane is a dependency. `RestoreController` reloads it after a database restore.

## Phase 1 notes (2026-10-19)

Shared betting rules in this crate were aligned with `BetController@store` and `MatchController@updateMatchStatus`.

- Odds stay in `src/services/odds.rs`: house cut is `event_percentage`, each side is `round(net / side total, 2)`, and a one-sided pool becomes odds `1` on create-bet and void.
- A completed fight pays `floor(bet_amount * odds)` in `src/services/settlement.rs`.
- Create-bet treats JSON `match_id` as `match_number`, and the open fight is the highest `match_number`.
- Receipts use `event_teller_id` + match number + 7 random digits (`RECEIPT_RANDOM_DIGITS`, default 7).
- A teller named `special` can bet a side whose lock is `1`.
- `create_log` inserts `transactions` the same way `Controller::createLog` does (`account_id`, type, message, `NOW()`). Per-route message text is checked in later phases.
- Tokens are stored in plaintext with `name` `teller-token` or `admin-token` and no `expires_at`, matching Laravel login. This API still requires the bearer token. Laravel's `account.token` middleware does not. A token from either app can be looked up by the other.

Teller handlers were compared with the PHP methods on 2026-10-19. Differences that the phone app can see were corrected: a wrong teller password returns `Invalid credentials! Please contact administrator!`; bet history (both routes) is only the active event's ongoing fight; unclaimed receipts are Draw and Cancelled stakes only; a payout is refused when the receipt belongs to another teller; login, teller-data, and latest-match return the three fight fields Laravel returns. Live websocket broadcasts are still open in checklist section 7.

The live schema was not confirmed. MariaDB 12.3 is listening on `127.0.0.1:3306`. `sabong_root` from `Sabong2026\api\.env` was rejected, and `root` with an empty password was rejected. The checklist item stays open until a working login can list the tables and the six views.

## Auth

| Surface | Mechanism | What it actually checks |
|---|---|---|
| Teller and admin phone API | `account.token` middleware, bearer token | The middleware is registered and currently does nothing. Handlers trust `event_teller_id` or `admin_id` from the body. |
| Token row | `personal_access_tokens` | Plain 60-character string from `Str::random(60)`, not the hashed Sanctum format. Logout deletes the row matching the bearer token. |
| Teller token | `tokenable_type = EventTeller`, `name = teller-token` | `tokenable_id` is `event_teller_id`. |
| Admin token | `tokenable_type = EventAdmin`, `name = admin-token` | `tokenable_id` is `admin_id`. |
| Website | Session keys `account_id`, `account_type`, `admin` | `isAdmin` allows `account_type == Admin` only. `POST /login` returns the body `1` or `0`. |
| Unclaimed page | Session key `unclaimed_authenticated` | A second password is hardcoded in `routes/web.php` before the list loads. |
| Print API | Header `X-Print-Key` | `hash_equals` against `PRINT_SERVICE_KEY`. Empty key returns 503. |
| nginx `/print` | `GET /internal/admin-session` | Must return 200 or 401, not a redirect. |

`isLoggedIn` lets guests onto `/login` and sends an already signed-in admin to `/dashboard`. The `isAll` group in `web.php` is empty. API group throttle is 60 requests per minute.

Teller login (`POST /api/login-app`):

1. Account must exist, `account_type = Teller`, `is_active = 1`.
2. Row must exist in `event_tellers_view` for an `Active` event.
3. Password is bcrypt.
4. An `Active` event must exist.
5. If `phone_uid` is already stored, the request must match it.
6. If `phone_uid` is empty, that device must not already belong to another teller.
7. Response includes token, teller, event, and the latest fight (`match_id`, `match_number`, `match_bet_status`).

Admin app login (`POST /api/login-app-admin`) reads `admins_view` where `is_active = 1`, requires an `Active` event, and also returns approved cash-in and cash-out sums.

## Fight rules

One event has `event_status = Active`.

A new fight is `Ongoing`, `match_bet_status = Open`, both side locks `0` (open), odds `0`. Creating it sets every teller's `teller_match_balance` to `0`.

`POST /api/create-bet`:

- Fight must be the latest `Ongoing` fight with betting `Open` on the active event.
- The JSON field named `match_id` is compared to `match_number`, not `matches.match_id`.
- Teller must exist and `phone_uid` must be set.
- Side is `Meron` or `Wala`. Side lock `1` rejects that side, except a teller whose name is `special`.
- Receipt code is `event_teller_id` + that match number + 7 random digits.
- `bet_status` starts at `1`.
- Odds: each side net is `side_total * (1 - event_percentage)`. Side odds are `round((meron net + wala net) / side total, 2)`. If either side total is `0`, both odds are `1`.
- After the match row commits, the stake is added to `teller_balance` and `teller_match_balance`.
- Broadcasts `MatchUpdated` and `TellerBalanceUpdated`.

Declaring a result is a website action, `MatchController@updateMatchStatus`:

- `Completed`: set `match_winner` to `Meron` or `Wala`, recompute side totals from bets with `bet_status = 1`, recompute odds, clear previous winners, then set `bet_is_winner = 1` and `bet_payout_amount = floor(bet_amount * odds)` on the winning side.
- `Draw` or `Cancelled`: `match_winner = N/A`, `bet_payout_amount = bet_amount` so the stake can be returned.
- `is_display` is set to `1`.
- Unless `dont_create` is sent, a new `Ongoing` fight is opened immediately.
- Broadcasts `TVDisplayUpdated`, `MatchUpdated`, and `MatchesUpdated`.

Void (`POST /api/void-bet`): own receipt, no claim row, fight still `Ongoing`, betting not `Closed`. Sets `bet_status = 2` and `bet_void_datetime`.

Claim (`POST /api/claim-winning`): refuses status `2`, a zero payout, a different teller, an inactive event, an existing `claims_view` row, or `teller_balance` below the payout. Inserts `claims`, stamps `bet_payout_datetime` and `bet_payout_by`, subtracts the payout from `teller_balance`.

Cash-in from a teller creates `cash_in_type = TellerCashIn` with status `Pending`. Cash-out is also `Pending`. Balance changes when an admin approves. Decline actions exist on the website and on the admin app approval routes.

## Status values

| Field | Values |
|---|---|
| `events.event_status` | `Active` is the live event. Other values block bets, claims, and cash requests. |
| `matches.match_status` | `Ongoing`, `Completed`, `Draw`, `Cancelled` |
| `matches.match_bet_status` | `Open`, `Closed` |
| `matches.meron_bet_status`, `wala_bet_status` | `0` open, `1` disabled |
| `bets.bet_status` | `1` live, `2` void |
| `bets.bet_side` | `Meron`, `Wala` |
| `bets.bet_is_winner` | `0` or `1` |
| `cash_ins.cash_in_status`, `cash_outs.cash_out_status` | `Pending`, `Approved`, plus decline actions |
| `accounts.account_type` | `Teller`, `Admin` |
| `accounts.is_active` | `0` or `1` |

## Models

Timestamps are off. No Eloquent relationships are declared.

| Model | Table | Primary key | Fields the handlers use |
|---|---|---|---|
| Account | `accounts` | `account_id` | `username`, `password`, `account_type`, `is_active`, `account_date_created` |
| Admin | `admins` | `admin_id` | `account_id`, `admin_name` |
| Teller | `tellers` | `teller_id` | `account_id`, `contact_number`, `teller_name`, `phone_uid` |
| Event | `events` | `event_id` | `event_name`, `event_description`, `event_percentage`, `event_date`, `event_status`, `admin_cash_on_hand`, `teller_initial_cash_on_hand`, `prizes`, `rd` |
| EventTeller | `event_tellers` | `event_teller_id` | `event_id`, `teller_id`, `teller_balance`, `teller_match_balance` |
| Fight | `matches` | `match_id` | `event_id`, `match_number`, `match_winner`, side totals, odds, side locks, `match_status`, `match_bet_status`, `match_created_datetime`, `is_display`, `meron_entry`, `wala_entry` |
| Bet | `bets` | `bet_id` | receipt, side, amount, win, payout, status, datetime, `bet_is_winner`, void time, `bet_payout_by` |
| Claim | `claims` | `claim_id` | `bet_id`, `event_teller_id`, `claim_amount`, `claim_datetime` |
| CashIn | `cash_ins` | `cash_in_id` | `admin_id`, `event_teller_id`, amount, datetime, `cash_in_type`, status |
| CashOut | `cash_outs` | `cash_out_id` | `event_teller_id`, `admin_id`, amount, datetime, status |
| Salary | `salaries` | `salary_id` | `event_id`, `employee_type`, `employee_name`, `daily_rate`, `overtime_rate`, `total_overtime`, `bonus` |
| TellerRemittance | `teller_remittances` | `teller_remittance_id` | `event_teller_id`, `denom_1000` down to `denom_1` |
| AdminRemittance | `admin_remittances` | `admin_remittance_id` | `event_id`, same denomination counts |
| TellerShort | `teller_shorts` | `teller_short_id` | `teller_remittance_id`, `short_amount`, `short_remarks` |
| AdminShort | `admin_shorts` | `admin_short_id` | `admin_remittance_id`, `short_amount`, `short_remarks` |
| Transaction | `transactions` | `transaction_id` | `account_id`, `transaction_type`, `transaction_message`, `transaction_datetime` |

## Broadcast channels

Public Pusher-compatible channels (`beyondcode/laravel-websockets`). `channels.php` only has the unused stock private user channel.

| Event | Channel | Client event name | Payload |
|---|---|---|---|
| `MatchUpdated` | `match-updates` | `MatchUpdated` | `match` |
| `MatchesUpdated` | `fight-history` | class name `MatchesUpdated` | `match` (the list) |
| `TVDisplayUpdated` | `tv-display` | class name `TVDisplayUpdated` | `match` |
| `TellerBalanceUpdated` | `event-tellers` | `TellerBalanceUpdated` | tellers with `total_bet` and `total_payout` |
| `CashInUpdated` | `cash-ins` | `CashInUpdated` | `cash_ins` |
| `CashOutUpdated` | `cash-outs` | `CashOutUpdated` | `cash_outs` |
| `EventUpdated` | `event-updates` | `.EventUpdated` | `event` |

`EventUpdated` prefixes the name with a dot. The others that set `broadcastAs` do not.

`TellerBalanceUpdated` rows add `total_bet` (sum of `bets_view.bet_amount` where `bet_status = 1` and match status is not `Draw` or `Cancelled`) and `total_payout` (sum of `claims.claim_amount`).

## Teller app routes

All `POST`. Prefixed with `/api`. Login is public. The rest are tagged `account.token`.

| Path | Handler | Role |
|---|---|---|
| `/api/login-app` | `AccountController@loginApp` | Teller login |
| `/api/logout` | `AccountController@logoutApp` | Delete bearer token |
| `/api/change-password` | `AccountController@changePasswordApp` | New password and `phone_uid` |
| `/api/teller-data` | `EventTellerController@getData` | Teller snapshot |
| `/api/latest-match` | `MatchController@getLatestMatch` | Latest fight for the teller |
| `/api/create-bet` | `BetController@store` | Place a bet |
| `/api/void-bet` | `BetController@voidBet` | Void own open receipt |
| `/api/voided-bets` | `BetController@voidedBets` | Voided receipts |
| `/api/reprint-last-receipt` | `BetController@reprint` | Last receipt |
| `/api/reprint-teller` | `BetController@reprintTeller` | Reprint a teller receipt |
| `/api/bet-history` | `BetController@betHistory` | Past bets |
| `/api/bet-history-ongoing` | `BetController@betHistoryOngoing` | Current fight |
| `/api/unclaimed-receipts` | `BetController@unclaimedReceipts` | Unclaimed winners |
| `/api/claim-winning` | `ClaimController@claimWinning` | Pay a receipt |
| `/api/claim-winning-reprint` | `ClaimController@claimWinningReprint` | Reprint a claim |
| `/api/claimed-receipts` | `ClaimController@cashOutHistory` | Claimed receipts |
| `/api/cash-in` | `CashInController@storeByTeller` | Pending cash-in |
| `/api/cash-in/history` | `CashInController@cashInHistory` | Teller cash-in list |
| `/api/cash-out` | `CashOutController@storeByTeller` | Pending cash-out |
| `/api/cash-out/history` | `CashOutController@cashOutHistory` | Teller cash-out list |

## Admin phone routes

| Path | Handler | Role |
|---|---|---|
| `/api/login-app-admin` | `AccountController@loginAppAdmin` | Admin app login |
| `/api/admin-data` | `AdminController@getData` | Admin payload |
| `/api/event-tellers-admin` | `EventTellerController@listTellersAdmin` | Tellers on the event |
| `/api/reprint-admin` | `BetController@reprintAdmin` | Reprint any receipt |
| `/api/cash-in-admin` | `CashInController@storeByAdminViaApp` | Admin cash-in |
| `/api/cash-in-admin/history` | `CashInController@cashInHistoryAdmin` | Cash-in list |
| `/api/cash-in-admin/approval` | `CashInController@approvalViaApp` | Approve or decline |
| `/api/cash-out-admin` | `CashOutController@storeByAdminViaApp` | Admin cash-out |
| `/api/cash-out-admin/history` | `CashOutController@cashOutHistoryAdmin` | Cash-out list |
| `/api/cash-out-admin/approval` | `CashOutController@approvalViaApp` | Approve or decline |

Fight open, close, side locks, and declaring a winner are not on this API. They are website routes under `/matches`.

## Admin website routes

Session, `isAdmin`, except the public rows. Menu file: `resources/menu/admin_menu.json`.

Screens: Home, Admins, Tellers, TV, TV New, Events, Matches, Cash Transactions, Teller Transactions, Bets, Unclaimed, Remittances, Salaries, Reports, Logs, Reset, Restore, Logout.

| Method | Path | Handler |
|---|---|---|
| GET | `/test/{side}/{amount}/{event_teller_id}/{match_id}` | `PageController@test` |
| GET | `/clear` | `AccountController@clear` (wipes every `phone_uid`) |
| GET | `/logout` | `AccountController@logout` |
| GET | `/lang/ar`, `/lang/en` | `PageController@dashboard` |
| GET | `/tv` | `PageController@tv` |
| GET | `/tv-new` | `PageController@tvNew` |
| GET | `/tv/get-started` | `MatchController@getStarted` |
| GET | `/internal/admin-session` | `AdminSessionController@check` |
| GET | `/`, `/login` | `PageController@login` (`isLoggedIn`) |
| POST | `/login` | `AccountController@loginAccount` |
| GET | `/dashboard` | `PageController@dashboard` |
| GET | `/accounts/admins` | `AdminController@index` |
| POST | `/accounts/admins/list` | `AdminController@list` |
| POST | `/accounts/admins/store` | `AdminController@store` |
| POST | `/accounts/admins/update` | `AdminController@update` |
| POST | `/accounts/admins/find/{admin_id}` | `AdminController@find` |
| DELETE | `/accounts/admins/{admin_id}` | `AdminController@delete` |
| GET | `/accounts/tellers` | `TellerController@index` |
| POST | `/accounts/tellers/list` | `TellerController@list` |
| POST | `/accounts/tellers/store` | `TellerController@store` |
| POST | `/accounts/tellers/update` | `TellerController@update` |
| POST | `/accounts/tellers/find/{teller_id}` | `TellerController@find` |
| POST | `/accounts/tellers/{teller_id}/clear-phone-uid` | `TellerController@clearPhoneUID` |
| DELETE | `/accounts/tellers/{teller_id}` | `TellerController@delete` |
| GET | `/events` | `EventController@index` |
| POST | `/events/list` | `EventController@list` |
| POST | `/events/{event}/tellers` | `EventTellerController@listByEvent` |
| POST | `/events/{event}/update-tellers` | `EventTellerController@updateTellers` |
| POST | `/events/store` | `EventController@store` |
| POST | `/events/update` | `EventController@update` |
| POST | `/events/find/{event_id}` | `EventController@find` |
| DELETE | `/events/{event_id}` | `EventController@delete` |
| GET | `/matches` | `MatchController@index` |
| POST | `/matches/list` | `MatchController@list` |
| GET | `/matches/get-started` | `MatchController@getStarted` |
| GET | `/matches/new-match` | `MatchController@newMatch` |
| POST | `/matches/{match_id}/toggle-meron` | `MatchController@toggleMeron` |
| POST | `/matches/{match_id}/toggle-wala` | `MatchController@toggleWala` |
| POST | `/matches/{match_id}/{match_bet_status}/update-bet-status` | `MatchController@updateMatchBetStatus` |
| POST | `/matches/{match_id}/{match_status}/{winner}/update-match-status` | `MatchController@updateMatchStatus` |
| POST | `/matches/{match_id}/update-display` | `MatchController@updatedisplay` |
| POST | `/matches/find/{match_id}` | `MatchController@find` |
| DELETE | `/matches/{match_id}` | `MatchController@delete` |
| GET | `/reports` | `EventController@reports` |
| POST | `/reports/report/{event_id}` | `MatchController@report` |
| GET | `/cash-transactions` | `PageController@cashTransactions` |
| GET | `/cash-transactions/tellers/load` | `EventTellerController@getTellers` |
| POST | `/cash-transactions/tellers/list` | `EventTellerController@listTellersByEvent` |
| POST | `/cash-transactions/cash-in` | `CashInController@storeByAdmin` |
| POST | `/cash-transactions/cash-in/approve` | `CashInController@approveCashIn` |
| POST | `/cash-transactions/cash-in/decline` | `CashInController@declineCashIn` |
| POST | `/cash-transactions/cash-out/approve` | `CashOutController@approveCashOut` |
| POST | `/cash-transactions/cash-out/decline` | `CashOutController@declineCashOut` |
| GET | `/teller-transactions` | `PageController@tellerTransactions` |
| POST | `/teller-transactions/get-data` | `EventController@getData` |
| POST | `/teller-transactions/history` | `BetController@history` |
| GET | `/logs` | `PageController@logs` |
| POST | `/logs/list` | `TransactionController@fetchLogs` |
| GET | `/bets` | `PageController@bets` |
| POST | `/bets/list/{event_id}` | `BetController@listAll` |
| GET | `/unclaimed-password` | password form |
| POST | `/unclaimed-password` | password check |
| GET | `/unclaimed` | `PageController@unclaimed` |
| POST | `/unclaimed/list/{event_id}` | `BetController@listAllUnclaimed` |
| GET | `/salaries` | `PageController@salaries` |
| POST | `/salaries/list` | `SalaryController@list` |
| POST | `/salaries/store/{event_id}` | `SalaryController@store` |
| POST | `/salaries/update` | `SalaryController@update` |
| POST | `/salaries/find/{salary_id}` | `SalaryController@find` |
| DELETE | `/salaries/{salary_id}` | `SalaryController@delete` |
| GET | `/remittances` | `PageController@remittances` |
| POST | `/remittances/tellers/list` | `TellerRemittanceController@list` |
| POST | `/remittances/admins/list` | `AdminRemittanceController@list` |
| POST | `/remittances/generate/{event_id}` | `TellerRemittanceController@generate` |
| POST | `/remittances/teller/update` | `TellerRemittanceController@update` |
| POST | `/remittances/admin/update` | `AdminRemittanceController@update` |
| POST | `/remittances/teller/find/{id}` | `TellerRemittanceController@find` |
| POST | `/remittances/admin/find/{id}` | `AdminRemittanceController@find` |
| DELETE | `/remittances/{teller_remittance_id}` | `TellerRemittanceController@delete` |
| GET | `/remittances/teller-shorts/list/{id}` | `TellerShortController@list` |
| POST | `/remittances/teller-shorts/store` | `TellerShortController@store` |
| DELETE | `/remittances/teller-shorts/delete/{id}` | `TellerShortController@delete` |
| GET | `/remittances/admin-shorts/list/{id}` | `AdminShortController@list` |
| POST | `/remittances/admin-shorts/store` | `AdminShortController@store` |
| DELETE | `/remittances/admin-shorts/delete/{id}` | `AdminShortController@delete` |
| GET | `/reset` | `ResetController@index` |
| POST | `/reset/execute` | `ResetController@execute` |
| GET | `/restore` | `RestoreController@index` |
| POST | `/restore/execute` | `RestoreController@execute` |

Controller methods with no route in `web.php` or `api.php`: `AccountController@updatePassword`, `PageController@changePassword`, `SalaryController@index`.

Reset and restore shell out to `mysqldump` and the `mysql` client. Reset also clears `phone_uid`.

## Print

Laravel read API, middleware `print.key`:

| Path | Handler | Returns |
|---|---|---|
| `GET /api/print-service/events` | `PrintApiController@events` | All events |
| `GET /api/print-service/tellers` | `PrintApiController@tellers` | Tellers plus bet, payout, cash-in, cash-out, cash on hand. `event_id` optional; defaults to the active event. |
| `GET /api/print-service/history/{eventTellerId}` | `PrintApiController@history` | Per-fight Meron and Wala totals |
| `GET /api/print-service/event-totals` | `PrintApiController@eventTotals` | Event totals |
| `GET /api/print-service/cash-transactions` | `PrintApiController@cashTransactions` | Cash movements |
| `GET /api/print-service/cash-transactions/{eventTellerId}` | `PrintApiController@tellerCashSummary` | One teller |
| `GET /api/print-service/analytics/event/{eventId}` | `PrintApiController@analyticsEvent` | Snapshot payload |

FastAPI (`print-service`, port 8100, nginx prefix `/print/`):

| Path | Role |
|---|---|
| `GET /` | Desk UI: Print Bets and Print Cash |
| `GET /analytics` | Snapshot browser |
| `GET /api/health` | Liveness. Detail requires `X-API-Key` when `PRINT_API_KEY` is set. |
| `GET /api/tellers` | Teller list via Laravel |
| `POST /api/print/{event_teller_id}` | 80mm fight-summary receipt |
| `POST /api/print/cash-transactions/{event_teller_id}` | Cash receipt |
| `GET /api/printer`, `PUT /api/printer` | Printer address. Default `10.254.0.13:9100`. |
| `POST /api/analytics/sync/{event_id}`, `POST /api/analytics/sync-all` | Copy into `sabong_print_analytics` |
| `GET /api/analytics/events`, `/snapshots/{id}`, `/compare`, `/monthly-top-grossers` | Read analytics |

The print app prefers `X-Print-Key`. If `LARAVEL_PRINT_KEY` is unset it logs in through `POST /login` with `LARAVEL_USERNAME` and `LARAVEL_PASSWORD`. Analytics MariaDB is separate from the betting database. `PRINT_DRY_RUN` skips the printer socket.

## Rust routes that Sabong2026 `api.php` does not have

These exist in `src/routes/mod.rs` from an earlier port. Sabong2026 does not expose them on the phone API.

| Rust path | Where Sabong2026 does it |
|---|---|
| `/api/login-qr` | No `loginAppQr` in this Laravel tree |
| `/api/register-desktop-uid` | No matching route. `phone_uid` is set by teller login and `change-password` |
| `/api/matches-admin/*` | Website `/matches/*`, session auth |
| `/api/events-admin/*` | Website `/events/*`, session auth |
| `/api/matches-admin/hold` | No hold route in this Laravel tree |
| `GET /api/matches/bridge-status` | No bridge-status route in this Laravel tree |

## Integration checklist

Check an item only after the Rust handler matches the Laravel behavior, including status codes, JSON fields, and the database writes. A route that already exists in this crate is listed under parity, not as finished.

### 1. Schema and shared rules

- [ ] Confirm the live MySQL database matches the models and the six views above
- [x] Port odds: `event_percentage` cut, round to 2 decimal places, odds `1` when either side is empty
- [x] Port payout as `floor(bet_amount * odds)`
- [x] Treat create-bet `match_id` as `match_number`
- [x] Receipt code: `event_teller_id` + match number + 7 digits
- [x] Allow teller name `special` to bet a disabled side
- [x] Write `transactions` audit rows the way `createLog` does
- [x] Decide whether API tokens stay unchecked, as in this Laravel app, or become enforced

### 2. Teller API parity

Compared with the Sabong2026 controllers on 2026-10-19. JSON shape, filters, and the database writes match those methods. Websocket broadcasts from these actions are still section 7.

- [x] `POST /api/login-app`
- [x] `POST /api/logout`
- [x] `POST /api/change-password`
- [x] `POST /api/teller-data`
- [x] `POST /api/latest-match`
- [x] `POST /api/create-bet`
- [x] `POST /api/void-bet`
- [x] `POST /api/voided-bets`
- [x] `POST /api/reprint-last-receipt`
- [x] `POST /api/reprint-teller`
- [x] `POST /api/bet-history`
- [x] `POST /api/bet-history-ongoing`
- [x] `POST /api/unclaimed-receipts`
- [x] `POST /api/claim-winning`
- [x] `POST /api/claim-winning-reprint`
- [x] `POST /api/claimed-receipts`
- [x] `POST /api/cash-in`
- [x] `POST /api/cash-in/history`
- [x] `POST /api/cash-out`
- [x] `POST /api/cash-out/history`

### 3. Admin phone API parity

Compared with the Sabong2026 controllers on 2026-10-19. JSON shape and the cash balance writes match those methods. Websocket broadcasts from these actions are still section 7.

- [x] `POST /api/login-app-admin`
- [x] `POST /api/admin-data`
- [x] `POST /api/event-tellers-admin`
- [x] `POST /api/reprint-admin`
- [x] `POST /api/cash-in-admin`
- [x] `POST /api/cash-in-admin/history`
- [x] `POST /api/cash-in-admin/approval`
- [x] `POST /api/cash-out-admin`
- [x] `POST /api/cash-out-admin/history`
- [x] `POST /api/cash-out-admin/approval`

### 4. Routes this crate added

- [ ] Keep, move, or remove `/api/login-qr`
- [ ] Keep, move, or remove `/api/register-desktop-uid`
- [ ] Point `/api/matches-admin/*` at `MatchController` website behavior, or leave them as an API-only extra
- [ ] Point `/api/events-admin/*` at `EventController` website behavior, or leave them as an API-only extra
- [ ] Remove `/api/matches-admin/hold` unless a client still calls it
- [ ] Remove `GET /api/matches/bridge-status` unless an operator still uses it

### 5. Print API

Header `X-Print-Key` must equal `PRINT_SERVICE_KEY`. An empty key answers 503. Sums PHP casts with `(float)` are JSON numbers. Event rows keep Laravel's string decimals.

- [x] `GET /api/print-service/events`
- [x] `GET /api/print-service/tellers`
- [x] `GET /api/print-service/history/{eventTellerId}`
- [x] `GET /api/print-service/event-totals`
- [x] `GET /api/print-service/cash-transactions`
- [x] `GET /api/print-service/cash-transactions/{eventTellerId}`
- [x] `GET /api/print-service/analytics/event/{eventId}`
- [x] JSON shapes match `print-service/app/laravel_client.py` (`events` is a list, tellers carry `total_bet` / `total_payout` / `total_cash_in` / `total_cash_out` / `cash_on_hand`, history has `fights`, analytics sorts tellers by `total_bet`)

### 6. Website actions not on `api.php`

The Blade site uses a session. These routes use the admin bearer token from `POST /api/login-app-admin`. Mutation replies that Laravel returns as `1`, `0`, or `-1` are that same plain text.

- [x] Accounts: `POST /api/accounts/admins|tellers/list|store|update|find/{id}`, `DELETE /api/accounts/admins|tellers/{id}`, `POST /api/accounts/tellers/{id}/clear-phone-uid`
- [x] Events: already on `/api/events-admin/*` (list, store, update, find, delete, tellers, update-tellers)
- [x] Matches: `/api/matches-admin/*` plus `GET /api/matches/new-match` and `GET /api/matches/get-started`. `web.php` names `find` and `delete`, and `MatchController` has neither method, so those two are not added
- [x] Cash desk: `POST /api/cash-transactions/cash-in`, `.../cash-in/approve`, `.../cash-in/decline`, `.../cash-out/approve`, `.../cash-out/decline`
- [x] Teller transaction history and bet lists: `POST /api/teller-transactions/get-data`, `POST /api/teller-transactions/history`, `POST /api/bets/list/{event_id}`
- [x] Unclaimed list: `POST /api/unclaimed/list/{event_id}`
- [x] Reports: `POST /api/reports/report/{event_id}`
- [x] Audit log list: `POST /api/logs/list`
- [x] Salaries: `POST /api/salaries/list|store/{event_id}|update|find/{id}`, `DELETE /api/salaries/{id}`
- [x] Remittances, denomination counts, and shorts: `/api/remittances/...`
- [x] TV payloads: `GET /api/tv/get-started` (no token, same as `web.php`)
- [x] Reset and restore stay off this API. `ResetController@execute` truncates bets, cash, claims, matches, tokens, tellers' `phone_uid`, logs, event tellers, shorts, salaries, and remittances after the operator types `RESET`, and it writes a mysqldump first. `RestoreController` reloads that dump. Run those from the Laravel operator pages, not from this service

### 7. Live updates

Published with the Pusher REST call Laravel uses (`POST /apps/{id}/events`, HMAC-SHA256). The database snapshot is taken before the HTTP response; the POST itself runs in the background.

Wire names are what `BroadcastEvent` sends and what the Blade `Echo` listeners format to. `MatchesUpdated` and `TVDisplayUpdated` have no `broadcastAs`, so the name is the class: `App\Events\MatchesUpdated` and `App\Events\TVDisplayUpdated`. Listeners written as `.listen("MatchesUpdated")` resolve to that class name.

- [x] `match-updates` / `MatchUpdated` / `{match}`
- [x] `fight-history` / `App\Events\MatchesUpdated` / `{match: [fights]}`
- [x] `tv-display` / `App\Events\TVDisplayUpdated` / `{match}`
- [x] `event-tellers` / `TellerBalanceUpdated` / `{event_tellers}` including `total_bet` and `total_payout`
- [x] `cash-ins` / `CashInUpdated` / `{cash_ins}`
- [x] `cash-outs` / `CashOutUpdated` / `{cash_outs}`
- [x] `event-updates` / `.EventUpdated` / `{event}` — Sabong2026 defines this event and never broadcasts it. This service publishes it when an event is stored or updated, with `broadcastAs` left as `.EventUpdated`
