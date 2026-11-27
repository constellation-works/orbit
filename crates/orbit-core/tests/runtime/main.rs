//! The composed runtime through its public surface: dispatch admission, the
//! distributed drain, `os:` and `ui` tag routing, retired deterministic stubs, final
//! recovery, repeated rebase recovery, relation auto-close, PR closure on
//! terminal decisions and requeues, settlement from a delivery PR merged
//! outside Orbit, orphaned delivery adoption, plugin inspection, the sandbox
//! opt-out, the agent sandbox mask and cold lexical search hydration.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`). Declare every top-level file
//! (`<name>.rs`, or a `<name>/mod.rs` directory) below; `every_top_level_module_is_declared`
//! fails and names a missing one; a `mod` line inside a comment or string does not
//! count. A build script is not used: it would rebuild
//! `orbit-core` and every dependent crate on any edit under this directory.

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// `scenario_table!` is a `macro_rules!` that most modules call by textual scope, which
// reaches only what follows its definition, so its path re-export and every `mod` line
// stay below it.
#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "`macro_rules!` textual scope requires `scenario_table` before the modules that call it"
)]

#[path = "../support/admission.rs"]
mod admission_test_support;
#[path = "../support/external_merge_fixture.rs"]
mod external_merge_fixture;
#[path = "../support/git.rs"]
mod git_test_support;
#[path = "../support/task_params.rs"]
mod task_params_test_support;

orbit_common::isolate_test_process!();

/// Keep related permutations in one table with isolated fixture state.
macro_rules! scenario_table {
    ($table:ident, $($row:ident),+ $(,)?) => {
        #[test]
        fn $table() {
            crate::scenarios::run(module_path!(), stringify!($table), &[
                $((stringify!($row), $row as fn() -> _)),+
            ]);
        }
    };
}
pub(crate) use scenario_table;

// Every top-level module in this directory, declared by hand. The guard test below
// fails when a top-level file or `<name>/mod.rs` is missing from this list. They follow
// `scenario_table`, so each module can use the macro.
mod actor_identity;
mod admission_audit;
mod admission_forecast;
mod admitted_crew_write_back;
mod agent_sandbox_mask;
mod application_authority;
mod artifact_tools;
mod attention_observation;
mod attention_provider;
mod attention_wait;
mod audit_commit_report;
mod auto_task_job_steps;
mod auto_task_update;
mod automation_checkpoints;
mod backlog_resource_fit;
mod backlog_review_modes;
mod baseline_hold_refresh;
mod baseline_hold_tick;
mod baseline_red_repair;
mod blocked_recheck;
mod bundle_validation;
mod cascade_outcomes;
mod child_wait_audit;
mod ci_retry_journal_wait;
mod claimed_model_access;
mod claude_worker_credential;
mod codebase_regressions;
mod codebase_related;
mod config_settings;
mod crew_accounts;
mod crew_memory;
mod decision_channel;
mod decision_precedent;
mod decision_raise;
mod decision_requested;
mod default_catalog;
mod delivery_holds;
mod dispatch_admission;
mod distributed_drain;
mod drain_approval;
mod drain_cancel;
mod drain_crew_pools;
mod drain_extend;
mod drain_live_agents;
mod escalation_triage_crews;
mod experiment_lifecycle;
mod experiment_readout;
mod final_recovery;
mod final_recovery_cards;
mod final_recovery_mechanical;
mod fixture_template;
mod flaky_validation;
mod footprint_hold;
mod forecast_accuracy;
mod forge_hold_resume;
mod friction_stats_observation;
mod git_protection;
mod granted_mutation_proposals;
mod granted_tools;
mod granted_tools_window;
mod held_child_guard;
mod host_labels;
mod host_os_executor;
mod host_os_routing;
mod host_resources;
mod host_ui_routing;
mod human_crew_work;
mod implement_blocker;
mod invocation_metrics;
mod job_failure_streak;
mod job_finalization;
mod job_run_records;
mod light_delivery;
mod local_provider_holds;
mod local_route_before_pr;
mod mid_run_steering;
mod muse_state_grant;
mod named_accepted_history;
mod notice_authority;
mod operator_ship_override;
mod origin_fetch_hold;
mod orphan_adoption;
mod pilot_warning_cards;
mod plugin_delivery_worktree;
mod plugin_inspection;
mod pr_forge_admission;
mod pr_infrastructure;
mod pre_family_tool_names;
mod process_isolation;
mod promotion_commit;
mod provider_failure_hold;
mod provider_limit_admission;
mod provider_limit_budget;
mod provider_limit_hold;
mod provider_roots;
mod purpose_crews;
mod rebase_recovery_attempts;
mod recovery_fixture;
mod recovery_provider_auth;
mod relation_auto_close;
mod resident_authority;
mod resident_cycle;
mod resident_digest;
mod resident_metrics;
mod resource_profiles;
mod resume_ownership;
mod retired_stubs;
mod review_admission_shape;
mod review_baseline_hold;
mod review_before_landing;
mod review_budget_hold;
mod review_continuation;
mod review_evidence_fulfilment;
mod review_evidence_paths;
mod review_evidence_writers;
mod review_failed_reviewer_hold;
mod review_gate_audit;

