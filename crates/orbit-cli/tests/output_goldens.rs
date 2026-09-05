#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Golden-file regression coverage for the plain and `json` forms of four
//! list commands, per `docs/design/terminal-interface/specs/output-modes.md`
//! and `docs/design/terminal-interface/specs/table-rendering.md` (ORB-10571).
//!
//! The "table" form (a real terminal, pinned width, truncation) cannot be
//! produced from this harness: `assert_cmd` captures stdout through a pipe,
//! so `std::io::stdout().is_terminal()` is always `false` inside the child
//! process, and both `crate::output::table::sink_width` and comfy-table's own
//! `should_style` gate on that same check. Table-form golden coverage lives
//! instead in `crates/orbit-cli/src/output/tests/table.rs`, which renders
//! directly at an explicit pinned width. See that module's doc comment for
//! the corresponding finding.
//!
//! ## Regenerating goldens
//!
//! `ORBIT_UPDATE_OUTPUT_GOLDENS=1 cargo test -p orbit-cli --test output_goldens`
//!
//! Regenerating is a deliberate act, not a fix for a failing test: it
//! overwrites the checked-in fixture with whatever the binary currently
//! produces. Review the diff before committing — a golden that changed
//! without an intentional rendering change is the regression this suite
//! exists to catch.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use regex::Regex;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const UPDATE_ENV: &str = "ORBIT_UPDATE_OUTPUT_GOLDENS";

/// One covered list command: its golden-file stem and the argv prefix
/// (without `--json`) that produces its list.
struct Command {
    name: &'static str,
    args: &'static [&'static str],
}

/// At least four list commands across table, plain, and json (ORB-10571
/// acceptance criteria). Each renders through `output::table::Table` for its
/// non-`--json` form, so the plain-form assertions here exercise the same
/// contract as the pinned-width fixtures in `output/tests/table.rs`.
const COMMANDS: &[Command] = &[
    Command {
        name: "tool_list",
        args: &["tool", "list"],
    },
    Command {
        name: "task_list",
        args: &["task", "list"],
    },
    Command {
        name: "policy_list",
        args: &["policy", "list"],
    },
    Command {
        name: "skill_list",
        args: &["skill", "list"],
    },
];

/// Deterministic seed data for `task list`. Priorities and types are chosen
/// to differ across rows so the TYPE/PRIORITY columns are not suppressed by
/// table-rendering.md §5's uniform-value rule, while status is left at its
/// default (`proposed`) for every row so STATUS *is* suppressed — the same
/// mixed suppression a real result set produces.
const SEED_TASKS: &[(&str, &str, &str, &str)] = &[
    (
        "Fix the flaky retry loop in the sync worker",
        "The worker drops events under backpressure instead of retrying them.",
        "high",
        "bug",
    ),
    (
        "Document the new sink resolution precedence",
        "Explain --format, ORBIT_FORMAT, and the per-command --json alias in one place.",
        "medium",
        "chore",
    ),
    (
        "Add pagination to the audit export command",
        "orbit audit export currently loads the entire event log into memory.",
        "low",
        "feature",
    ),
];

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    /// A fresh workspace with the deterministic task seed applied. Policies
    /// and skills need no seeding: `orbit workspace init` seeds the default
    /// policy and the default skill catalog on every fresh workspace.
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        fixture.run(&["workspace", "init", "--name", "output-goldens"], &[]);
        for (title, description, priority, task_type) in SEED_TASKS {
            fixture.run(
                &[
                    "task",
                    "add",
                    "--title",
                    title,
                    "--description",
                    description,
                    "--priority",
                    priority,
                    "--complexity",
                    "medium",
                    "--type",
                    task_type,
                ],
                &[],
            );
        }
        fixture
    }

    /// Run `orbit` with pinned identity and geometry so the only remaining
    /// non-determinism is timestamps and the workspace's own temp paths,
    /// both handled by [`redact`]. `extra_env` layers on top for the
    /// color-configuration sweep.
    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
        let mut command = cargo_bin_cmd!("orbit");
        // ORB-11300: one shared list, so this fixture and its siblings cannot
        // drift apart on which ambient variable still routes a write. The
        // pinned identity below is deliberate and applies after the scrub.
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("COLUMNS", "100")
            .env("ORBIT_AGENT_NAME", "claude")
            .env("ORBIT_AGENT_MODEL", "claude")
            .env_remove("ORBIT_FORMAT")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("TERM");
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let output = command.args(args).output().expect("run orbit");
        assert!(
            output.status.success(),
            "`orbit {}` failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn redact(&self, text: &str) -> String {
        redact(text, &self.home)
    }
}

