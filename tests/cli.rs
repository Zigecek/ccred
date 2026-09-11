//! End-to-end tests against the built binary.
//!
//! These drive a throwaway home directory, so they exercise the real argument
//! parsing, the real file layout and the real exit codes.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use tempfile::TempDir;

/// Distinctive strings so a leak test can prove exactly what did not appear.
const TOKEN_A: &str = "sk-ant-oat01-SENTINELACCESSAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const REFRESH_A: &str = "sk-ant-ort01-SENTINELREFRESHAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const TOKEN_B: &str = "sk-ant-oat01-SENTINELACCESSBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const REFRESH_B: &str = "sk-ant-ort01-SENTINELREFRESHBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

/// Far in the future, so tests never depend on the clock.
const FAR_FUTURE: i64 = 4_102_444_800_000;

struct Sandbox {
    home: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        let sb = Sandbox { home };
        sb.login_a();
        sb
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    /// Both files get 0600, because that is what a real install has: Claude
    /// Code writes its credential file that way, measured on a live machine.
    /// A fixture that writes 0644 is not a sandbox of anything.
    #[cfg(unix)]
    fn make_private(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    fn make_private(_path: &Path) {}

    fn write_login(&self, access: &str, refresh: &str, expiry: i64, email: &str, uuid: &str) {
        std::fs::write(
            self.path().join(".claude").join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}",
                   "expiresAt":{FAR_FUTURE},"refreshTokenExpiresAt":{expiry},
                   "scopes":["user:inference"],"subscriptionType":"max",
                   "rateLimitTier":"tier"}},"organizationUuid":"org"}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            self.path().join(".claude.json"),
            format!(
                r#"{{"numStartups":42,
                   "projects":{{"/some/code":{{"hasTrustDialogAccepted":true}}}},
                   "mcpServers":{{"srv":{{"command":"x"}}}},
                   "oauthAccount":{{"accountUuid":"{uuid}","emailAddress":"{email}",
                   "organizationName":"Org","profileFetchedAt":1788000000000}}}}"#
            ),
        )
        .unwrap();
        Self::make_private(&self.path().join(".claude").join(".credentials.json"));
        Self::make_private(&self.path().join(".claude.json"));
    }

    fn login_a(&self) {
        self.write_login(
            TOKEN_A,
            REFRESH_A,
            FAR_FUTURE,
            "alice@example.com",
            "uuid-a",
        );
    }

    fn login_b(&self) {
        self.write_login(
            TOKEN_B,
            REFRESH_B,
            FAR_FUTURE + 86_400_000, // a LONGER window, like the real near-miss
            "bob@example.com",
            "uuid-b",
        );
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        Command::cargo_bin("ccred")
            .unwrap()
            .args(args)
            .env("HOME", self.path())
            .env("USERPROFILE", self.path())
            .env_remove("CCRED_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            // Pin the presentation: assertions below are about wording, and
            // must not depend on whether the machine running the suite has a
            // UTF-8 terminal or a colour-capable one.
            .env("NO_COLOR", "1")
            .env("CCRED_UNICODE", "0")
            // A save that creates a second profile registers the refresh
            // schedule. That writes to the real platform scheduler, which a
            // test must never do on the machine running it.
            .env("CCRED_NO_AUTO_SCHEDULE", "1")
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args);
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn config_json(&self) -> serde_json::Value {
        let raw = std::fs::read(self.path().join(".claude.json")).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }
}

#[test]
fn save_list_switch_round_trip() {
    let sb = Sandbox::new();

    let (out, _, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 0, "save failed: {out}");
    assert!(out.contains("alice@example.com"), "{out}");

    sb.login_b();
    let (out, _, _) = sb.run(&["save", "personal"]);
    assert!(out.contains("bob@example.com"), "{out}");

    let (out, _, _) = sb.run(&["list"]);
    assert!(out.contains("work"), "{out}");
    assert!(out.contains("personal"), "{out}");

    let (out, _, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("personal -> work"), "{out}");

    let (out, _, _) = sb.run(&["current"]);
    assert!(out.contains("alice@example.com"), "{out}");
}

