//! The real mobile/desktop clients were built against the Laravel API, which accepts a request
//! body as either `application/json` or `application/x-www-form-urlencoded` transparently
//! (`$request->all()` doesn't care which). Confirmed in practice: the Android teller app POSTs
//! form-urlencoded bodies, which an `axum::Json`-only extractor rejects with a 415 *before ever
//! reaching the handler* -- no log line, no business logic, just a generic failure the app shows
//! as "Invalid credentials". `JsonOrForm` dispatches on the `Content-Type` header to whichever
//! extractor actually matches, so this API stays usable by the existing clients, not just a
//! fresh JSON-only one.
//!
//! The form branch deliberately does NOT use `axum::Form` (which decodes with
//! `serde_html_form`/`serde_urlencoded`, both of which only understand a repeated plain
//! `key=a&key=b` -- NOT PHP/Laravel's `key[]=a&key[]=b` bracket-array convention, which is what
//! PHP's own `parse_str` produces and what a Laravel-targeting form submission is likely to use
//! for an array field). Confirmed the hard way: `tellers[]=150&tellers[]=154&...` through
//! `axum::Form` silently kept only one element of the array instead of erroring -- a request
//! that looked like a normal form submission wiped out most of an Active event's assigned
//! tellers because the "rest" of the desired list vanished during decoding. `serde_qs`
//! understands the bracket convention (and plain repeated keys, and indexed `key[0]=`), so it's
//! used for the form branch instead.
//!
//! A third source matters too: Laravel's `$request->input()`/magic property access reads from
//! the URL query string and the body interchangeably -- callers don't have to pick one. The
//! Android teller app relies on exactly this: `void-bet`, `cash-in/history`, and
//! `cash-out/history` are POSTed with the params in the query string (e.g.
//! `POST /api/void-bet?event_teller_id=130&receipt_code=...`) and an empty/no body, which has no
//! `Content-Type` the first two branches recognize -- confirmed via the real app's requests in
//! the nginx access log, all three returning a clean 415 before ever reaching the handler. So:
//! when there's no body to speak of (empty bytes, or a `Content-Type` that matches neither JSON
//! nor form), fall back to parsing the URI's query string the same way.

use axum::async_trait;
use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::json;

pub struct JsonOrForm<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for JsonOrForm<T>
where
    T: DeserializeOwned + Send + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let query = req.uri().query().unwrap_or("").to_string();

        if content_type.starts_with("application/json") {
            let Json(value) = Json::<T>::from_request(req, state).await.map_err(|e| e.into_response())?;
            Ok(JsonOrForm(value))
        } else if content_type.starts_with("application/x-www-form-urlencoded") {
            let bytes = Bytes::from_request(req, state).await.map_err(|e| e.into_response())?;
            if bytes.is_empty() {
                return parse_query(&query);
            }
            let value: T = serde_qs::Config::new(5, false).deserialize_bytes(&bytes).map_err(|e| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "message": format!("Invalid form body: {e}") })),
                )
                    .into_response()
            })?;
            Ok(JsonOrForm(value))
        } else {
            parse_query(&query)
        }
    }
}

fn parse_query<T: DeserializeOwned>(query: &str) -> Result<JsonOrForm<T>, Response> {
    serde_qs::Config::new(5, false)
        .deserialize_str(query)
        .map(JsonOrForm)
        .map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "message": format!("Invalid request: {e}") })),
            )
                .into_response()
        })
}
