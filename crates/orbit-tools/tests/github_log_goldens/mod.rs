//! Public GitHub CLI parsing and recovery contracts over sanitized fixtures.
use std::io::Write;
use std::path::{Path, PathBuf};

use orbit_tools::github_cli as gh;
#[cfg(unix)]
use orbit_tools::{ToolContext, ToolRegistry};
use serde_json::{Value, json};
#[cfg(unix)]
use tempfile::TempDir;

fn directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/github_log_goldens")
}
fn read(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(directory().join(name)).unwrap()).unwrap()
}
fn source(parts: &Value) -> String {
    parts
        .as_array()
        .unwrap()
        .iter()
        .map(|part| {
            if let Some(text) = part.as_str() {
                text.to_string()
            } else {
                part["text"]
                    .as_str()
                    .unwrap()
                    .repeat(part["repeat"].as_u64().unwrap() as usize)
            }
        })
        .collect()
}
fn progress(message: std::fmt::Arguments<'_>) {
    writeln!(std::io::stderr().lock(), "{message}").unwrap();
}

fn golden(name: &str, cases: &Value, mut render: impl FnMut(&Value) -> Value) {
    let update = std::env::var_os("ORBIT_UPDATE_LOG_GOLDENS").is_some();
    let expected = if update {
        json!({})
    } else {
        read(&format!("{name}.golden.json"))
    };
    let filter = std::env::var("ORBIT_LOG_GOLDEN_CASE").ok();
    assert!(
        !update || filter.is_none(),
        "filtered replay cannot regenerate goldens"
    );
    let mut outputs = serde_json::Map::new();
    for case in cases.as_array().unwrap() {
        let key = case["name"].as_str().unwrap();
        if filter.as_ref().is_some_and(|filter| key != filter) {
            continue;
        }
        progress(format_args!("golden row start: {name}/{key}"));
        let started = std::time::Instant::now();
        let actual = render(case);
        if !update {
            assert_eq!(
                actual, expected[key],
                "{name} fixture {key}; regenerate with make goldens UPDATE=1"
            );
        }
        assert!(
            outputs.insert(key.to_string(), actual).is_none(),
            "duplicate fixture {key}"
        );
        progress(format_args!(
            "golden row complete: {name}/{key} elapsed_seconds={:.6}",
            started.elapsed().as_secs_f64()
        ));
    }
    if update {
        std::fs::write(
            directory().join(format!("{name}.golden.json")),
            serde_json::to_string_pretty(&outputs).unwrap() + "\n",
        )
        .unwrap();
    } else if filter.is_some() {
        assert_eq!(
            outputs.len(),
            1,
            "filtered replay must run exactly one fixture"
        );
    } else {
        assert_eq!(
            outputs.len(),
            expected.as_object().unwrap().len(),
            "all golden cases must run"
        );
    }
}
fn streamed(log: gh::StreamedLog) -> Value {
    json!({"log": log.text, "truncated": log.truncated, "total_bytes": log.total_bytes,
        "returned_bytes": log.returned_bytes, "source_complete": log.source_complete,
        "diagnostic_unit": log.diagnostic, "failure_regions": log.failure_regions,
        "checkout_commits": log.checkout_evidence.commits, "checkout_evidence": log.checkout_evidence.lines,
        "checkout_evidence_complete": log.checkout_evidence.complete,
        "checkout_evidence_scanned_bytes": log.checkout_evidence.scanned_bytes,
        "checkout_evidence_source_truncated": log.checkout_evidence.source_truncated,
        "checkout_evidence_display_truncated": log.checkout_evidence.display_truncated})
}

