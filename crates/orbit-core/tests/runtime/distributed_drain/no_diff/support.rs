//! Shared clean-leaf fixture and stand-in commands for the `no_diff` scenarios.
//!
//! Extracted from `no_diff.rs` (ORB-17794) so the scenario file holds only
//! scenarios; every helper here is shared by at least one scenario or by
//! `task_artifacts`.
use super::super::claimed_review::ToOwner;
use super::super::*;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::already_landed_scope;

pub(crate) const CHECK: &str = "git diff --exit-code";
pub(crate) fn engine_action(
    host: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, OrbitError> {
    orbit_engine::execute_deterministic_action(
        host,
        action,
        &json!({}),
        input,
        false,
        &Default::default(),
        None,
    )
}
/// A review baseline gate the follower's own sandbox could not fully run,
/// kept outside both checkouts so either host's configuration can name it and
/// neither tree changes. What it prints and reports is set per case; each run
/// is counted.
pub(crate) struct Gate {
    pub(crate) _dir: TempDir,
    pub(crate) script: PathBuf,
    pub(crate) runs: PathBuf,
}
/// Which hosts' `review.baseline_commands` name the [`Gate`]. Per-host
/// configuration is untracked, so the replica's copy can drift from the
/// owner's [ORB-15622].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateOn {
    Both,
    OwnerOnly,
    FollowerOnly,
}
impl GateOn {
    pub(crate) fn owner(self) -> bool {
        self != Self::FollowerOnly
    }

    pub(crate) fn follower(self) -> bool {
        self != Self::OwnerOnly
    }
}
impl Gate {
    pub(crate) fn new() -> Self {
        let dir = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let gate = Self {
            script: dir.path().join("affected-gate.sh"),
            runs: dir.path().join("affected-gate.runs"),
            _dir: dir,
        };
        gate.executes(3);
        gate
    }

    /// The gate as a host's configuration names it.
    pub(crate) fn command(&self) -> String {
        format!("sh '{}'", self.script.display())
    }

    pub(crate) fn set(&self, body: &str) {
        std::fs::write(
            &self.script,
            format!("echo ran >> '{}'\n{body}\n", self.runs.display()),
        )
        .unwrap();
    }

    pub(crate) fn summary(tests: u64) -> String {
        format!(
            "printf '{{\"schema_version\":1,\"selection\":{{\"packages\":[\"fixture\"]}},\
             \"tests_run\":{tests}}}' > \"$ORBIT_VALIDATION_SUMMARY\""
        )
    }

    /// Runs `tests` tests and reports them, as `make ci-test-affected` does.
    pub(crate) fn executes(&self, tests: u64) {
        self.set(&Self::summary(tests));
    }

    /// Defers its Bubblewrap tests again: this host cannot create the
    /// namespaces either.
    pub(crate) fn defers(&self) {
        self.set(&format!(
            "echo '{}' >&2\n{}",
            Self::notice(),
            Self::summary(3)
        ));
    }

    pub(crate) fn notice() -> String {
        orbit_exec::bwrap_deferral_notice(
            "spawn_under_linux_bwrap",
            "bwrap: No permissions to create new namespace",
        )
    }

    pub(crate) fn seatbelt_notice() -> String {
        orbit_exec::seatbelt_deferral_notice(
            "spawn_under_macos_sandbox",
            orbit_types::workflow::SEATBELT_APPLY_REFUSAL,
        )
    }

    pub(crate) fn runs(&self) -> usize {
        std::fs::read_to_string(&self.runs).map_or(0, |runs| runs.lines().count())
    }
}
/// A required command that fails with libtest's report of one test until
/// told to pass, kept outside both checkouts so neither tree changes; each
/// run, on either host, is counted [ORB-15427].
pub(crate) struct RedCheck {
    pub(crate) _dir: TempDir,
    pub(crate) script: PathBuf,
    pub(crate) passes: PathBuf,
    pub(crate) runs: PathBuf,
}
impl RedCheck {
    /// The failure identity its report names.
    pub(crate) const FAILURE: &'static str = "test suite::broken";

    pub(crate) fn new() -> Self {
        let dir = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let check = Self {
            script: dir.path().join("red-check.sh"),
            passes: dir.path().join("passes"),
            runs: dir.path().join("runs"),
            _dir: dir,
        };
        std::fs::write(
            &check.script,
            format!(
                "echo ran >> '{runs}'\nif [ -e '{passes}' ]; then echo 'test suite::broken ... ok'; \
                 exit 0; fi\necho 'test suite::broken ... FAILED'\nexit 3\n",
                runs = check.runs.display(),
                passes = check.passes.display(),
            ),
        )
        .unwrap();
        check
    }

    pub(crate) fn command(&self) -> String {
        format!("sh '{}'", self.script.display())
    }

    /// From now on the command passes, wherever it runs.
    pub(crate) fn pass(&self) {
        std::fs::write(&self.passes, "").unwrap();
    }

