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
        self.cmd_env(args, &[])
    }

    fn cmd_env(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        Command::cargo_bin("ccred")
            .unwrap()
            .args(args)
            .envs(env.iter().copied())
            .env("HOME", self.path())
            .env("USERPROFILE", self.path())
            // Where the release installer keeps its receipt, and systemd its
            // user units. Left alone, `uninstall --dry-run` read the real
            // machine's receipt.
            .env("LOCALAPPDATA", self.path().join("AppData").join("Local"))
            .env("XDG_CONFIG_HOME", self.path().join(".config"))
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
        &["restore", "work"],
        &["restore", "nope"], // fails: not found
        &["refresh"],         // leaves a run log and a last-run record
        &["refresh", "--dry-run"],
        &["log"],
        &["log", "--json"],
        &["uninstall", "--dry-run"],
        &["uninstall", "--purge", "--dry-run", "--json"],
        // Never a real `uninstall` here: the platform scheduler is not
        // sandboxed, so a regression in the consent check would remove the
        // job of whoever runs the suite. The refusal is unit-tested instead.
        &["rename", "personal", "personal2"],
        &["rm", "personal2"],
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

    // And nothing written beside the credentials holds one: metadata, the
    // account blob, the pointer, the journal, the run log. Only files that
    // are credentials by design -- the store, its last-known-good copy and
    // the backups of both -- may.
    let mut checked = 0;
    for path in walk(&sb.path().join(".ccred")) {
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.starts_with(".credentials.json") || name.starts_with("credentials.") {
            continue;
        }
        checked += 1;
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("sk-ant-"),
            "{} holds something token-shaped",
            path.display()
        );
    }
    // Profile metadata, the account blob, the pointer, the run log and the
    // last-run record, at least.
    assert!(checked >= 5, "only {checked} files were checked");
}

#[test]
fn a_bare_profile_name_suggests_switch_instead_of_guessing() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);

    let (_, err, code) = sb.run(&["work"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("ccred switch work"), "{err}");
}

/// Switching to the profile that is already active is a resync: the live
/// credentials go into it and come back out. Reported as `work -> work` it
/// reads like the command misfired.
#[test]
fn switching_to_the_active_profile_says_it_was_already_active() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let live = sb.path().join(".claude/.credentials.json");
    let read = |p: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    let before = read(&live);

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("already active"), "{out}");
    assert!(!out.contains("work -> work"), "{out}");
    // Compared as JSON, not as bytes: the fixture is written pretty and ccred
    // writes the one-line shape Claude Code uses, so the file is reformatted
    // on the way through. What must not change is what it says.
    assert_eq!(
        read(&live),
        before,
        "a resync must leave the live credentials as they were"
    );
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

    // One rotation per account, so another account's copies can never push
    // this one out.
    let orphaned: Vec<_> = std::fs::read_dir(
        sb.path()
            .join(".ccred")
            .join("backups")
            .join(".orphaned")
            .join("uuid-c"),
    )
    .expect("no backup directory was created for the orphaned account")
    .flatten()
    .map(|e| e.path())
    .collect();
    assert_eq!(orphaned.len(), 1, "expected exactly one copy: {orphaned:?}");

    let kept = std::fs::read_to_string(&orphaned[0]).unwrap();
    assert!(
        kept.contains(REFRESH_C),
        "the copy must hold the account that was about to be overwritten"
    );

    // And the user has to be told where it went, or the copy is useless --
    // spelled the way the platform spells a path, since the label is joined
    // on by hand and used to carry its own separator onto Windows.
    let want = format!(".orphaned{}uuid-c", std::path::MAIN_SEPARATOR);
    assert!(out.contains(&want), "expected {want} in: {out}");
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

/// A last-known-good copy and a backup hold the same tokens as the store, and
/// a backup is the file most likely to have been copied away and back.
#[cfg(unix)]
#[test]
fn doctor_checks_the_copies_of_credentials_too() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]); // active, so 'work' can go
    let (out, err, code) = sb.run(&["rm", "work"]); // leaves a backup
    assert_eq!(code, 0, "{out}{err}");
    let (out, _, code) = sb.run(&["doctor"]);
    assert!(out.contains("none readable by anyone else"), "{code} {out}");

    let backup = walk(&sb.path().join(".ccred/backups"))
        .into_iter()
        .find(|p| p.is_file())
        .expect("rm left no backup");
    std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(code, 7, "{out}");
    assert!(
        out.contains("backups"),
        "the exposed backup must be named: {out}"
    );
    std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600)).unwrap();

    let lkg = sb
        .path()
        .join(".ccred/profiles/personal/.credentials.json.lkg");
    assert!(lkg.is_file(), "a save leaves a last-known-good copy");
    std::fs::set_permissions(&lkg, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(code, 7, "{out}");
    assert!(out.contains(".lkg"), "{out}");
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
    // The count and the warning are two styled runs joined by hand, and the
    // join once put two spaces after a comma.
    assert!(
        out.contains("2 profiles, 1 needs attention"),
        "the summary spacing: {out}"
    );
}