mod review_gate_multi_package;
mod review_held_resume;
mod review_ledger;
mod review_owner_host_evidence;
mod review_record_ids;
mod review_report_revisions;
mod review_run_option;
mod reviewable_lines;
mod routine_collision_deadline;
mod run_cli_invocations;
mod run_failure_diagnostic;
mod run_history;
mod run_outcome_contract;
mod sandbox_off;
mod scenarios;
mod scheduled_tasks;
mod script_env_pass;
mod script_file_inputs;
mod script_grant_request;
mod script_roots;
mod search_hydration;
mod security_alert_sweep;
mod session_events;
mod shared_root_identity;
mod standing_drain_keeper;
mod step_recovery;
mod store_gc;
mod system_work_crews;
mod tag_instructions;
mod task_context_comment_budget;
mod task_delivery;
mod task_external_merge;
mod task_lint_paths;
mod task_pilot;
mod task_pr_closure;
mod task_selector_hints;
mod task_update;
mod task_workspace_projection;
mod upgrade_pending_requeue;
mod upgrade_resume;
mod usage_pressure_draws;
mod vision_probe;
mod worker_exit_diagnostic;
mod worker_fixture;
mod workspace_seed;
mod worktree_reclaim;

// `readiness/` has no `mod.rs`; its one file is mounted here by path.
#[path = "readiness/population.rs"]
mod readiness_population;

/// A top-level file that no `mod` line declares is never compiled, so its tests
/// would silently not run. Fail and name it instead.
#[test]
fn every_top_level_module_is_declared() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime");
    let missing = unregistered_top_level_modules(&dir, include_str!("main.rs"));
    assert!(
        missing.is_empty(),
        "add `mod <name>;` to crates/orbit-core/tests/runtime/main.rs for: {missing:?}"
    );
}

/// A fixture file: its name beside the crate root, and its text.
type FixtureFile = (&'static str, &'static str);

/// Runs the registration check over a fixture directory holding `files`, with
/// `main_source` as the crate root.
fn unregistered_in_fixture(main_source: &str, files: &[FixtureFile]) -> Vec<String> {
    let root = tempfile::tempdir().expect("create runtime fixture");
    for (name, text) in files {
        std::fs::write(root.path().join(name), text).expect("write runtime fixture file");
    }
    unregistered_top_level_modules(root.path(), main_source)
}

