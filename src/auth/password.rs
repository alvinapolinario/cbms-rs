/// Verifies a plaintext password against a bcrypt hash produced by PHP's `Hash::make()`.
///
/// PHP's bcrypt hashes use the `$2y$` variant marker; the `bcrypt` crate's own hasher emits
/// `$2b$`. The two are algorithmically identical (the `2a`/`2b`/`2y` distinction is about
/// historical implementation bugs, not the algorithm itself) but some bcrypt parsers are picky
/// about the exact prefix, so it's normalized to `$2b$` before verifying as a cheap precaution.
pub fn verify_password(plain: &str, hash: &str) -> bool {
    let normalized = if let Some(rest) = hash.strip_prefix("$2y$") {
        format!("$2b${rest}")
    } else {
        hash.to_string()
    };
    bcrypt::verify(plain, &normalized).unwrap_or(false)
}