/// Renaming was impossible: `rm` and `save` again only works for the account
/// that happens to be logged in, so a profile named in haste was named that
/// for good -- short of moving directories by hand.
#[test]
fn a_profile_can_be_given_another_name() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    sb.login_b();
    sb.run(&["save", "personal"]); // bob, active

    // An inactive one, whose account is not the live one: the case that
    // `rm` and `save` cannot do at all.
    let before = std::fs::read(sb.path().join(".ccred/profiles/work/.credentials.json")).unwrap();
    let (out, err, code) = sb.run(&["rename", "work", "job"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(!sb.path().join(".ccred/profiles/work").exists(), "{out}");
    assert_eq!(
        std::fs::read(sb.path().join(".ccred/profiles/job/.credentials.json")).unwrap(),
        before,
        "the credentials moved untouched"
    );

    // The active one: the pointer has to follow.
    let (out, err, code) = sb.run(&["rename", "personal", "main"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("still is"), "{out}");
    assert_eq!(
        std::fs::read_to_string(sb.path().join(".ccred/state/current"))
            .unwrap()
            .trim(),
        "main"
    );
    let (out, _, _) = sb.run(&["current"]);
    assert!(out.contains("main"), "{out}");

    // Onto a name already in use, and from a name that is not there.
    let (_, err, code) = sb.run(&["rename", "job", "main"]);
    assert_eq!(code, 7, "{err}");
    assert!(err.contains("already exists"), "{err}");
    let (_, err, code) = sb.run(&["rename", "ghost", "whatever"]);
    assert_eq!(code, 3, "{err}");

    // The copies a later `rm` would look for move with the profile.
    sb.run(&["rm", "job"]);
    assert!(
        sb.path().join(".ccred/backups/job").is_dir(),
        "the copy is under the new name"
    );

    // A change of case only: the one way to fix a spelling where the file
    // system ignores it. The pointer has to follow that too -- read after
    // the move, the old name resolved to the new one and the rename decided
    // the profile had not been active, leaving a pointer to nothing on any
    // file system that does tell them apart.
    let (out, err, code) = sb.run(&["rename", "main", "MAIN"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("still is"), "{out}");
    assert_eq!(
        std::fs::read_to_string(sb.path().join(".ccred/state/current"))
            .unwrap()
            .trim(),
        "MAIN"
    );
    let (out, _, _) = sb.run(&["list"]);
    assert!(out.contains("MAIN"), "{out}");
    assert!(!out.contains("does not exist"), "a dangling pointer: {out}");
}

/// `ccred rm --help` listed `<NAME>` with nothing beside it, and so did
/// `switch` and `restore`: every flag was documented and the arguments were
/// not. The long help is what someone reads when they are unsure, which is
/// the moment a blank line is least welcome.
#[test]
fn every_argument_in_the_help_is_described() {
    let sb = Sandbox::new();
    for command in ["save", "switch", "rm", "restore", "log", "schedule"] {
        let (out, err, code) = sb.run(&[command, "--help"]);
        assert_eq!(code, 0, "{command}: {err}");
        let lines: Vec<&str> = out.lines().collect();
        let Some(start) = lines.iter().position(|l| l.trim() == "Arguments:") else {
            continue; // no positional arguments at all
        };
        for (i, line) in lines.iter().enumerate().skip(start) {
            let is_argument = line.trim_start().starts_with('<') && line.trim().ends_with('>');
            if !is_argument {
                continue;
            }
            let described = lines.get(i + 1).is_some_and(|next| !next.trim().is_empty());
            assert!(described, "{command}: {} has no description", line.trim());
        }
    }
}

/// `rm` keeps a copy, which is right for a mistyped name and wrong for the
/// person whose point was to remove the account. `--purge` is for the second
/// one, and it has to mean it: the rotation under the profile's name and the
/// one keyed by its account both go.
#[test]
fn rm_purge_leaves_none_of_that_account_behind() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    sb.login_b();
    sb.run(&["save", "personal"]); // bob, active

    // An ordinary `rm` first, which leaves a copy of alice behind.
    sb.run(&["rm", "work"]);
    let copies = sb.path().join(".ccred").join("backups").join("work");
    assert!(copies.is_dir(), "the safety copy is the default");

    // Save alice again so there is a profile to purge, then leave it.
    sb.login_a();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["switch", "personal"]);

    let (out, err, code) = sb.run(&["rm", "work", "--purge"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("deleted the copies"), "{out}");
    assert!(!copies.exists(), "the copies outlived the purge: {out}");
    assert!(
        !sb.path().join(".ccred/profiles/work").exists(),
        "the profile is gone too"
    );

    // The account that was not asked about is untouched.
    assert!(
        sb.path().join(".ccred/profiles/personal").is_dir(),
        "another profile was caught in it"
    );

    // And with nothing to delete, it says so rather than implying otherwise.
    sb.login_a();
    sb.run(&["save", "spare"]);
    sb.login_b();
    sb.run(&["switch", "personal"]);
    let (out, _, code) = sb.run(&["rm", "spare", "--purge"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("no earlier copies"), "{out}");
}

/// The account blob is a cached copy of something Claude Code refetches. The
/// credentials are not. So a blob that cannot be read costs a warning, not
/// the switch -- which by then has already replaced the live credentials.
#[test]
fn account_details_that_cannot_be_read_do_not_stop_a_switch() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let blob = sb.path().join(".ccred/profiles/work/oauth-account.json");
    std::fs::remove_file(&blob).unwrap();
    std::fs::create_dir(&blob).unwrap();

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        out.contains("refetch"),
        "the warning explains itself: {out}"
    );

    // The part that cannot be refetched did move.
    let live = std::fs::read_to_string(sb.path().join(".claude/.credentials.json")).unwrap();
    assert!(live.contains(TOKEN_A), "the credentials are the target's");
}

/// A profile so damaged that its metadata cannot be read is no reason to
/// strand someone on it. The mirror is refused -- nothing else could be
/// safe -- and the credentials that were live are copied aside first.
#[test]
fn a_profile_that_cannot_be_written_is_not_a_prison() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    sb.login_b();
    sb.run(&["save", "personal"]); // bob, now active

    // Something a crash cannot produce, which is the point: whatever the
    // state of the outgoing profile, leaving it must still work.
    let meta = sb.path().join(".ccred/profiles/personal/ccred.json");
    std::fs::remove_file(&meta).unwrap();
    std::fs::create_dir(&meta).unwrap();

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("did not update 'personal'"), "{out}");
    assert!(
        out.contains("not in any profile"),
        "the copy is named: {out}"
    );

    // Bob's credentials, which nothing else holds now, are in the backups.
    let orphans = sb.path().join(".ccred").join("backups").join(".orphaned");
    let kept: Vec<_> = std::fs::read_dir(&orphans)
        .expect("no copy was kept")
        .flatten()
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");

    // And the switch itself happened.
    assert_eq!(
        std::fs::read_to_string(sb.path().join(".ccred/state/current"))
            .unwrap()
            .trim(),
        "work"
    );
}

/// A profile from an older ccred, written by hand with only the keys that
/// have always been required. Every field added since carries `#[serde(
/// default)]` and unknown ones are kept in `extra`, and this is what says so
/// -- checked against a real 0.2.24 binary once, and pinned here so no
/// network is needed to keep checking.
#[test]
fn a_profile_from_an_older_version_is_read_as_it_is() {
    let sb = Sandbox::new();
    sb.run(&["save", "current"]); // so there is something to switch back to

    let old = sb.path().join(".ccred").join("profiles").join("ancient");
    std::fs::create_dir_all(&old).unwrap();
    // No `account`, no `refresh`, no `last_synced_at_ms`: the shape before
    // any of them existed, plus a key from a version that is not this one.
    std::fs::write(
        old.join("ccred.json"),
        r#"{"schema":1,"name":"ancient","created_at_ms":1780000000000,
            "somethingFromTheFuture":{"keep":"me"}}"#,
    )
    .unwrap();
    std::fs::write(
        old.join(".credentials.json"),
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN_B}","refreshToken":"{REFRESH_B}",
               "expiresAt":{FAR_FUTURE},"refreshTokenExpiresAt":{FAR_FUTURE},
               "scopes":["user:inference"]}}}}"#
        ),
    )
    .unwrap();

    let (out, err, code) = sb.run(&["list"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("ancient"), "{out}");

    let (out, err, code) = sb.run(&["switch", "ancient"]);
    assert_eq!(code, 0, "{err}{out}");

    // And what this version did not understand is still there afterwards.
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(old.join("ccred.json")).unwrap()).unwrap();
    assert_eq!(
        meta["somethingFromTheFuture"]["keep"],
        serde_json::json!("me"),
        "an unknown key was dropped: {meta}"
    );
}