/// Rust compiles a file only through a declaration it reads. A `mod` line in a block
/// or line comment, or inside a string, declares nothing, so the file stays
/// unregistered and its tests would silently not run.
#[test]
fn commented_or_quoted_module_declaration_is_unregistered() {
    let activity = [("activity.rs", "")];
    let cases: [(&str, &[FixtureFile], &[&str]); 6] = [
        ("/*\nmod activity;\n*/\n", &activity, &["activity"]),
        ("// mod activity;\n", &activity, &["activity"]),
        (
            "/* outer /* inner */ mod activity; */\n",
            &activity,
            &["activity"],
        ),
        (
            "const NOTE: &str = \"mod activity;\";\n",
            &activity,
            &["activity"],
        ),
        (
            "mod host;\n",
            &[
                (
                    "host.rs",
                    "/*\n#[path = \"activity.rs\"]\nmod activity;\n*/\n",
                ),
                ("activity.rs", ""),
            ],
            &["activity"],
        ),
        (
            "",
            &[
                ("host.rs", "#[path = \"activity.rs\"]\nmod activity;\n"),
                ("activity.rs", ""),
            ],
            &["activity", "host"],
        ),
    ];
    for (main_source, files, expected) in cases {
        assert_eq!(
            unregistered_in_fixture(main_source, files),
            expected,
            "crate root {main_source:?}"
        );
    }
}

/// A declaration Rust reads still registers its file, and so does a `#[path]` mount
/// in a registered sibling.
#[test]
fn live_module_declaration_and_sibling_mount_are_registered() {
    let activity = [("activity.rs", "")];
    let cases: [(&str, &[FixtureFile]); 3] = [
        ("mod activity;\n", &activity),
        (
            "#[path = \"activity.rs\"]\npub(crate) mod activity;\n",
            &activity,
        ),
        (
            "mod host;\n",
            &[
                ("host.rs", "#[path = \"activity.rs\"]\nmod activity;\n"),
                ("activity.rs", ""),
            ],
        ),
    ];
    for (main_source, files) in cases {
        assert_eq!(
            unregistered_in_fixture(main_source, files),
            Vec::<String>::new(),
            "crate root {main_source:?}"
        );
    }
}

/// Rust reaches the ordinary sibling through the explicitly mounted hidden source.
#[test]
fn mounted_dot_file_reaches_sibling() {
    let root = tempfile::tempdir().expect("create runtime fixture");
    let source = "#[path = \".bridge.rs\"] mod bridge;\n";
    std::fs::write(root.path().join("main.rs"), source).expect("write fixture root");
    std::fs::write(
        root.path().join(".bridge.rs"),
        "#[path = \"activity.rs\"] mod activity;\n",
    )
    .expect("write hidden mount");
    std::fs::write(
        root.path().join("activity.rs"),
        "#[test] fn reached_real_module() {}\n",
    )
    .expect("write sibling");
    let binary = root.path().join("fixture");
    let compiled = std::process::Command::new("rustc")
        .args(["--edition=2024", "--test"])
        .arg(root.path().join("main.rs"))
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("run rustc");
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let listing = std::process::Command::new(binary)
        .arg("--list")
        .output()
        .expect("list fixture tests");
    assert!(listing.status.success());
    assert!(
        String::from_utf8_lossy(&listing.stdout)
            .contains("bridge::activity::reached_real_module: test")
    );
    assert_eq!(
        unregistered_top_level_modules(root.path(), source),
        Vec::<String>::new()
    );
}

