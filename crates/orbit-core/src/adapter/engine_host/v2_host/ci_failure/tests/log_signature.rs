//! Failed-step log analysis as filing sees it: excerpts, signatures, and the
//! keys that must neither fragment one regression nor merge distinct ones.

use serde_json::{Value, json};

use super::filing::{CHECKOUT, NEXT_HEAD, failure, file, filed_task_ids, snapshot};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;

pub(super) fn signature_line(description: &str) -> String {
    description
        .lines()
        .find(|line| line.contains("Normalized error signature"))
        .expect("signature line")
        .to_string()
}

pub(super) fn excerpt_block(description: &str) -> String {
    let start = description
        .find("## Failed-step log excerpt")
        .expect("excerpt heading");
    let rest = &description[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Realistic GitHub failed-step log: `##[group]Run …`, a large `env:` dump,
/// then the trailing compiler diagnostic. Each env line is ~90 bytes so 50
/// lines already sit past the 4,000-byte description budget.
fn realistic_github_step_log(command: &str, env_lines: usize, trailing: &str) -> String {
    let prefix = |msg: &str| format!("build\tRun go build\t2026-08-30T01:00:00Z {msg}");
    let mut out = String::new();
    out.push_str(&prefix(&format!("##[group]Run {command}")));
    out.push('\n');
    out.push_str(&prefix("env:"));
    out.push('\n');
    for index in 0..env_lines {
        out.push_str(&prefix(&format!("  VAR_{index}: {}", "x".repeat(60))));
        out.push('\n');
    }
    out.push_str(&prefix("##[endgroup]"));
    out.push('\n');
    out.push_str(trailing);
    if !trailing.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn dani_10111_style_log(commit_subject: &str, error: Option<&str>) -> String {
    let prefix = |msg: &str| format!("Vulnerability scan\tCheckout\t2026-08-30T01:00:00Z {msg}\n");
    let mut out = String::new();
    out.push_str(&prefix("##[group]Run actions/checkout@v4"));
    out.push_str(&prefix("with:"));
    out.push_str(&prefix("  repository: acme/monodev"));
    out.push_str(&prefix("  token: ***"));
    out.push_str(&prefix("env:"));
    out.push_str(&prefix("  GITHUB_TOKEN: ***"));
    out.push_str(&prefix("##[endgroup]"));
    out.push_str(&prefix("Syncing repository: acme/monodev"));
    out.push_str(&prefix(&format!("HEAD is now at e5c1dc9 {commit_subject}")));
    if let Some(error) = error {
        out.push_str(&prefix(error));
    }
    out
}

fn filed_description(runtime: &OrbitRuntime, log: &str) -> (Value, String) {
    let output = file(
        runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "ci",
            "build",
            "cargo build",
            log,
            CHECKOUT,
        )])}),
    );
    let task_id = filed_task_ids(&output)
        .first()
        .cloned()
        .expect("one filed task");
    let task = runtime.get_task(&task_id).expect("read filed task");
    (output, task.description)
}

#[test]
fn excerpt_keeps_the_run_command_and_trailing_error_not_the_env_dump() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let trailing = concat!(
        "build\tRun go build\t2026-08-30T01:00:00Z ##[command]go build ./...\n",
        "build\tRun go build\t2026-08-30T01:00:00Z ./main.go:10:2: undefined: Foo\n",
        "build\tRun go build\t2026-08-30T01:00:00Z ##[error]Process completed with exit code 1.\n",
    );
    let log = realistic_github_step_log("go build ./...", 80, trailing);
    let error_at = log
        .find("undefined: Foo")
        .expect("fixture must contain the trailing error");
    assert!(
        error_at > 4_000,
        "fixture must place the error past the 4,000-byte description budget, at {error_at}"
    );

    let (_output, description) = filed_description(&runtime, &log);
    let excerpt = excerpt_block(&description);
    assert!(
        excerpt.contains("##[group]Run go build ./..."),
        "excerpt must keep the runner command:\n{excerpt}"
    );
    assert!(
        excerpt.contains("undefined: Foo"),
        "excerpt must carry the trailing error, not a head window:\n{excerpt}"
    );
    assert!(
        excerpt.contains("##[error]Process completed with exit code 1."),
        "excerpt must carry the annotated error:\n{excerpt}"
    );
    assert!(
        !excerpt.contains("VAR_0:"),
        "excerpt must drop the env dump:\n{excerpt}"
    );
}