/// A scheduled run reads no shell profile, so a `claude` that reaches PATH
/// from one has to be named at install time. A path that is not there is
/// refused while a person is present to fix it, rather than twice a week into
/// a log nobody opens.
#[test]
fn a_scheduled_job_can_be_told_where_claude_is() {
    let sb = Sandbox::new();
    let claude = sb.path().join("elsewhere").join("claude");
    std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
    std::fs::write(
        &claude,
        b"#!/bin/sh
",
    )
    .unwrap();

    let (out, err, code) = sb.run(&[
        "schedule",
        "install",
        "--dry-run",
        "--claude-path",
        &claude.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("--claude-path"), "{out}");
    assert!(out.contains("claude"), "{out}");

    let (_, err, code) = sb.run(&[
        "schedule",
        "install",
        "--dry-run",
        "--claude-path",
        &sb.path().join("nowhere").join("claude").to_string_lossy(),
    ]);
    assert_eq!(code, 8, "a path that is not there is refused: {err}");
    assert!(err.contains("does not exist"), "{err}");
}

/// Six bytes decide which profile is live. When they turn to nonsense, the
/// commands someone runs to find out what is wrong must still answer, and the
/// one command that writes a new pointer must be willing to.
#[test]
fn a_pointer_that_cannot_be_read_is_reported_not_fatal() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let pointer = sb.path().join(".ccred/state/current");
    std::fs::write(&pointer, [0xff, 0xfe, 0x00, 0x01]).unwrap();

    for args in [&["list"][..], &["current"][..]] {
        let (out, err, code) = sb.run(args);
        assert_eq!(code, 0, "{args:?} must still answer: {err}{out}");
        assert!(
            out.contains("pointer cannot be read") || out.contains("pointer at"),
            "{args:?}: {out}"
        );
    }

    // And a switch repairs it, rather than refusing because of it.
    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert_eq!(
        std::fs::read_to_string(&pointer).unwrap().trim(),
        "work",
        "the switch must leave a pointer that reads"
    );
}

/// The live credentials belong to nobody: no profile is active and none holds
/// these tokens. The switch below overwrites them, so they are copied aside
/// first -- the same loss a wrong pointer is already guarded against.
#[test]
fn switching_with_no_active_profile_keeps_the_live_credentials() {
    const TOKEN_C: &str = "sk-ant-oat01-NOBODYSACCESSCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    const REFRESH_C: &str = "sk-ant-ort01-NOBODYSREFRESHCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    // Carol logs in and is saved nowhere, and the pointer is gone.
    sb.write_login(
        TOKEN_C,
        REFRESH_C,
        FAR_FUTURE,
        "carol@example.com",
        "uuid-c",
    );
    std::fs::remove_file(sb.path().join(".ccred/state/current")).unwrap();

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("not in any profile"), "{out}");

    let kept: Vec<_> = std::fs::read_dir(
        sb.path()
            .join(".ccred")
            .join("backups")
            .join(".orphaned")
            .join("uuid-c"),
    )
    .expect("carol's credentials were overwritten with no copy kept")
    .flatten()
    .map(|e| std::fs::read_to_string(e.path()).unwrap())
    .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(kept[0].contains(REFRESH_C), "the copy must hold carol");
}

/// A pointer left naming a profile that was removed behind ccred's back. The
/// table shows an absent marker, which reads as "nothing is active" rather
/// than "the one you were using is gone", so the summary says it outright.
#[test]
fn list_says_so_when_the_active_profile_is_not_there() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    std::fs::write(sb.path().join(".ccred/state/current"), "ghost").unwrap();

    let (out, err, code) = sb.run(&["list"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        out.contains("the active profile 'ghost' does not exist"),
        "{out}"
    );

    // The JSON shape is an array of profiles and stays one: a script reading
    // it predates the note, and `current --json` already reports the pointer.
    let (out, _, code) = sb.run(&["list", "--json"]);
    assert_eq!(code, 0, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert!(parsed.is_array(), "{parsed}");
}

/// `current` is the first thing anyone runs. A live credential file that will
/// not parse must produce a report saying so, not a bare error -- `doctor` is
/// where that fails loudly.
#[test]
fn current_reports_an_unreadable_live_store_instead_of_refusing() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    std::fs::write(
        sb.path().join(".claude/.credentials.json"),
        "{ this is not json",
    )
    .unwrap();

    let (out, err, code) = sb.run(&["current"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("cannot be read"), "{out}");

    // And doctor is where it is an error.
    let (dout, _, dcode) = sb.run(&["doctor"]);
    assert_eq!(dcode, 7, "{dout}");
}

/// `rm` is the one command here whose mistake cannot be taken back: a
/// mistyped name deletes the only stored copy of an account. Every other
/// write in this tool is recoverable, so this one should be too.
#[test]
fn removing_a_profile_leaves_a_copy_of_its_credentials() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]); // 'personal' is active, so 'work' can go

    let (out, err, code) = sb.run(&["rm", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        !sb.path().join(".ccred/profiles/work").exists(),
        "the profile must actually be gone"
    );

    let kept: Vec<_> = std::fs::read_dir(sb.path().join(".ccred/backups/work"))
        .expect("a copy must survive the deletion")
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        kept[0].contains(REFRESH_A),
        "the copy must hold the credentials"
    );

    // And the user has to be told where it went, or the copy is useless.
    assert!(out.contains("backups"), "{out}");
}

/// Corrupt metadata on the *active* profile used to end `doctor` and
/// `current` with a bare error. `list` was fixed for this; these two were
/// not, and they are the ones someone reaches for first.
#[test]
fn corrupt_metadata_on_the_active_profile_does_not_silence_any_command() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // 'work' becomes active
    std::fs::write(
        sb.path().join(".ccred/profiles/work/ccred.json"),
        "{ not json",
    )
    .unwrap();

    let (out, _, code) = sb.run(&["current"]);
    assert_eq!(code, 0, "current must still report: {out}");
    assert!(out.contains("work"), "{out}");

    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(code, 7, "doctor must fail loudly: {out}");
    assert!(
        out.contains("config directory"),
        "the rest of the report must survive: {out}"
    );

    let (out, _, code) = sb.run(&["list"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("work"), "{out}");
}

/// An explicit `null` in the credential file used to make it permanently
/// unreadable, because the lossless guard counted it as a dropped key.
#[test]
fn an_explicit_null_in_the_live_store_is_not_fatal() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.path().join(".claude/.credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
           "refreshToken":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
           "expiresAt":4102444800000,"refreshTokenExpiresAt":4102444800000,
           "scopes":["user:inference"],"subscriptionType":null}}"#,
    )
    .unwrap();

    let (out, err, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 0, "{err}{out}");
    let (out, _, _) = sb.run(&["list"]);
    assert!(out.contains("ok"), "{out}");
}