/// A dot-prefixed file names no module, so a leftover one beside the crate root, such
/// as the `.generated_modules.rs` a removed build script wrote, is not reported. A
/// plain unregistered file still is.
#[test]
fn dot_prefixed_leftover_is_not_a_module() {
    // Compile the actual guard and its scanner into a test binary whose manifest
    // directory and crate root belong to the fixture, then run the guard itself.
    let source = include_str!("main.rs");
    let guard = source
        .split_once("#[test]\nfn every_top_level_module_is_declared()")
        .expect("guard entry point")
        .1
        .split_once("/// A fixture file")
        .expect("end of guard")
        .0;
    let scanner = source
        .rsplit_once("fn unregistered_top_level_modules(")
        .expect("scanner")
        .1;
    for (declaration, should_pass) in [("mod activity;\n", true), ("", false)] {
        let root = tempfile::tempdir().expect("create runtime fixture");
        let dir = root.path().join("tests/runtime");
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        std::fs::write(dir.join(".generated_modules.rs"), "mod actor_identity;\n")
            .expect("write leftover");
        std::fs::write(dir.join("activity.rs"), "").expect("write activity");
        let fixture = format!(
            "{declaration}#[test]\nfn every_top_level_module_is_declared(){guard}\nfn unregistered_top_level_modules({scanner}"
        );
        let main = dir.join("main.rs");
        std::fs::write(&main, fixture).expect("write fixture root");
        let binary = root.path().join("guard-test");
        let compiled = std::process::Command::new("rustc")
            .args(["--edition=2024", "--test"])
            .env("CARGO_MANIFEST_DIR", root.path())
            .arg(&main)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("compile guard fixture");
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let output = std::process::Command::new(binary)
            .arg("every_top_level_module_is_declared")
            .output()
            .expect("run guard fixture");
        assert_eq!(
            output.status.success(),
            should_pass,
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        if !should_pass {
            assert!(String::from_utf8_lossy(&output.stdout).contains("\"activity\""));
        }
    }
}

/// Names of the top-level test files and `mod.rs` directories that Rust never
/// compiles: no uncommented `mod <name>;` at the crate root of `main_source` reaches
/// them, directly or through a `#[path = "<name>.rs"]` mount in a compiled file.
/// Dot-prefixed entries are not reporting candidates, but explicitly mounted Rust
/// files are indexed so their mounts can reach ordinary siblings.
fn unregistered_top_level_modules(dir: &std::path::Path, main_source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut files = std::collections::HashMap::new();
    for entry in std::fs::read_dir(dir)
        .expect("read tests/runtime")
        .flatten()
    {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !file_name.starts_with('.') && path.join("mod.rs").is_file() {
                names.push(file_name);
            }
        } else if let Some(stem) = file_name.strip_suffix(".rs").filter(|s| *s != "main") {
            files.insert(stem.to_owned(), path);
            if !file_name.starts_with('.') {
                names.push(stem.to_owned());
            }
        }
    }
    // The crate root's declarations, then the files they mount, transitively. A
    // sibling's plain `mod` lines name nested paths, not files beside it, so only its
    // mounts reach another top-level file.
    let root = registration(main_source);
    let mut pending = root.declared;
    pending.extend(
        root.mounts
            .iter()
            .filter_map(|path| mounted_name(path))
            .map(str::to_owned),
    );
    let mut reached = std::collections::BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !reached.insert(name.clone()) {
            continue;
        }
        if let Some(path) = files.get(&name) {
            let text = std::fs::read_to_string(path).expect("read runtime test file");
            let file = registration(&text);
            pending.extend(
                file.mounts
                    .iter()
                    .filter_map(|path| mounted_name(path))
                    .map(str::to_owned),
            );
        }
    }
    names.sort();
    names
        .into_iter()
        .filter(|name| !reached.contains(name))
        .collect()
}

/// The module declarations a source file makes at its top level, as Rust reads
/// them: comments and literals are not code.
#[derive(Default)]
struct Registration {
    /// Names of `mod <name>;` items.
    declared: Vec<String>,
    /// Values of the `#[path = "..."]` attributes on `mod <name>;` items.
    mounts: Vec<String>,
}

/// The top-level module that a `#[path]` value mounts: `<name>.rs` or
/// `<name>/mod.rs`, directly under the test directory.
fn mounted_name(path: &str) -> Option<&str> {
    let stem = path
        .strip_suffix("/mod.rs")
        .or_else(|| path.strip_suffix(".rs"))?;
    (!stem.contains('/')).then_some(stem)
}