#[test]
fn excerpt_without_an_error_anchor_says_so_and_still_shows_the_command() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = realistic_github_step_log("go build ./...", 80, "");

    let (_output, description) = filed_description(&runtime, &log);
    let excerpt = excerpt_block(&description);
    assert!(
        excerpt.contains("##[group]Run go build ./..."),
        "command line must still be shown:\n{excerpt}"
    );
    assert!(
        excerpt.contains("No error anchor was present in the retained excerpt"),
        "missing anchor must be stated, not implied by dumping env:\n{excerpt}"
    );
    assert!(
        !excerpt.contains("VAR_0:"),
        "env dump must not be presented as evidence:\n{excerpt}"
    );
}

#[test]
fn error_signature_prefers_an_annotated_error_over_a_checkout_commit_message() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = dani_10111_style_log(
        "chore: add ci failure sweep routine",
        Some("##[error]GO-2024-2611: yaml: vulnerable dependency"),
    );

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description);
    assert!(
        !signature.to_ascii_lowercase().contains("head is now at"),
        "checkout bookkeeping must not become the signature: {signature}"
    );
    assert!(
        signature.contains("yaml") && signature.contains("vulnerable"),
        "signature must come from the ##[error] line: {signature}"
    );
}

#[test]
fn generic_runner_trailer_does_not_collapse_distinct_unannotated_diagnostics() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let trailer =
        "build\tRun go build\t2026-08-30T01:00:00Z ##[error]Process completed with exit code 1.\n";
    let foo_log = realistic_github_step_log(
        "go build ./...",
        2,
        &format!(
            "build\tRun go build\t2026-08-30T01:00:00Z ./main.go:10:2: undefined: Foo\n{trailer}"
        ),
    );
    let bar_log = realistic_github_step_log(
        "go build ./...",
        2,
        &format!(
            "build\tRun go build\t2026-08-30T01:00:00Z ./main.go:14:2: undefined: Bar\n{trailer}"
        ),
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "ci", "build", "go build", &foo_log, CHECKOUT),
            failure(11, "ci", "build", "go build", &bar_log, CHECKOUT),
        ])}),
    );

    assert_eq!(first["filed_count"], json!(2));
    let filed = first["filed"].as_array().expect("filed");
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
    for (task_id, diagnostic) in filed_task_ids(&first).iter().zip(["foo", "bar"]) {
        let description = runtime
            .get_task(task_id)
            .expect("read filed task")
            .description;
        let signature = signature_line(&description).to_ascii_lowercase();
        assert!(
            signature.contains(diagnostic),
            "specific diagnostic must be the signature: {signature}"
        );
        assert!(
            !signature.contains("process completed"),
            "generic runner trailer must not be the signature: {signature}"
        );
    }

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            12, "ci", "build", "go build", &foo_log, NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], filed[0]["failure_key"],
        "the same diagnostic must retain its failure key across commits"
    );
}

#[test]
fn checkout_commit_message_containing_failure_is_not_the_signature() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = dani_10111_style_log("chore: add ci failure sweep routine", None);

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description);
    assert!(
        signature.contains("step-name fallback"),
        "bookkeeping-only excerpt must label the step-name fallback: {signature}"
    );
    assert!(
        !signature.to_ascii_lowercase().contains("head is now at"),
        "HEAD is now at <hex> chore: … failure … must not be chosen: {signature}"
    );
}