/// `uninstall --dry-run` is the way to read what an uninstall would destroy,
/// so it must name the profiles and remove nothing -- not the data, and not
/// the binary running the suite.
///
/// Only the dry run is exercised end to end. A real uninstall would remove
/// the platform scheduler's job, and the scheduler is not sandboxed: on a
/// developer's machine that is their real schedule. The decision to ask or
/// refuse is covered by `consent`'s unit tests instead.
#[test]
fn uninstall_dry_run_names_what_it_would_delete_and_removes_nothing() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let exe = assert_cmd::cargo::cargo_bin("ccred");
    let before = walk(sb.path());

    let (out, err, code) = sb.run(&["uninstall", "--purge", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("DELETED: work"), "{out}");
    assert!(out.contains("dry run"), "{out}");
    assert!(out.contains("not touched"), "{out}");

    let (out, err, code) = sb.run(&["uninstall", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{err}");
    let plan: serde_json::Value = serde_json::from_str(&out).expect(&out);
    assert_eq!(plan["purge"], false, "{out}");
    assert_eq!(plan["profiles"][0], "work", "{out}");
    assert!(
        plan["data_dir"]
            .as_str()
            .is_some_and(|d| d.ends_with(".ccred")),
        "{out}"
    );

    assert_eq!(
        before,
        walk(sb.path()),
        "a dry run must not change anything"
    );
    assert!(exe.exists(), "a dry run must not remove the binary");
}

/// A journal is also what a switch *in progress* looks like. Healing it from
/// another process rolled the pointer back underneath the running switch, so
/// recovery happens only under the lock that switch holds -- and a busy lock
/// means the journal is not ours to touch.
#[test]
fn a_switch_in_progress_is_not_healed_from_outside() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let journal = sb.path().join(".ccred/state/switch.journal");
    std::fs::write(
        &journal,
        r#"{"from":"personal","to":"work","phase":"outgoing_synced",
            "started_at_ms":1788000000000,"pid":999999}"#,
    )
    .unwrap();

    // Another writer holds the lock and keeps it alive, the way the running
    // switch would. Without the heartbeat a slow runner could see it go stale
    // mid-test and reclaim it.
    let lock = sb.path().join(".claude/.storage-write.lock");
    std::fs::create_dir(&lock).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let beat = {
        let stop = Arc::clone(&stop);
        let lock = lock.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let f = lock.join("beat");
                let _ = std::fs::write(&f, b"1");
                let _ = std::fs::remove_file(&f);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })
    };

    let (out, err, code) = sb.run(&["switch", "work"]);
    stop.store(true, Ordering::Relaxed);
    beat.join().unwrap();

    assert_eq!(code, 6, "a held lock is Busy: {out}{err}");
    assert!(
        journal.exists(),
        "a journal was healed while its switch still held the lock"
    );
}

// --- refresh against a stand-in `claude` -----------------------------------

const RENEWED_ACCESS: &str = "sk-ant-oat01-SENTINELRENEWEDACCESSRRRRRRRRRRRRRRRRRRRRRRRRRRR";
const DAY_MS: i64 = 86_400_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A stand-in `claude`, built once per test binary with plain `rustc`.
///
/// A compiled program rather than a script, because the same stand-in has to
/// run on all three platforms and a `.cmd` file cannot rewrite JSON sanely.
fn fake_claude() -> &'static Path {
    static EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let dir = Box::leak(Box::new(TempDir::new().unwrap())).path();
        let src = dir.join("fake_claude.rs");
        std::fs::write(&src, include_str!("support/fake_claude.rs")).unwrap();
        // Named `claude`, because running-session detection checks the name.
        let exe = dir.join(format!("claude{}", std::env::consts::EXE_SUFFIX));
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let out = Command::new(rustc)
            .args(["--edition", "2021", "-o"])
            .arg(&exe)
            .arg(&src)
            .output()
            .expect("rustc must be available wherever the tests are");
        assert!(
            out.status.success(),
            "fake claude did not build:
{}",
            String::from_utf8_lossy(&out.stderr)
        );
        exe
    })
}

/// One sandbox's calls to the stand-in.
struct Probe {
    log: TempDir,
}

impl Probe {
    fn new() -> Self {
        Probe {
            log: TempDir::new().unwrap(),
        }
    }

    fn refresh(&self, sb: &Sandbox, mode: &str, extra: &[&str]) -> std::process::Output {
        self.run(sb, mode, &[], extra)
    }

    /// `before` goes ahead of the subcommand, where global options live.
    fn run(
        &self,
        sb: &Sandbox,
        mode: &str,
        before: &[&str],
        extra: &[&str],
    ) -> std::process::Output {
        let exe = fake_claude().to_string_lossy().to_string();
        let log = self.log.path().join("calls.log");
        let log = log.to_string_lossy().to_string();
        let mut args = before.to_vec();
        args.push("refresh");
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--claude-path", exe.as_str()]);
        sb.cmd_env(
            &args,
            &[
                ("FAKE_CLAUDE_LOG", log.as_str()),
                ("FAKE_CLAUDE_MODE", mode),
                // Set in the caller, so that the probe not seeing it proves
                // it was scrubbed.
                (
                    "CLAUDE_CODE_OAUTH_TOKEN",
                    "sk-ant-oat01-SENTINELMUSTNOTREACHTHEPROBEXXXXXXXXXXXXXXXX",
                ),
            ],
        )
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.log.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// A storm of the four commands that write, in every order the modes allow,
/// against a `claude` that behaves differently each round -- including the
/// mode that wipes a profile, which a real run once did.
///
/// One property, the one the tool exists for: **no profile ends the storm
/// without usable credentials.** Written after a fuzz run outside the suite
/// found no way to break it; kept so that the next change has to face it too.
#[test]
fn no_sequence_of_writes_leaves_a_profile_empty() {
    for mode in ["", "clear", "signed_out", "inert"] {
        let (sb, _work) = sandbox_with_a_due_profile();
        let probe = Probe::new();

        for round in 0..3 {
            probe.refresh(&sb, mode, if round == 1 { &["--force"] } else { &[] });
            sb.run(&["switch", if round % 2 == 0 { "work" } else { "personal" }]);
            sb.run(&["save", if round % 2 == 0 { "work" } else { "personal" }]);
        }

        for name in ["work", "personal"] {
            let raw = std::fs::read_to_string(
                sb.path()
                    .join(".ccred/profiles")
                    .join(name)
                    .join(".credentials.json"),
            )
            .unwrap_or_else(|e| panic!("[{mode}] {name} has no credential file: {e}"));
            let parsed: serde_json::Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("[{mode}] {name} will not parse: {e}"));
            let token = parsed["claudeAiOauth"]["refreshToken"]
                .as_str()
                .unwrap_or_default();
            assert!(
                token.len() >= 40,
                "[{mode}] {name} kept no usable refresh token"
            );
        }
    }
}

/// Two profiles, `personal` active and `work` idle, with `work` inside the
/// ten-day window and its access token expired: the state a refresh is for.
fn sandbox_with_a_due_profile() -> (Sandbox, std::path::PathBuf) {
    sandbox_with_work_access_expiring_in(-3_600_000)
}

fn sandbox_with_work_access_expiring_in(ms: i64) -> (Sandbox, std::path::PathBuf) {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let creds = sb.path().join(".ccred/profiles/work/.credentials.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&creds).unwrap()).unwrap();
    let oauth = &mut doc["claudeAiOauth"];
    oauth["expiresAt"] = (now_ms() + ms).into();
    oauth["refreshTokenExpiresAt"] = (now_ms() + 7 * DAY_MS).into();
    std::fs::write(&creds, serde_json::to_vec(&doc).unwrap()).unwrap();
    Sandbox::make_private(&creds);
    (sb, creds)
}

fn stored_oauth(creds: &Path) -> serde_json::Value {
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(creds).unwrap()).unwrap();
    doc["claudeAiOauth"].clone()
}

/// The whole refresh path, end to end: decide, spawn, judge by the access
/// token, remember the rung, and never hand the probe a way to authenticate
/// as anything but the profile.
#[test]
fn a_due_profile_is_renewed_through_the_first_rung_that_works() {
    let (sb, creds) = sandbox_with_a_due_profile();
    let probe = Probe::new();

    let out = probe.refresh(&sb, "", &[]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("refreshed"), "{text}");
    assert!(
        !text.contains("sk-ant-"),
        "a token reached the output:
{text}"
    );

    let oauth = stored_oauth(&creds);
    assert_eq!(
        oauth["accessToken"], RENEWED_ACCESS,
        "the renewal was not kept"
    );
    assert!(oauth["expiresAt"].as_i64().unwrap() > now_ms());

    // `auth status` does not exchange anything, so the ladder went on to
    // `mcp list` -- and stopped there, spending no quota on a prompt.
    let calls = probe.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("auth status --json|"), "{calls:?}");
    assert!(calls[1].starts_with("mcp list|"), "{calls:?}");
    for call in &calls {
        assert!(call.contains("oauth_token_set=false"), "{calls:?}");
        assert!(call.contains("|config_dir=-|"), "{calls:?}");
    }

