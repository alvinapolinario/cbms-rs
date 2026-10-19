use crate::money::Money;
use crate::types::DbDateTime;
use serde::Serialize;

/// `matches` table / Eloquent model `Fight`. One round of cockfighting. See
/// docs/WORKFLOW.md section 3.1/4/7.2 for the full state-machine explanation.
///
/// `meron_odds`/`wala_odds` are a MySQL `float` column -- genuinely 4-byte IEEE754 single
/// precision, not `double`/`decimal` -- and carry **no** Eloquent cast, so -- unlike every
/// `Money` field here -- they serialize as plain JSON *numbers*, not strings (PDO/mysqlnd
/// returns native floats for `FLOAT` columns; only `DECIMAL`/`NUMERIC` columns come back as
/// strings). Declared `f32` here to match the column's actual wire width (sqlx's MySQL decoder
/// does not widen a 4-byte `FLOAT` into an 8-byte `f64` -- doing so panics on the raw byte
/// buffer rather than erroring cleanly, confirmed the hard way); computation (`services::odds`,
/// `php_round`) still happens in `f64` to match PHP's semantics (PHP has no native 32-bit float
/// type), narrowing to `f32` only at the DB boundary -- the real app loses the same precision at
/// that same boundary, since Laravel stores into this same 4-byte column too.
///
/// `meron_bet_status`/`wala_bet_status` are `tinyint(1)` (1 byte); the Eloquent model casts them
/// to `integer` specifically so a `!` (PHP bool) toggle still serializes as `0`/`1` rather than
/// `true`/`false` -- `i8` here already serializes as a plain number either way, and matches the
/// column's actual width (the same panic-not-error width trap as the odds columns).
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Fight {
    pub match_id: i32,
    pub event_id: i32,
    pub match_number: i32,
    pub match_winner: String,
    pub meron_total_bet: Money,
    pub wala_total_bet: Money,
    #[serde(serialize_with = "crate::services::odds::serialize_odds")]
    pub meron_odds: f32,
    #[serde(serialize_with = "crate::services::odds::serialize_odds")]
    pub wala_odds: f32,
    pub match_status: String,
    pub match_bet_status: String,
    pub meron_bet_status: i8,
    pub wala_bet_status: i8,
    pub match_created_datetime: DbDateTime,
    pub is_display: i8,
    pub meron_entry: Option<String>,
    pub wala_entry: Option<String>,
    pub source_fight_uid: Option<i32>,
    pub source_call_version: i32,
    pub meron_details: Option<String>,
    pub wala_details: Option<String>,
    pub called_at: Option<DbDateTime>,
    pub bet_opened_at: Option<DbDateTime>,
    pub hold_reason: Option<String>,
}

impl Fight {
    pub fn is_bridge_linked(&self) -> bool {
        self.source_fight_uid.is_some()
    }
}