#[test]
fn streamed_log_fixture_goldens() {
    golden("streamed", &read("streamed.json"), |case| {
        let preparation = std::time::Instant::now();
        let mut raw = source(&case["parts"]);
        if case["conflicting_column"] == true {
            raw = raw
                .lines()
                .enumerate()
                .map(|(index, line)| {
                    format!(
                        "{}\tTests\t{line}\n",
                        if index == 500 { "Other" } else { "CI" }
                    )
                })
                .collect();
        }
        let cap = case["max_bytes"].as_u64().unwrap() as usize;
        let evidence = case["max_evidence_lines"].as_u64().unwrap() as usize;
        let chunks = case["chunks"]
            .as_array()
            .map(|chunks| {
                chunks
                    .iter()
                    .map(|chunk| chunk.as_u64().unwrap() as usize)
                    .collect::<Vec<_>>()
            })
            .unwrap_or(vec![1, 7, 4096]);
        let mut output = None;
        progress(format_args!(
            "streamed source: {} bytes={} preparation_seconds={:.6}",
            case["name"].as_str().unwrap(),
            raw.len(),
            preparation.elapsed().as_secs_f64()
        ));
        for chunk in chunks {
            progress(format_args!(
                "streamed collector start: {} chunk={chunk}",
                case["name"].as_str().unwrap()
            ));
            let pushing = std::time::Instant::now();
            let mut collector = if case["tail_weighted"] == true {
                gh::StreamedLogCollector::tail_weighted(cap, evidence)
            } else {
                gh::StreamedLogCollector::new(cap, evidence)
            };
            for bytes in raw.as_bytes().chunks(chunk) {
                collector.push(bytes);
            }
            let push_seconds = pushing.elapsed().as_secs_f64();
            let finishing = std::time::Instant::now();
            let actual = streamed(collector.finish());
            progress(format_args!(
                "streamed collector complete: {} chunk={chunk} pushes={} push_seconds={push_seconds:.6} finish_seconds={:.6}",
                case["name"].as_str().unwrap(),
                raw.len().div_ceil(chunk),
                finishing.elapsed().as_secs_f64()
            ));
            if let Some(previous) = &output {
                assert_eq!(
                    &actual, previous,
                    "chunk boundary must not change {} at {chunk} bytes",
                    case["name"]
                );
            }
            output = Some(actual);
        }
        output.unwrap()
    });
}

#[test]
fn mid_log_nextest_blocks_survive_independent_command_and_display_limits() {
    // F2026-10-271: nextest reports a failure when its test finishes, long
    // before the final summary in a multi-megabyte CI log.
    let block = "FAIL [ 0.123s] (2021/2810) suite failing_test\n──────── STDOUT ────────\noutput from the failing test\n──────── STDERR ────────\nthread 'failing_test' panicked at tests/sample.rs:12:3:\nexpected a completed operation\n";
    let padding = "PASS [ 0.001s] suite passing_test\n".repeat(32_768);
    for (prefix, suffix) in [
        ("##[group]Run cargo nextest run\n##[endgroup]\n".to_string(), "##[error]Process completed with exit code 100.\n"),
        (String::new(), ""),
        (format!("{}\n##[group]Run cargo nextest run\n##[endgroup]\n", "unrelated".repeat(4096)), "##[error]Process completed with exit code 100.\n"),
        ("##[group]Run earlier\n##[endgroup]\n##[error]Process completed with exit code 1.\n##[group]Run cargo nextest run\n##[endgroup]\n".to_string(), "##[error]Process completed with exit code 100.\n"),
    ] {
        let raw = format!("{prefix}{padding}{block}{padding}Summary [ 1.0s] 2810 tests run: 1 failed\n{suffix}");
        assert!(raw.len() > 2_000_000);
        for chunk in [7, 4096, raw.len()] {
            let mut collector = gh::StreamedLogCollector::new(262_144, 40);
            for bytes in raw.as_bytes().chunks(chunk) {
                collector.push(bytes);
            }
            let log = collector.finish();
            assert!(log.truncated && log.source_complete);
            assert!(log.text.contains(block), "F2026-10-271: retain the mid-log panic");
            assert!(log.returned_bytes <= 262_144 + 65_536 + 256);
            let regions = log.failure_regions.unwrap();
            let range = &regions["byte_ranges"][0];
            let start = range["start_byte"].as_u64().unwrap() as usize;
            let end = range["end_byte"].as_u64().unwrap() as usize;
            assert_eq!(&raw[start..end], block);
            assert_eq!(regions["text"], block);
            assert_eq!(regions["selection_complete"], true);
        }
    }

    let huge = format!(
        "{block}{}",
        "captured payload with unicode λ\n".repeat(4096)
    );
    let second = block.replace("failing_test", "second_test");
    let raw = format!("{padding}{huge}{second}{padding}");
    let mut collector = gh::StreamedLogCollector::new(128, 40);
    collector.push(raw.as_bytes());
    let log = collector.finish();
    let regions = log.failure_regions.unwrap();
    assert_eq!(regions["byte_ranges"].as_array().unwrap().len(), 2);
    assert!(log.text.contains("output block cut;"));
    assert!(
        log.text.contains(&second),
        "an oversized first failure must leave room for the next"
    );
    assert!(regions["text"].as_str().unwrap().len() <= 65_536);
    assert!(regions["byte_ranges"][0]["omitted_bytes"].as_u64().unwrap() > 0);

    for count in [8, 129] {
        let raw = if count == 8 {
            huge.repeat(count)
        } else {
            block.repeat(count)
        };
        let mut collector = gh::StreamedLogCollector::new(128, 40);
        for bytes in raw.as_bytes().chunks(4096) {
            collector.push(bytes);
        }
        let log = collector.finish();
        let regions = log.failure_regions.unwrap();
        assert!(regions["text"].as_str().unwrap().len() <= 65_536);
        assert_eq!(regions["failure_anchor_count"], count);
        if count == 8 {
            assert_eq!(regions["byte_ranges"].as_array().unwrap().len(), count);
            assert_eq!(regions["selection_complete"], true);
        } else {
            assert_eq!(regions["byte_ranges"].as_array().unwrap().len(), 128);
            assert_eq!(
                regions["selection_complete"], false,
                "the block cap must disclose incomplete selection"
            );
        }
    }
}

