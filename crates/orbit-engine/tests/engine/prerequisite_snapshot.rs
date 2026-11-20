//! How long the current-gates snapshot keeps reading unreadable evidence
//! [ORB-25870].
//!
//! A substitute `gh` serves one open pull request with no required gates and
//! logs every endpoint it is asked for. A fixture counter makes the pull
//! request read stale for the first N snapshots, or the check-run list
//! truncated for every snapshot. The retry waits are captured through the
//! fixed-wait fixture and added to the prerequisite clock, so no check sleeps.

use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;

use orbit_common::OrbitError;
use orbit_engine::run_private_vcs_operation;
use orbit_engine::test_support::{with_prerequisite_clock, with_vcs_retry_wait};
use serde_json::{Value, json};

use super::git_fixture;

const CI_CURRENT_GATES: &str = "ci.current_gates";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const START_MS: u64 = 1_000_000;
const PULL_READ: &str = "repos/{owner}/{repo}/pulls/42";

#[test]
fn truncated_check_runs_are_returned_at_once_without_waiting() {
    git_fixture::isolated(
        module_path!(),
        "truncated_check_runs_are_returned_at_once_without_waiting",
        || {
            let fixture = Fixture::open("truncated");
            fixture.write_forge(0, true);
            let (result, waits) = fixture.run(START_MS + 3_600_000, 30);
            let error = result.expect_err("a truncated list is never readable on a retry");
            assert_eq!(unreadable_reason(&error), "truncated check_runs");
            assert!(waits.is_empty(), "a retry waited {waits:?} ms");
            assert_eq!(fixture.reads(PULL_READ), 1, "the snapshot read again");
        },
    );
}

#[test]
fn transient_unreadable_evidence_backs_off_to_the_poll_interval() {
    git_fixture::isolated(
        module_path!(),
        "transient_unreadable_evidence_backs_off_to_the_poll_interval",
        || {
            let fixture = Fixture::open("backoff");
            fixture.write_forge(6, false);
            let (result, waits) = fixture.run(START_MS + 3_600_000, 40);
            let value = result.expect("the evidence becomes readable on the seventh read");
            assert_eq!(value["gates_passed"], json!(true), "{value}");
            assert_eq!(
                waits,
                [5_000, 10_000, 20_000, 40_000, 40_000, 40_000],
                "waits grow from 5 s to the poll interval and stay there"
            );
            assert_eq!(fixture.reads(PULL_READ), 7, "no read after it succeeded");
        },
    );
}

#[test]
fn deadline_ending_a_transient_wait_returns_the_last_unreadable_cause() {
    git_fixture::isolated(
        module_path!(),
        "deadline_ending_a_transient_wait_returns_the_last_unreadable_cause",
        || {
            let fixture = Fixture::open("deadline");
            fixture.write_forge(1_000, false);
            let (result, waits) = fixture.run(START_MS + 12_000, 30);
            let error = result.expect_err("the evidence never becomes readable");
            assert_eq!(unreadable_reason(&error), "foreign or changed head");
            assert_eq!(
                waits,
                [5_000, 7_000],
                "the last wait is the budget that remains"
            );
            assert_eq!(fixture.reads(PULL_READ), 2);
        },
    );
}