#[test]
fn same_failure_under_a_different_commit_message_reuses_the_failure_key() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let first_log = dani_10111_style_log(
        "chore: add ci failure sweep routine",
        Some("##[error]GO-2024-2611: yaml: vulnerable dependency"),
    );
    let second_log = dani_10111_style_log(
        "chore: mention failure in a later commit",
        Some("##[error]GO-2024-2611: yaml: vulnerable dependency"),
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10, "ci", "build", "cargo build", &first_log, CHECKOUT,
        )])}),
    );
    let task_id = filed_task_ids(&first)
        .first()
        .cloned()
        .expect("first sweep files one task");
    let first_key = first["filed"][0]["failure_key"].clone();

    let second = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11, "ci", "build", "cargo build", &second_log, NEXT_HEAD,
        )])}),
    );

    assert_eq!(second["filed_count"], json!(0));
    let skipped = second["skipped_existing"].as_array().expect("skipped");
    assert!(
        skipped.iter().any(|entry| {
            entry["task_id"] == json!(task_id.clone()) && entry["failure_key"] == first_key
        }),
        "skip_if_open must suppress the second filing under a different commit message: {skipped:?}"
    );
}

/// Captured Coverage / Collect workspace coverage shape from ORB-11340:
/// passing libtest names that contain `error`/`failure`, then `failures:`,
/// the panic, cargo wrappers, and GitHub's generic exit trailer. Collection
/// often drops the `test … FAILED` line with the middle of the log.
fn orb_11340_style_rust_test_log(passing: &[&str], failing: &str) -> String {
    let prefix = |msg: &str| {
        format!(
            "Coverage (informational)\tCollect workspace coverage\t2026-09-06T00:12:19.7226670Z {msg}\n"
        )
    };
    let mut out = String::new();
    out.push_str(&prefix(
        "##[group]Run cargo llvm-cov --workspace --locked --no-report",
    ));
    for name in passing {
        out.push_str(&prefix(&format!("test {name} ... ok")));
    }
    out.push_str(&prefix(""));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(""));
    out.push_str(&prefix(&format!("---- {failing} stdout ----")));
    out.push_str(&prefix(""));
    out.push_str(&prefix(&format!(
        "thread '{failing}' (10411) panicked at crates/orbit-cli/tests/mcp_roundtrip.rs:1416:33:"
    )));
    out.push_str(&prefix(
        "spawn destination-issued command: Os { code: 26, kind: ExecutableFileBusy, message: \"Text file busy\" }",
    ));
    out.push_str(&prefix(
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
    ));
    out.push_str(&prefix(""));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&format!("    {failing}")));
    out.push_str(&prefix(""));
    out.push_str(&prefix(
        "test result: FAILED. 46 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 81.42s",
    ));
    out.push_str(&prefix(""));
    out.push_str(&prefix(
        "error: test failed, to rerun pass `-p orbit-cli --test mcp_roundtrip`",
    ));
    out.push_str(&prefix(
        "error: process didn't exit successfully: `/home/runner/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --tests --manifest-path /home/runner/work/orbit/orbit/Cargo.toml --target-dir /home/runner/work/orbit/orbit/target/llvm-cov-target --workspace --locked` (exit status: 101)",
    ));
    out.push_str(&prefix("##[error]Process completed with exit code 101."));
    out
}

const ORB_11340_PASSING: &[&str] = &[
    "unmanaged_orbit_workspace_env_does_not_bind_mcp",
    "mcp_serve_error_paths_return_tool_errors_and_keep_serving",
    "task_show_is_global_by_default_across_tool_run_and_mcp",
];

const ORB_11340_FAILING: &str = "a_forced_command_ignores_the_command_the_caller_asked_for";

#[test]
fn passing_test_names_with_error_words_are_not_the_signature() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = orb_11340_style_rust_test_log(ORB_11340_PASSING, ORB_11340_FAILING);

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description).to_ascii_lowercase();
    assert!(
        signature.contains(ORB_11340_FAILING) && signature.contains("panicked"),
        "signature must be the panic diagnostic: {signature}"
    );
    assert!(
        !signature.contains("mcp_serve_error_paths")
            && !signature.contains("... ok")
            && !signature.contains("keep_serving"),
        "a passing test whose name contains error must not be the signature: {signature}"
    );
    assert!(
        !signature.contains("process completed"),
        "generic runner trailer must not be the signature: {signature}"
    );
}