    // The rung that worked is tried first next time. `--force` makes the
    // run exchange again although the renewed access token is live.
    let out = probe.refresh(&sb, "", &["--force"]);
    assert_eq!(out.status.code(), Some(0));
    let calls = probe.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[2].starts_with("mcp list|"), "{calls:?}");

    let (out, _, _) = sb.run(&["log"]);
    assert!(out.contains("work refresh"), "{out}");
}

/// "Why did the schedule leave that profile alone" should not cost an
/// exchange to answer. A preview decides and stops: nothing spawned, nothing
/// written, not even the log.
#[test]
fn a_dry_run_refresh_decides_and_touches_nothing() {
    let (sb, creds) = sandbox_with_a_due_profile();
    let before = std::fs::read(&creds).unwrap();

    let (out, err, code) = sb.run(&["refresh", "--dry-run"]);
    assert_eq!(code, 0, "{err}{out}");
    assert!(out.contains("would refresh"), "{out}");
    assert!(out.contains("dry run"), "{out}");
    assert!(!out.contains("refreshed"), "nothing happened: {out}");

    assert_eq!(
        std::fs::read(&creds).unwrap(),
        before,
        "a preview must not write a credential file"
    );
    assert!(
        !sb.path().join(".ccred/logs/ccred.jsonl").exists(),
        "a preview must not leave a log entry"
    );
    assert!(
        !sb.path().join(".ccred/state/last-run.json").exists(),
        "a preview must not count as a run"
    );

    // And the same decisions come back as JSON.
    let (out, _, code) = sb.run(&["refresh", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{out}");
    let report: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(report["status"], serde_json::json!("dry run"));
    assert!(
        report["profiles"].as_array().unwrap().len() >= 2,
        "{report}"
    );

    // The preview meets the gate the scheduler's own invocation meets. A
    // preview of `--if-older-than 48` that showed work the real command would
    // refuse to do is worse than no preview: that flag is the one the
    // registered job actually passes.
    let probe = Probe::new();
    probe.refresh(&sb, "", &[]); // records a run
    let (out, _, code) = sb.run(&["refresh", "--if-older-than", "48", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("nothing to do"), "{out}");
    assert!(out.contains("last run was recent"), "{out}");
    assert!(out.contains("dry run"), "still a preview: {out}");
}

/// A firing that found nothing to do left no trace at all, so `ccred log`
/// could not tell it from a timer that never fired -- the one question the
/// log exists to answer, on a platform where Task Scheduler discards
/// everything a job prints.
#[test]
fn a_run_that_did_nothing_is_still_in_the_log() {
    let (sb, _creds) = sandbox_with_a_due_profile();
    let probe = Probe::new();

    probe.refresh(&sb, "", &[]); // a real run, which records itself
    probe.refresh(&sb, "", &["--if-older-than", "48"]); // the job, rate-limited

    let (out, _, code) = sb.run(&["log", "--json"]);
    assert_eq!(code, 0, "{out}");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(entries.len(), 2, "the skip is a line too: {entries:#?}");
    let skip = entries.last().unwrap();
    assert!(
        skip["status"].as_str().unwrap().starts_with("skipped"),
        "{skip}"
    );
    assert_eq!(skip["scheduled"], serde_json::json!(true));

    // But it is not recorded as the last run: the rate limit measures from
    // the last run that did something, and a skip that moved it forward
    // would suppress the real ones for as long as the timer kept firing.
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sb.path().join(".ccred/state/last-run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["status"], serde_json::json!("ran"), "{record}");
}

/// "The timer has not fired since Tuesday and you have been refreshing by
/// hand" is a thing the log could not say: every run looked alike. Only the
/// registered job passes `--if-older-than`, so that is what tells them apart.
#[test]
fn the_log_says_which_runs_the_schedule_started() {
    let (sb, _creds) = sandbox_with_a_due_profile();
    let probe = Probe::new();

    probe.refresh(&sb, "", &[]); // typed
    probe.refresh(&sb, "", &["--if-older-than", "0"]); // the job

    let (out, _, code) = sb.run(&["log", "--json"]);
    assert_eq!(code, 0, "{out}");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(entries.len(), 2, "{entries:#?}");
    assert_eq!(entries[0]["scheduled"], serde_json::json!(false), "typed");
    assert_eq!(entries[1]["scheduled"], serde_json::json!(true), "the job");

    let (out, _, _) = sb.run(&["log"]);
    assert!(out.contains("FROM"), "{out}");
    assert!(out.contains("timer"), "{out}");
    assert!(out.contains("you"), "{out}");
}

/// The incident this path was hardened for: the spawned binary decided the
/// profile was signed out and wrote empty tokens over it.
#[test]
fn a_probe_that_clears_the_profile_is_undone() {
    let (sb, creds) = sandbox_with_a_due_profile();
    let before = stored_oauth(&creds);
    let probe = Probe::new();

    let out = probe.refresh(&sb, "clear", &[]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code(),
        Some(4),
        "a person is needed:
{text}"
    );
    assert!(text.contains("needs login"), "{text}");
    assert_eq!(
        stored_oauth(&creds),
        before,
        "the cleared credentials were left in place"
    );
}

#[test]
fn a_probe_that_renews_nothing_backs_off_and_changes_nothing() {
    // A live access token, so `--force` has to backdate it to get an
    // exchange -- and has to put it back when none happens.
    let (sb, creds) = sandbox_with_work_access_expiring_in(3_600_000);
    let before = stored_oauth(&creds);
    let probe = Probe::new();

    let out = probe.refresh(&sb, "inert", &["--force"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("backing off"), "{text}");
    // Every rung was tried, and none of them helped.
    assert_eq!(probe.calls().len(), 3, "{:?}", probe.calls());
    // That includes the forced run's backdated expiry: nothing was renewed,
    // so the real one is back.
    assert_eq!(stored_oauth(&creds), before);
}

#[test]
fn a_signed_out_profile_is_reported_not_retried() {
    let (sb, creds) = sandbox_with_a_due_profile();
    let before = stored_oauth(&creds);
    let probe = Probe::new();

    let out = probe.refresh(&sb, "signed_out", &[]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(probe.calls().len(), 1, "{:?}", probe.calls());
    assert_eq!(stored_oauth(&creds), before);
}

/// A relocated configuration reaches the probe as the exact directory this
/// program used -- even when it was given as a flag, which is how a scheduled
/// job receives it, and the environment says nothing.
#[test]
fn a_chosen_config_dir_is_handed_to_the_probe() {
    let (sb, _) = sandbox_with_a_due_profile();
    // With the variable set, `.claude.json` lives inside the directory.
    let dir = sb.path().join(".claude");
    std::fs::copy(sb.path().join(".claude.json"), dir.join(".claude.json")).unwrap();
    let dir_arg = dir.to_string_lossy().to_string();
    let probe = Probe::new();

    let out = probe.run(&sb, "", &["--claude-config-dir", &dir_arg], &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let calls = probe.calls();
    assert!(!calls.is_empty(), "the probe never ran");
    let expected = format!(
        "|config_dir={}|",
        std::path::absolute(&dir).unwrap().display()
    );
    for call in &calls {
        assert!(call.contains(&expected), "{calls:?}");
    }
}

/// Everything follows `--ccred-home`, and a schedule registered with it
/// carries it -- the job will not inherit the shell that chose it.
#[test]
fn a_chosen_home_is_used_and_carried_into_the_schedule() {
    let sb = Sandbox::new();
    let elsewhere = sb.path().join("elsewhere");
    let arg = elsewhere.to_string_lossy().to_string();

    let (out, err, code) = sb.run(&["--ccred-home", &arg, "save", "work"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(elsewhere.join("profiles").join("work").is_dir());
    assert!(
        !sb.path().join(".ccred").exists(),
        "the default home was written as well"
    );

    let (out, _, _) = sb.run(&["--ccred-home", &arg, "list"]);
    assert!(out.contains("work"), "{out}");

    let (out, err, code) = sb.run(&["--ccred-home", &arg, "schedule", "install", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("--ccred-home"),
        "the job would run against the default home:\n{out}"
    );
}

/// Kills the stand-in session however the test ends.
struct Session(std::process::Child);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A live session holds the old account in memory and writes its next
/// refreshed token into whatever file is live by then -- which a switch has
/// made another profile's. So a running Claude Code refuses the switch, and a
/// session file left behind by one that has exited does not.
#[test]
fn switching_is_refused_while_claude_code_runs() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let session = Session(
        Command::new(fake_claude())
            .arg("--sleep")
            .spawn()
            .expect("start the stand-in session"),
    );
    let pid = session.0.id();
    let sessions = sb.path().join(".claude").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join(format!("{pid}.json")), b"{}").unwrap();

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 7, "a live session must block the switch:\n{out}{err}");
    assert!(err.contains("Claude Code is running"), "{err}");
    let (out, _, _) = sb.run(&["current"]);
    assert!(
        out.contains("bob@example.com"),
        "nothing may have changed:\n{out}"
    );

    drop(session);
    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(
        code, 0,
        "the session file outlived its process:\n{out}{err}"
    );
}

/// A journal nobody can read used to fail every later save and switch on the
/// same parse error, for good. It is now moved aside -- and the command that
/// found it still stops, because the switch it described may have left the
/// live tokens and the named account disagreeing.
#[test]
fn an_unreadable_journal_is_set_aside_and_stops_that_command_only() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let journal = sb.path().join(".ccred/state/switch.journal");
    std::fs::write(&journal, b"{ this is not a journal").unwrap();

    let (out, err, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 7, "{out}{err}");
    assert!(
        err.contains("ccred current"),
        "the way out must be named: {err}"
    );
    assert!(!journal.exists(), "the journal is still in the way");
    let aside: Vec<_> = std::fs::read_dir(journal.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("switch.journal.unreadable-")
        })
        .collect();
    assert_eq!(
        aside.len(),
        1,
        "the unreadable journal must be kept, not deleted"
    );

    let (out, err, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 0, "the next attempt must go through: {out}{err}");
}

/// The state a switch leaves when it dies between writing the live tokens
/// and the account name: bob's tokens live, alice named, work active.
fn sandbox_after_a_killed_switch(journal: &[u8]) -> (Sandbox, std::path::PathBuf, Vec<u8>) {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    sb.login_b();
    sb.run(&["save", "personal"]); // bob
    let (_, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}");
    let work = sb.path().join(".ccred/profiles/work/.credentials.json");
    let before = std::fs::read(&work).unwrap();

    std::fs::copy(
        sb.path().join(".ccred/profiles/personal/.credentials.json"),
        sb.path().join(".claude/.credentials.json"),
    )
    .unwrap();
    std::fs::write(sb.path().join(".ccred/state/switch.journal"), journal).unwrap();
    (sb, work, before)
}

const KILLED_AFTER_LIVE_WRITE: &[u8] =
    br#"{"from":"work","to":"personal","phase":"live_creds_written",
    "started_at_ms":1788000000000,"pid":999999}"#;

/// The scheduled mirror copies the live tokens into the active profile. With
/// the switch unsettled, "active" still meant work, and bob's tokens went
/// into alice's profile.
#[test]
fn the_scheduled_mirror_settles_a_killed_switch_first() {
    let (sb, work, before) = sandbox_after_a_killed_switch(KILLED_AFTER_LIVE_WRITE);

    let (out, err, _) = sb.run(&["refresh"]);
    assert_eq!(
        std::fs::read(&work).unwrap(),
        before,
        "work now holds bob's tokens:\n{out}{err}"
    );
    let (out, _, _) = sb.run(&["current"]);
    assert!(
        out.contains("personal"),
        "the switch was not finished: {out}"
    );
}

/// Even with no journal to go on, tokens that another profile holds are not
/// stored under a different name: the refresh token says whose they are.
#[test]
fn tokens_another_profile_holds_are_never_stored_under_this_one() {
    let (sb, work, before) = sandbox_after_a_killed_switch(b"{ unreadable");
    // First attempt stops on the journal; after that there is nothing to go on.
    sb.run(&["save", "work"]);
    let (_, err, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 7);
    assert!(err.contains("stored as profile 'personal'"), "{err}");

    for args in [&["refresh"][..], &["switch", "personal"][..]] {
        let (out, err, _) = sb.run(args);
        assert_eq!(
            std::fs::read(&work).unwrap(),
            before,
            "`ccred {}` stored bob's tokens as work:\n{out}{err}",
            args.join(" ")
        );
    }
}

/// A switch killed after writing the live credentials leaves three things:
/// the target's tokens live, the old account still named in `.claude.json`,
/// and its lock behind. A `save` of the old profile used to give up on
/// healing while that lock was fresh, wait for it to go stale, and then store
/// the target's tokens under the old account's name -- which the identity
/// check could not catch, because `.claude.json` still said the old account.
#[test]
fn a_save_after_a_killed_switch_never_stores_the_wrong_account() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    sb.login_b();
    sb.run(&["save", "personal"]); // bob, now active
    let (_, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}");
    let work = sb.path().join(".ccred/profiles/work/.credentials.json");
    let work_before = std::fs::read(&work).unwrap();

    // The killed `switch personal`: bob's tokens are live, alice is still
    // named, the journal says the live write happened.
    std::fs::copy(
        sb.path().join(".ccred/profiles/personal/.credentials.json"),
        sb.path().join(".claude/.credentials.json"),
    )
    .unwrap();
    std::fs::write(
        sb.path().join(".ccred/state/switch.journal"),
        r#"{"from":"work","to":"personal","phase":"live_creds_written",
            "started_at_ms":1788000000000,"pid":999999}"#,
    )
    .unwrap();
    // Its lock, a few seconds from counting as abandoned.
    let lock = sb.path().join(".claude/.storage-write.lock");
    std::fs::create_dir(&lock).unwrap();
    let ten_seconds_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
    filetime::set_file_mtime(&lock, ten_seconds_ago.into()).unwrap();

    let (out, err, code) = sb.run(&["save", "work"]);

    assert_eq!(
        std::fs::read(&work).unwrap(),
        work_before,
        "work now holds another account's tokens:\n{out}{err}"
    );
    assert_ne!(code, 0, "{out}{err}");
    // The switch was finished instead: bob is live and named.
    let (out, _, _) = sb.run(&["current"]);
    assert!(out.contains("bob@example.com"), "{out}");
}

/// `--force` backdates a live access token to make an exchange happen. A
/// probe that answers "signed out" is not a renewal, so the real expiry has
/// to be put back -- as on every other path that renews nothing.
#[test]
fn a_forced_run_that_meets_a_signed_out_account_leaves_the_real_expiry() {
    let (sb, creds) = sandbox_with_work_access_expiring_in(3_600_000);
    let before = stored_oauth(&creds);
    let probe = Probe::new();

    let out = probe.refresh(&sb, "signed_out", &["--force"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(stored_oauth(&creds), before);
}

/// After Claude Code renews the live tokens, the old refresh token is dead
/// and the new pair exists only in the live store until ccred copies it.
/// Each exchange recomputes the fixed refresh deadline and can land it a
/// fraction of a second *earlier* -- measured: 809 ms -- and the write gate
/// used to refuse that as a shrinking window, so the profile kept the dead
/// token.
#[test]
fn renewed_live_tokens_are_kept_even_when_the_deadline_jitters_back() {
    const TOKEN_A2: &str = "sk-ant-oat01-SENTINELACCESSA2A2A2A2A2A2A2A2A2A2A2A2A2A2A2A2A2";
    const REFRESH_A2: &str = "sk-ant-ort01-SENTINELREFRESHA2A2A2A2A2A2A2A2A2A2A2A2A2A2A2A2";
    const REFRESH_A3: &str = "sk-ant-ort01-SENTINELREFRESHA3A3A3A3A3A3A3A3A3A3A3A3A3A3A3A3";

    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice
    sb.login_b();
    sb.run(&["save", "personal"]);
    let (_, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{err}");
    let work = sb.path().join(".ccred/profiles/work/.credentials.json");

    // Claude Code renews alice's tokens in the live store.
    sb.write_login(
        TOKEN_A2,
        REFRESH_A2,
        FAR_FUTURE - 809,
        "alice@example.com",
        "uuid-a",
    );
    // The scheduled mirror of the active profile must keep them.
    let (out, err, code) = sb.run(&["refresh"]);
    assert_eq!(code, 0, "{out}{err}");
    let stored = std::fs::read_to_string(&work).unwrap();
    assert!(
        stored.contains(REFRESH_A2),
        "the mirror dropped them:\n{out}"
    );

    // And again, with switching away doing the copy.
    sb.write_login(
        TOKEN_A2,
        REFRESH_A3,
        FAR_FUTURE - 1_618,
        "alice@example.com",
        "uuid-a",
    );
    let (out, err, code) = sb.run(&["switch", "personal"]);
    assert_eq!(code, 0, "{out}{err}");
    let stored = std::fs::read_to_string(&work).unwrap();
    assert!(
        stored.contains(REFRESH_A3),
        "switching away left a dead token behind:\n{out}"
    );
}

/// A mirror that cannot be done is something a person has to look at: the
/// live account is not the one the active profile names. It used to be
/// listed as "mirrored", with the refusal in small print, and exit 0.
#[test]
fn a_mirror_that_is_refused_is_not_reported_as_done() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]); // alice, active
    sb.login_b(); // bob logs in; the pointer still says work

    let (out, err, code) = sb.run(&["refresh"]);
    assert_eq!(code, 4, "a person is needed:\n{out}{err}");
    assert!(out.contains("blocked"), "{out}");
    assert!(
        out.contains("not mirrored"),
        "the reason must be given: {out}"
    );
    assert!(out.contains("1 needs attention"), "{out}");

    // Nothing a retry can change, so it must not lift the over-fire limit:
    // every scheduler firing used to become a full run.
    let (out, _, code) = sb.run(&["refresh", "--if-older-than", "12"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("nothing to do"), "the limit was lifted: {out}");
    assert!(
        !out.contains("need a login"),
        "logging in is not the fix: {out}"
    );
}

/// The profile a killed switch was moving to holds the very tokens that are
/// now live. Treated as idle, it was refreshed through its own store -- which
/// rotates the live session's refresh token away. The switch is settled
/// before anything is decided, so it counts as active and is never probed.
#[test]
fn the_target_of_a_killed_switch_is_not_refreshed_as_if_idle() {
    let (sb, _, _) = sandbox_after_a_killed_switch(KILLED_AFTER_LIVE_WRITE);
    // Make its stored copy due for a refresh.
    let creds = sb.path().join(".ccred/profiles/personal/.credentials.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&creds).unwrap()).unwrap();
    doc["claudeAiOauth"]["expiresAt"] = (now_ms() - 3_600_000).into();
    doc["claudeAiOauth"]["refreshTokenExpiresAt"] = (now_ms() + 7 * DAY_MS).into();
    std::fs::write(&creds, serde_json::to_vec(&doc).unwrap()).unwrap();
    Sandbox::make_private(&creds);

    let probe = Probe::new();
    let out = probe.refresh(&sb, "", &[]);
    assert!(
        probe.calls().is_empty(),
        "personal was probed:\n{:?}\n{}",
        probe.calls(),
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `switch` meets an unreadable journal the same way `save` does: it stops,
/// and copies nothing into the profile it was leaving.
#[test]
fn a_switch_that_meets_an_unreadable_journal_stops() {
    let (sb, work, before) = sandbox_after_a_killed_switch(b"{ unreadable");
    let (out, err, code) = sb.run(&["switch", "personal"]);
    assert_eq!(code, 7, "{out}{err}");
    assert!(err.contains("ccred current"), "{err}");
    assert_eq!(std::fs::read(&work).unwrap(), before);
}

/// A killed switch leaves bob's tokens live under alice's name. `current`
/// and `doctor` used to trust the name; the tokens say whose they are.
#[test]
fn live_tokens_that_are_another_profiles_are_pointed_out() {
    let (sb, _, _) = sandbox_after_a_killed_switch(b"{ unreadable");
    std::fs::remove_file(sb.path().join(".ccred/state/switch.journal")).unwrap();

    let (out, _, _) = sb.run(&["current"]);
    assert!(
        out.contains("stored as profile 'personal'"),
        "current trusted the name: {out}"
    );
    let (out, _, code) = sb.run(&["doctor"]);
    assert_eq!(code, 7, "{out}");
    assert!(out.contains("stored as profile 'personal'"), "{out}");
}

/// Two profiles with one refresh token -- possible before `save` refused it
/// -- are a trap: refreshing one signs the other out.
#[test]
fn profiles_sharing_a_token_are_reported_and_not_broken() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    // A second name for the same login, as an older version allowed.
    let from = sb.path().join(".ccred/profiles/work");
    let to = sb.path().join(".ccred/profiles/work2");
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(&from).unwrap().flatten() {
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
    let meta = std::fs::read_to_string(to.join("ccred.json"))
        .unwrap()
        .replace("\"work\"", "\"work2\"");
    std::fs::write(to.join("ccred.json"), meta).unwrap();

    let (out, _, _) = sb.run(&["doctor"]);
    assert!(out.contains("hold the same refresh token"), "{out}");

    // The active one still mirrors: nothing new is written, so nothing is
    // refused.
    let (out, _, code) = sb.run(&["refresh"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("blocked"), "{out}");
}

/// Holds `~/.ccred/state/.profiles.lock` the way a running refresh does,
/// heartbeat included, until dropped.
struct ProfilesHeld {
    lock: std::path::PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    beat: Option<std::thread::JoinHandle<()>>,
}

impl ProfilesHeld {
    fn new(sb: &Sandbox) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let lock = sb.path().join(".ccred/state/.profiles.lock");
        std::fs::create_dir_all(&lock).unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let beat = {
            let stop = std::sync::Arc::clone(&stop);
            let lock = lock.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let f = lock.join("beat");
                    let _ = std::fs::write(&f, b"1");
                    let _ = std::fs::remove_file(&f);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            })
        };
        ProfilesHeld {
            lock,
            stop,
            beat: Some(beat),
        }
    }
}

impl Drop for ProfilesHeld {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(beat) = self.beat.take() {
            let _ = beat.join();
        }
        let _ = std::fs::remove_dir_all(&self.lock);
    }
}

/// While a refresh is probing a profile, a switch must not make that
/// profile live -- the probe would then retire the token the new live
/// session holds -- and a refresh must not probe what a switch is moving.
#[test]
fn a_switch_and_a_refresh_never_work_on_the_profiles_at_once() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);
    let held = ProfilesHeld::new(&sb);

    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 6, "a busy lock is Busy:\n{out}{err}");
    assert!(err.contains("try again shortly"), "{err}");

    let (out, _, code) = sb.run(&["refresh"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("left for the next run"), "{out}");

    drop(held);
    let (out, err, code) = sb.run(&["switch", "work"]);
    assert_eq!(code, 0, "{out}{err}");
}

/// Windows and macOS do not tell `work` from `WORK`, so switching to a
/// different spelling wrote a pointer that matched no directory: the active
/// marker vanished and `rm` would delete the profile whose credentials were
/// live.
#[test]
fn a_profile_is_the_same_profile_however_it_is_spelled() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    let (out, err, code) = sb.run(&["switch", "WORK"]);
    if cfg!(windows) || cfg!(target_os = "macos") {
        assert_eq!(code, 0, "{out}{err}");
        let (out, _, _) = sb.run(&["list"]);
        let active_line = out
            .lines()
            .find(|l| l.contains("work"))
            .expect("work must be listed");
        assert!(
            active_line.contains('*') || active_line.contains('\u{25cf}'),
            "work is live but not marked active: {out}"
        );

        let (out, err, code) = sb.run(&["rm", "work"]);
        assert_eq!(
            code, 7,
            "the live profile must not be removable:\n{out}{err}"
        );
        assert!(
            sb.path().join(".ccred/profiles/work").is_dir(),
            "the active profile was deleted"
        );

        // And saving under the other spelling updates it, rather than being
        // refused as another profile's credentials.
        let (out, err, code) = sb.run(&["save", "WORK"]);
        assert_eq!(code, 0, "{out}{err}");
        let dirs: Vec<String> = std::fs::read_dir(sb.path().join(".ccred/profiles"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            dirs,
            ["personal", "work"],
            "a second directory was created for the other spelling"
        );
    } else {
        assert_eq!(code, 3, "on a case-sensitive file system it is not found");
    }
}

/// `current` and `list` are what someone runs *because* a file looks wrong.
/// A timestamp of i64::MIN made the subtraction overflow and the process
/// panicked with exit 101, outside the documented codes.
#[test]
fn an_absurd_timestamp_is_reported_not_panicked_on() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let live = sb.path().join(".claude/.credentials.json");
    let text = std::fs::read_to_string(&live)
        .unwrap()
        .replace(&FAR_FUTURE.to_string(), "-9223372036854775808");
    std::fs::write(&live, text).unwrap();

    for args in [&["current"][..], &["list"][..], &["doctor"][..]] {
        let (out, err, code) = sb.run(args);
        assert_ne!(code, 101, "`ccred {}` panicked:\n{out}{err}", args[0]);
        assert!(
            (0..=8).contains(&code),
            "exit {code} is outside the contract"
        );
    }
}

