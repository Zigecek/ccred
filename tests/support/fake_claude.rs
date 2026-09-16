//! A stand-in for `claude`, compiled by `tests/cli.rs` with plain `rustc`.
//!
//! It does what the real binary was measured to do against a profile whose
//! access token has expired: `auth status --json` reports without exchanging
//! anything, and `mcp list` exchanges the tokens -- rotating both, moving
//! `expiresAt` eight hours ahead and leaving `refreshTokenExpiresAt` alone.
//!
//! `FAKE_CLAUDE_MODE` selects a misbehaviour instead:
//!
//! - `clear`: `mcp list` wipes the tokens, as a real run once did.
//! - `signed_out`: `auth status` reports `loggedIn: false`.
//! - `inert`: nothing is ever written.
//!
//! Every invocation appends one line to `FAKE_CLAUDE_LOG`: the arguments,
//! plus the two environment facts the caller must get right.
//!
//! Standard library only, so it builds without Cargo.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const RENEWED_ACCESS: &str = "sk-ant-oat01-SENTINELRENEWEDACCESSRRRRRRRRRRRRRRRRRRRRRRRRRRR";
const RENEWED_REFRESH: &str = "sk-ant-ort01-SENTINELRENEWEDREFRESHRRRRRRRRRRRRRRRRRRRRRRRRRR";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Stand in for an open Claude Code session: stay alive until killed.
    if args.first().map(String::as_str) == Some("--sleep") {
        std::thread::sleep(std::time::Duration::from_secs(120));
        return;
    }
    let mode = std::env::var("FAKE_CLAUDE_MODE").unwrap_or_default();

    let line = format!(
        "{}|config_dir={}|oauth_token_set={}\n",
        args.join(" "),
        std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| "-".into()),
        std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some(),
    );
    let log = std::env::var("FAKE_CLAUDE_LOG").expect("FAKE_CLAUDE_LOG");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .expect("log");

    let store = std::path::Path::new(
        &std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR")
            .expect("a probe must be pointed at a profile's store"),
    )
    .join(".credentials.json");

    match args.first().map(String::as_str) {
        Some("auth") => {
            let logged_in = mode != "signed_out";
            println!("{{\"loggedIn\":{logged_in},\"authMethod\":\"claude.ai\"}}");
        }
        Some("mcp") => {
            match mode.as_str() {
                "inert" | "signed_out" => {}
                "clear" => rewrite(&store, "", "", 0),
                _ => rewrite(&store, RENEWED_ACCESS, RENEWED_REFRESH, now_ms() + 8 * 3_600_000),
            }
            println!("No MCP servers configured.");
        }
        _ => println!("{{\"type\":\"result\",\"result\":\"hi\"}}"),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn rewrite(path: &std::path::Path, access: &str, refresh: &str, expires_at: i64) {
    let text = std::fs::read_to_string(path).expect("read store");
    let text = set_string(&text, "accessToken", access);
    let text = set_string(&text, "refreshToken", refresh);
    let text = set_number(&text, "expiresAt", expires_at);
    std::fs::write(path, text).expect("write store");
}

/// The value after `"key":`. The leading quote keeps `"expiresAt"` from
/// matching inside `"refreshTokenExpiresAt"`.
fn value_start(text: &str, key: &str) -> usize {
    let pattern = format!("\"{key}\"");
    let at = text.find(&pattern).expect("key") + pattern.len();
    let colon = at + text[at..].find(':').expect("colon");
    colon + 1 + (text[colon + 1..].len() - text[colon + 1..].trim_start().len())
}

fn set_string(text: &str, key: &str, value: &str) -> String {
    let open = value_start(text, key);
    let close = open + 1 + text[open + 1..].find('"').expect("closing quote");
    format!("{}\"{}{}", &text[..open], value, &text[close..])
}

fn set_number(text: &str, key: &str, value: i64) -> String {
    let start = value_start(text, key);
    let len = text[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .expect("end of number");
    format!("{}{}{}", &text[..start], value, &text[start + len..])
}