/// The regression test for a real near-miss: after logging in as a second
/// account the pointer still named the first one, and a scheduled sync was
/// minutes from storing the wrong credentials.
#[test]
fn a_pointer_that_disagrees_with_the_live_account_is_reported() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();

    let (out, _, _) = sb.run(&["current"]);
    assert!(
        out.contains("the active profile is 'work'"),
        "no mismatch warning in: {out}"
    );
    assert!(out.contains("bob@example.com"), "{out}");

    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(code, 7, "doctor should fail loudly: {out}");
    assert!(
        out.contains("the active profile is not the account that is logged in"),
        "{out}"
    );
}

#[test]
fn storing_a_different_account_into_an_existing_profile_is_refused() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let before = std::fs::read(sb.path().join(".ccred/profiles/work/.credentials.json")).unwrap();

    sb.login_b();
    let (_, err, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 7, "expected an unsafe-write exit: {err}");
    assert!(err.contains("belongs to"), "{err}");

    let after = std::fs::read(sb.path().join(".ccred/profiles/work/.credentials.json")).unwrap();
    assert_eq!(before, after, "the profile must not have been touched");
}

/// The highest-value security test: no command, on any path, may print a token.
#[test]
fn no_command_ever_prints_a_token() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let invocations: &[&[&str]] = &[
        &["current"],
        &["current", "--json"],
        &["list"],
        &["list", "--json"],
        &["doctor"],
        &["doctor", "--json"],
        &["save", "work"], // fails: account mismatch
        &["switch", "work"],
        &["switch", "nope"],      // fails: not found
        &["switch", "../../etc"], // fails: invalid name
        &["rm", "personal"],
        &["stray-argument"], // fails: unknown command
    ];

    for args in invocations {
        let out = sb.cmd(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for secret in [TOKEN_A, REFRESH_A, TOKEN_B, REFRESH_B] {
            assert!(
                !text.contains(secret),
                "`ccred {}` leaked a token:\n{text}",
                args.join(" ")
            );
        }
        assert!(
            !text.contains("sk-ant-"),
            "`ccred {}` printed something token-shaped:\n{text}",
            args.join(" ")
        );
    }
}

#[test]
fn a_bare_profile_name_suggests_switch_instead_of_guessing() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let (_, err, code) = sb.run(&["work"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("ccred switch work"), "{err}");
}

#[test]
fn switching_preserves_unrelated_config_state() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);
    sb.run(&["switch", "work"]);

    let cfg = sb.config_json();
    assert_eq!(cfg["numStartups"], 42, "unrelated key was lost");
    assert_eq!(cfg["mcpServers"]["srv"]["command"], "x");
    assert_eq!(
        cfg["projects"]["/some/code"]["hasTrustDialogAccepted"],
        true
    );
    assert_eq!(cfg["oauthAccount"]["emailAddress"], "alice@example.com");
    // A field outside our projection must survive the round trip.
    assert_eq!(cfg["oauthAccount"]["profileFetchedAt"], 1788000000000_i64);
}

#[test]
fn guards_report_distinct_exit_codes() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    // Removing the active profile is refused.
    let (_, err, code) = sb.run(&["rm", "work"]);
    assert_eq!(code, 7, "{err}");

    // An unknown profile is a lookup failure, not an unsafe write.
    let (_, _, code) = sb.run(&["switch", "nope"]);
    assert_eq!(code, 3);

    // Path traversal is rejected by name validation.
    let (_, err, code) = sb.run(&["switch", "../../etc"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("invalid profile name"), "{err}");
}

#[test]
fn json_output_is_machine_readable() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let (out, _, _) = sb.run(&["list", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&out).expect("list --json must parse");
    assert_eq!(rows[0]["name"], "work");
    assert_eq!(rows[0]["active"], true);

    let (out, _, _) = sb.run(&["current", "--json"]);
    let cur: serde_json::Value = serde_json::from_str(&out).expect("current --json must parse");
    assert_eq!(cur["active_profile"], "work");
    assert_eq!(cur["logged_in"], true);
}