#[test]
fn distinct_rust_panics_with_the_same_passing_preamble_keep_distinct_keys() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let foo_log = orb_11340_style_rust_test_log(ORB_11340_PASSING, ORB_11340_FAILING);
    let bar_log = orb_11340_style_rust_test_log(
        ORB_11340_PASSING,
        "another_forced_command_ignores_the_command_the_caller_asked_for",
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "ci", "coverage", "collect coverage", &foo_log, CHECKOUT),
            failure(11, "ci", "coverage", "collect coverage", &bar_log, CHECKOUT),
        ])}),
    );

    assert_eq!(first["filed_count"], json!(2));
    let filed = first["filed"].as_array().expect("filed");
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
    for (task_id, needle) in filed_task_ids(&first).iter().zip([
        ORB_11340_FAILING,
        "another_forced_command_ignores_the_command_the_caller_asked_for",
    ]) {
        let description = runtime
            .get_task(task_id)
            .expect("read filed task")
            .description;
        let signature = signature_line(&description).to_ascii_lowercase();
        assert!(
            signature.contains(needle) && signature.contains("panicked"),
            "each panic must be its own signature: {signature}"
        );
        assert!(
            !signature.contains("mcp_serve_error_paths"),
            "shared passing preamble must not become the signature: {signature}"
        );
    }

    let renamed_preamble = orb_11340_style_rust_test_log(
        &[
            "renamed_mcp_serve_error_paths_return_tool_errors_and_keep_serving",
            "workspace_init_mcp_config_reaches_a_governed_tool_over_the_real_transport",
        ],
        ORB_11340_FAILING,
    );
    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            12,
            "ci",
            "coverage",
            "collect coverage",
            &renamed_preamble,
            NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], filed[0]["failure_key"],
        "the same panic must retain its failure key across passing-test names, run ids, and commits"
    );
}

fn ansi_bold_red(text: &str) -> String {
    format!("\u{1b}[31;1m{text}\u{1b}[0m")
}

fn github_line(job: &str, step: &str, payload: &str) -> String {
    format!("{job}\t{step}\t2026-09-07T07:24:42.8592482Z {payload}\n")
}

/// ORB-11509: nextest cancellation and summary wrap a FAIL line. ANSI styling
/// must not become the signature, and colored/uncolored logs must match.
///
/// `elapsed` is the per-test duration nextest prints in the FAIL line; it
/// differs on every rerun of the same regression.
fn orb_11509_style_nextest_log(colored: bool, failing: &str, elapsed: &str) -> String {
    let job = "Check / Clippy / Test";
    let step = "Run CI guardrails";
    let paint = |text: &str, color: bool| {
        if color {
            ansi_bold_red(text)
        } else {
            text.to_string()
        }
    };
    let mut out = String::new();
    out.push_str(&github_line(job, step, "##[group]Run cargo nextest run"));
    out.push_str(&github_line(
        job,
        step,
        "test mcp_serve_error_paths_return_tool_errors_and_keep_serving ... ok",
    ));
    out.push_str(&github_line(
        job,
        step,
        &format!(
            "{} due to {}: ",
            paint("  Cancelling", colored),
            paint("test failure", colored)
        ),
    ));
    out.push_str(&github_line(job, step, "────────────"));
    out.push_str(&github_line(
        job,
        step,
        &format!(
            "{} [ 177.529s] 2786/4410 tests run: 2785 passed (2 slow), 1 failed, 10 skipped",
            paint("     Summary", colored)
        ),
    ));
    out.push_str(&github_line(
        job,
        step,
        &format!(
            "{} [   {elapsed}] (2786/4410) {} {}",
            paint("        FAIL", colored),
            paint("orbit-cli::output_goldens", colored),
            paint(failing, colored)
        ),
    ));
    out.push_str(&github_line(
        job,
        step,
        "warning: 1624/4410 tests were not run due to test failure (run with --no-fail-fast to run all tests)",
    ));
    out.push_str(&github_line(
        job,
        step,
        &format!("{}: test run failed", paint("error", colored)),
    ));
    out.push_str(&github_line(
        job,
        step,
        "##[error]Process completed with exit code 100.",
    ));
    out
}

