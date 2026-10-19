//! Small shared wrapper types for DB columns whose JSON shape wouldn't match Laravel's if we
//! just used the obvious Rust type directly.

use chrono::NaiveDateTime;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A MySQL `datetime` column with **no** Eloquent cast applied (the normal case here -- none of
/// Fight/Bet/Event/EventTeller cast their datetime columns; only `created_at`/`updated_at` get
/// automatic Carbon treatment, and these tables don't even have timestamps). Laravel/PDO returns
/// these as plain `"YYYY-MM-DD HH:MM:SS"` strings, not Carbon's ISO-8601 `toJson()` format -- so
/// this wraps `NaiveDateTime` with that exact format instead of chrono's default RFC3339 serde
/// impl.
#[derive(Clone, Copy, Debug, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct DbDateTime(pub NaiveDateTime);

const DB_DATETIME_FMT: &str = "%Y-%m-%d %H:%M:%S";

impl DbDateTime {
    pub fn now() -> Self {
        DbDateTime(chrono::Utc::now().naive_utc())
    }
}

impl Serialize for DbDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.format(DB_DATETIME_FMT).to_string())
    }
}

impl<'de> Deserialize<'de> for DbDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        NaiveDateTime::parse_from_str(&s, DB_DATETIME_FMT)
            .map(DbDateTime)
            .map_err(serde::de::Error::custom)
    }
}

/// `events.event_percentage` is `DECIMAL(5,3)` (e.g. `0.070` = 7%) -- same string-not-number
/// JSON quirk as `Money`'s `DECIMAL(16,2)` columns, just three decimal places instead of two.
#[derive(Clone, Copy, Debug, Default, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct Rate(pub rust_decimal::Decimal);

impl Rate {
    pub fn value(self) -> f64 {
        self.0.to_string().parse().unwrap_or(0.0)
    }

    pub fn to_db_string(self) -> String {
        format!("{:.3}", self.value())
    }
}

impl Serialize for Rate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_db_string())
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum NumOrStr {
            Num(f64),
            Str(String),
        }
        let v = match NumOrStr::deserialize(deserializer)? {
            NumOrStr::Num(n) => n,
            NumOrStr::Str(s) => s.trim().parse::<f64>().map_err(serde::de::Error::custom)?,
        };
        let decimal: rust_decimal::Decimal = format!("{:.3}", v)
            .parse()
            .map_err(serde::de::Error::custom)?;
        Ok(Rate(decimal))
    }
}