    pub(crate) fn runs(&self) -> usize {
        std::fs::read_to_string(&self.runs).map_or(0, |runs| runs.lines().count())
    }
}
/// Preserve the real owner/claim boundary while enforcing the production
/// MCP line ceiling without allocating a giant serialized response.
pub(crate) struct EvidenceOwner {
    pub(crate) owner: ToOwner,
    pub(crate) reads: Arc<Mutex<Vec<(String, Value)>>>,
    pub(crate) lose_read: Arc<Mutex<bool>>,
}
impl OwnerCoordinator for EvidenceOwner {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.reads
            .lock()
            .unwrap()
            .push((name.into(), input.clone()));
        let answer = self.owner.call(name, input, session)?;
        if name == "orbit.task.artifact.get" && std::mem::take(&mut *self.lose_read.lock().unwrap())
        {
            return Err(OrbitError::OutcomeUnknown {
                mcp_call_id: "lost-evidence-read".into(),
                message: "reply lost after owner read the named artifact".into(),
            });
        }
        pub(crate) struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(0);
        serde_json::to_writer(&mut count, &answer).unwrap();
        // Ordinary output alone suffices to overflow; MCP also mirrors it
        // into a text block. Never raise the production 64 MiB ceiling.
        if count.0 > 67_108_864 {
            return Err(OrbitError::OutcomeUnknown {
                mcp_call_id: format!("oversize-{name}"),
                message: format!(
                    "owner reply exceeds MCP 67108864-byte line ceiling ({} bytes)",
                    count.0
                ),
            });
        }
        Ok(answer)
    }
}
/// Evidence prepared on the owner before admission; every case still owns its
/// task, store and artifact bytes. A clean-tree report is optional independently
/// of whether there is unrelated media already in the task bundle.
pub(crate) struct EvidenceSeed<'a> {
    pub(crate) report: bool,
    pub(crate) artifacts: &'a [TaskArtifact],
}
impl From<bool> for EvidenceSeed<'_> {
    fn from(report: bool) -> Self {
        Self {
            report,
            artifacts: &[],
        }
    }
}
pub(crate) struct CleanLeaf {
    pub(crate) pair: Pair,
    pub(crate) gate: Option<Gate>,
    pub(crate) drain: String,
    pub(crate) leaf: String,
    pub(crate) task: String,
    pub(crate) bound: OrbitRuntime,
    pub(crate) base: String,
    pub(crate) input: Value,
    pub(crate) evidence_reads: Arc<Mutex<Vec<(String, Value)>>>,
    pub(crate) lose_evidence_read: Arc<Mutex<bool>>,
}
/// Seed the unobserved clean Git graph in-process. Every leaf still has its
/// own objects, index, refs and bare origin, and its commit names its real task.
/// Production actions below continue to use their ordinary Git subprocesses.
pub(crate) fn prepare_clean_owner(pair: &Pair, task: &str) -> String {
    let url = "https://github.com/owner/repository.git";
    let repo = &pair.owner_repo;
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    let owner = git2::Repository::init_opts(
        repo,
        git2::RepositoryInitOptions::new().initial_head("main"),
    )
    .unwrap();
    let mut index = owner.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = owner.find_tree(index.write_tree().unwrap()).unwrap();
    let author = git2::Signature::now("Orbit Test", "test@orbit.invalid").unwrap();
    let base = owner
        .commit(
            Some("HEAD"),
            &author,
            &author,
            &format!("Deliver [{task}]"),
            &tree,
            &[],
        )
        .unwrap();
    let origin = repo.with_file_name("origin.git");
    git2::Repository::init_opts(
        &origin,
        git2::RepositoryInitOptions::new()
            .bare(true)
            .initial_head("main"),
    )
    .unwrap();
    let redirect = format!("url.{}.insteadOf", origin.display());
    owner.config().unwrap().set_str(&redirect, url).unwrap();
    owner
        .remote("origin", url)
        .unwrap()
        .push(&["refs/heads/main:refs/heads/main"], None)
        .unwrap();
    base.to_string()
}
/// Match the replica's initial fetch/reset after the leaf has been admitted.
pub(crate) fn prepare_clean_follower(pair: &Pair, task: &str, base: &str) {
    let url = "https://github.com/owner/repository.git";
    let redirect = format!(
        "url.{}.insteadOf",
        pair.owner_repo.with_file_name("origin.git").display()
    );
    let follower = git2::Repository::init_opts(
        &pair.follower_repo,
        git2::RepositoryInitOptions::new().initial_head("main"),
    )
    .unwrap();
    follower.config().unwrap().set_str(&redirect, url).unwrap();
    follower
        .remote("origin", url)
        .unwrap()
        .fetch(&["main"], None, None)
        .unwrap();
    let commit = follower
        .find_commit(git2::Oid::from_str(base).unwrap())
        .unwrap();
    follower
        .reset(commit.as_object(), git2::ResetType::Hard, None)
        .unwrap();
    let branch = format!("orbit/{task}");
    follower.branch(&branch, &commit, false).unwrap();
    follower.set_head(&format!("refs/heads/{branch}")).unwrap();
}
impl CleanLeaf {
    pub(crate) fn new(already_landed: bool, completion: &str) -> Self {
        Self::build(already_landed, completion, &[], true)
    }

    /// A clean leaf whose owner already holds unrelated evidence. This avoids
    /// creating a large worker mutation receipt just to prepare read fixtures.
    pub(crate) fn with_evidence(
        already_landed: bool,
        completion: &str,
        artifacts: &[TaskArtifact],
    ) -> Self {
        Self::build_requiring(
            already_landed,
            completion,
            &[],
            EvidenceSeed {
                report: true,
                artifacts,
            },
            None,
            CHECK,
            "",
            None,
            None,
        )
    }