/// ORB-11470 / ORB-11467: cargo's colored `error: test failed, to rerun pass`
/// trailer can appear before the panic when the excerpt is a recovered job log.
fn orb_11470_style_macos_log(failing: &str, cargo_before_panic: bool) -> String {
    let job = "macOS Sandbox";
    let step = "Run orbit-exec sandbox tests (real sandbox-exec)";
    let prefix = |payload: &str| github_line(job, step, payload);
    let cargo = format!(
        "{}: test failed, to rerun pass `-p orbit-exec --lib`",
        ansi_bold_red("error")
    );
    let header = format!("---- {failing} stdout ----");
    let panic = format!(
        "thread '{failing}' (14083) panicked at crates/orbit-exec/src/macos_sandbox/tests/compile.rs:454:5:"
    );
    let mut out = String::new();
    out.push_str(&prefix("##[group]Run cargo test -p orbit-exec --locked"));
    out.push_str(&prefix(
        "test macos_sandbox::tests::spawn::spawn_under_macos_sandbox_runs_program_in_provided_cwd ... ok",
    ));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&header));
    if cargo_before_panic {
        out.push_str(&prefix(&cargo));
        out.push_str(&prefix(&panic));
    } else {
        out.push_str(&prefix(&panic));
        out.push_str(&prefix(&cargo));
    }
    out.push_str(&prefix(
        "an explicit denyRead must still outrank the public CA default",
    ));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&format!("    {failing}")));
    out.push_str(&prefix(
        "test result: FAILED. 67 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.57s",
    ));
    out.push_str(&prefix("##[error]Process completed with exit code 101."));
    out
}

/// ORB-11498 / ORB-11502: golden assertion payload quotes github.run.logs help
/// text containing `failed steps`. The failing test name is in the libtest
/// summary list; the panic line is omitted as in a head/tail truncated excerpt.
fn orb_11498_style_golden_log(failing: &str) -> String {
    let job = "Coverage (informational)";
    let step = "Collect workspace coverage";
    let prefix = |payload: &str| github_line(job, step, payload);
    let mut out = String::new();
    out.push_str(&prefix(
        "##[group]Run cargo llvm-cov --workspace --locked --no-report",
    ));
    out.push_str(&prefix(
        "test no_ansi_escapes_under_any_color_configuration ... ok",
    ));
    out.push_str(&prefix(
        r#"        "description": "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence. The source stream is drained incrementally; checkout extraction stops after 8 MiB.""#,
    ));
    out.push_str(&prefix(
        r#"  right: "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence.""#,
    ));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&format!("    {failing}")));
    out.push_str(&prefix(
        "test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.64s",
    ));
    out.push_str(&prefix(
        "error: test failed, to rerun pass `-p orbit-cli --test output_goldens`",
    ));
    out.push_str(&prefix(
        "error: process didn't exit successfully: `/home/runner/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --tests` (exit status: 101)",
    ));
    out.push_str(&prefix("##[error]Process completed with exit code 101."));
    out
}

/// ORB-11513: Wrangler colored `[ERROR]` plus the missing-field diagnostic,
/// then GitHub's generic `The process 'npx' failed with exit code`.
pub(in crate::adapter::engine_host::v2_host) fn orb_11513_style_wrangler_log(
    title: &str,
    detail: &str,
) -> String {
    let job = "Publish to Cloudflare Pages";
    let step = "Deploy static site";
    let prefix = |payload: &str| github_line(job, step, payload);
    let wrangler_error = format!(
        "\u{1b}[31m✘ \u{1b}[41;31m[\u{1b}[41;97mERROR\u{1b}[41;31m]\u{1b}[0m \u{1b}[1m{title}:\u{1b}[0m"
    );
    let mut out = String::new();
    out.push_str(&prefix(
        "##[group]Run cloudflare/wrangler-action@ebbaa1584979971c8614a24965b4405ff95890e0",
    ));
    out.push_str(&prefix("[command]/usr/local/bin/npm i wrangler@4.129.0"));
    out.push_str(&prefix(
        "[command]/usr/local/bin/npx --no-install wrangler --version",
    ));
    out.push_str(&prefix(
        "[command]/usr/local/bin/npx wrangler pages deploy dist --project-name=orbit-website --branch=main --commit-hash=a93caa13890764380e184d996fa709b1bcbe278c",
    ));
    out.push_str(&prefix(&wrangler_error));
    out.push_str(&prefix(&format!("    - {detail}")));
    out.push_str(&prefix(
        "##[error]The process '/usr/local/bin/npx' failed with exit code 1",
    ));
    out.push_str(&prefix("##[error]🚨 Action failed"));
    out
}