/// Replace the two sources of run-to-run non-determinism a fresh workspace
/// still carries once identity and geometry are pinned: wall-clock
/// timestamps (`created_at`/`updated_at`, and the table's own
/// `%Y-%m-%d %H:%M` formatting) and this test's own temp-directory paths
/// (which `skill list --json` echoes back verbatim).
fn redact(text: &str, home: &Path) -> String {
    static RFC3339: OnceLock<Regex> = OnceLock::new();
    static SHORT_DATE: OnceLock<Regex> = OnceLock::new();
    let rfc3339 = RFC3339.get_or_init(|| {
        Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})")
            .expect("valid regex")
    });
    let short_date = SHORT_DATE
        .get_or_init(|| Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}").expect("valid regex"));
    let text = rfc3339.replace_all(text, "<TIMESTAMP>");
    let text = short_date.replace_all(&text, "<DATE>");
    text.replace(&home.display().to_string(), "<HOME>")
}

#[test]
fn fixture_ignores_inherited_managed_routing_and_identity() {
    let sentinel = Fixture::new();
    let sentinel_registry = sentinel.home.join(".orbit");
    let sentinel_registry = sentinel_registry
        .to_str()
        .expect("sentinel registry path is UTF-8");
    let before = sentinel.run(&["task", "list", "--json"], &[]);

    let _managed_env = test_env::scoped([
        ("ORBIT_ROOT", Some("/sentinel/orbit-root")),
        ("ORBIT_SESSION_ID", Some("sentinel-session")),
        ("ORBIT_TASK_ID", Some("sentinel-task")),
        ("ORBIT_ACTIVE_TASK_ID", Some("sentinel-task")),
        ("ORBIT_RUN_ID", Some("sentinel-run")),
        ("ORBIT_ACTIVITY_ID", Some("sentinel-activity")),
        ("ORBIT_STEP_INDEX", Some("sentinel-step")),
        ("ORBIT_AGENT_NAME", Some("sentinel-agent")),
        ("ORBIT_AGENT_MODEL", Some("sentinel-model")),
        ("ORBIT_OPERATOR", Some("1")),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_TASK_ACTOR_KIND", Some("sentinel-actor")),
        ("ORBIT_REGISTRY_ROOT", Some(sentinel_registry)),
        ("ORBIT_WORKSPACE", Some("output-goldens")),
    ]);

    let fixture = Fixture::new();

    let after = sentinel.run(&["task", "list", "--json"], &[]);
    assert_eq!(
        after.stdout, before.stdout,
        "sentinel workspace was modified"
    );

    let tasks: Value =
        serde_json::from_slice(&fixture.run(&["task", "list", "--json"], &[]).stdout)
            .expect("fixture task list JSON");
    let tasks = tasks.as_array().expect("fixture task list");
    assert_eq!(tasks.len(), SEED_TASKS.len());
    assert!(
        tasks
            .iter()
            .all(|task| task["created_by"] == json!("claude")),
        "fixture task creation must retain the pinned display identity: {tasks:?}"
    );
}