    /// A clean leaf whose task carries the `no-diff-expected` tag, claimed by
    /// the follower through ordinary pull admission [ORB-14474].
    pub(crate) fn tagged(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            true,
        )
    }

    /// A tagged review whose implementer files findings and attaches its
    /// coverage, but writes no clean-tree report [ORB-14791].
    pub(crate) fn review_only(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            false,
        )
    }

    /// A tagged clean leaf whose task a `delivery-code-review` auto-task
    /// minted. Like `review_only`, it writes no clean-tree report [ORB-14791].
    pub(crate) fn delivery_review(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[
                orbit_types::task::NO_DIFF_EXPECTED_TAG,
                "delivery-code-review",
                "auto-task:delivery-code-review",
            ],
            false,
        )
    }

    /// A verified clean leaf whose follower names a baseline gate beside the
    /// required command, as `make ci-test-affected` is in the shipped
    /// configuration. The implementer may hand its Bubblewrap deferrals off
    /// by setting `implementation.deferred_sandbox_validation` [ORB-15287].
    /// A `no-diff-expected` skip replays nothing [ORB-15764], so the leaf
    /// proves its clean tree with `no-diff.json`.
    pub(crate) fn gated(completion: &str) -> Self {
        Self::gated_on(completion, GateOn::Both)
    }

    /// [`Self::gated`], with the gate named only where `on` says.
    pub(crate) fn gated_on(completion: &str, on: GateOn) -> Self {
        Self::build_with(false, completion, &[], true, Some(on))
    }

    pub(crate) fn build(
        already_landed: bool,
        completion: &str,
        tags: &[&str],
        report: bool,
    ) -> Self {
        Self::build_with(already_landed, completion, tags, report, None)
    }

    pub(crate) fn build_with(
        already_landed: bool,
        completion: &str,
        tags: &[&str],
        report: bool,
        gated: Option<GateOn>,
    ) -> Self {
        Self::build_requiring(
            already_landed,
            completion,
            tags,
            report.into(),
            gated,
            CHECK,
            "",
            None,
            None,
        )
    }

    /// A verified clean leaf whose owner and follower require `red`'s command
    /// instead of [`CHECK`] [ORB-15427].
    pub(crate) fn red(red: &RedCheck) -> Self {
        Self::red_on(red, None, None)
    }

    /// [`Self::red`] admitted with the owner and follower on `owner` and
    /// `follower`. `None` leaves the runtime's own host OS.
    pub(crate) fn red_on(
        red: &RedCheck,
        owner: Option<orbit_types::task::HostOs>,
        follower: Option<orbit_types::task::HostOs>,
    ) -> Self {
        Self::build_requiring(
            false,
            "done",
            &[],
            true.into(),
            None,
            &red.command(),
            "",
            owner,
            follower,
        )
    }

    /// A tagged review whose owner and follower require `check`'s command and
    /// name the [`Gate`] as a baseline command: the skip must run neither
    /// [ORB-15764].
    pub(crate) fn tagged_requiring(
        completion: &str,
        check: &RedCheck,
        artifacts: &[TaskArtifact],
    ) -> Self {
        Self::build_requiring(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            EvidenceSeed {
                report: false,
                artifacts,
            },
            Some(GateOn::Both),
            &check.command(),
            "",
            None,
            None,
        )
    }

    /// An untagged clean leaf whose implementer reports `no-diff.json` and
    /// names `selectors` as its already-resolved outcome, on a follower whose
    /// validation PATH resolves `cargo` to `replay` [ORB-15747].
    #[cfg(unix)]
    pub(crate) fn resolved(replay: &Replay, selectors: &[&str]) -> Self {
        let mut fixture = Self::build_requiring(
            false,
            "done",
            &[],
            true.into(),
            None,
            CHECK,
            &replay.config(),
            None,
            None,
        );
        fixture.input["implementation"]["already_resolved"] = json!({"selectors": selectors});
        fixture
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_requiring(
        already_landed: bool,
        completion: &str,
        tags: &[&str],
        evidence: EvidenceSeed<'_>,
        gated: Option<GateOn>,
        required: &str,
        follower_config: &str,
        owner_os: Option<orbit_types::task::HostOs>,
        follower_os: Option<orbit_types::task::HostOs>,
    ) -> Self {
        let config = format!(
            "[workflow]\ndistributed_completion = \"{completion}\"\nrequired_validation_commands = [\"{required}\"]\n[review]\nbefore_pr = true\n[operation]\nreview_crew = \"sol\"\n"
        );
        let gate = gated.map(|_| Gate::new());
        let naming = |named: bool| match (&gate, named) {
            (Some(gate), true) => config.replace(
                "before_pr = true\n",
                &format!(
                    "before_pr = true\nbaseline_commands = [\"{}\"]\n",
                    gate.command()
                ),
            ),
            _ => config.clone(),
        };
        let owner_config = naming(gated.is_some_and(GateOn::owner));
        let config = naming(gated.is_some_and(GateOn::follower)) + follower_config;
        let mut pair = match owner_os {
            Some(os) => Pair::with_owner_host(&owner_config, "", &[None], |owner| {
                owner.with_host_os(Some(os))
            }),
            None => Pair::with_owner_config(&owner_config, &[None]),
        };
        if let Some(os) = follower_os {
            pair.follower = pair.follower.clone().with_host_os(Some(os));
        }
        let task = pair.tasks[0].clone();
        let base = prepare_clean_owner(&pair, &task);
        if !tags.is_empty() {
            pair.wire
                .owner
                .update_task_as_human(
                    &task,
                    orbit_core::application::task::TaskUpdateParams {
                        tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
                        ..Default::default()
                    },
                    "fixture operator".into(),
                )
                .unwrap();
        }
        if !evidence.artifacts.is_empty() {
            pair.wire
                .owner
                .update_task_as_human(
                    &task,
                    orbit_core::application::task::TaskUpdateParams {
                        upsert_artifacts: evidence.artifacts.to_vec(),
                        ..Default::default()
                    },
                    "fixture operator".into(),
                )
                .unwrap();
        }
        let drain = pair.run_drain();
        let leaf = pair.launched_leaf(&drain, 1, std::process::id());
        assert_eq!(pair.claimed_task(&leaf), task);

        prepare_clean_follower(&pair, &task, &base);
        let follower = &pair.follower_repo;
        std::fs::write(follower.join(".orbit/config.toml"), &config).unwrap();
        let record = pair.admission(&leaf);
        let claim = record.receipt.as_ref().unwrap().claim.as_ref().unwrap();
        let evidence_reads = Arc::new(Mutex::new(Vec::new()));
        let lose_evidence_read = Arc::new(Mutex::new(false));
        let bound =
            OrbitRuntime::from_roots(&pair.follower.global_root(), &follower.join(".orbit"))
                .unwrap()
                .with_automation_machine_identity(Some(FOLLOWER.into()))
                .with_coordination_write_owner(Some(OWNER.into()))
                .with_drain_owner_transport(pair.wire.clone())
                .with_worker_invocation(
                    WorkerInvocation {
                        owner_machine_id: OWNER.into(),
                        owner_workspace_id: record.destination.owner_workspace_id.clone(),
                        owner_destination: record.destination.selector.clone(),
                        task_id: task.clone(),
                        claim_id: claim.claim_id.clone(),
                        execution: claim.executed_on.clone(),
                        bound_run_id: leaf.clone(),
                    },
                    Arc::new(EvidenceOwner {
                        owner: ToOwner(pair.wire.owner.clone()),
                        reads: evidence_reads.clone(),
                        lose_read: lose_evidence_read.clone(),
                    }),
                )
                .unwrap();
        let bound = calm_host(bound);
        let input = json!({
            "job_run_id": leaf, "run_id": leaf, "scope": "all",
            "workspace_path": follower, "base_sha": base, "base_ref": "origin/main",
            "base": "main", "base_sync": "remote", "completed_task_ids": [task],
            "verify_already_landed": true,
            "implementation": {"execution_summary": "Outcome: success\nCurrent base satisfies the acceptance criteria; no change needed."},
        });
        let mut fixture = Self {
            pair,
            gate,
            drain,
            leaf,
            task,
            bound,
            base,
            input,
            evidence_reads,
            lose_evidence_read,
        };
        if !evidence.report {
            return fixture;
        }
        fixture.write_evidence(
            "implementation-validation.json",
            json!({
                "run_id": fixture.leaf, "tested_head": fixture.base,
                "command": CHECK, "exit_code": 0, "output": "",
            }),
        );
        let report = if already_landed {
            let task = RuntimeHost::get_task(&fixture.bound, &fixture.task).unwrap();
            let comments = RuntimeHost::get_task_comments(&fixture.bound, &fixture.task).unwrap();
            json!({
                "schema_version": 1, "task_id": fixture.task, "run_id": fixture.leaf,
                "tested_head": fixture.base, "covering_commit": fixture.base,
                "covering_task_id": fixture.task, "scope": already_landed_scope(&task, &comments),
                "required_commands": [CHECK], "criteria_evidence": ["The base contains the delivered scope and validation passes."],
                "validation": [{"command": CHECK, "outcome": "passed", "role": "required", "log_artifact": "implementation-validation.json"}],
            })
        } else {
            json!({
                "schema_version": 1, "task_id": fixture.task, "run_id": fixture.leaf,
                "tested_head": fixture.base, "reason": "The requested behavior is already satisfied on this clean base.",
                "validation": [{"command": CHECK, "exit_code": 0, "log_artifact": "implementation-validation.json"}],
            })
        };
        fixture.write_evidence(
            if already_landed {
                "already-landed.json"
            } else {
                "no-diff.json"
            },
            report,
        );
        fixture
    }

    pub(crate) fn write_evidence(&mut self, path: &str, value: Value) {
        let scratch = self.pair.follower_repo.join(".orbit/tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join(path), serde_json::to_vec(&value).unwrap()).unwrap();
        let artifacts = self.input["implementation"]
            .as_object_mut()
            .unwrap()
            .entry("no_diff_artifacts")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        artifacts.push(json!({"path": path, "source_path": format!(".orbit/tmp/{path}")}));
    }

    /// Real stored media sufficient to exceed the old numeric-byte response.
    /// Every case stores its own copy before admission, in one public owner
    /// update, without preparing an unrelated worker replay receipt.
    pub(crate) fn large_unrelated_evidence() -> Vec<TaskArtifact> {
        (0..17)
            .map(|index| TaskArtifact {
                path: format!("qa/media-{index}.bin"),
                media_type: "application/octet-stream".into(),
                content: vec![255; 1_048_576],
                created_by: None,
            })
            .collect()
    }

    pub(crate) fn assert_bulk_evidence_exceeds_ceiling(&self) {
        let error = RuntimeHost::get_task_artifacts(&self.bound, &self.task).expect_err(
            "ORB-16120: former bulk evidence read must overflow the unchanged MCP ceiling",
        );
        assert!(error.to_string().contains("67108864"), "{error}");
        self.evidence_reads.lock().unwrap().clear();
    }

    pub(crate) fn assert_named_reads_and_retained_evidence(&self, artifacts: &[TaskArtifact]) {
        let reads = self.evidence_reads.lock().unwrap();
        assert!(
            !reads
                .iter()
                .any(|(_, input)| input["_worker_read"] == "artifacts"),
            "ORB-16120: verification must not request unrelated evidence: {reads:?}"
        );
        assert!(
            reads
                .iter()
                .any(|(name, input)| name == "orbit.task.artifact.get"
                    && input["path"]
                        .as_str()
                        .is_some_and(|path| path.starts_with("no-diff-handoff/"))),
            "checkpoint bytes must cross the real authorized owner boundary: {reads:?}"
        );
        assert!(
            !reads
                .iter()
                .any(|(name, input)| name == "orbit.task.artifact.get"
                    && input["path"]
                        .as_str()
                        .is_some_and(|path| path.starts_with("qa/"))),
            "verification must never request unrelated media payloads"
        );
        let retained = self.pair.wire.owner.get_task_artifacts(&self.task).unwrap();
        for artifact in artifacts {
            let kept = retained
                .iter()
                .find(|kept| kept.path == artifact.path)
                .unwrap();
            assert_eq!(
                kept.content, artifact.content,
                "unrelated evidence must remain intact"
            );
        }
    }

    pub(crate) fn attach(&self, path: &str, value: Value) {
        self.bound
            .attach_claim_validation_log(path, serde_json::to_vec(&value).unwrap())
            .unwrap();
    }

    pub(crate) fn action(&self, action: &str, input: &Value) -> Value {
        if action == "review_gate_admit" {
            self.bound
                .run_deterministic(action, &json!({}), input, ToolContext::default())
                .unwrap()
        } else {
            engine_action(&self.bound, action, input).unwrap()
        }
    }

    /// The `claim_validate` input after the real commit verifier and review
    /// gate ran on the clean checkout.
    pub(crate) fn validate_input(&self) -> Value {
        let committed = self.action("git_commit", &self.input);
        assert!(
            matches!(
                committed["decision"].as_str(),
                Some("verified_no_diff" | "verified_already_landed" | "skipped_no_diff_expected")
            ),
            "{committed}"
        );
        let mut input = self.input.clone();
        input["skipped_no_diff_expected"] = committed["skipped_no_diff_expected"].clone();
        input["already_landed_checkpoint"] = committed;
        let review = self.action("review_gate_admit", &input);
        assert_eq!(
            review["applies"], false,
            "a clean base opens no PR and runs no reviewer: {review}"
        );
        input
    }

    /// Advance the owner's published base by one commit, as a busy drain does
    /// while a claimed chore runs, and return the new tip.
    pub(crate) fn advance_base(&self, round: usize) -> String {
        let repo = &self.pair.owner_repo;
        std::fs::write(repo.join("src/advanced.rs"), format!("// round {round}\n")).unwrap();
        git(repo, &["add", "src/advanced.rs"]);
        git(
            repo,
            &["commit", "-q", "-m", &format!("Base advances {round}")],
        );
        publish_origin(repo);
        git(repo, &["rev-parse", "HEAD"]).trim().to_string()
    }

    pub(crate) fn prepare_handoff(&self) -> TaskHandoff {
        let mut input = self.validate_input();
        let validated = self.action("claim_validate", &input);
        assert_eq!(validated["candidate"]["delivery"]["kind"], "no_diff");
        input["no_diff_evidence"] = validated["no_diff_evidence"].clone();
        input["candidate"] = validated["candidate"].clone();
        input["validation"] = validated["validation"].clone();
        let delivered = self.action("claim_handoff", &input);
        assert_eq!(delivered["delivery"], "no_diff");
        assert_eq!(delivered["merged"], false);
        let admission = self.pair.admission(&self.leaf);
        let ClaimMutation::AcceptHandoff(handoff) = admission.settlement.unwrap() else {
            panic!("typed handoff required")
        };
        handoff
    }

    pub(crate) fn run_clean_pipeline(&self, name: &str) -> TaskHandoff {
        let outcome = self.execute_clean_pipeline(name).unwrap();
        assert!(outcome.success, "{outcome:#?}");
        for step in ["prepare_branch", "sync_base", "review", "push", "pr_open"] {
            assert!(outcome.pipeline.get(step).is_none(), "{outcome:#?}");
        }
        let admission = self.pair.admission(&self.leaf);
        let ClaimMutation::AcceptHandoff(handoff) = admission.settlement.unwrap() else {
            panic!("shipped clean pipeline must record a typed handoff")
        };
        handoff
    }

    pub(crate) fn execute_clean_pipeline(
        &self,
        name: &str,
    ) -> Result<orbit_engine::JobOutcome, orbit_engine::DispatchError> {
        use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
        use orbit_engine::{
            V2AuditWriter, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
        };

        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut catalog = V2ActivityCatalog::new();
        for entry in std::fs::read_dir(assets.join("activities")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "yaml") {
                let asset = load_activity_asset(&std::fs::read_to_string(path).unwrap()).unwrap();
                catalog.insert(asset.name, asset.spec);
            }
        }
        let mut job = load_job_asset(
            &std::fs::read_to_string(assets.join("jobs").join(format!("{name}.yaml"))).unwrap(),
        )
        .unwrap()
        .spec;
        resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
        // Resume after the real deterministic commit verifier. No agent or
        // worker is dispatched; the shipped delivery steps execute unchanged.
        let input = json!({"task_ids": [self.task], "base_sync": "remote"});
        let worktree = self.input.clone();
        let implementation = self.input["implementation"].clone();
        let committed = self.action("git_commit", &self.input);
        let mut resume = PipelineState::new(self.leaf.clone(), name.into(), input.clone());
        resume.record_step(0, JobRunState::Success, Some(worktree), None);
        resume.record_step(1, JobRunState::Success, Some(json!({})), None);
        resume.compound_outputs.insert(
            1,
            [("implement_one".into(), implementation)]
                .into_iter()
                .collect(),
        );
        resume.record_step(2, JobRunState::Success, Some(committed), None);
        let audit = V2AuditWriter::with_disk_sinks(
            &self.pair.follower_repo.join(".orbit/tmp/pipeline-audit"),
            self.bound.v2_audit_store().unwrap(),
            self.bound.workspace_id().unwrap(),
            &self.leaf,
            "clean-leaf-fixture",
            Some(&self.pair.follower_repo),
        )
        .unwrap();
        execute_job_with_resume(&job, input, &self.leaf, audit, &self.bound, Some(&resume))
    }

    pub(crate) fn settle(&self, handoff: &TaskHandoff) -> Result<Value, OrbitError> {
        self.pair.wire.call(
            "",
            "orbit.drain.claim.settle",
            json!({
                "claim_id": handoff.claim_id, "run_id": self.leaf,
                "settlement": ClaimMutation::AcceptHandoff(handoff.clone()),
            }),
        )
    }

    /// The refusal of a shipped clean pipeline that must refuse.
    pub(crate) fn refusal_of(&self, name: &str) -> String {
        match self.execute_clean_pipeline(name) {
            Err(error) => error.to_string(),
            Ok(outcome) => {
                assert!(!outcome.success, "{name} must refuse: {outcome:#?}");
                format!("{outcome:#?}")
            }
        }
    }

    pub(crate) fn assert_no_blocked(&self) {
        let task = self.pair.owner_task(&self.task);
        assert!(
            task["history"]
                .as_array()
                .unwrap()
                .iter()
                .all(|event| event["to_status"] != "blocked"),
            "{task:#}"
        );
        assert!(
            task["external_refs"].as_array().unwrap().is_empty(),
            "NoDiff creates no PR: {task:#}"
        );
    }
}
/// The frozen batch an owner's after-landing review consumer admits for
/// `task`: one delivery on the `reviewed` branch, published beside `main`
/// so the clean base the leaf reviews does not move.
pub(crate) fn reviewed_batch(
    fixture: &CleanLeaf,
) -> orbit_types::workflow::automation::BatchAttempt {
    use orbit_types::workflow::automation::{
        BatchAttempt, BatchState, CoverageBatch, CoverageClass, Delivery,
    };
    let repo = &fixture.pair.owner_repo;
    git(repo, &["checkout", "-q", "-b", "reviewed"]);
    std::fs::write(repo.join("src/reviewed.rs"), "fn reviewed() {}\n").unwrap();
    git(repo, &["add", "src/reviewed.rs"]);
    git(repo, &["commit", "-q", "-m", "Squash-merge #42"]);
    git(repo, &["push", "-q", "origin", "HEAD:refs/heads/reviewed"]);
    git(repo, &["checkout", "-q", "main"]);
    git(repo, &["fetch", "-q", "origin"]);
    let revision = |spec: &str| {
        let commit = git(repo, &["rev-parse", spec]).trim().to_string();
        let tree = git(repo, &["rev-parse", &format!("{commit}^{{tree}}")]);
        SourceRevision {
            commit,
            tree: tree.trim().to_string(),
        }
    };
    let (before, after) = (revision(&fixture.base), revision("reviewed"));
    let owner = &fixture.pair.wire.owner;
    let consumer = format!(
        "{OWNER}/{}/auto-task/landed-review",
        owner.workspace_id().unwrap()
    );
    BatchAttempt {
        batch: CoverageBatch {
            schema_version: 1,
            id: "batch-reviewed".into(),
            consumer,
            epoch: "epoch".into(),
            repository: "owner/repository".into(),
            branch: "reviewed".into(),
            coverage: CoverageClass::LandedCodeReviewV1,
            from_exclusive: before.clone(),
            through_inclusive: after.clone(),
            commits: vec![after.commit.clone()],
            deliveries: vec![Delivery {
                key: "pr:owner/repository:reviewed:42".into(),
                repository: "owner/repository".into(),
                branch: "reviewed".into(),
                before,
                commits: vec![after.commit.clone()],
                after,
                task_ids: vec![],
                unattributed: None,
                evidence_reference: "pull:42".into(),
                evidence_digest: "digest".into(),
                landed_at: Utc::now(),
            }],
            exclusions: vec![],
            created_at: Utc::now(),
            max_attempts: 1,
            retry_until: Utc::now() + chrono::Duration::hours(4),
        },
        input_digest: "input-digest".into(),
        attempt: 1,
        action_key: "automation:batch-reviewed:1".into(),
        action_id: Some(fixture.task.clone()),
        state: BatchState::Admitted,
        reason: None,
        retry_after: None,
        reissue: None,
    }
}
/// Record `attempt` as its consumer's admitted action on the owner.
pub(crate) fn admit(
    owner: &OrbitRuntime,
    attempt: &orbit_types::workflow::automation::BatchAttempt,
) {
    use orbit_types::workflow::automation::AutomationState;
    let from = attempt.batch.from_exclusive.clone();
    let baseline = AutomationState {
        members: None,
        consumer: attempt.batch.consumer.clone(),
        epoch: attempt.batch.epoch.clone(),
        trigger: None,
        repository: attempt.batch.repository.clone(),
        branch: attempt.batch.branch.clone(),
        generation: 0,
        baseline: from.clone(),
        observed: from.clone(),
        covered: from,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        active: None,
        stall: None,
    };
    let store = owner.automation_store().unwrap();
    assert!(store.automation_initialize(&baseline).unwrap());
    let mut admitted = baseline.clone();
    admitted.generation = 1;
    admitted.observed = attempt.batch.through_inclusive.clone();
    admitted.pending_commits = attempt.batch.commits.clone();
    admitted.pending = attempt.batch.deliveries.clone();
    admitted.active = Some(attempt.clone());
    assert!(store.automation_commit(&baseline, &admitted, None).unwrap());
}
/// The one file named `name` under `root`.
pub(crate) fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}
/// The check the implement step boundary runs on the implementer's deferral
/// `record`: its gate summary is derived against `gates`, and its probe must
/// show the sandbox's refusal.
pub(crate) fn check_deferral(
    record: &Value,
    gates: &orbit_types::workflow::HostValidationGates,
    base: &str,
) -> Result<orbit_types::workflow::DeferredSandboxValidation, String> {
    let output = json!({"deferred_sandbox_validation": {"summary": ".orbit/tmp/gate.json"}});
    let handoff =
        orbit_types::workflow::deferral_handoff(&output)?.expect("the handoff names a summary");
    let summary = json!({
        "schema_version": 1,
        "command": record["command"],
        "exit_code": record["exit_code"],
        "tests_run": record["tests_run"],
        "base": record["base"],
        "notices": record["notices"],
    });
    let probe = serde_json::from_value(record["probe"].clone()).expect("the probe decodes");
    handoff
        .derive(
            &serde_json::to_vec(&summary).expect("summary encodes"),
            gates,
            Some(base),
            orbit_types::workflow::DeferralLane {
                claimed: true,
                os: orbit_types::workflow::EvidenceHostOs::current(),
            },
        )?
        .complete(vec![probe])
}
impl CleanLeaf {
    /// The implementer's record of a passing gate whose only gap is a
    /// Bubblewrap deferral, backed by a failing namespace probe [ORB-15287].
    pub(crate) fn hand_off_deferral(&mut self) {
        self.input["implementation"]["deferred_sandbox_validation"] = self.deferral(false);
    }