#[test]
fn colored_and_uncolored_nextest_cancellation_share_the_fail_identity() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "plain_and_json_forms_match_their_goldens";
    let colored = orb_11509_style_nextest_log(true, FAILING, "1.399s");
    let plain = orb_11509_style_nextest_log(false, FAILING, "1.399s");

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10, "CI", "Check / Clippy / Test", "Run CI guardrails", &colored, CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let task_id = filed_task_ids(&first).remove(0);
    let signature = signature_line(
        &runtime
            .get_task(&task_id)
            .expect("read filed task")
            .description,
    )
    .to_ascii_lowercase();
    assert!(
        signature.contains(FAILING) && signature.contains("fail"),
        "nextest FAIL line must be the signature: {signature}"
    );
    assert!(
        !signature.contains("cancelling")
            && !signature.contains("process completed")
            && !signature.contains("test run failed")
            && !signature.contains('\u{1b}'),
        "cancellation, cargo trailer, and ANSI must not be the signature: {signature}"
    );
    assert!(
        runtime
            .get_task(&task_id)
            .expect("read filed task")
            .description
            .contains("Cancelling"),
        "raw colored excerpt must remain in the description"
    );

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11, "CI", "Check / Clippy / Test", "Run CI guardrails", &plain, NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], first["filed"][0]["failure_key"],
        "colored and uncolored nextest FAIL logs must share a failure key"
    );
}

/// A colored log can reach the sweep with an escape sequence cut short — the
/// stripper must not split the multi-byte character that follows it.
#[test]
fn a_truncated_escape_before_a_multibyte_character_still_files() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = format!(
        "{}{}{}",
        github_line("build", "cargo test", "##[group]Run cargo nextest run"),
        github_line("build", "cargo test", "\u{1b}────────────"),
        github_line(
            "build",
            "cargo test",
            "    FAIL [   1.399s] orbit-core truncated_escape_case",
        ),
    );

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description).to_ascii_lowercase();
    assert!(
        signature.contains("truncated_escape_case"),
        "the FAIL identity must survive a truncated escape: {signature}"
    );
}

#[test]
fn nextest_fail_durations_do_not_fragment_one_regression() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "plain_and_json_forms_match_their_goldens";
    let first_run = orb_11509_style_nextest_log(true, FAILING, "1.399s");
    let rerun = orb_11509_style_nextest_log(true, FAILING, "2.004s");

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10, "CI", "Check / Clippy / Test", "Run CI guardrails", &first_run, CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11, "CI", "Check / Clippy / Test", "Run CI guardrails", &rerun, NEXT_HEAD,
        )])}),
    );
    assert_eq!(
        repeated["filed_count"],
        json!(0),
        "a rerun of the same test must not file a second task"
    );
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], first["filed"][0]["failure_key"],
        "the per-test duration must not change the failure key"
    );
}

#[test]
fn distinct_nextest_fail_lines_in_the_same_job_keep_distinct_keys() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let foo =
        orb_11509_style_nextest_log(true, "plain_and_json_forms_match_their_goldens", "1.399s");
    let bar = orb_11509_style_nextest_log(false, "another_golden_does_not_match", "2.004s");

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "CI", "Check / Clippy / Test", "Run CI guardrails", &foo, CHECKOUT),
            failure(11, "CI", "Check / Clippy / Test", "Run CI guardrails", &bar, CHECKOUT),
        ])}),
    );
    assert_eq!(output["filed_count"], json!(2));
    let filed = output["filed"].as_array().expect("filed");
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
}