/// `skill list`'s `content_hash` column is a SHA-256 digest over the bundled
/// skill's own `SKILL.md` (`crates/orbit-core/src/command/skill.rs`'s
/// `DEFAULT_SKILL_FILES`), so it changes whenever unrelated skill prose is
/// edited even though no rendering code did. Redact it like `<TIMESTAMP>`/
/// `<HOME>` above — but scoped to `skill_list` only, so the other three
/// commands (which carry no content-derived field; see sibling goldens)
/// still pin their output byte-for-byte.
///
/// Extracts the true hash(es) from the `--json` form first and asserts each
/// is a well-formed 64-char lowercase hex digest, so this redaction cannot
/// silently swallow a malformed or missing field. It also cross-checks that
/// the plain form's 10-char `HASH` column is a genuine prefix of that same
/// digest before redacting it, so the truncation performed by
/// `crates/orbit-cli/src/command/skill/list.rs`'s rendering path stays
/// covered rather than becoming a no-op once the hash itself is redacted.
fn redact_skill_content_hash(json_text: &str, plain_text: &str) -> (String, String) {
    static FULL_HASH: OnceLock<Regex> = OnceLock::new();
    let full_hash = FULL_HASH
        .get_or_init(|| Regex::new(r#""content_hash": "([0-9a-f]{64})""#).expect("valid regex"));

    let hashes: Vec<String> = full_hash
        .captures_iter(json_text)
        .map(|caps| caps[1].to_string())
        .collect();
    assert!(
        !hashes.is_empty(),
        "no content_hash field found in skill_list.json output:\n{json_text}"
    );
    for hash in &hashes {
        assert!(
            hash.len() == 64
                && hash
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "content_hash is not a well-formed 64-char lowercase hex digest: {hash}"
        );
    }

    let mut plain_redacted = plain_text.to_string();
    let mut json_redacted = json_text.to_string();
    for hash in &hashes {
        let short = &hash[..10];
        assert!(
            plain_text.contains(short),
            "plain form's truncated HASH column ({short}) is not a prefix of the json form's \
             content_hash ({hash}); truncation rendering coverage would be lost by redaction"
        );
        plain_redacted = plain_redacted.replace(short, "<CONTENT_HASH>");
        json_redacted = json_redacted.replace(hash.as_str(), "<CONTENT_HASH>");
    }
    (json_redacted, plain_redacted)
}

fn golden_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/output_goldens")
        .join(file_name)
}

/// Compare against (or, with `ORBIT_UPDATE_OUTPUT_GOLDENS=1`, overwrite) the
/// checked-in golden file.
fn assert_golden(file_name: &str, actual: &str) {
    let path = golden_path(file_name);
    if std::env::var(UPDATE_ENV).as_deref() == Ok("1") {
        std::fs::write(&path, actual)
            .unwrap_or_else(|err| panic!("write golden {}: {err}", path.display()));
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read {} ({err}); regenerate with `{UPDATE_ENV}=1 cargo test -p orbit-cli --test output_goldens`",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} drifted from its golden. If the new rendering is correct, regenerate with \
         `{UPDATE_ENV}=1 cargo test -p orbit-cli --test output_goldens` and review the diff \
         before committing.",
        path.display()
    );
}

#[test]
fn plain_and_json_forms_match_their_goldens() {
    let fixture = Fixture::new();

    for command in COMMANDS {
        let plain = fixture.run(command.args, &[]);
        let mut plain_stdout = fixture.redact(&String::from_utf8_lossy(&plain.stdout));

        let mut json_args = command.args.to_vec();
        json_args.push("--json");
        let json = fixture.run(&json_args, &[]);
        let mut json_stdout = fixture.redact(&String::from_utf8_lossy(&json.stdout));

        if command.name == "skill_list" {
            let (redacted_json, redacted_plain) =
                redact_skill_content_hash(&json_stdout, &plain_stdout);
            json_stdout = redacted_json;
            plain_stdout = redacted_plain;
        }

        assert_golden(&format!("{}.plain.txt", command.name), &plain_stdout);
        assert_golden(&format!("{}.json", command.name), &json_stdout);
    }
}

/// table-rendering.md §4: "truncation never applies to json ... or the
/// plain piped form." color-and-styling.md §4: json/ndjson/plain carry no
/// escape sequences under any flag. Since this harness never gives the
/// child a tty, `is_tty` is `false` for every run regardless of these
/// variables (output-modes.md §1's invariant); the sweep exists to pin that
/// invariant against a regression, not because any one combination is
/// expected to behave differently from the others.
#[test]
fn no_ansi_escapes_under_any_color_configuration() {
    let fixture = Fixture::new();
    let combinations: &[&[(&str, &str)]] = &[
        &[],
        &[("NO_COLOR", "1")],
        &[("CLICOLOR_FORCE", "1")],
        &[("TERM", "dumb")],
        &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")],
    ];

    for command in COMMANDS {
        for extra_env in combinations {
            for json in [false, true] {
                let mut args = command.args.to_vec();
                if json {
                    args.push("--json");
                }
                let output = fixture.run(&args, extra_env);
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    !stdout.contains('\u{1b}'),
                    "`orbit {}` emitted an ANSI escape under {extra_env:?}:\n{stdout}",
                    args.join(" ")
                );
            }
        }
    }
}
