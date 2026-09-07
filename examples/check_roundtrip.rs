//! Verify that the model reads and writes a real `.credentials.json` without
//! losing anything.
//!
//! Never prints values -- only key names, lengths and fingerprints.
//!
//!     cargo run --example check_roundtrip -- ~/.claude/.credentials.json

use ccred::model::credentials::{CredentialsFile, assert_lossless};
use ccred::validate::validate_credentials;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: check_roundtrip <path>")?;
    let bytes = std::fs::read(&path)?;
    println!("file            : {path} ({} bytes)", bytes.len());

    let parsed: CredentialsFile = serde_json::from_slice(&bytes)?;

    let mut top: Vec<&str> = parsed.extra.keys().map(String::as_str).collect();
    top.sort_unstable();
    println!("siblings of claudeAiOauth : {top:?}");

    let mut inner: Vec<&str> = parsed.oauth.extra.keys().map(String::as_str).collect();
    inner.sort_unstable();
    println!("unmodelled inner keys     : {inner:?}");

    println!(
        "accessToken     : {} bytes, {}",
        parsed.oauth.access_token.len(),
        parsed.oauth.access_token.fingerprint()
    );
    println!(
        "refreshToken    : {} bytes, {}",
        parsed.oauth.refresh_token.len(),
        parsed.oauth.refresh_token.fingerprint()
    );

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    match validate_credentials(&parsed.oauth, now_ms) {
        Ok(h) => println!(
            "validation      : OK (access_expired={}, refresh_expired={}, {} days left)",
            h.access_expired,
            h.refresh_expired,
            h.refresh_window_left_ms.unwrap_or(0) / 86_400_000
        ),
        Err(e) => println!("validation      : REFUSED -- {e}"),
    }

    let back = serde_json::to_value(&parsed)?;
    assert_lossless(&bytes, &back)?;
    println!("round-trip      : LOSSLESS");

    Ok(())
}