    /// The implementer's record of the gate passing with one sandbox's
    /// deferrals: Seatbelt's, as a macOS lane reports them, or Bubblewrap's.
    pub(crate) fn deferral(&self, seatbelt: bool) -> Value {
        let (notice, probe) = if seatbelt {
            (
                Gate::seatbelt_notice(),
                json!({
                    "command": orbit_types::workflow::SEATBELT_APPLY_PROBE,
                    "exit_code": orbit_types::workflow::SEATBELT_APPLY_REFUSAL_EXIT,
                    "output": orbit_types::workflow::SEATBELT_APPLY_REFUSAL,
                }),
            )
        } else {
            (
                Gate::notice(),
                json!({
                    "command": orbit_types::workflow::BUBBLEWRAP_NAMESPACE_PROBE,
                    "exit_code": 1,
                    "output": "bwrap: No permissions to create new namespace",
                }),
            )
        };
        json!({
            "command": self.gate.as_ref().unwrap().command(),
            "exit_code": 0,
            "tests_run": 2164,
            "base": self.base,
            "notices": [notice],
            "probe": probe,
        })
    }

    /// The commands the owner holds a passing log for.
    pub(crate) fn owner_validated_commands(&self, handoff: &TaskHandoff) -> Vec<String> {
        let artifacts = RuntimeHost::get_task_artifacts(&self.pair.wire.owner, &self.task).unwrap();
        handoff
            .validation
            .iter()
            .map(|reference| {
                let artifact = artifacts
                    .iter()
                    .find(|artifact| artifact.path == reference.path)
                    .unwrap();
                let log: Value = serde_json::from_slice(&artifact.content).unwrap();
                assert_eq!(log["exit_code"], 0, "{log}");
                log["command"].as_str().unwrap().to_string()
            })
            .collect()
    }

