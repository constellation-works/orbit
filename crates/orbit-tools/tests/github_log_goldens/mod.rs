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
    let mut first_shape = true;
    for (prefix, suffix) in [
        ("##[group]Run cargo nextest run\n##[endgroup]\n".to_string(), "##[error]Process completed with exit code 100.\n"),
        (String::new(), ""),
        (format!("{}\n##[group]Run cargo nextest run\n##[endgroup]\n", "unrelated".repeat(4096)), "##[error]Process completed with exit code 100.\n"),
        ("##[group]Run earlier\n##[endgroup]\n##[error]Process completed with exit code 1.\n##[group]Run cargo nextest run\n##[endgroup]\n".to_string(), "##[error]Process completed with exit code 100.\n"),
    ] {
        let raw = format!("{prefix}{padding}{block}{padding}Summary [ 1.0s] 2810 tests run: 1 failed\n{suffix}");
        assert!(raw.len() > 2_000_000);
        // `push` re-batches to a fixed size, so the chunk schedule only needs
        // to vary once; the other shapes run once over the whole log.
        let chunks: &[usize] = if first_shape { &[7, 4096, raw.len()] } else { &[4096] };
        first_shape = false;
        for &chunk in chunks {
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
    let equals_value = "B6w2R9k4N7c1F3a8Y5mQ0";
    let colon_value = "Z8p3T1v6H4d9L2x5C7nA0";
    let service_value = "Yh5Dc8Bn2Mj6Qe1Ua4Wr9";
    let split_value = "Gt3Pk7Lz0Xv5Fs8Ob2Hd6";
    let raw = format!(
        "Authorization: Bearer {github_token}\n\
         HTTPAPIKey={header_value}\n\
         note= HTTPAPIKey={equals_value}\n\
         Error: HTTPAPIKey={colon_value}\n\
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
        (equals_value, span("note=", equals_value)),
        (colon_value, span("Error:", colon_value)),
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
        // A log shorter than both heads is untruncated and joins head and tail,
        // so an even and a tail-weighted split render it identically at the
        // large cap. A window of at most `cap` bytes is untruncated at either
        // cap, so one shape covers it.
        let shapes: &[(usize, bool)] = if covered_end - covered_start <= 40 {
            &[(16_384, false)]
        } else {
            &[(16_384, false), (40, false), (40, true)]
        };
        for &(cap, tail_weighted) in shapes {
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
fn isolated(test: &str) -> bool {
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
  *actions/jobs/*)
    [ -f "$fixture_dir/denied" ] && { echo 'HTTP 404: Not Found' >&2; exit 1; }
    [ -f "$fixture_dir/job_metadata" ] && { /bin/cat "$fixture_dir/job_metadata"; exit 0; }
    exit 1 ;;
  *actions/runs/*)
    [ -f "$fixture_dir/denied" ] && { echo 'HTTP 404: Not Found' >&2; exit 1; }
    [ -f "$fixture_dir/run_metadata" ] && { /bin/cat "$fixture_dir/run_metadata"; exit 0; }
    exit 1 ;;
  *--log*)
    [ -f "$fixture_dir/denied" ] && { echo 'HTTP 404: Not Found' >&2; exit 1; }
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
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("ORBIT_ROOT", home.path().join("orbit-root"))
        .env("PATH", path)
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut command, test, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

#[test]
#[cfg(unix)]
fn fallback_fixture_goldens() {
    if !isolated("public_tool_surface::github_log_goldens::fallback_fixture_goldens") {
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

/// What the registered log tool did over one scripted job or whole run, as the
/// fake `gh` saw it. Every read goes through `ToolRegistry`, so the cache is
/// enabled and located by the shipped wiring, not by the test.
#[cfg(unix)]
struct CachedLogRead {
    root: TempDir,
    registry: ToolRegistry,
    job: Option<&'static str>,
}

#[cfg(unix)]
impl CachedLogRead {
    /// A completed job or run whose log is numbered lines tagged `tag`, in a
    /// fresh fixture directory and an empty cache.
    fn new(tag: &str, job: Option<&'static str>) -> (Self, orbit_common::test_env::ScopedEnv) {
        let _ = std::fs::remove_dir_all(Self::cache_dir());
        let root = TempDir::new().unwrap();
        let env = orbit_common::test_env::scoped([(
            "GH_GOLDEN_CASE",
            Some(root.path().to_str().unwrap()),
        )]);
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        let read = Self {
            root,
            registry,
            job,
        };
        read.set_log(tag);
        read.set_status("completed", "acme/orbit", 1);
        (read, env)
    }

    fn cache_dir() -> PathBuf {
        PathBuf::from(std::env::var_os("ORBIT_ROOT").unwrap())
            .join(orbit_tools::GITHUB_RUN_LOG_CACHE_DIR)
    }

    fn set_log(&self, tag: &str) {
        let log: String = (1..=40)
            .map(|line| format!("build\tTest\t2026-11-20T02:00:00.0000000Z {tag} line {line}\n"))
            .collect();
        std::fs::write(self.root.path().join("job_9"), &log).unwrap();
        std::fs::write(self.root.path().join("run_log"), log).unwrap();
    }

    fn set_status(&self, status: &str, repository: &str, run_attempt: u64) {
        let job_url = format!("https://api.github.com/repos/{repository}/actions/jobs/9");
        let job_body = json!({"id": 9, "run_id": 123, "status": status, "url": job_url});
        std::fs::write(self.root.path().join("job_metadata"), job_body.to_string()).unwrap();

        let run_url = format!("https://api.github.com/repos/{repository}/actions/runs/123");
        let run_body = json!({
            "id": 123,
            "status": status,
            "run_attempt": run_attempt,
            "url": run_url,
        });
        std::fs::write(self.root.path().join("run_metadata"), run_body.to_string()).unwrap();
    }

    fn deny(&self) {
        std::fs::write(self.root.path().join("denied"), "").unwrap();
    }

    fn read(&self, window: Value) -> Result<Value, orbit_common::OrbitError> {
        let mut input = json!({"run": "123", "scope": "all"});
        if let Some(job) = self.job {
            input["job"] = json!(job);
        }
        for (key, value) in window.as_object().unwrap() {
            input[key] = value.clone();
        }
        self.registry
            .execute("github.run.logs", &ToolContext::default(), input)
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    /// Whole-log downloads, as opposed to status requests.
    fn downloads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.starts_with("run view") && call.contains("--log"))
            .count()
    }

    fn cache_entries(&self) -> Vec<PathBuf> {
        std::fs::read_dir(Self::cache_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect()
    }

    /// A run whose run-scoped read prints nothing (the primary blind spot).
    /// Its one job, 9, is a completed failure whose log API serves the log, so
    /// the run-scoped read recovers the excerpt from that job.
    fn fallback(tag: &str) -> (Self, orbit_common::test_env::ScopedEnv) {
        let (read, env) = Self::new(tag);
        std::fs::write(read.root.path().join("run_log"), "").unwrap();
        let view = json!({
            "databaseId": 123,
            "url": "https://github.com/acme/orbit/actions/runs/123",
            "jobs": [{
                "databaseId": 9,
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
            }],
        });
        std::fs::write(read.root.path().join("view.json"), view.to_string()).unwrap();
        let run = json!({
            "id": 123,
            "status": "completed",
            "run_attempt": 1,
            "url": "https://api.github.com/repos/acme/orbit/actions/runs/123",
        });
        std::fs::write(read.root.path().join("run_metadata"), run.to_string()).unwrap();
        (read, env)
    }

    /// A run-scoped read with the default scope, which is the request that
    /// reaches the fallback.
    fn run_read(&self, window: Value) -> Result<Value, orbit_common::OrbitError> {
        let mut input = json!({"run": "123"});
        for (key, value) in window.as_object().unwrap() {
            input[key] = value.clone();
        }
        self.registry
            .execute("github.run.logs", &ToolContext::default(), input)
    }

    /// Whole-log downloads of the fallback job's own log API, as opposed to
    /// the run-scoped primary reads and status requests.
    fn fallback_downloads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.contains("actions/jobs/9/logs"))
            .count()
    }
}

#[cfg(unix)]
fn log_text(output: &Value) -> &str {
    output["log"].as_str().unwrap()
}

/// A second window of a completed job's log is served from the cache: one
/// download for both, and only the status request that proves current access.
#[test]
#[cfg(unix)]
fn two_windows_of_a_completed_job_download_its_log_once() {
    if !isolated(
        "public_tool_surface::github_log_goldens::two_windows_of_a_completed_job_download_its_log_once",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let (job, _env) = CachedLogRead::new("first", Some("9"));
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

    let early = job.read(json!({"byte_start": 0, "byte_end": 60})).unwrap();
    let calls_after_first = job.calls();
    let later = job.read(json!({"line_start": 5, "line_end": 8})).unwrap();
    let repeat = job.read(json!({"byte_start": 0, "byte_end": 60})).unwrap();

    assert_eq!(
        job.downloads(),
        1,
        "three reads of one completed job download it once: {:?}",
        job.calls()
    );
    assert_eq!(
        job.calls()[calls_after_first.len()..],
        [
            "api --method GET repos/{owner}/{repo}/actions/jobs/9",
            "api --method GET repos/{owner}/{repo}/actions/jobs/9",
        ],
        "a warm read makes only the status request that proves current access"
    );
    assert!(log_text(&early).contains("first line 1"));
    assert!(log_text(&later).contains("first line 5"));
    assert_eq!(early, repeat, "a warm read renders what the cold one did");

    let entries = job.cache_entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(mode(&CachedLogRead::cache_dir()), 0o700);
    assert_eq!(mode(&entries[0]), 0o600);

    // An entry past the age bound is a miss: the log is downloaded again.
    let day_and_a_bit = std::time::Duration::from_secs(25 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&entries[0])
        .unwrap()
        .set_modified(std::time::SystemTime::now() - day_and_a_bit)
        .unwrap();
    job.read(json!({"line_start": 5, "line_end": 8})).unwrap();
    assert_eq!(job.downloads(), 2, "an expired entry is not served");
}

/// The cache never widens what a caller may read: only a job or run the
/// provider reports completed is cached, and a cached log is served only while
/// the caller's own status request succeeds for the same resolved repository.
#[test]
#[cfg(unix)]
fn a_cached_job_log_is_served_only_to_a_caller_that_may_read_it_now() {
    if !isolated(
        "public_tool_surface::github_log_goldens::a_cached_job_log_is_served_only_to_a_caller_that_may_read_it_now",
    ) {
        return;
    }
    let window = || json!({"line_start": 5, "line_end": 8});

    for job in [Some("9"), None] {
        let (running, env) = CachedLogRead::new("first", job);
        running.set_status("in_progress", "acme/orbit", 1);
        running.read(window()).unwrap();
        running.read(window()).unwrap();
        assert_eq!(
            running.downloads(),
            2,
            "job={job:?}: a running job or run is always re-read"
        );
        assert!(
            running.cache_entries().is_empty(),
            "job={job:?}: and never cached"
        );
        drop(env);

        let (refused, env) = CachedLogRead::new("first", job);
        let cold = refused.read(window()).unwrap();
        let warm = refused.read(window()).unwrap();
        assert_eq!(cold, warm, "job={job:?}: warm read matches cold fill");
        assert_eq!(refused.downloads(), 1, "job={job:?}: warm read hits cache");
        assert_eq!(refused.cache_entries().len(), 1, "job={job:?}");
        refused.deny();
        let error = refused.read(window()).unwrap_err().to_string();
        assert!(!error.contains("first line"), "job={job:?}: {error}");
        assert_eq!(
            refused.downloads(),
            2,
            "job={job:?}: a refused read still asks the provider rather than serving the cache"
        );
        drop(env);

        let (moved, _env2) = CachedLogRead::new("first", job);
        // The unqualified `{owner}/{repo}` request resolves to another
        // repository once the checkout it runs in changes; the same ids (and
        // run attempt) belong to another repository.
        let first = moved.read(window()).unwrap();
        assert!(log_text(&first).contains("first line 5"), "job={job:?}");
        let warm = moved.read(window()).unwrap();
        assert_eq!(warm, first, "job={job:?}: warm read hits cache");
        assert_eq!(moved.downloads(), 1, "job={job:?}");
        moved.set_status("completed", "other/repo", 1);
        moved.set_log("other");
        let second = moved.read(window()).unwrap();
        assert!(
            log_text(&second).contains("other line 5"),
            "job={job:?}: another repository's read never returns the first one's log"
        );
        assert_eq!(moved.downloads(), 2, "job={job:?}");
        assert_eq!(moved.cache_entries().len(), 2, "job={job:?}");
    }
}

/// Two windows of a run's fallback log download the failed job's log once. Each
/// window still reports the job log API and the job that supplied the bytes,
/// and each cached window's log text matches an uncached read of that window.
#[test]
#[cfg(unix)]
fn two_windows_of_a_completed_fallback_job_download_its_log_once() {
    if !isolated(
        "public_tool_surface::github_log_goldens::two_windows_of_a_completed_fallback_job_download_its_log_once",
    ) {
        return;
    }
    let (fallback, _env) = CachedJobRead::fallback("fallback");
    let windows = [
        json!({"line_start": 5, "line_end": 8}),
        json!({"byte_start": 0, "byte_end": 60}),
    ];

    let early = fallback.run_read(windows[0].clone()).unwrap();
    let later = fallback.run_read(windows[1].clone()).unwrap();

    assert_eq!(
        fallback.fallback_downloads(),
        1,
        "the failed job's log is downloaded once: {:?}",
        fallback.calls()
    );
    for read in [&early, &later] {
        assert_eq!(read["source"], "job_api_log");
        assert_eq!(read["source_jobs"][0]["job_id"], 9);
    }
    assert!(log_text(&early).contains("fallback line 5"));
    assert!(log_text(&later).contains("fallback line 1"));
    assert_eq!(fallback.cache_entries().len(), 1);

    for (read, window) in [(&early, &windows[0]), (&later, &windows[1])] {
        let mut input = window.clone();
        input["run"] = json!("123");
        let requests = gh::RunLogRequests::from_input(&input).unwrap();
        let uncached = gh::read_run_log(&requests, gh::LogReadBounds::new(262_144), None).unwrap();
        assert_eq!(log_text(read), uncached.log.text, "{window}");
    }
}

/// Only a completed fallback job's log is cached. A running job, a refused read
/// and a failed job-log read each leave the cache empty, and the first read
/// that succeeds stores exactly one entry.
#[test]
#[cfg(unix)]
fn a_fallback_job_log_that_is_not_complete_or_not_readable_is_never_cached() {
    if !isolated(
        "public_tool_surface::github_log_goldens::a_fallback_job_log_that_is_not_complete_or_not_readable_is_never_cached",
    ) {
        return;
    }
    let window = || json!({"line_start": 2, "line_end": 4});

    // The job log API still reports the job as running.
    let (running, env) = CachedJobRead::fallback("partial");
    running.set_job("in_progress", "acme/orbit");
    running.run_read(window()).unwrap();
    running.run_read(window()).unwrap();
    assert_eq!(
        running.fallback_downloads(),
        2,
        "a running job's log is re-read"
    );
    assert!(running.cache_entries().is_empty(), "and never cached");
    drop(env);

    // The caller's access is refused, so the read fails and stores nothing.
    let (refused, env) = CachedJobRead::fallback("denied");
    refused.deny();
    refused.run_read(window()).unwrap_err();
    assert!(
        refused.cache_entries().is_empty(),
        "a refused read stores nothing"
    );
    drop(env);

    // The job log API fails, then answers. The failed read leaves nothing that
    // the later read could mistake for the log.
    let (failed, _env) = CachedJobRead::fallback("later");
    let job_log = failed.root.path().join("job_9");
    let log = std::fs::read(&job_log).unwrap();
    std::fs::remove_file(&job_log).unwrap();
    failed.run_read(window()).unwrap_err();
    assert!(failed.cache_entries().is_empty());
    std::fs::write(&job_log, log).unwrap();
    let read = failed.run_read(window()).unwrap();
    assert!(log_text(&read).contains("later line 2"));
    assert_eq!(failed.cache_entries().len(), 1);
}