#[test]
fn an_interrupted_switch_is_healed_on_the_next_command() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    // Simulate a crash before the pointer was updated.
    let journal = sb.path().join(".ccred/state/switch.journal");
    std::fs::write(
        &journal,
        r#"{"from":"personal","to":"work","phase":"live_creds_written",
            "started_at_ms":1788000000000,"pid":999999}"#,
    )
    .unwrap();

    sb.run(&["current"]); // read-only: leaves it alone
    assert!(journal.exists(), "a read-only command must not heal");

    sb.run(&["switch", "personal"]);
    assert!(!journal.exists(), "a switch must clear the journal");
}

#[test]
fn refresh_leaves_healthy_profiles_alone_and_never_spawns() {
    // Every profile here has a long window, so nothing should be launched --
    // which also means this passes on a machine with no `claude` installed.
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let (out, err, code) = sb.run(&["refresh"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("work"), "{out}");
    assert!(
        out.contains("up to date") || out.contains("mirrored"),
        "nothing should have been refreshed:
{out}"
    );
}

#[test]
fn refresh_rate_limits_itself_so_over_firing_is_harmless() {
    // Schedulers double-fire: systemd catches up, launchd coalesces, a Windows
    // task can have both a boot trigger and a schedule. The command has to be
    // safe to call too often.
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let (_, _, code) = sb.run(&["refresh"]);
    assert_eq!(code, 0);

    let (out, _, code) = sb.run(&["refresh", "--if-older-than", "24"]);
    assert_eq!(code, 0, "an over-fire must not be an error");
    assert!(out.contains("nothing to do"), "{out}");
}

#[test]
fn refresh_output_never_prints_a_token() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    for args in [
        vec!["refresh"],
        vec!["refresh", "--json"],
        vec!["refresh", "--claude-path", "/definitely/not/here"],
    ] {
        let out = sb.cmd(&args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !text.contains("sk-ant-"),
            "`{args:?}` leaked:
{text}"
        );
    }
}

#[test]
fn schedule_dry_run_prints_the_artifact_and_touches_nothing() {
    // A tool that holds credentials must let you read exactly what it would
    // register with the operating system before it does it.
    let sb = Sandbox::new();
    let before: Vec<_> = walk(sb.path());

    let (out, err, code) = sb.run(&["schedule", "install", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("ccred"),
        "the command line should be visible:
{out}"
    );
    assert!(
        out.contains("---"),
        "a file header should be printed:
{out}"
    );

    let after: Vec<_> = walk(sb.path());
    assert_eq!(before, after, "--dry-run must not create anything");
}

#[test]
fn schedule_status_reports_a_state_rather_than_failing() {
    // Whether a schedule happens to be registered belongs to the machine
    // running the suite, not to the code under test -- `HOME` is sandboxed but
    // the platform scheduler is not. What has to hold either way is that
    // asking is never an error and always names a state.
    let sb = Sandbox::new();
    let (out, err, code) = sb.run(&["schedule", "status", "--json"]);
    assert_eq!(code, 0, "{err}");
    let state: serde_json::Value = serde_json::from_str(&out).expect(&out);
    assert!(
        matches!(
            state["state"].as_str(),
            Some("installed" | "not_installed" | "unsupported")
        ),
        "{out}"
    );
}

#[test]
fn schedule_output_never_prints_a_token() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    for args in [
        vec!["schedule", "status"],
        vec!["schedule", "status", "--json"],
        vec!["schedule", "install", "--dry-run"],
    ] {
        let out = sb.cmd(&args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !text.contains("sk-ant-"),
            "`{args:?}` leaked:
{text}"
        );
    }
}

/// Every path under a directory, sorted -- for proving nothing was written.
fn walk(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out.sort();
    out
}