    /// Run a shipped clean pipeline that must refuse, and return the refusal.
    pub(crate) fn refused_pipeline(&self, name: &str) -> String {
        let refusal = self.refusal_of(name);
        assert!(
            self.pair.admission(&self.leaf).settlement.is_none(),
            "a refused leaf records no handoff"
        );
        let artifacts = RuntimeHost::get_task_artifacts(&self.pair.wire.owner, &self.task).unwrap();
        let logs = artifacts
            .iter()
            .filter(|artifact| artifact.path.starts_with("validation/"))
            .collect::<Vec<_>>();
        assert_eq!(
            logs.len(),
            1,
            "the refused command retains its diagnostic log"
        );
        assert!(
            logs[0].path.ends_with(".failed.json"),
            "no passing handoff log: {}",
            logs[0].path
        );
        let log: Value = serde_json::from_slice(&logs[0].content).unwrap();
        assert_eq!(
            log["exit_code"], 0,
            "the zero-exit replay still lacks executed coverage"
        );
        assert!(
            refusal.contains(&logs[0].path),
            "the refusal points to its evidence"
        );
        self.assert_no_blocked();
        refusal
    }
}
pub(crate) const NO_DIFF_ROUTES: [&str; 2] =
    ["task_claimed_pr_pipeline", "task_claimed_local_pipeline"];