#[test]
fn cargo_test_failed_trailer_does_not_outrank_the_panic() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "macos_sandbox::tests::compile::compiled_codex_profile_reads_public_ca_material_but_not_private_credentials";
    let trailer_first = orb_11470_style_macos_log(FAILING, true);
    let panic_first = orb_11470_style_macos_log(FAILING, false);

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "macOS Platform",
            "macOS Sandbox",
            "Run orbit-exec sandbox tests (real sandbox-exec)",
            &trailer_first,
            CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let signature = signature_line(
        &runtime
            .get_task(&filed_task_ids(&first)[0])
            .expect("read filed task")
            .description,
    )
    .to_ascii_lowercase();
    assert!(
        signature.contains(FAILING) && signature.contains("panicked"),
        "panic must outrank the cargo trailer: {signature}"
    );
    assert!(
        !signature.contains("to rerun pass") && !signature.contains("process completed"),
        "cargo/github wrappers must not be the signature: {signature}"
    );

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11,
            "macOS Platform",
            "macOS Sandbox",
            "Run orbit-exec sandbox tests (real sandbox-exec)",
            &panic_first,
            NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"],
        first["filed"][0]["failure_key"]
    );
}

#[test]
fn golden_assertion_help_text_does_not_outrank_the_failing_test_name() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "plain_and_json_forms_match_their_goldens";
    let log = orb_11498_style_golden_log(FAILING);

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "CI",
            "Coverage (informational)",
            "Collect workspace coverage",
            &log,
            CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let task_id = filed_task_ids(&first).remove(0);
    let description = runtime
        .get_task(&task_id)
        .expect("read filed task")
        .description;
    let signature = signature_line(&description).to_ascii_lowercase();
    assert!(
        signature.contains(FAILING),
        "listed golden test name must be the signature: {signature}"
    );
    assert!(
        !signature.contains("failed steps")
            && !signature.contains("bounded excerpt")
            && !signature.contains("to rerun pass"),
        "assertion payload and cargo trailer must not be the signature: {signature}"
    );
    assert!(
        description.contains("failed steps by default"),
        "raw assertion payload must remain in the excerpt"
    );

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11,
            "CI",
            "Coverage (informational)",
            "Collect workspace coverage",
            &log,
            NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
}

#[test]
fn wrangler_error_outranks_generic_npx_process_failed() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let missing_name = orb_11513_style_wrangler_log(
        "Running configuration file validation for Pages",
        "Missing top-level field \"name\" in configuration file.",
    );
    let missing_pages = orb_11513_style_wrangler_log(
        "Failed to publish your Function",
        "Pages build output directory is missing.",
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "Website",
            "Publish to Cloudflare Pages",
            "Deploy static site",
            &missing_name,
            CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let signature = signature_line(
        &runtime
            .get_task(&filed_task_ids(&first)[0])
            .expect("read filed task")
            .description,
    )
    .to_ascii_lowercase();
    assert!(
        signature.contains("configuration file validation")
            || signature.contains("missing top-level field"),
        "wrangler diagnostic must outrank npx process-failed: {signature}"
    );
    assert!(
        !signature.contains("usr/local/bin/npx") && !signature.contains("action failed"),
        "generic process/action trailers must not be the signature: {signature}"
    );

    let mut second_failure = failure(
        11,
        "Website",
        "Publish to Cloudflare Pages",
        "Deploy static site",
        &missing_pages,
        NEXT_HEAD,
    );
    second_failure["event_reported_head_sha"] = json!(NEXT_HEAD);
    second_failure["current_ref_head_sha"] = json!(NEXT_HEAD);
    let second = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![second_failure])}),
    );
    assert_eq!(second["filed_count"], json!(1));
    assert_ne!(
        first["filed"][0]["failure_key"], second["filed"][0]["failure_key"],
        "distinct wrangler diagnostics in the same job must not collapse"
    );
}

#[test]
fn generic_only_truncated_excerpt_labels_step_name_fallback() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = format!(
        "{}{}{}",
        github_line("build", "cargo test", "##[group]Run cargo test"),
        github_line(
            "build",
            "cargo test",
            "error: test failed, to rerun pass `-p orbit-exec --lib`",
        ),
        github_line(
            "build",
            "cargo test",
            "##[error]Process completed with exit code 101.",
        ),
    );

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description);
    assert!(
        signature.contains("step-name fallback"),
        "generic-only excerpt must label fallback uncertainty: {signature}"
    );
    assert!(
        description.contains("test failed, to rerun pass")
            && description.contains("Process completed with exit code 101."),
        "raw generic trailers must remain in the excerpt:\n{description}"
    );
}