/// The `mod` items at the top level of `source`, not inside a block or another item.
fn registration(source: &str) -> Registration {
    let toks = tokenize(source);
    let mut reg = Registration::default();
    let mut depth = 0_usize;
    let mut mount: Option<String> = None;
    for (i, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Punct('{') => {
                depth += 1;
                mount = None;
            }
            Tok::Punct('}') => {
                depth = depth.saturating_sub(1);
                mount = None;
            }
            Tok::Punct(';') => mount = None,
            Tok::Punct('#') if depth == 0 => mount = path_attribute(&toks[i..]).or(mount),
            Tok::Word(word) if word == "mod" && depth == 0 => {
                if let [Tok::Word(name), Tok::Punct(';'), ..] = &toks[i + 1..] {
                    match mount.take() {
                        Some(path) => reg.mounts.push(path),
                        None => reg.declared.push(name.clone()),
                    }
                }
            }
            _ => {}
        }
    }
    reg
}

/// The value of a `#[path = "..."]` attribute that opens `toks`, if one does.
fn path_attribute(toks: &[Tok]) -> Option<String> {
    match toks {
        [
            Tok::Punct('#'),
            Tok::Punct('['),
            Tok::Word(attr),
            Tok::Punct('='),
            Tok::Str(path),
            Tok::Punct(']'),
            ..,
        ] if attr == "path" => Some(path.clone()),
        _ => None,
    }
}

/// A lexical token. `Str` keeps the text of a string literal; other literals and all
/// comments are dropped.
enum Tok {
    Word(String),
    Str(String),
    Punct(char),
}

/// Splits Rust source into tokens. Line comments and nested block comments are
/// dropped. Plain, byte and raw string literals and char literals are read whole, so
/// a `mod` inside any of them is never code.
fn tokenize(source: &str) -> Vec<Tok> {
    let chars: Vec<char> = source.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut toks = Vec::new();
    let mut i = 0;
    while let Some(c) = at(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == Some('/') {
            while at(i).is_some_and(|c| c != '\n') {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            let mut depth = 0_usize;
            while let Some(c) = at(i) {
                if c == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    i += 2;
                } else if c == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == '"' {
            let (text, next) = quoted(&chars, i + 1);
            toks.push(Tok::Str(text));
            i = next;
        } else if c == '\'' {
            if at(i + 1) == Some('\\') {
                // An escaped char literal such as '\n' or '\u{1F600}'.
                i += 3;
                while at(i).is_some_and(|c| c != '\'') {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == Some('\'') {
                i += 3;
            } else {
                // A lifetime or a loop label.
                toks.push(Tok::Punct('\''));
                i += 1;
            }
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while at(i).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let mut hashes = 0;
            while at(i + hashes) == Some('#') {
                hashes += 1;
            }
            if matches!(word.as_str(), "r" | "br") && at(i + hashes) == Some('"') {
                let (text, next) = raw_quoted(&chars, i + hashes + 1, hashes);
                toks.push(Tok::Str(text));
                i = next;
            } else if word == "b" && at(i) == Some('"') {
                let (text, next) = quoted(&chars, i + 1);
                toks.push(Tok::Str(text));
                i = next;
            } else {
                toks.push(Tok::Word(word));
            }
        } else {
            toks.push(Tok::Punct(c));
            i += 1;
        }
    }
    toks
}

/// Reads a plain string literal whose body starts at `start`. Returns its text and
/// the index just past the closing quote.
fn quoted(chars: &[char], start: usize) -> (String, usize) {
    let mut text = String::new();
    let mut i = start;
    while let Some(&c) = chars.get(i) {
        i += 1;
        match c {
            '"' => break,
            '\\' => {
                text.push(c);
                if let Some(&escaped) = chars.get(i) {
                    text.push(escaped);
                }
                i += 1;
            }
            _ => text.push(c),
        }
    }
    (text, i)
}

/// Reads a raw string literal whose body starts at `start` and closes with a quote
/// followed by `hashes` `#`s. Returns its text and the index just past the closer.
fn raw_quoted(chars: &[char], start: usize, hashes: usize) -> (String, usize) {
    let closes_at = |j: usize| {
        chars.get(j) == Some(&'"') && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#'))
    };
    let mut end = start;
    while end < chars.len() && !closes_at(end) {
        end += 1;
    }
    (chars[start..end].iter().collect(), end + 1 + hashes)
}