#[test]
fn nextest_block_with_invalid_utf8_keeps_source_byte_accounting() {
    let mut raw = b"FAIL [ 0.123s] suite failing_test\n".to_vec();
    raw.extend_from_slice(b"output \xff\xfe from the failing test\n");
    raw.extend_from_slice(b"thread 'failing_test' panicked at tests/sample.rs:12:3:\n");
    raw.extend_from_slice(b"Summary [ 1.0s] 1 tests run: 1 failed\n");
    for chunk in [1, 7, raw.len()] {
        let mut collector = gh::StreamedLogCollector::new(262_144, 40);
        for bytes in raw.chunks(chunk) {
            collector.push(bytes);
        }
        let log = collector.finish();
        let regions = log.failure_regions.unwrap();
        let number = |value: &Value| value.as_u64().unwrap();
        for range in regions["byte_ranges"].as_array().unwrap() {
            assert_eq!(
                number(&range["retained_source_bytes"]) + number(&range["omitted_bytes"]),
                number(&range["end_byte"]) - number(&range["start_byte"]),
                "chunk {chunk}: every block accounts for its source bytes"
            );
        }
        assert_eq!(
            number(&regions["retained_source_bytes"]) + number(&regions["omitted_bytes"]),
            number(&regions["command_bytes"]),
            "chunk {chunk}: the total accounts for the command bytes"
        );
        assert!(number(&regions["omitted_bytes"]) < raw.len() as u64);
        let text = regions["text"].as_str().unwrap();
        assert!(text.contains("panicked at tests/sample.rs"));
        assert!(!text.contains('\u{fffd}'));
    }
}