/// The known-red record a `known_red_validation` history event of a task's
/// JSON view carries as its typed decision, beside the record's note.
pub(crate) fn known_red_decision(event: &Value) -> orbit_types::workflow::KnownRedValidation {
    assert_eq!(event["decision"]["kind"], "known_red_validation", "{event}");
    let Ok(orbit_types::task::TaskDecision::KnownRedValidation(known)) =
        serde_json::from_value(event["decision"].clone())
    else {
        panic!("the decision is a known red: {event}");
    };
    assert!(
        event["note"]
            .as_str()
            .is_some_and(|note| note.starts_with(&known.text(""))),
        "the note still carries the record: {event}"
    );
    known
}
impl RedCheck {
    /// From now on the command runs `body` wherever it runs, still counted.
    pub(crate) fn runs_as(&self, body: &str) {
        std::fs::write(
            &self.script,
            format!("echo ran >> '{}'\n{body}\n", self.runs.display()),
        )
        .unwrap();
    }
}
/// A stand-in `cargo` the follower's validation PATH resolves first, for an
/// already-resolved replay [ORB-15747]. It records each argv and prints what
/// it was last told to, from outside both checkouts so neither tree changes.
#[cfg(unix)]
pub(crate) struct Replay {
    pub(crate) _dir: TempDir,
    pub(crate) bin: PathBuf,
    pub(crate) script: PathBuf,
    pub(crate) runs: PathBuf,
}
#[cfg(unix)]
impl Replay {
    /// The incident's selector: a test that skips inside the agent sandbox.
    pub(crate) const SELECTOR: &'static str =
        "cargo test -p orbit-cli --test process cli_ctrl_c_during_proc_spawn_reports_interrupt";
    pub(crate) const TEST: &'static str = "cli_ctrl_c_during_proc_spawn_reports_interrupt";