/// Switching must not be able to destroy an account outright.
///
/// The setup is the shape of a real near-miss: the live login is an account
/// that belongs to no profile, because the pointer still names the profile
/// that was active before the login. `sync_outgoing` deliberately refuses to
/// mirror those credentials into the wrong profile -- and the switch then
/// overwrites the live file. Until this was fixed, that account's credentials
/// existed nowhere afterwards.
#[test]
fn switching_away_from_an_unsaved_account_keeps_a_copy_of_it() {
    const TOKEN_C: &str = "sk-ant-oat01-SENTINELACCESSCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    const REFRESH_C: &str = "sk-ant-ort01-SENTINELREFRESHCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice -> profile 'work'
    sb.login_b();
    sb.run(&["save", "personal"]); // bob -> profile 'personal', now active

    // A third account logs in. It matches no profile, and the pointer still
    // says 'personal'.
    sb.write_login(
        TOKEN_C,
        REFRESH_C,
        FAR_FUTURE,
        "carol@example.com",
        "uuid-c",
    );

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");

    let orphaned: Vec<_> =
        std::fs::read_dir(sb.path().join(".ccred").join("backups").join(".orphaned"))
            .expect("no .orphaned backup directory was created")
            .flatten()
            .map(|e| e.path())
            .collect();
    assert_eq!(orphaned.len(), 1, "expected exactly one copy: {orphaned:?}");

    let kept = std::fs::read_to_string(&orphaned[0]).unwrap();
    assert!(
        kept.contains(REFRESH_C),
        "the copy must hold the account that was about to be overwritten"
    );

    // And the user has to be told where it went, or the copy is useless.
    assert!(out.contains(".orphaned"), "{out}");
}

/// The counter-case: a logged-out live store has nothing worth keeping, and a
/// copy of it would push a real backup out of the rotation.
#[test]
fn switching_away_from_a_logged_out_store_keeps_nothing() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    std::fs::write(
        sb.path().join(".claude").join(".credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,
           "refreshTokenExpiresAt":0,"scopes":["user:inference"]}}"#,
    )
    .unwrap();

    let (_, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !sb.path()
            .join(".ccred")
            .join("backups")
            .join(".orphaned")
            .exists(),
        "an unusable credential blob must not be backed up"
    );
}

/// The recovery path the incident needed and did not have.
///
/// A spawned Claude Code signed itself out and wrote an empty credential blob
/// over a profile. Nothing read the last-known-good copy that sat beside it,
/// so the only way back was copying files by hand over SSH.
#[test]
fn a_damaged_profile_can_be_restored_from_its_last_known_good_copy() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let creds = sb.path().join(".ccred/profiles/work/.credentials.json");
    assert!(
        sb.path()
            .join(".ccred/profiles/work/.credentials.json.lkg")
            .exists(),
        "a save must leave a last-known-good copy"
    );

    // Exactly what the real failure wrote.
    std::fs::write(
        &creds,
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,
           "refreshTokenExpiresAt":0,"scopes":["user:inference"]}}"#,
    )
    .unwrap();

    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(code, 7, "a wiped profile must fail loudly: {out}");
    assert!(
        out.contains("ccred restore work"),
        "doctor must point at the way back: {out}"
    );

    let (out, err, code) = sb.run(&["restore", "work"]);
    assert_eq!(code, 0, "{err}{out}");

    let after = std::fs::read_to_string(&creds).unwrap();
    assert!(after.contains(REFRESH_A), "the good token must be back");
    assert!(
        sb.run(&["list"]).0.contains("ok"),
        "the profile must read as healthy again"
    );
}

/// Restoring is only offered when there is something worth restoring.
#[test]
fn restoring_a_profile_with_no_usable_copy_is_refused() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    std::fs::write(
        sb.path().join(".ccred/profiles/work/.credentials.json.lkg"),
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"scopes":[]}}"#,
    )
    .unwrap();

    let (_, err, code) = sb.run(&["restore", "work"]);
    assert_eq!(code, 7, "{err}");
    assert!(err.contains("no usable earlier copy"), "{err}");
}