/// Cuts one credential-bearing log at every byte and line boundary, under two
/// excerpt caps and both collector shapes. A window that cuts a credential,
/// through its introducer or its value, may expose no fragment of it. Any other
/// window may expose a fragment only when the complete log exposes it too. The
/// fixtures pin one window per spelling; this sweep covers the shapes they miss.
#[test]
fn every_selected_window_hides_credential_fragments() {
    const FRAGMENT_BYTES: usize = 12;
    let github_token = format!("ghp_{}", "a".repeat(36));
    let header_value = "q7Lx2Rv9Kp4Nm8Zs3Tw6";
    let service_value = "Yh5Dc8Bn2Mj6Qe1Ua4Wr9";
    let split_value = "Gt3Pk7Lz0Xv5Fs8Ob2Hd6";
    let raw = format!(
        "Authorization: Bearer {github_token}\n\
         HTTPAPIKey={header_value}\n\
         2026-10-26T14:42:00.0000000Z SERVICEAPIKey: {service_value}\n\
         api_key:\n\
         {split_value}\n"
    );
    // A credential runs from its introducer to the end of its value.
    let span = |introducer: &str, value: &str| {
        let start = raw.find(introducer).unwrap();
        (start, raw.find(value).unwrap() + value.len())
    };
    let credentials = [
        (
            github_token.as_str(),
            span("Authorization:", github_token.as_str()),
        ),
        (header_value, span("HTTPAPIKey=", header_value)),
        (service_value, span("SERVICEAPIKey:", service_value)),
        (split_value, span("api_key:", split_value)),
    ];
    let fragments: Vec<Vec<&str>> = credentials
        .iter()
        .map(|&(secret, _)| {
            let mut parts: Vec<&str> = (0..=secret.len() - FRAGMENT_BYTES)
                .map(|start| &secret[start..start + FRAGMENT_BYTES])
                .collect();
            parts.sort_unstable();
            parts.dedup();
            parts
        })
        .collect();
    let mut line_starts = vec![0];
    line_starts.extend(raw.match_indices('\n').map(|(index, _)| index + 1));
    // Each window carries the source bytes it covers, for the cut test.
    let mut windows = Vec::new();
    for start in 0..raw.len() {
        for end in start + 1..=raw.len() {
            windows.push((json!({"byte_start": start, "byte_end": end}), (start, end)));
        }
    }
    for first in 1..line_starts.len() {
        for last in first + 1..=line_starts.len() {
            windows.push((
                json!({"line_start": first, "line_end": last}),
                (line_starts[first - 1], line_starts[last - 1]),
            ));
        }
    }
    let complete = {
        let mut collector = gh::StreamedLogCollector::new(16_384, 40);
        collector.push(raw.as_bytes());
        collector.finish().text
    };
    let mut leaks = Vec::new();
    for (window, (covered_start, covered_end)) in &windows {
        let selection = gh::log_selection_from_input(window)
            .unwrap()
            .expect("a byte or line window selects log text");
        // A window that starts or ends inside a credential cuts it.
        let cut: Vec<bool> = credentials
            .iter()
            .map(|&(_, (start, end))| {
                (start < *covered_start && *covered_start < end)
                    || (start < *covered_end && *covered_end < end)
            })
            .collect();
        for (cap, tail_weighted) in [(16_384, false), (16_384, true), (40, false), (40, true)] {
            let mut collector = if tail_weighted {
                gh::StreamedLogCollector::tail_weighted_scoped(cap, 40, selection.clone())
            } else {
                gh::StreamedLogCollector::scoped(cap, 40, selection.clone())
            };
            collector.push(raw.as_bytes());
            let text = collector.finish().text;
            for (index, parts) in fragments.iter().enumerate() {
                let exposed = parts.iter().find(|fragment| {
                    text.contains(**fragment) && (cut[index] || !complete.contains(**fragment))
                });
                if let Some(fragment) = exposed {
                    leaks.push(format!(
                        "{window} cap={cap} tail_weighted={tail_weighted} cut={} exposes {fragment:?}: {text:?}",
                        cut[index]
                    ));
                }
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "{} window variants exposed a credential fragment: {:#?}",
        leaks.len(),
        &leaks[..leaks.len().min(5)]
    );
}

#[test]
fn request_fixture_goldens() {
    golden("requests", &read("requests.json"), |case| {
        let input = &case["input"];
        let raw = &case["raw"];
        let (request, projected) = match case["op"].as_str().unwrap() {
            "dependabot" => (
                gh::dependabot_alerts_request(input),
                gh::project_dependabot_alert(raw),
            ),
            "dependabot_pr" => (
                gh::dependabot_pull_requests_request(input),
                gh::project_dependabot_pull_request(raw),
            ),
            "code_scanning" => (
                gh::code_scanning_alerts_request(input),
                gh::project_code_scanning_alert(raw),
            ),
            "secret_scanning" => (
                gh::secret_scanning_alerts_request(input),
                gh::project_secret_scanning_alert(raw),
            ),
            "secret_location" => (
                gh::secret_scanning_locations_request(input),
                gh::project_secret_location(raw),
            ),
            "run_list" => (gh::run_list_request(input), gh::project_run(raw)),
            "run_view" => (gh::run_view_request(input), gh::project_run_view(raw)),
            "run_jobs" => (
                gh::run_jobs_request(input),
                json!(gh::project_job_labels(raw)),
            ),
            "job_annotations" => (
                gh::job_annotations_request(input),
                json!(gh::concurrency_cancellation(raw)),
            ),
            "closed_pr_head" => (
                gh::closed_pr_head_request(input),
                gh::project_pull_request(raw),
            ),
            "open_pr_head" => (
                gh::open_pr_head_request(input),
                gh::project_pull_request(raw),
            ),
            "pr_list" => (gh::pr_list_request(input), gh::project_pull_request(raw)),
            "repo_view" => (gh::repo_view_request(input), gh::project_repo_view(raw)),
            "logs" => (
                gh::RunLogRequests::from_input(input).map(|requests| requests.run_log),
                Value::Null,
            ),
            op => panic!("unknown fixture operation {op}"),
        };
        match request {
            Ok(request) => {
                json!({"argv": request.args, "cwd": request.current_dir, "projected": projected})
            }
            Err(error) => json!({"error": error.to_string()}),
        }
    });
}

#[test]
fn landing_request_refuses_empty_and_dot_only_repository_segments() {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    assert!(gh::commit_pull_requests_request("acme/orbit", commit).is_ok());
    for repository in [
        "acme/..", "../orbit", "acme/.", "./orbit", "acme/", "/orbit", "acme", "a/b/c", "",
    ] {
        assert!(
            gh::commit_pull_requests_request(repository, commit).is_err(),
            "{repository:?} must be refused"
        );
    }
}

#[cfg(unix)]
fn isolated() -> bool {
    const MARKER: &str = "ORBIT_TEST_GITHUB_LOG_GOLDEN_CHILD";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let home = TempDir::new().unwrap();
    let bin = home.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("gh");
    std::fs::write(
        &program,
        r#"#!/bin/sh
fixture_dir=${GH_GOLDEN_CASE:?}
printf '%s\n' "$*" >> "$fixture_dir/calls"
pwd -P >> "$fixture_dir/cwds"
case "$*" in
  *actions/jobs/*/logs*)
    for arg in "$@"; do
      case "$arg" in
        repos/*/actions/jobs/*/logs) job=${arg%/logs}; job=${job##*/} ;;
      esac
    done
    if [ -f "$fixture_dir/job_$job" ]; then /bin/cat "$fixture_dir/job_$job"; exit 0; fi
    printf 'gh: Not Found (HTTP 404) token=ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n' >&2
    exit 1 ;;
  *--log*)
    if [ -f "$fixture_dir/run_error" ]; then /bin/cat "$fixture_dir/run_error" >&2; exit 1; fi
    /bin/cat "$fixture_dir/run_log"; exit 0 ;;
  *--json*) /bin/cat "$fixture_dir/view.json"; exit 0 ;;
esac
exit 1
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    command
        .args([
            "--exact",
            "public_tool_surface::github_log_goldens::fallback_fixture_goldens",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PATH", path)
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(
        &mut command,
        "public_tool_surface::github_log_goldens::fallback_fixture_goldens",
        home.path(),
    );
    orbit_common::test_env::assert_child_test_passed(
        "public_tool_surface::github_log_goldens::fallback_fixture_goldens",
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

#[test]
#[cfg(unix)]
fn fallback_fixture_goldens() {
    if !isolated() {
        return;
    }
    golden("fallback", &read("fallback.json"), |case| {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let case_path = root.path().to_str().unwrap();
        let _env = orbit_common::test_env::scoped([("GH_GOLDEN_CASE", Some(case_path))]);
        std::fs::write(root.path().join("view.json"), case["view"].to_string()).unwrap();
        std::fs::write(
            root.path().join("run_log"),
            case.get("run_log").map(source).unwrap_or_default(),
        )
        .unwrap();
        if let Some(error) = case["run_error"].as_str() {
            std::fs::write(root.path().join("run_error"), error).unwrap();
        }
        for (job, log) in case["job_logs"].as_object().unwrap() {
            std::fs::write(root.path().join(format!("job_{job}")), source(log)).unwrap();
        }
        let input = case["input"].clone();
        let selected = case["selected"] != false;
        let result = if case["registered"] == true {
            let mut registry = ToolRegistry::new();
            registry.register_builtins();
            let tool = case["tool"].as_str().unwrap_or("github.run.logs");
            let mut input = input;
            if tool == "github.run.logs" {
                input["max_bytes"] = case["max_bytes"].clone();
            }
            // List tools serve arrays, while view/log recovery serves one run.
            if tool.ends_with("list") {
                std::fs::write(
                    root.path().join("view.json"),
                    json!([case["view"].clone()]).to_string(),
                )
                .unwrap();
            }
            registry.execute(
                tool,
                &ToolContext {
                    workspace_root: selected.then(|| workspace.clone()),
                    ..ToolContext::default()
                },
                input,
            )
        } else {
            gh::RunLogRequests::from_input(&input).and_then(|requests| {
                let mut requests = requests.in_directory(workspace.to_str().unwrap());
                let program = PathBuf::from(std::env::var_os("HOME").unwrap()).join("bin/gh").to_string_lossy().into_owned();
                requests.run_log.program.clone_from(&program);
                requests.run_view.program = program;
                gh::read_run_log(&requests, gh::LogReadBounds::new(case["max_bytes"].as_u64().unwrap() as usize), None).map(|read| {
                    json!({"source": read.source, "source_jobs": read.source_jobs, "fallback_error": read.fallback_error, "parsed": streamed(read.log)})
                })
            })
        };
        if let Some(expected) = case.get("expected_log") {
            let output = result.as_ref().unwrap();
            assert_eq!(
                &output["log"], expected,
                "byte selection must not expose a cut credential: {}",
                case["name"]
            );
            let selection = &output["selection"];
            assert_eq!(selection["byte_start"], case["input"]["byte_start"]);
            assert_eq!(selection["byte_end"], case["input"]["byte_end"]);
            assert_eq!(
                selection["selected_bytes"].as_u64().unwrap(),
                case["input"]["byte_end"].as_u64().unwrap()
                    - case["input"]["byte_start"].as_u64().unwrap(),
                "redaction must preserve source-coordinate selection counts"
            );
        }
        let calls = std::fs::read_to_string(root.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let cwds = std::fs::read_to_string(root.path().join("cwds"))
            .unwrap_or_default()
            .lines()
            .map(|cwd| {
                let actual = std::fs::canonicalize(cwd).unwrap();
                if actual == std::fs::canonicalize(&workspace).unwrap() {
                    "selected_workspace"
                } else {
                    assert_eq!(
                        actual,
                        std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
                        "unexpected gh cwd"
                    );
                    "caller_directory"
                }
            })
            .collect::<Vec<_>>();
        match result {
            Ok(output) => json!({"output": output, "calls": calls, "cwds": cwds}),
            Err(error) => json!({"error": error.to_string(), "calls": calls, "cwds": cwds}),
        }
    });
}