fn unreadable_reason(error: &OrbitError) -> &str {
    match error {
        OrbitError::WorkflowEvidenceUnavailable(evidence) => &evidence.reason,
        other => panic!("expected unreadable evidence, got {other}"),
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn open(name: &str) -> Self {
        let root = std::env::current_dir().unwrap().join(name);
        fs::create_dir_all(root.join("bin")).unwrap();
        let fixture = Self { root };
        fixture.install_gh();
        fixture
    }

    fn put(&self, name: &str, value: Value) {
        fs::write(self.root.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }

    /// `stale_reads` pull-request reads name another head before the pull
    /// request is current; `truncated` makes the check-run list incomplete.
    fn write_forge(&self, stale_reads: u32, truncated: bool) {
        let repo = json!({"id":500,"full_name":"orbit/test","url":"https://api.github.com/repos/orbit/test"});
        let pull = |head: &str| {
            json!({"number":42,"state":"open","merged":false,
                "head":{"sha":head,"ref":"feature","repo":repo},
                "base":{"sha":"b".repeat(40),"ref":"main","repo":repo}})
        };
        self.put("repo", repo.clone());
        self.put("pr", pull(HEAD));
        self.put("pr-stale", pull(&"c".repeat(40)));
        fs::write(self.root.join("stale-reads"), stale_reads.to_string()).unwrap();
        self.put(
            "policy",
            json!({"data":{"repository":{"pullRequest":{"headRefOid":HEAD,"baseRefName":"main",
                "reviewDecision":null,"baseRef":{"branchProtectionRule":null}}}}}),
        );
        self.put("rules", json!([]));
        self.put(
            "checks",
            json!({"total_count":if truncated {150} else {0},"check_runs":[]}),
        );
        self.put(
            "status",
            json!({"sha":HEAD,"repository":repo,"total_count":0,"statuses":[]}),
        );
        self.put("runs", json!({"total_count":0,"workflow_runs":[]}));
    }

    fn install_gh(&self) {
        let gh = self.root.join("bin/gh");
        fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
dir='{dir}'
printf '%s\n' "$2" >> "$dir/calls"
case "$2" in
  'repos/{{owner}}/{{repo}}') cat "$dir/repo" ;;
  '{pull}')
    left=$(cat "$dir/stale-reads")
    if [ "$left" -gt 0 ]; then
      echo $((left - 1)) > "$dir/stale-reads"
      cat "$dir/pr-stale"
    else
      cat "$dir/pr"
    fi ;;
  graphql) cat "$dir/policy" ;;
  'repos/{{owner}}/{{repo}}/rules/branches/'*) cat "$dir/rules" ;;
  'repos/{{owner}}/{{repo}}/commits/'*'/check-runs'*) cat "$dir/checks" ;;
  'repos/{{owner}}/{{repo}}/commits/'*'/status'*) cat "$dir/status" ;;
  'repos/{{owner}}/{{repo}}/actions/runs?'*) cat "$dir/runs" ;;
  *) echo "unexpected read $2" >&2; exit 1 ;;
esac
"#,
                dir = self.root.display(),
                pull = PULL_READ,
            ),
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![self.root.join("bin")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        // SAFETY: this runs inside the isolated child process, before any
        // other thread reads PATH. The parent keeps its own environment.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    }

    fn reads(&self, endpoint: &str) -> usize {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == endpoint)
            .count()
    }

    /// Run the current-gates read under a clock that every captured wait
    /// advances; returns the result and the waits in milliseconds.
    fn run(
        &self,
        deadline_ms: u64,
        poll_interval_seconds: u64,
    ) -> (Result<Value, OrbitError>, Vec<u64>) {
        let now = Rc::new(Cell::new(START_MS));
        let waits = Rc::new(RefCell::new(Vec::new()));
        let (clock, wait_clock, wait_log) = (Rc::clone(&now), Rc::clone(&now), Rc::clone(&waits));
        let input = json!({
            "workspace_path": self.root, "pr": "42", "head_sha": HEAD,
            "head_branch": "feature", "base_branch": "main",
            "deadline_ms": deadline_ms, "poll_interval_seconds": poll_interval_seconds,
        });
        let result = with_prerequisite_clock(
            move || clock.get(),
            |_, _| {},
            || {
                with_vcs_retry_wait(
                    move |duration| {
                        let ms = u64::try_from(duration.as_millis()).unwrap();
                        wait_log.borrow_mut().push(ms);
                        wait_clock.set(wait_clock.get() + ms);
                    },
                    || run_private_vcs_operation(CI_CURRENT_GATES, &input),
                )
            },
        );
        let waits = waits.borrow().clone();
        (result, waits)
    }
}