    pub(crate) fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let replay = Self {
            bin: dir.path().join("bin"),
            script: dir.path().join("replay.sh"),
            runs: dir.path().join("replay.runs"),
            _dir: dir,
        };
        std::fs::create_dir_all(&replay.bin).unwrap();
        let cargo = replay.bin.join("cargo");
        std::fs::write(
            &cargo,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexec sh '{}'\n",
                replay.runs.display(),
                replay.script.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        replay.passes(1);
        replay
    }

    /// The follower configuration that puts this `cargo` first.
    pub(crate) fn config(&self) -> String {
        format!(
            "[workflow.validation_env]\nlogin_shell = false\npath = [\"{}\"]\n",
            self.bin.display()
        )
    }

    pub(crate) fn set(&self, body: &str) {
        std::fs::write(&self.script, body).unwrap();
    }

    /// libtest's report of `tests` passing tests.
    pub(crate) fn passes(&self, tests: u64) {
        self.set(&format!(
            "echo 'test {} ... ok'\necho 'test result: ok. {tests} passed; 0 failed; 0 ignored; \
             0 measured; 0 filtered out; finished in 0.01s'\n",
            Self::TEST
        ));
    }

    /// The named test still fails natively.
    pub(crate) fn fails(&self) {
        self.set(&format!(
            "echo 'test {test} ... FAILED'\necho 'interrupt was not reported'\necho 'test \
             result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.01s'\nexit 101\n",
            test = Self::TEST
        ));
    }