/// The crash-safety rule, driven through the real recovery rather than the
/// pure function: a switch interrupted **before** the live store was touched
/// must roll the pointer back, not carry on forwards.
#[test]
fn a_switch_interrupted_before_the_live_write_rolls_back() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]); // active is now 'personal'

    let journal = sb.path().join(".ccred/state/switch.journal");
    for phase in ["started", "outgoing_synced"] {
        std::fs::write(
            &journal,
            format!(
                r#"{{"from":"personal","to":"work","phase":"{phase}",
                    "started_at_ms":1788000000000,"pid":999999}}"#
            ),
        )
        .unwrap();

        let (out, err, code) = sb.run(&["save", "personal"]);
        assert_eq!(code, 0, "{phase}: {err}{out}");
        assert!(!journal.exists(), "{phase}: the journal must be cleared");

        let active = std::fs::read_to_string(sb.path().join(".ccred/state/current")).unwrap();
        assert_eq!(
            active.trim(),
            "personal",
            "{phase}: the pointer must go back to where the switch started"
        );
    }
}

/// A journal left behind after the switch had finished is not a switch to
/// redo -- it is litter. Healing it must not move anything.
#[test]
fn a_finished_switch_journal_is_only_cleared() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let journal = sb.path().join(".ccred/state/switch.journal");
    std::fs::write(
        &journal,
        r#"{"from":"work","to":"personal","phase":"pointer_updated",
            "started_at_ms":1788000000000,"pid":999999}"#,
    )
    .unwrap();
    let before = std::fs::read_to_string(sb.path().join(".ccred/state/current")).unwrap();

    let (out, err, code) = sb.run(&["list"]);
    assert_eq!(code, 0, "{err}{out}");
    // `list` is read-only, so the journal survives it.
    assert!(journal.exists(), "a read-only command must not heal");

    let (_, err, code) = sb.run(&["save", "personal"]);
    assert_eq!(code, 0, "{err}");
    assert!(!journal.exists(), "a mutating command must clear it");
    assert_eq!(
        std::fs::read_to_string(sb.path().join(".ccred/state/current")).unwrap(),
        before,
        "a finished switch must not be replayed"
    );
}

/// `restore_identity` degrading rather than failing.
///
/// A profile saved before account details were captured has no
/// `oauth-account.json`. Switching to it must still work -- Claude Code
/// refetches the account on next start -- and must say so rather than
/// leaving the user to notice.
#[test]
fn switching_to_a_profile_with_no_stored_account_details_still_works() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    std::fs::remove_file(sb.path().join(".ccred/profiles/work/oauth-account.json")).unwrap();

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        out.contains("account details were not restored"),
        "the degradation must be stated: {out}"
    );

    // The credentials are what matter, and they must have moved.
    let live = std::fs::read_to_string(sb.path().join(".claude/.credentials.json")).unwrap();
    assert!(
        live.contains(REFRESH_A),
        "the target's credentials must be live"
    );
}

/// The outgoing mirror is skipped when the profile it names has been deleted
/// from under the pointer. That must not stop the switch, and must not be
/// silent either.
#[test]
fn switching_away_from_a_deleted_profile_is_reported_not_fatal() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]); // pointer names 'personal'

    std::fs::remove_dir_all(sb.path().join(".ccred/profiles/personal")).unwrap();

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        out.contains("no longer exists"),
        "the skipped mirror must be explained: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(sb.path().join(".ccred/state/current"))
            .unwrap()
            .trim(),
        "work"
    );
}

/// Everything this tool writes must land inside the home it was given.
///
/// `log_dir` used to be derived from `LOCALAPPDATA` / `XDG_STATE_HOME`, read
/// from the process environment rather than from the sandbox -- so this
/// suite was quietly appending its runs to the real user's log file, and
/// `CCRED_HOME` did not relocate the log either.
#[test]
fn nothing_is_written_outside_the_home_it_was_given() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);
    sb.run(&["refresh"]);

    let log = sb.path().join(".ccred/logs/ccred.jsonl");
    assert!(log.exists(), "the run must be recorded inside the sandbox");

    let (out, err, code) = sb.run(&["log"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        out.contains("mirror active") || out.contains("skip"),
        "{out}"
    );
}