/// A profile whose credential file no longer parses is exactly what `save`
/// is for; it used to refuse, because reading the old file came first.
#[test]
fn a_profile_whose_file_is_corrupt_can_be_saved_over() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    let creds = sb.path().join(".ccred/profiles/work/.credentials.json");
    std::fs::write(&creds, b"{ this is not json").unwrap();

    let (out, err, code) = sb.run(&["save", "work"]);
    assert_eq!(code, 0, "{out}{err}");
    let repaired = std::fs::read_to_string(&creds).unwrap();
    assert!(repaired.contains(REFRESH_A), "the profile was not repaired");
    // The damaged bytes are kept, as every overwrite is.
    let backups: Vec<_> = walk(&sb.path().join(".ccred/backups"))
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    assert!(!backups.is_empty(), "no copy of the damaged file was kept");
}

/// "Add `--json` to any of them" is a promise the README, the help text and
/// the examples all make. `restore` printed nothing at all, and
/// `schedule install --dry-run` printed the human block; a script piping
/// either into a parser got an error.
#[test]
fn every_documented_command_speaks_json() {
    let sb = Sandbox::new();
    sb.run(&["save", "work"]);
    sb.login_b();
    sb.run(&["save", "personal"]);

    // Read-only or sandboxed, in an order where each has something to say.
    let invocations: &[&[&str]] = &[
        &["current"],
        &["list"],
        &["save", "personal"],
        &["switch", "work"],
        &["restore", "work"],
        &["refresh"],
        &["log"],
        &["rm", "personal"],
        &["refresh", "--dry-run"],
        &["schedule", "status"],
        &["schedule", "install", "--dry-run"],
        &["uninstall", "--dry-run"],
    ];

    for args in invocations {
        let mut with_json = args.to_vec();
        with_json.push("--json");
        let out = sb.cmd(&with_json);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`ccred {}` failed:\n{stdout}{}",
            with_json.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.trim().is_empty(),
            "`ccred {}` printed nothing",
            with_json.join(" ")
        );
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
            panic!(
                "`ccred {}` did not print JSON: {e}\n{stdout}",
                with_json.join(" ")
            )
        });
    }
}