    /// The test returns without exercising its path, even natively.
    pub(crate) fn skips(&self) {
        self.set(&format!(
            "echo 'skipping: {} needs a terminal'\necho 'test result: ok. 1 passed; 0 failed; \
             0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\n",
            Self::TEST
        ));
    }

    /// Each argv the stand-in was run with.
    pub(crate) fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.runs)
            .map(|runs| runs.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// The command line the claim host runs for [`Self::SELECTOR`].
    pub(crate) fn host_command() -> String {
        format!("{} -- --nocapture", Self::SELECTOR)
    }
}

impl CleanLeaf {
    /// The empty coverage every clean review attaches before its pipeline.
    pub(crate) fn attach_coverage(&self) {
        self.attach("automation-coverage.json", json!({"findings": []}));
    }

    /// The owner's accepted handoff for this leaf's claim.
    pub(crate) fn accepted(
        &self,
        handoff: &TaskHandoff,
    ) -> orbit_types::workflow::handoff::AcceptedHandoff {
        self.pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .unwrap()
    }

    /// Land the accepted handoff on the owner and return the landing.
    pub(crate) fn land(&self, handoff: &TaskHandoff) -> Value {
        let accepted = self.accepted(handoff);
        engine_action(
            &self.pair.wire.owner,
            "handoff_land",
            &json!({"handoff_id": accepted.handoff_id}),
        )
        .unwrap()
    }

    /// Settle and assert the owner holds the task for review.
    pub(crate) fn settle_review(&self, handoff: &TaskHandoff) -> Value {
        let settled = self.settle(handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{settled}");
        settled
    }

    /// Assert the owner's status for this leaf's task.
    pub(crate) fn assert_status(&self, status: &str) {
        assert_eq!(self.pair.owner_status(&self.task), status);
    }

    /// Assert no handoff was recorded for this leaf.
    pub(crate) fn assert_no_handoff(&self) {
        assert!(
            self.pair.admission(&self.leaf).settlement.is_none(),
            "a refused leaf records no handoff"
        );
    }

    /// Every artifact the owner holds for this leaf's task.
    pub(crate) fn owner_artifacts(&self) -> Vec<TaskArtifact> {
        RuntimeHost::get_task_artifacts(&self.pair.wire.owner, &self.task).unwrap()
    }
}
