use std::env;

/// All configuration loaded from the environment once at startup. Mirrors the Laravel
/// `.env` variable names where one exists (see docs/WORKFLOW.md section 2 in the sibling
/// `sabonglara` repo), so the two apps can share a single `.env` if desired.
#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub host: String,
    pub port: u16,

    pub bridge_enabled: bool,
    pub bridge_matching_url: String,
    /// Verifies incoming matching -> betting requests; matching signs with this same value.
    pub bridge_key_from_matching: String,
    /// Signs outgoing betting -> matching requests; matching verifies with this same value.
    pub bridge_key_to_matching: String,
    /// Hardcoded 300 on the Laravel side (config/bridge.php) -- not actually env-driven there,
    /// kept as a named constant here for the same reason.
    pub bridge_max_clock_skew_secs: i64,

    pub bet_min: f64,
    pub bet_max: f64,
    pub cash_max: f64,
    /// Loaded for compatibility with older env files. Sabong2026 does not set
    /// `expires_at` on API tokens, so this value is not applied.
    #[allow(dead_code)]
    pub api_token_hours: i64,
    pub require_closed_before_result: bool,
    /// Sabong2026 hardcodes 7 digits in `BetController@store` (`mt_rand` loop).
    pub receipt_random_digits: u32,
    /// Shared secret for `GET /api/print-service/*`. Empty means those routes answer 503,
    /// the same as Laravel's `PrintServiceKey` when `PRINT_SERVICE_KEY` is unset.
    pub print_service_key: String,

    pub pusher_host: String,
    pub pusher_port: u16,
    pub pusher_scheme: String,
    pub pusher_app_id: String,
    pub pusher_app_key: String,
    pub pusher_app_secret: String,
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => default,
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            host: env_or("HOST", "0.0.0.0"),
            port: env_num("PORT", 8080),

            bridge_enabled: env_bool("MATCHING_BRIDGE_ENABLED", false),
            bridge_matching_url: env_or("MATCHING_BRIDGE_URL", "").trim_end_matches('/').to_string(),
            bridge_key_from_matching: env_or("MATCHING_TO_BETTING_KEY", ""),
            bridge_key_to_matching: env_or("BETTING_TO_MATCHING_KEY", ""),
            bridge_max_clock_skew_secs: 300,

            bet_min: env_num("BET_MIN", 1.0),
            bet_max: env_num("BET_MAX", 1_000_000.0),
            cash_max: env_num("CASH_MAX", 1_000_000.0),
            api_token_hours: env_num("API_TOKEN_HOURS", 18),
            require_closed_before_result: env_bool("REQUIRE_CLOSED_BEFORE_RESULT", true),
            receipt_random_digits: env_num("RECEIPT_RANDOM_DIGITS", 7),
            print_service_key: env_or("PRINT_SERVICE_KEY", ""),

            pusher_host: env_or("PUSHER_HOST", "127.0.0.1"),
            pusher_port: env_num("PUSHER_PORT", 6001),
            pusher_scheme: env_or("PUSHER_SCHEME", "http"),
            pusher_app_id: env_or("PUSHER_APP_ID", "local"),
            pusher_app_key: env_or("PUSHER_APP_KEY", "local"),
            pusher_app_secret: env_or("PUSHER_APP_SECRET", "local"),
        }
    }

    /// Mirrors `MatchingBridge::enabled()` / Node's `bridge.enabled()`: all four pieces must be
    /// present, not just the boolean flag.
    pub fn bridge_is_enabled(&self) -> bool {
        self.bridge_enabled
            && !self.bridge_matching_url.is_empty()
            && !self.bridge_key_from_matching.is_empty()
            && !self.bridge_key_to_matching.is_empty()
    }
}