/// Decisions are written in the serde spelling. `{:?}` squashes
/// `MirrorActive` into `mirroractive`, which is neither the enum name nor a
/// readable phrase.
#[test]
fn the_log_records_decisions_in_a_readable_spelling() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.run(&["refresh"]);

    let raw = std::fs::read_to_string(sb.path().join(".ccred/logs/ccred.jsonl")).unwrap();
    assert!(raw.contains("mirror_active"), "{raw}");
    assert!(!raw.contains("mirroractive"), "{raw}");
    // And the rule the module exists under: no rendered error text.
    assert!(!raw.contains("detail"), "{raw}");
    assert!(!raw.contains("sk-ant-"), "{raw}");
}

/// A mode is only what was asked for. A credential file restored from a
/// backup, copied with `cp -p`, or synced from another machine can arrive
/// readable by everyone, and nothing would have said so.
#[cfg(unix)]
#[test]
fn doctor_reports_credentials_other_users_can_read() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let (out, _, code) = sb.run(&["doctor"]);
    assert_ne!(code, 7, "a freshly saved profile must be private: {out}");
    assert!(out.contains("none readable by anyone else"), "{out}");

    let creds = sb.path().join(".ccred/profiles/work/.credentials.json");
    std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o644)).unwrap();

    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(
        code, 7,
        "a world-readable credential file must fail loudly: {out}"
    );
    assert!(out.contains("readable by other users"), "{out}");
    assert!(out.contains("644"), "the mode found must be named: {out}");
}

/// One broken profile must not end the run for the rest.
///
/// An unattended job that gives up on every account because one is unreadable
/// is worse than one that reports the unreadable account and carries on.
#[test]
fn a_profile_that_cannot_be_read_does_not_end_the_run() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    // Valid JSON, unusable credentials: it parses, so it reaches the refresh
    // logic rather than being skipped at load.
    std::fs::write(
        sb.path().join(".ccred/profiles/work/.credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,
           "refreshTokenExpiresAt":0,"scopes":["user:inference"]}}"#,
    )
    .unwrap();

    let (out, err, code) = sb.run(&["refresh"]);
    assert_eq!(code, 4, "a broken profile needs attention: {err}{out}");
    assert!(out.contains("work"), "the broken one must be named: {out}");
    assert!(
        out.contains("personal"),
        "the healthy one must still be reported: {out}"
    );
}

/// `doctor` must produce a report even when the thing it reports on is
/// broken. Refusing to say anything is the one response that cannot help
/// somebody who has just run it because something is wrong.
#[cfg(unix)]
#[test]
fn doctor_still_reports_when_the_profiles_directory_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let dir = sb.path().join(".ccred/profiles");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (out, err, code) = sb.run(&["doctor"]);
    // Restore before asserting, or a failure leaves the tempdir undeletable.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(code, 7, "it must fail loudly: {err}{out}");
    assert!(
        out.contains("config directory"),
        "the rest of the report must still be there: {out}"
    );
    assert!(
        out.contains("cannot be read") || out.contains("unreadable"),
        "and it must say what it could not read: {out}"
    );
}

/// `list` is how someone finds out which profile is the broken one, so it has
/// to survive meeting it.
#[test]
fn list_shows_a_profile_whose_metadata_will_not_parse() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    std::fs::write(
        sb.path().join(".ccred/profiles/work/ccred.json"),
        "{ not json",
    )
    .unwrap();

    let (out, err, code) = sb.run(&["list"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("work"), "the broken one must appear: {out}");
    assert!(out.contains("personal"), "and so must the rest: {out}");
    assert!(out.contains("unreadable"), "{out}");
}
