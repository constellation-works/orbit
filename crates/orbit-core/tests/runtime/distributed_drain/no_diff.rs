//! Real clean-base claim execution and owner settlement, without a provider PR.
use super::claimed_review::ToOwner;
use super::*;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::already_landed_scope;

const CHECK: &str = "git diff --exit-code";

fn engine_action(host: &OrbitRuntime, action: &str, input: &Value) -> Result<Value, OrbitError> {
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
struct Gate {
    _dir: TempDir,
    script: PathBuf,
    runs: PathBuf,
}

/// Which hosts' `review.baseline_commands` name the [`Gate`]. Per-host
/// configuration is untracked, so the replica's copy can drift from the
/// owner's [ORB-15622].
#[derive(Clone, Copy, PartialEq, Eq)]
enum GateOn {
    Both,
    OwnerOnly,
    FollowerOnly,
}

impl GateOn {
    fn owner(self) -> bool {
        self != Self::FollowerOnly
    }

    fn follower(self) -> bool {
        self != Self::OwnerOnly
    }
}

impl Gate {
    fn new() -> Self {
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
    fn command(&self) -> String {
        format!("sh '{}'", self.script.display())
    }

    fn set(&self, body: &str) {
        std::fs::write(
            &self.script,
            format!("echo ran >> '{}'\n{body}\n", self.runs.display()),
        )
        .unwrap();
    }

    fn summary(tests: u64) -> String {
        format!(
            "printf '{{\"schema_version\":1,\"selection\":{{\"packages\":[\"fixture\"]}},\
             \"tests_run\":{tests}}}' > \"$ORBIT_VALIDATION_SUMMARY\""
        )
    }

    /// Runs `tests` tests and reports them, as `make ci-test-affected` does.
    fn executes(&self, tests: u64) {
        self.set(&Self::summary(tests));
    }

    /// Defers its Bubblewrap tests again: this host cannot create the
    /// namespaces either.
    fn defers(&self) {
        self.set(&format!(
            "echo '{}' >&2\n{}",
            Self::notice(),
            Self::summary(3)
        ));
    }

    fn notice() -> String {
        orbit_exec::bwrap_deferral_notice(
            "spawn_under_linux_bwrap",
            "bwrap: No permissions to create new namespace",
        )
    }

    fn seatbelt_notice() -> String {
        orbit_exec::seatbelt_deferral_notice(
            "spawn_under_macos_sandbox",
            orbit_types::workflow::SEATBELT_APPLY_REFUSAL,
        )
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.runs).map_or(0, |runs| runs.lines().count())
    }
}

/// A required command that fails with libtest's report of one test until
/// told to pass, kept outside both checkouts so neither tree changes; each
/// run, on either host, is counted [ORB-15427].
struct RedCheck {
    _dir: TempDir,
    script: PathBuf,
    passes: PathBuf,
    runs: PathBuf,
}

impl RedCheck {
    /// The failure identity its report names.
    const FAILURE: &'static str = "test suite::broken";

    fn new() -> Self {
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

    fn command(&self) -> String {
        format!("sh '{}'", self.script.display())
    }

    /// From now on the command passes, wherever it runs.
    fn pass(&self) {
        std::fs::write(&self.passes, "").unwrap();
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.runs).map_or(0, |runs| runs.lines().count())
    }
}

/// Preserve the real owner/claim boundary while enforcing the production
/// MCP line ceiling without allocating a giant serialized response.
struct EvidenceOwner {
    owner: ToOwner,
    reads: Arc<Mutex<Vec<(String, Value)>>>,
    lose_read: Arc<Mutex<bool>>,
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
        struct Count(usize);
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
struct EvidenceSeed<'a> {
    report: bool,
    artifacts: &'a [TaskArtifact],
}

impl From<bool> for EvidenceSeed<'_> {
    fn from(report: bool) -> Self {
        Self {
            report,
            artifacts: &[],
        }
    }
}

struct CleanLeaf {
    pair: Pair,
    gate: Option<Gate>,
    drain: String,
    leaf: String,
    task: String,
    bound: OrbitRuntime,
    base: String,
    input: Value,
    evidence_reads: Arc<Mutex<Vec<(String, Value)>>>,
    lose_evidence_read: Arc<Mutex<bool>>,
}

/// Seed the unobserved clean Git graph in-process. Every leaf still has its
/// own objects, index, refs and bare origin, and its commit names its real task.
/// Production actions below continue to use their ordinary Git subprocesses.
fn prepare_clean_owner(pair: &Pair, task: &str) -> String {
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
fn prepare_clean_follower(pair: &Pair, task: &str, base: &str) {
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
    fn new(already_landed: bool, completion: &str) -> Self {
        Self::build(already_landed, completion, &[], true)
    }

    /// A clean leaf whose owner already holds unrelated evidence. This avoids
    /// creating a large worker mutation receipt just to prepare read fixtures.
    fn with_evidence(already_landed: bool, completion: &str, artifacts: &[TaskArtifact]) -> Self {
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
        )
    }

    /// A clean leaf whose task carries the `no-diff-expected` tag, claimed by
    /// the follower through ordinary pull admission [ORB-14474].
    fn tagged(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            true,
        )
    }

    /// A tagged review whose implementer files findings and attaches its
    /// coverage, but writes no clean-tree report [ORB-14791].
    fn review_only(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            false,
        )
    }

    /// A tagged clean leaf whose task a `delivery-code-review` auto-task
    /// minted. Like `review_only`, it writes no clean-tree report [ORB-14791].
    fn delivery_review(completion: &str) -> Self {
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
    fn gated(completion: &str) -> Self {
        Self::gated_on(completion, GateOn::Both)
    }

    /// [`Self::gated`], with the gate named only where `on` says.
    fn gated_on(completion: &str, on: GateOn) -> Self {
        Self::build_with(false, completion, &[], true, Some(on))
    }

    fn build(already_landed: bool, completion: &str, tags: &[&str], report: bool) -> Self {
        Self::build_with(already_landed, completion, tags, report, None)
    }

    fn build_with(
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
        )
    }

    /// A verified clean leaf whose owner and follower require `red`'s command
    /// instead of [`CHECK`] [ORB-15427].
    fn red(red: &RedCheck) -> Self {
        Self::build_requiring(false, "done", &[], true.into(), None, &red.command(), "")
    }

    /// A tagged review whose owner and follower require `check`'s command and
    /// name the [`Gate`] as a baseline command: the skip must run neither
    /// [ORB-15764].
    fn tagged_requiring(completion: &str, check: &RedCheck, artifacts: &[TaskArtifact]) -> Self {
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
        )
    }

    /// An untagged clean leaf whose implementer reports `no-diff.json` and
    /// names `selectors` as its already-resolved outcome, on a follower whose
    /// validation PATH resolves `cargo` to `replay` [ORB-15747].
    #[cfg(unix)]
    fn resolved(replay: &Replay, selectors: &[&str]) -> Self {
        let mut fixture = Self::build_requiring(
            false,
            "done",
            &[],
            true.into(),
            None,
            CHECK,
            &replay.config(),
        );
        fixture.input["implementation"]["already_resolved"] = json!({"selectors": selectors});
        fixture
    }

    fn build_requiring(
        already_landed: bool,
        completion: &str,
        tags: &[&str],
        evidence: EvidenceSeed<'_>,
        gated: Option<GateOn>,
        required: &str,
        follower_config: &str,
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
        let pair = Pair::with_owner_config(&owner_config, &[None]);
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

    fn write_evidence(&mut self, path: &str, value: Value) {
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
    fn large_unrelated_evidence() -> Vec<TaskArtifact> {
        (0..17)
            .map(|index| TaskArtifact {
                path: format!("qa/media-{index}.bin"),
                media_type: "application/octet-stream".into(),
                content: vec![255; 1_048_576],
                created_by: None,
            })
            .collect()
    }

    fn assert_bulk_evidence_exceeds_ceiling(&self) {
        let error = RuntimeHost::get_task_artifacts(&self.bound, &self.task).expect_err(
            "ORB-16120: former bulk evidence read must overflow the unchanged MCP ceiling",
        );
        assert!(error.to_string().contains("67108864"), "{error}");
        self.evidence_reads.lock().unwrap().clear();
    }

    fn assert_named_reads_and_retained_evidence(&self, artifacts: &[TaskArtifact]) {
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

    fn attach(&self, path: &str, value: Value) {
        self.bound
            .attach_claim_validation_log(path, serde_json::to_vec(&value).unwrap())
            .unwrap();
    }

    fn action(&self, action: &str, input: &Value) -> Value {
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
    fn validate_input(&self) -> Value {
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
    fn advance_base(&self, round: usize) -> String {
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

    fn prepare_handoff(&self) -> TaskHandoff {
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

    fn run_clean_pipeline(&self, name: &str) -> TaskHandoff {
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

    fn execute_clean_pipeline(
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

    fn settle(&self, handoff: &TaskHandoff) -> Result<Value, OrbitError> {
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
    fn refusal_of(&self, name: &str) -> String {
        match self.execute_clean_pipeline(name) {
            Err(error) => error.to_string(),
            Ok(outcome) => {
                assert!(!outcome.success, "{name} must refuse: {outcome:#?}");
                format!("{outcome:#?}")
            }
        }
    }

    fn assert_no_blocked(&self) {
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

fn verified_no_diff_and_already_satisfied_base_complete_without_a_pr() {
    for (already_landed, job) in [
        (false, "task_claimed_pr_pipeline"),
        (true, "task_claimed_pr_pipeline"),
        (false, "task_claimed_local_pipeline"),
    ] {
        let unrelated = if job == "task_claimed_pr_pipeline" {
            CleanLeaf::large_unrelated_evidence()
        } else {
            Vec::new()
        };
        let fixture = CleanLeaf::with_evidence(already_landed, "done", &unrelated);
        if !unrelated.is_empty() {
            fixture.assert_bulk_evidence_exceeds_ceiling();
        }
        let handoff = fixture.run_clean_pipeline(job);
        fixture.assert_named_reads_and_retained_evidence(&unrelated);
        let settled = fixture
            .settle(&handoff)
            .expect("owner independently verifies the report");
        assert_eq!(settled["status"], "review", "{settled}");
        let accepted = fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .unwrap();
        let landed = engine_action(
            &fixture.pair.wire.owner,
            "handoff_land",
            &json!({"handoff_id": accepted.handoff_id}),
        )
        .unwrap();
        assert_eq!(landed["evidence"]["external_merge"], false, "{landed}");
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
        assert_eq!(
            git(&fixture.pair.owner_repo, &["rev-parse", "main"]).trim(),
            fixture.base
        );
        fixture.assert_no_blocked();
        let replay = fixture.settle(&handoff).unwrap();
        assert_eq!(
            replay["status"], "review",
            "journal replays acceptance without repeating completion"
        );
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
    }
}

/// The full-code-review chore shape: a `no-diff-expected` task the follower
/// claims, whose leaf files findings on the owner through the claimed-owner
/// broker and hands off `NoDiff`, with no PR [ORB-14474].
fn a_follower_claims_no_diff_expected_work_files_findings_and_completes_without_a_pr() {
    let fixture = CleanLeaf::tagged("done");
    let owner_task = fixture.pair.owner_task(&fixture.task);
    assert!(
        owner_task["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == orbit_types::task::NO_DIFF_EXPECTED_TAG),
        "{owner_task:#}"
    );

    let filed = fixture
        .bound
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Finding from the review chore",
                "description": "Found while reviewing the crate.",
                "complexity": "low", "model": "codex",
                "relations": [{"type": "spawned_from", "target": fixture.task}],
            }),
        )
        .expect("a claimed no-diff leaf files a finding on the owner");
    let id = filed["id"].as_str().expect("the new task's id");
    let prefix = |id: &str| id.split('-').next().unwrap().to_string();
    assert_eq!(prefix(id), prefix(&fixture.task), "owner prefix: {filed}");
    let stored = fixture
        .pair
        .wire
        .owner
        .run_tool("orbit.task.show", json!({"id": id}))
        .expect("the owner holds the filed task");
    assert_eq!(stored["id"], id, "{stored}");

    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture
        .settle(&handoff)
        .expect("owner independently verifies the report");
    let accepted = fixture
        .pair
        .wire
        .owner
        .accepted_task_handoff(&handoff.claim_id)
        .unwrap();
    engine_action(
        &fixture.pair.wire.owner,
        "handoff_land",
        &json!({"handoff_id": accepted.handoff_id}),
    )
    .unwrap();
    assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
    fixture.assert_no_blocked();
}

/// The frozen batch an owner's after-landing review consumer admits for
/// `task`: one delivery on the `reviewed` branch, published beside `main`
/// so the clean base the leaf reviews does not move.
fn reviewed_batch(fixture: &CleanLeaf) -> orbit_types::workflow::automation::BatchAttempt {
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
fn admit(owner: &OrbitRuntime, attempt: &orbit_types::workflow::automation::BatchAttempt) {
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
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
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

/// [ORB-15962] A claimed review's coverage, put through the worker's own
/// artifact route, is stamped by the owner with the claim, follower and bound
/// run it authenticated. After the no-diff landing settles that claim and its
/// mutation receipts are gone, the owner still accepts exactly those bytes
/// for the frozen batch naming the task, though its store has no record of
/// the follower's run. A witness naming any other claim, machine or run, a
/// late put from the landed claim, and bytes the witness does not cover are
/// all refused. Before, the claimed route stored no witness, so the batch
/// stayed owed as `unauthorized_submitter`.
#[test]
fn claimed_coverage_is_authenticated_by_the_owners_claim_journal_after_landing() {
    use orbit_types::workflow::automation::{
        COVERAGE_ARTIFACT, ClaimedSubmission, EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission,
        ExaminationCheck, evidence_template,
    };

    if !isolated(
        module_path!(),
        "claimed_coverage_is_authenticated_by_the_owners_claim_journal_after_landing",
    ) {
        return;
    }
    let fixture = CleanLeaf::review_only("done");
    let owner = &fixture.pair.wire.owner;
    let attempt = reviewed_batch(&fixture);
    let mut coverage = evidence_template(&attempt);
    coverage.examination_complete = true;
    coverage.checks = vec![ExaminationCheck {
        subject: "src/reviewed.rs".into(),
        method: "read the delivery diff".into(),
        observation: "adds one empty function".into(),
    }];
    coverage.delivery_examinations[0].examined_paths = vec!["src/reviewed.rs".into()];
    coverage.delivery_examinations[0].rationale =
        "The delivery adds an empty function that nothing calls yet.".into();
    fixture.attach(COVERAGE_ARTIFACT, serde_json::to_value(&coverage).unwrap());
    let refused = fixture
        .bound
        .attach_claim_validation_log(EVIDENCE_AUTHORITY_ARTIFACT, b"{}".to_vec())
        .expect_err("a worker cannot write the owner's witness");
    assert!(refused.to_string().contains("reserved"), "{refused}");

    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture.settle(&handoff).expect("the owner accepts it");
    let accepted = owner.accepted_task_handoff(&handoff.claim_id).unwrap();
    engine_action(
        owner,
        "handoff_land",
        &json!({"handoff_id": accepted.handoff_id}),
    )
    .unwrap();
    assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
    admit(owner, &attempt);

    let bytes = owner
        .get_task_artifact(&fixture.task, COVERAGE_ARTIFACT)
        .unwrap()
        .unwrap()
        .content;
    let witness_bytes = owner
        .get_task_artifact(&fixture.task, EVIDENCE_AUTHORITY_ARTIFACT)
        .unwrap()
        .expect("the owner stamps the claimed submission")
        .content;
    let witness: EvidenceSubmission = serde_json::from_slice(&witness_bytes).unwrap();
    assert_eq!(
        witness,
        EvidenceSubmission {
            action_id: fixture.task.clone(),
            evidence_digest: sha256_hex(&bytes),
            run_id: fixture.leaf.clone(),
            claim: Some(ClaimedSubmission {
                claim_id: handoff.claim_id.clone(),
                machine_id: FOLLOWER.into(),
            }),
        }
    );

    // Settlement prunes a landed claim's receipts after a week; the proof
    // must not need them, nor the follower's run, which this store lacks.
    let workspace = owner.workspace_id().unwrap();
    let pruned = owner
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            Ok(tx
                .connection()
                .execute(
                    "DELETE FROM task_coordination_rows WHERE workspace_id = ?1 AND kind = ?2",
                    [workspace.as_str(), "distributed-claim-mutation-v1"],
                )
                .unwrap())
        })
        .unwrap();
    assert!(pruned > 0, "the claim's mutation receipts existed");
    assert!(owner.show_job_run(&fixture.leaf).is_err());
    let accepted_coverage = || RuntimeHost::accepted_automation_coverage(owner, &fixture.task);
    assert_eq!(accepted_coverage().unwrap(), Some(coverage.clone()));

    let witness_path = find_file(&owner.paths().global_dir, EVIDENCE_AUTHORITY_ARTIFACT)
        .or_else(|| find_file(&owner.paths().orbit_dir, EVIDENCE_AUTHORITY_ARTIFACT))
        .expect("the witness file");
    let foreign = |edit: fn(&mut EvidenceSubmission)| {
        let mut forged = witness.clone();
        edit(&mut forged);
        forged
    };
    for (case, forged) in [
        (
            "foreign claim",
            foreign(|w| w.claim.as_mut().unwrap().claim_id = "claim-elsewhere".into()),
        ),
        (
            "foreign machine",
            foreign(|w| w.claim.as_mut().unwrap().machine_id = OWNER.into()),
        ),
        ("stale run", foreign(|w| w.run_id = "jrun-earlier".into())),
        ("unclaimed follower run", foreign(|w| w.claim = None)),
    ] {
        std::fs::write(&witness_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(accepted_coverage().unwrap(), None, "{case}");
    }
    std::fs::write(&witness_path, &witness_bytes).unwrap();
    assert_eq!(accepted_coverage().unwrap(), Some(coverage.clone()));

    // The landed claim takes no further put, and bytes the owner writes
    // itself carry no claimed witness.
    let mut replaced = coverage.clone();
    replaced.findings = vec!["ORB-1".into()];
    let late = fixture
        .bound
        .attach_claim_validation_log(COVERAGE_ARTIFACT, serde_json::to_vec(&replaced).unwrap())
        .expect_err("a landed claim refuses its worker's put");
    assert!(late.to_string().contains("stale_claim"), "{late}");
    owner
        .update_task_as_human(
            &fixture.task,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: COVERAGE_ARTIFACT.into(),
                    media_type: "application/json".into(),
                    content: serde_json::to_vec(&replaced).unwrap(),
                    created_by: None,
                }],
                ..Default::default()
            },
            "fixture operator".into(),
        )
        .unwrap();
    assert_eq!(accepted_coverage().unwrap(), None, "tampered bytes");
}

/// [ORB-15764] A claimed `no-diff-expected` review compiles nothing, as the
/// owner's own pipelines skip `validate` for the tag: its leaf runs no
/// required command and replays no deferred gate on its clean base, and hands
/// off `NoDiff` with no validation logs. The owner accepts, approves and lands
/// that handoff without running them either. The required command here fails
/// whenever it runs, and both it and the gate count every run.
#[test]
fn a_claimed_no_diff_expected_skip_runs_no_validation_and_the_owner_lands_it() {
    use orbit_core::application::review::ExpectedCandidate;

    if !isolated(
        module_path!(),
        "a_claimed_no_diff_expected_skip_runs_no_validation_and_the_owner_lands_it",
    ) {
        return;
    }
    for (job, step, completion) in [
        ("task_claimed_pr_pipeline", "validate_no_diff", "done"),
        ("task_claimed_local_pipeline", "validate", "done"),
        ("task_claimed_pr_pipeline", "validate_no_diff", "review"),
    ] {
        let check = RedCheck::new();
        let unrelated = if completion == "done" && job == "task_claimed_pr_pipeline" {
            CleanLeaf::large_unrelated_evidence()
        } else {
            Vec::new()
        };
        let mut fixture = CleanLeaf::tagged_requiring(completion, &check, &unrelated);
        if !unrelated.is_empty() {
            fixture.assert_bulk_evidence_exceeds_ceiling();
        }
        fixture.hand_off_deferral();
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        let outcome = fixture.execute_clean_pipeline(job).unwrap();
        assert!(outcome.success, "{job}: {outcome:#?}");
        // The same matrix also pins the original ORB-14791 clean-review route.
        fixture.assert_named_reads_and_retained_evidence(&unrelated);
        let committed = &outcome.pipeline["commit"];
        assert_eq!(committed["decision"], "skipped_no_diff_expected", "{job}");
        assert_eq!(committed["base_sha"], fixture.base, "{job}");
        for skipped in ["prepare_branch", "sync_base", "review", "push", "pr_open"] {
            assert!(
                outcome.pipeline.get(skipped).is_none(),
                "{job}: {outcome:#?}"
            );
        }
        let validated = &outcome.pipeline[step];
        assert_eq!(
            validated["decision"], "skipped_no_diff_expected",
            "{job}: {validated:#}"
        );
        assert_eq!(validated["validation"], json!([]), "{job}");
        let ClaimMutation::AcceptHandoff(handoff) =
            fixture.pair.admission(&fixture.leaf).settlement.unwrap()
        else {
            panic!("{job}: typed handoff required")
        };
        assert!(
            matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }),
            "{job}"
        );
        assert!(handoff.validation.is_empty(), "{job}: {handoff:?}");
        assert!(
            handoff
                .execution_summary
                .contains("no-diff-expected and changed nothing"),
            "{job}: {}",
            handoff.execution_summary
        );

        let settled = fixture
            .settle(&handoff)
            .expect("the owner accepts a tagged skip without logs");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        let owner = &fixture.pair.wire.owner;
        assert_eq!(fixture.pair.owner_status(&fixture.task), "review", "{job}");
        let artifacts = RuntimeHost::get_task_artifacts(owner, &fixture.task)
            .unwrap()
            .into_iter()
            .map(|artifact| artifact.path)
            .collect::<Vec<_>>();
        assert!(
            artifacts
                .iter()
                .any(|path| path == "automation-coverage.json"),
            "{job}: coverage reaches the owner: {artifacts:?}"
        );
        assert!(
            !artifacts
                .iter()
                .any(|path| path == "already-landed.json" || path == "no-diff.json"),
            "{job}: a review writes no clean-tree report: {artifacts:?}"
        );
        let accepted = owner.accepted_task_handoff(&handoff.claim_id).unwrap();
        if completion == "review" {
            assert!(owner.landing_start_requests().unwrap().is_empty(), "{job}");
            owner
                .approve_handoff_as_operator(
                    &accepted.handoff_id,
                    &ExpectedCandidate {
                        candidate_commit: fixture.base.clone(),
                        base_commit: fixture.base.clone(),
                    },
                    "operator",
                    "approve-no-diff-expected",
                )
                .expect("the operator approves a tagged skip without logs");
        }
        let landed = engine_action(
            owner,
            "handoff_land",
            &json!({"handoff_id": accepted.handoff_id}),
        )
        .expect("the owner lands a tagged skip without logs");
        assert_eq!(landed["evidence"]["delivery"], "no_diff", "{landed}");
        assert_eq!(
            landed["evidence"]["external_merge"], false,
            "{job}: {landed}"
        );
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done", "{job}");
        assert_eq!(check.runs(), 0, "{job}: no host ran the required command");
        assert_eq!(
            fixture.gate.as_ref().unwrap().runs(),
            0,
            "{job}: no host replayed the deferred gate"
        );
        fixture.assert_no_blocked();
    }
}

/// [ORB-15764] A claim admitted before the owner upgraded can settle after it.
/// Its older leaf still pinned a log per required command for the tagged
/// skip; the upgraded owner accepts that handoff, and still checks each log
/// it carries, so one pinned for another run is refused.
fn an_owner_accepts_a_tagged_skip_that_still_carries_logs_and_checks_them() {
    for forged in [false, true] {
        let check = RedCheck::new();
        let fixture = CleanLeaf::tagged_requiring("done", &check, &[]);
        let mut handoff = fixture.prepare_handoff();
        assert!(handoff.validation.is_empty(), "{handoff:?}");
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: handoff.workspace_id.clone(),
            task_id: handoff.task_id.clone(),
            claim_id: handoff.claim_id.clone(),
            machine_id: handoff.machine_id.clone(),
            run_id: if forged {
                "another-run".into()
            } else {
                handoff.run_id.clone()
            },
            candidate: handoff.candidate.clone(),
            tested_head: handoff.candidate.candidate.commit.clone(),
            command: check.command(),
            exit_code: 0,
            output: "test suite::broken ... ok".into(),
            known_red: None,
            flaky: None,
        };
        let path = format!("validation/{}/0.json", handoff.claim_id);
        let content = serde_json::to_vec(&log).unwrap();
        fixture
            .bound
            .attach_claim_validation_log(&path, content.clone())
            .unwrap();
        handoff.validation.push(HandoffArtifactRef {
            path,
            sha256: sha256_hex(&content),
        });
        let settled = fixture.settle(&handoff);
        if forged {
            let error = settled.expect_err("a log for another run is refused");
            assert!(
                error
                    .to_string()
                    .contains("validation must pass for the exact claim/run/candidate/base"),
                "{error}"
            );
            assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
        } else {
            let settled = settled.expect("the owner accepts the older leaf's logs");
            assert_eq!(settled["status"], "review", "{settled}");
        }
        assert_eq!(check.runs(), 0, "forged={forged}");
        fixture.assert_no_blocked();
    }
}

/// A claimed tagged task hands off `NoDiff` and has no PR route for a change,
/// so a pending diff or a commit of its own is refused by name, before the
/// index is touched [ORB-14791].
fn a_claimed_no_diff_expected_task_that_changed_the_worktree_is_refused() {
    for committed in [false, true] {
        let fixture = CleanLeaf::review_only("done");
        let follower = &fixture.pair.follower_repo;
        std::fs::write(follower.join("src/f0.rs"), "fn reviewed() {}\n").unwrap();
        if committed {
            git(follower, &["commit", "-q", "-am", "Review edits code"]);
        }
        let error = engine_action(&fixture.bound, "git_commit", &fixture.input).unwrap_err();
        assert!(
            error.to_string().contains("no_diff_expected_changed"),
            "committed={committed}: {error}"
        );
        assert!(
            git(follower, &["diff", "--cached", "--name-only"]).is_empty(),
            "the refusal stages nothing"
        );
        if !committed {
            assert_eq!(git(follower, &["rev-parse", "HEAD"]).trim(), fixture.base);
        }
        assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
        fixture.assert_no_blocked();
    }
}

/// The tag is the only authority a review's skip carries, and the owner's copy
/// of the task holds it: a skip checkpoint pinned for an untagged task is
/// refused at settlement, without blocking the task [ORB-14791].
fn owner_refuses_a_no_diff_expected_skip_for_an_untagged_task() {
    let fixture = CleanLeaf::new(false, "done");
    let mut handoff = fixture.prepare_handoff();
    let skip = json!({
        "phase": "commit", "decision": "skipped_no_diff_expected", "committed": false,
        "skipped_no_diff_expected": true, "task_id": fixture.task,
        "job_run_id": fixture.leaf, "base_sha": fixture.base,
    });
    let path = "no-diff-handoff/forged-skip.json";
    fixture.attach(path, skip.clone());
    handoff.candidate.delivery = HandoffDelivery::NoDiff {
        evidence: HandoffArtifactRef {
            path: path.into(),
            sha256: sha256_hex(&serde_json::to_vec(&skip).unwrap()),
        },
    };
    let error = fixture.settle(&handoff).unwrap_err();
    assert!(
        error.to_string().contains("no-diff-expected"),
        "the refusal names the missing tag: {error}"
    );
    assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
    assert!(
        fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .is_err()
    );
    fixture.assert_no_blocked();
}

/// A follower's claimed `delivery-code-review` files each finding on the
/// owner with `regression_from` the task that introduced it, which the owner
/// checks is one of its own tasks, and the owner records the claim and run
/// that filed it. A finding the owner still refuses reaches it as the review
/// task's `unfiled-findings.json`, which the handoff attaches [ORB-14792].
fn a_claimed_delivery_review_files_regression_findings_and_hands_off_the_rest() {
    let mut fixture = CleanLeaf::delivery_review("done");
    let owner = fixture.pair.wire.owner.clone();
    let culprit = owner
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "The delivery that introduced the defect",
                "description": "Landed in the reviewed batch.",
                "complexity": "low", "model": "codex",
                "workspace": fixture.pair.owner_repo.to_string_lossy(),
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let relations = |culprit: &str| {
        json!([{"type": "spawned_from", "target": fixture.task},
               {"type": "regression_from", "target": culprit}])
    };
    let file = |culprit: &str| {
        fixture.bound.run_tool(
            "orbit.task.add",
            json!({
                "title": "Finding from the delivery review",
                "description": "The culprit delivery broke the invariant.",
                "complexity": "low", "type": "bug", "model": "codex",
                "tags": ["delivery-code-review"], "relations": relations(culprit),
            }),
        )
    };

    let filed = file(&culprit).expect("a claimed review files a regression finding");
    let id = filed["id"].as_str().expect("the finding's id");
    assert_eq!(
        fixture.pair.owner_task(id)["relations"],
        relations(&culprit)
    );
    let binding = fixture.bound.worker_invocation().unwrap().clone();
    assert!(
        owner
            .get_task_comments(id)
            .unwrap()
            .iter()
            .any(|comment| comment.message.contains(&binding.claim_id)
                && comment.message.contains(&fixture.leaf)),
        "the finding records the claim and run that filed it"
    );

    let before = owner.list_tasks().unwrap().len();
    let prefix = culprit.split('-').next().unwrap();
    let missing = file(&format!("{prefix}-999999")).unwrap_err().to_string();
    assert!(
        missing.contains("not a task in the claimed task's workspace"),
        "a culprit outside the owner's workspace is refused: {missing}"
    );
    assert_eq!(owner.list_tasks().unwrap().len(), before);

    let unfiled = json!([{
        "title": "A finding the owner refused",
        "description": "The owner refused it during the run.",
        "relations": relations(&culprit),
    }]);
    fixture.input["implementation"]["unfiled_findings"] = unfiled.clone();
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let attached = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "unfiled-findings.json"}),
        )
        .expect("the handoff attaches the unfiled findings to the review task");
    let record: Value = serde_json::from_str(attached["content"].as_str().unwrap()).unwrap();
    assert_eq!(record["findings"], unfiled, "{record}");
    assert_eq!(record["claim_id"], binding.claim_id);
    assert_eq!(record["run_id"], fixture.leaf);
    assert!(
        handoff.execution_summary.contains("unfiled-findings.json"),
        "the owner's summary points at the attachment: {}",
        handoff.execution_summary
    );
    fixture
        .settle(&handoff)
        .expect("the owner accepts the review's handoff");
}

/// A candidate that was published with plain-string `unfiled_findings` (the
/// implement step now refuses them before delivery) still hands off: each
/// string becomes a finding object that keeps its full text, the entry and
/// the owner's summary say it was normalized, and object entries pass through
/// untouched. A malformed entry that is not a string stays refused, and so
/// does a malformed field other than the findings [ORB-14927].
fn a_published_candidates_string_findings_are_normalized_at_handoff() {
    let mut fixture = CleanLeaf::delivery_review("done");
    let owner = fixture.pair.wire.owner.clone();
    let long = format!("{} and on", "A very long first sentence".repeat(8));
    let object = json!({
        "title": "Already structured",
        "description": "Kept as returned.",
        "relations": [{"type": "spawned_from", "target": fixture.task}],
    });
    fixture.input["implementation"]["unfiled_findings"] = json!([
        "The owner refused this. It needs a follow-up on the retry path.",
        object,
        long,
    ]);
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let attached = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "unfiled-findings.json"}),
        )
        .expect("the handoff attaches the normalized findings");
    let record: Value = serde_json::from_str(attached["content"].as_str().unwrap()).unwrap();
    let findings = record["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3, "{record}");
    assert_eq!(findings[0]["title"], "The owner refused this.");
    assert_eq!(
        findings[0]["description"],
        "The owner refused this. It needs a follow-up on the retry path.",
        "the full text survives"
    );
    assert_eq!(findings[0]["normalized_from"], "string");
    assert_eq!(findings[1], object, "an object entry is kept verbatim");
    assert_eq!(findings[2]["description"], long);
    assert!(findings[2]["title"].as_str().unwrap().len() <= 120);
    assert_eq!(record["normalized_string_entries"], 2);
    assert!(
        handoff.execution_summary.contains("normalized_from"),
        "the owner's summary records the repair: {}",
        handoff.execution_summary
    );
    fixture
        .settle(&handoff)
        .expect("the owner accepts the normalized handoff");

    for value in [json!([42]), json!(["   "]), json!("not an array")] {
        let mut fixture = CleanLeaf::delivery_review("done");
        fixture.input["implementation"]["unfiled_findings"] = value.clone();
        let handed_off = fixture
            .execute_clean_pipeline("task_claimed_pr_pipeline")
            .is_ok_and(|outcome| outcome.success);
        assert!(!handed_off, "{value} is still refused at handoff");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "{value} records no handoff"
        );
    }
}

fn no_diff_retains_owner_completion_authority() {
    let fixture = CleanLeaf::new(false, "review");
    let handoff = fixture.prepare_handoff();
    fixture.settle(&handoff).unwrap();
    assert_eq!(fixture.pair.owner_status(&fixture.task), "review");
    assert!(
        fixture
            .pair
            .wire
            .owner
            .landing_start_requests()
            .unwrap()
            .is_empty()
    );
    fixture.assert_no_blocked();
}

fn owner_refuses_changed_no_diff_evidence_base_or_missing_required_checks_without_blocking() {
    for changed in [
        "report",
        "log",
        "rewritten_base",
        "required_check",
        "missing_log",
        "oversize_log",
        "wrong_log_run",
        "missing_checkpoint",
        "wrong_task",
        "wrong_run",
        "checkpoint_base",
        "removed_tag",
        "oversize_checkpoint",
        "truncated_checkpoint",
        "foreign_path",
    ] {
        let tagged = !matches!(
            changed,
            "report"
                | "log"
                | "rewritten_base"
                | "required_check"
                | "missing_log"
                | "oversize_log"
                | "wrong_log_run"
        );
        let fixture = if tagged {
            CleanLeaf::review_only("done")
        } else {
            CleanLeaf::new(false, "done")
        };
        let mut handoff = fixture.prepare_handoff();
        let mut restore_blob = None;
        match changed {
            "report" => {
                let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                fixture.attach(&evidence.path, json!({"decision": "verified_no_diff"}));
            }
            "log" => fixture.attach(
                "implementation-validation.json",
                json!({
                    "run_id": fixture.leaf, "tested_head": fixture.base, "command": CHECK,
                    "exit_code": 0, "output": "changed after checkpoint",
                }),
            ),
            // An advance is accepted [ORB-15074]; a base the owner's history
            // no longer contains is not.
            "rewritten_base" => {
                let repo = &fixture.pair.owner_repo;
                std::fs::write(repo.join("src/f0.rs"), "fn rewritten() {}\n").unwrap();
                git(
                    repo,
                    &["commit", "-q", "-a", "--amend", "-m", "Base rewritten"],
                );
                git(
                    repo,
                    &["push", "-q", "-f", "origin", "HEAD:refs/heads/main"],
                );
            }
            "missing_log" => {
                let artifact = fixture
                    .pair
                    .wire
                    .owner
                    .get_task_artifact(&fixture.task, "no-diff.json")
                    .unwrap()
                    .unwrap();
                let mut report: Value = serde_json::from_slice(&artifact.content).unwrap();
                report["validation"][0]["log_artifact"] = json!("missing-validation.json");
                fixture.attach("no-diff.json", report);
            }
            "oversize_log" => {
                fixture
                    .bound
                    .attach_claim_validation_log(
                        "implementation-validation.json",
                        vec![b' '; 1_048_577],
                    )
                    .unwrap();
                let error = fixture
                    .bound
                    .get_task_artifact(&fixture.task, "implementation-validation.json")
                    .unwrap_err();
                assert!(error.to_string().contains("inline read limit"), "{error}");
            }
            "wrong_log_run" => fixture.attach(
                "implementation-validation.json",
                json!({
                    "run_id": "another-run", "tested_head": fixture.base, "command": CHECK,
                    "exit_code": 0, "output": "",
                }),
            ),
            "required_check" => handoff.validation.clear(),
            "missing_checkpoint" | "foreign_path" => {
                let HandoffDelivery::NoDiff { evidence } = &mut handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                evidence.path = if changed == "foreign_path" {
                    "../checkpoint.json"
                } else {
                    "no-diff-handoff/missing.json"
                }
                .into();
            }
            "wrong_task" | "wrong_run" | "checkpoint_base" => {
                let HandoffDelivery::NoDiff { evidence } = &mut handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                let artifact = fixture
                    .pair
                    .wire
                    .owner
                    .get_task_artifact(&fixture.task, &evidence.path)
                    .unwrap()
                    .unwrap();
                let mut report: Value = serde_json::from_slice(&artifact.content).unwrap();
                report[match changed {
                    "wrong_task" => "task_id",
                    "wrong_run" => "job_run_id",
                    _ => "base_sha",
                }] = json!("another-binding");
                fixture.attach(&evidence.path, report.clone());
                evidence.sha256 = sha256_hex(&serde_json::to_vec(&report).unwrap());
            }
            "removed_tag" => {
                let owner = &fixture.pair.wire.owner;
                let path = find_file(&owner.paths().global_dir, "task.yaml")
                    .or_else(|| find_file(&owner.paths().orbit_dir, "task.yaml"))
                    .unwrap();
                let mut envelope: orbit_types::task::TaskEnvelopeV2 =
                    serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(envelope.id, fixture.task);
                // Hostile persisted state: ordinary active-claim updates cannot
                // remove tags, but the handoff must still recheck their live value.
                envelope.tags.clear();
                std::fs::write(path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
            }
            "oversize_checkpoint" => {
                let HandoffDelivery::NoDiff { evidence } = &mut handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                let content = vec![b' '; 1_048_577];
                fixture
                    .bound
                    .attach_claim_validation_log(&evidence.path, content.clone())
                    .unwrap();
                evidence.sha256 = sha256_hex(&content);
                // Exercise the executor's named inline read too, before owner settlement.
                let error = fixture
                    .bound
                    .get_task_artifact(&fixture.task, &evidence.path)
                    .unwrap_err();
                assert!(error.to_string().contains("inline read limit"), "{error}");
            }
            "truncated_checkpoint" => {
                let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                let manifest = fixture
                    .pair
                    .wire
                    .owner
                    .get_task_artifact_manifest(&fixture.task)
                    .unwrap();
                let file = manifest
                    .iter()
                    .find(|file| file.path == evidence.path)
                    .unwrap();
                // Resolve the selected manifest blob in the disposable owner store.
                let name = Path::new(&file.blob).file_name().unwrap().to_str().unwrap();
                let blob = find_file(&fixture.pair.wire.owner.paths().global_dir, name)
                    .or_else(|| find_file(&fixture.pair.wire.owner.paths().orbit_dir, name))
                    .unwrap();
                restore_blob = Some((blob.clone(), std::fs::read(&blob).unwrap()));
                std::fs::write(blob, b"{").unwrap();
            }
            _ => unreachable!(),
        }
        let error = fixture.settle(&handoff).expect_err(changed);
        assert!(
            matches!(
                error,
                OrbitError::PolicyDenied(_)
                    | OrbitError::InvalidInput(_)
                    | OrbitError::Execution(_)
                    | OrbitError::Store(_)
                    | OrbitError::TaskBundleCorrupt { .. }
            ),
            "{changed}: {error}"
        );
        let expected = match changed {
            "report" => Some("NoDiff verifier report changed"),
            "missing_checkpoint" => Some("NoDiff verifier report is missing"),
            "wrong_task" | "wrong_run" => Some("this task/run"),
            "checkpoint_base" => Some("checkpoint's base"),
            "removed_tag" => Some("no-diff-expected"),
            "oversize_checkpoint" | "oversize_log" => Some("inline read limit"),
            "missing_log" => Some("missing task artifact missing-validation.json"),
            "wrong_log_run" => Some("validation log must capture the exact run"),
            "truncated_checkpoint" => Some("size mismatch"),
            "foreign_path" => Some("artifact path"),
            _ => None,
        };
        if let Some(expected) = expected {
            assert!(error.to_string().contains(expected), "{changed}: {error}");
        }
        if changed == "rewritten_base" {
            assert!(
                error
                    .to_string()
                    .contains("is not contained in the owner's base"),
                "the refusal names the rewritten base: {error}"
            );
        }
        if let Some((path, content)) = restore_blob {
            // Restore exactly the hostile fixture bytes to inspect unchanged
            // lifecycle state; the corruption refusal itself was asserted above.
            std::fs::write(path, content).unwrap();
        }
        assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
        assert!(
            fixture
                .pair
                .wire
                .owner
                .accepted_task_handoff(&handoff.claim_id)
                .is_err()
        );
        fixture.assert_no_blocked();
    }
}

fn claimed_no_diff_requires_a_verifier_checkpoint_and_passing_logs() {
    let fixture = CleanLeaf::new(false, "done");
    let mut input = fixture.input.clone();
    input["skipped_no_diff_expected"] = json!(true);
    let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
    assert!(
        error.to_string().contains("skip flag or tag alone"),
        "{error}"
    );
    let mut escaped = fixture.input.clone();
    escaped["implementation"]["no_diff_artifacts"][0]["source_path"] = json!("src/f0.rs");
    let error = engine_action(&fixture.bound, "git_commit", &escaped).unwrap_err();
    assert!(
        error.to_string().contains("outside workspace_root"),
        "scratch import cannot read other checkout files: {error}"
    );
    std::fs::write(
        fixture
            .pair
            .follower_repo
            .join(".orbit/tmp/implementation-validation.json"),
        serde_json::to_vec(&json!({
            "run_id": fixture.leaf, "tested_head": fixture.base, "command": CHECK,
            "exit_code": 1, "output": "failed",
        }))
        .unwrap(),
    )
    .unwrap();
    let error = engine_action(&fixture.bound, "git_commit", &fixture.input).unwrap_err();
    assert!(error.to_string().contains("validation log"), "{error}");
    assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
    for path in ["../no-diff.json", "/no-diff.json"] {
        let error = fixture
            .bound
            .get_task_artifact(&fixture.task, path)
            .unwrap_err();
        assert!(
            error.to_string().contains("artifact path"),
            "{path}: {error}"
        );
    }
    let error = fixture
        .bound
        .get_task_artifact("ORB-99999", "no-diff.json")
        .unwrap_err();
    assert!(
        error.to_string().contains("worker task binding mismatch"),
        "{error}"
    );
    fixture.evidence_reads.lock().unwrap().clear();
    *fixture.lose_evidence_read.lock().unwrap() = true;
    let error = fixture
        .bound
        .get_task_artifact(&fixture.task, "no-diff.json")
        .unwrap_err();
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "{error}"
    );
    assert_eq!(
        fixture.evidence_reads.lock().unwrap().len(),
        1,
        "an unknown artifact read is neither retried nor replaced by bulk or missing evidence"
    );
    assert!(
        fixture
            .bound
            .get_task_artifact(&fixture.task, "no-diff.json")
            .unwrap()
            .is_some()
    );
    let mut binding = fixture.bound.worker_invocation().unwrap().clone();
    binding.claim_id = "another-claim".into();
    let error = ToOwner(fixture.pair.wire.owner.clone())
        .call(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "no-diff.json", "_worker_read": "artifact"}),
            ToolSessionContext {
                worker_invocation: Some(binding),
                worker_host_call: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("stale_claim"),
        "foreign claim: {error}"
    );
    fixture.assert_no_blocked();
}

/// The QA-sweep shape that blocked on a follower: a claimed no-diff chore whose
/// `origin/<base>` advances after its run synchronized. `claim_validate` judges
/// the base the run synchronized onto, the handoff records that base rather
/// than the new tip, and the owner accepts and completes it with no operator
/// action, even when the base advances again before landing [ORB-15074].
fn a_claimed_no_diff_chore_completes_after_its_base_advances() {
    for tagged in [false, true] {
        let fixture = if tagged {
            let fixture = CleanLeaf::review_only("done");
            fixture.attach("automation-coverage.json", json!({"findings": []}));
            fixture
        } else {
            CleanLeaf::new(false, "done")
        };
        let tip = fixture.advance_base(1);
        assert_ne!(tip, fixture.base);

        // The shipped pipeline's `validate_no_diff` step is `claim_validate`.
        let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
        assert_eq!(
            handoff.candidate.base.commit, fixture.base,
            "tagged={tagged}"
        );
        assert_eq!(handoff.candidate.candidate, handoff.candidate.base);
        assert_ne!(handoff.candidate.base.commit, tip);
        let settled = fixture
            .settle(&handoff)
            .expect("owner accepts the synchronized base its base still contains");
        assert_eq!(settled["status"], "review", "tagged={tagged}: {settled}");

        let landing_tip = fixture.advance_base(2);
        let accepted = fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .unwrap();
        let landed = engine_action(
            &fixture.pair.wire.owner,
            "handoff_land",
            &json!({"handoff_id": accepted.handoff_id}),
        )
        .unwrap();
        assert_eq!(landed["evidence"]["delivery"], "no_diff", "{landed}");
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
        assert_eq!(
            git(&fixture.pair.owner_repo, &["rev-parse", "main"]).trim(),
            landing_tip,
            "a NoDiff landing moves no branch"
        );
        fixture.assert_no_blocked();
    }
}

/// The same advanced base does not excuse a change: a NoDiff leaf whose
/// worktree carries an uncommitted tracked change or an extra commit after its
/// commit verifier ran is refused at `claim_validate`, naming the change, and
/// the task is not blocked [ORB-15074].
fn a_claimed_no_diff_chore_that_changed_its_worktree_is_refused_by_name() {
    for (tagged, committed, action) in [false, true].into_iter().flat_map(|tagged| {
        [false, true].into_iter().flat_map(move |committed| {
            ["claim_validate", "claim_handoff"]
                .into_iter()
                .map(move |action| (tagged, committed, action))
        })
    }) {
        let fixture = if tagged {
            CleanLeaf::review_only("done")
        } else {
            CleanLeaf::new(false, "done")
        };
        let mut input = fixture.validate_input();
        if action == "claim_handoff" {
            let validated = fixture.action("claim_validate", &input);
            for field in ["candidate", "validation", "no_diff_evidence"] {
                input[field] = validated[field].clone();
            }
        }
        fixture.advance_base(1);
        let follower = &fixture.pair.follower_repo;
        std::fs::write(follower.join("src/f0.rs"), "fn reviewed() {}\n").unwrap();
        if committed {
            git(follower, &["commit", "-q", "-am", "Chore edits code"]);
        }
        let error = engine_action(&fixture.bound, action, &input).unwrap_err();
        let named = if action == "claim_handoff" {
            if committed {
                "carries commits beyond base"
            } else {
                "worktree is not clean"
            }
        } else if committed {
            "Chore edits code"
        } else {
            "src/f0.rs"
        };
        assert!(
            matches!(
                error,
                OrbitError::PolicyDenied(_) | OrbitError::Execution(_)
            ) && error.to_string().contains(named),
            "{action}: tagged={tagged} committed={committed}: {error}"
        );
        assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "a refused leaf records no handoff"
        );
        fixture.assert_no_blocked();
    }
}

/// The check the implement step boundary runs on the implementer's deferral
/// `record`: its gate summary is derived against `gates`, and its probe must
/// show the sandbox's refusal.
fn check_deferral(
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
    fn hand_off_deferral(&mut self) {
        self.input["implementation"]["deferred_sandbox_validation"] = self.deferral(false);
    }

    /// The implementer's record of the gate passing with one sandbox's
    /// deferrals: Seatbelt's, as a macOS lane reports them, or Bubblewrap's.
    fn deferral(&self, seatbelt: bool) -> Value {
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
    fn owner_validated_commands(&self, handoff: &TaskHandoff) -> Vec<String> {
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
    fn refused_pipeline(&self, name: &str) -> String {
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

const NO_DIFF_ROUTES: [&str; 2] = ["task_claimed_pr_pipeline", "task_claimed_local_pipeline"];

/// A clean-base NoDiff handoff replays the gate the implementer's sandbox
/// deferred, even when only `review.baseline_commands` names it, and attaches
/// its log beside the required command's. Before, only the required commands
/// ran and the deferral was silently dropped [ORB-15287].
fn a_claimed_no_diff_handoff_replays_a_baseline_only_deferred_gate() {
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated("done");
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        fixture.hand_off_deferral();
        let handoff = fixture.run_clean_pipeline(job);
        let gate = fixture.gate.as_ref().unwrap();
        assert_eq!(gate.runs(), 1, "{job}: the deferred gate replays once");
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string(), gate.command()],
            "{job}: the replay's log is attached beside the required command's"
        );
        assert!(
            matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }),
            "{job}: the clean-base checkpoint is preserved"
        );
        let settled = fixture
            .settle(&handoff)
            .expect("the owner accepts the replayed handoff");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        fixture.assert_no_blocked();
    }
}

/// A replay that still defers on the validating host never ran the paths it
/// was handed off for: the validation environment's failure, with no passing
/// log attached and no handoff recorded.
fn a_claimed_no_diff_replay_that_still_defers_is_refused() {
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated("done");
        fixture.hand_off_deferral();
        fixture.gate.as_ref().unwrap().defers();
        let refusal = fixture.refused_pipeline(job);
        assert!(
            orbit_types::workflow::is_validation_environment_failure(None, Some(&refusal)),
            "{job}: {refusal}"
        );
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 1, "{job}");
    }
}

/// A replay that reports no executed test replayed nothing, so it cannot
/// carry a deferral.
fn a_claimed_no_diff_replay_that_ran_no_tests_is_refused() {
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated("done");
        fixture.hand_off_deferral();
        fixture.gate.as_ref().unwrap().executes(0);
        let refusal = fixture.refused_pipeline(job);
        assert!(
            !orbit_types::workflow::is_validation_environment_failure(None, Some(&refusal))
                && refusal.contains("replayed nothing"),
            "{job}: a policy denial, not an environment failure: {refusal}"
        );
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 1, "{job}");
    }
}

/// With nothing handed off, a gate named only as a baseline command stays
/// unrun and the ordinary clean-base handoff delivers as before.
fn a_claimed_no_diff_handoff_without_a_deferral_replays_nothing() {
    for job in NO_DIFF_ROUTES {
        let fixture = CleanLeaf::gated("done");
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        let handoff = fixture.run_clean_pipeline(job);
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 0, "{job}");
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string()],
            "{job}"
        );
        let settled = fixture.settle(&handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        fixture.assert_no_blocked();
    }
}

/// [ORB-15622] A replica whose `review.baseline_commands` lacks a gate the
/// owner lists accepts its implementer's Seatbelt or Bubblewrap deferral of
/// that gate, because the leaf holds the owner's gates its claim captured, and
/// `claim_validate` replays it. Before, the replica's own empty list refused
/// every such deferral and the leaf ended held.
fn a_replica_lacking_the_owners_baseline_gate_accepts_and_replays_its_deferral() {
    for (job, seatbelt) in NO_DIFF_ROUTES.into_iter().zip([true, false]) {
        let mut fixture = CleanLeaf::gated_on("done", GateOn::OwnerOnly);
        let gate = fixture.gate.as_ref().unwrap().command();
        assert!(
            RuntimeHost::review_baseline_commands(&fixture.pair.follower).is_empty(),
            "{job}: the replica's own configuration names no baseline gate"
        );
        let gates = RuntimeHost::validation_gates(&fixture.bound);
        assert_eq!(
            gates.source,
            orbit_types::workflow::ValidationGateSource::OwnerContract,
            "{job}"
        );
        assert_eq!(gates.gates.baseline_commands, vec![gate.clone()], "{job}");
        let record = fixture.deferral(seatbelt);
        check_deferral(&record, &gates, &fixture.base)
            .unwrap_or_else(|refusal| panic!("{job} seatbelt={seatbelt}: {refusal}"));
        fixture.input["implementation"]["deferred_sandbox_validation"] = record;
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        let handoff = fixture.run_clean_pipeline(job);
        assert_eq!(
            fixture.gate.as_ref().unwrap().runs(),
            1,
            "{job}: replayed once"
        );
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string(), gate],
            "{job}"
        );
        let settled = fixture.settle(&handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
    }
}

/// [ORB-15622] The inverse drift: a gate only the replica lists is not one
/// of the owner's, so a deferral of it is refused, and the refusal names the
/// gate list it checked and that it came from the claim's admission contract.
fn a_deferral_of_a_replica_only_gate_is_refused_naming_the_owners_gates() {
    let checked = format!(
        "the gates checked are [`{CHECK}`] from the owner's gates captured in this claim's \
         admission contract"
    );
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated_on("done", GateOn::FollowerOnly);
        let refusal = check_deferral(
            &fixture.deferral(false),
            &RuntimeHost::validation_gates(&fixture.bound),
            &fixture.base,
        )
        .unwrap_err();
        assert!(refusal.contains(&checked), "{job}: {refusal}");
        fixture.hand_off_deferral();
        // The policy denial is raised before any command runs, so unlike a
        // replay that ran, it leaves no validation log to point at.
        let refused = fixture.refusal_of(job);
        assert!(refused.contains(&checked), "{job}: {refused}");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "{job}: a refused leaf records no handoff"
        );
        let artifacts =
            RuntimeHost::get_task_artifacts(&fixture.pair.wire.owner, &fixture.task).unwrap();
        assert!(
            artifacts
                .iter()
                .all(|artifact| !artifact.path.starts_with("validation/")),
            "{job}: nothing ran, so nothing is logged"
        );
        fixture.assert_no_blocked();
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 0, "{job}");
    }
}

/// [ORB-15427] A claimed clean-base leaf whose required command fails only
/// with tests its base fails too hands off a known-red log instead of
/// releasing the claim, and the owner takes none of it on the leaf's word:
/// settling the handoff runs the command on the base on the owner itself.
/// When that run fails the same test, the handoff is accepted and the
/// owner's task history names the test. When it passes, the owner releases
/// the claim instead [ORB-15566], with nothing recorded as known red.
#[test]
fn a_claimed_known_red_log_is_accepted_only_on_the_owners_own_base_run() {
    use orbit_types::workflow::{KNOWN_RED_VALIDATION_EVENT, KnownRedValidation};

    if !isolated(
        module_path!(),
        "a_claimed_known_red_log_is_accepted_only_on_the_owners_own_base_run",
    ) {
        return;
    }
    for owner_base_red in [true, false] {
        let red = RedCheck::new();
        let fixture = CleanLeaf::red(&red);
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
        let owner = &fixture.pair.wire.owner;
        let artifacts = RuntimeHost::get_task_artifacts(owner, &fixture.task).unwrap();
        let logs = handoff
            .validation
            .iter()
            .map(|reference| {
                let artifact = artifacts
                    .iter()
                    .find(|artifact| artifact.path == reference.path)
                    .unwrap();
                serde_json::from_slice::<HandoffValidationLog>(&artifact.content).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert_eq!(logs[0].command, red.command());
        assert_eq!(logs[0].exit_code, 3, "the log keeps the exit status");
        let report = logs[0]
            .known_red
            .as_ref()
            .expect("the leaf reports known red");
        assert_eq!(report.base_sha, fixture.base);
        assert_eq!(
            report.failures,
            BTreeSet::from([RedCheck::FAILURE.to_string()])
        );

        let leaf_runs = red.runs();
        if !owner_base_red {
            red.pass();
        }
        let settled = fixture.settle(&handoff);
        assert!(
            red.runs() > leaf_runs,
            "owner_base_red={owner_base_red}: the owner ran the base itself"
        );
        let known_red = fixture.pair.owner_task(&fixture.task)["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["event"] == KNOWN_RED_VALIDATION_EVENT)
            .map(|event| KnownRedValidation::from_text(event["note"].as_str().unwrap()).unwrap())
            .collect::<Vec<_>>();
        if owner_base_red {
            let settled = settled.expect("the owner's base run fails the reported test");
            assert_eq!(settled["status"], "review", "{settled}");
            assert_eq!(known_red.len(), 1, "{known_red:?}");
            assert_eq!(known_red[0].command, red.command());
            assert_eq!(known_red[0].base_sha, fixture.base);
            assert_eq!(
                known_red[0].failures.identities,
                vec![RedCheck::FAILURE.to_string()]
            );
        } else {
            let error = settled
                .expect_err("the owner's base run passes the reported test")
                .to_string();
            assert!(
                error.contains("known_red_contradicted")
                    && error.contains(&format!("base {} passes it", fixture.base)),
                "{error}"
            );
            assert_eq!(fixture.pair.owner_status(&fixture.task), "backlog");
            assert!(known_red.is_empty(), "{known_red:?}");
            assert!(owner.accepted_task_handoff(&handoff.claim_id).is_err());
        }
        fixture.assert_no_blocked();
    }
}

impl RedCheck {
    /// From now on the command runs `body` wherever it runs, still counted.
    fn runs_as(&self, body: &str) {
        std::fs::write(
            &self.script,
            format!("echo ran >> '{}'\n{body}\n", self.runs.display()),
        )
        .unwrap();
    }
}

/// [ORB-15566] A known-red handoff the owner's own base run contradicts can
/// never be accepted, so the owner ends the claim rather than refusing the
/// settlement while holding it: the base passing the reported test, or
/// failing without it, releases the claim as a `transient` failure naming
/// the owner's verdict, and the task returns to the backlog to be pulled
/// again. The follower reconciles the refusal against the ended claim and
/// closes its pending settlement obsolete, freeing its slot. An inconclusive
/// owner check (here, a tool the base run lacks) still refuses while the
/// owner holds the claim, and the follower retries it after its backoff.
fn a_known_red_handoff_the_owners_base_run_contradicts_ends_the_claim() {
    let cases = [
        (
            "passes",
            "echo 'test suite::broken ... ok'\nexit 0",
            Some("passes it"),
        ),
        (
            "fails without it",
            "echo 'test suite::broken ... ok'\necho 'test suite::other ... FAILED'\nexit 3",
            Some("does not fail `test suite::broken`"),
        ),
        (
            "lacks a tool",
            "echo 'sh: 1: frobnicate: not found' >&2\nexit 127",
            None,
        ),
    ];
    for (case, base, contradiction) in cases {
        let red = RedCheck::new();
        let fixture = CleanLeaf::red(&red);
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
        let pair = &fixture.pair;
        assert_eq!(
            pair.admission(&fixture.leaf).phase,
            LocalPullPhase::Settling,
            "{case}: the handoff waits for delivery"
        );
        let leaf_runs = red.runs();
        red.runs_as(base);

        pair.follower.deliver_recorded_pull_settlements();
        assert_eq!(
            pair.wire.calls("orbit.drain.claim.settle").len(),
            1,
            "{case}"
        );
        assert_eq!(
            red.runs(),
            leaf_runs + 1,
            "{case}: the owner ran the base itself"
        );
        let claim = pair.owner_claim(&handoff.claim_id);
        let follower = pair
            .follower
            .pull_leaf_claim(&fixture.leaf)
            .unwrap()
            .unwrap();
        assert!(
            pair.wire
                .owner
                .accepted_task_handoff(&handoff.claim_id)
                .is_err(),
            "{case}: the leaf's log is never accepted"
        );
        let Some(verdict) = contradiction else {
            assert_eq!(claim["claim"]["phase"], "running", "{case}: {claim:#}");
            assert_eq!(pair.owner_status(&fixture.task), "in-progress", "{case}");
            assert_eq!(follower.settlement_phase, "settling", "{case}");
            let refused = follower.settlement_refusal.expect("a recorded refusal");
            assert!(
                refused.reason.contains("known_red_unverified")
                    && refused.reason.contains("lacked a tool"),
                "{case}: {refused:?}"
            );
            assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

            // The follower delivers the same settlement again once its
            // backoff elapses, and the owner answers the same.
            pair.elapse_settlement_backoff(&fixture.leaf);
            pair.follower.deliver_recorded_pull_settlements();
            assert_eq!(
                pair.wire.calls("orbit.drain.claim.settle").len(),
                2,
                "{case}"
            );
            let follower = pair
                .follower
                .pull_leaf_claim(&fixture.leaf)
                .unwrap()
                .unwrap();
            assert_eq!(follower.settlement_phase, "settling", "{case}");
            assert_eq!(
                follower.settlement_refusal.map(|refused| refused.refusals),
                Some(2),
                "{case}"
            );
            assert_eq!(pair.owner_status(&fixture.task), "in-progress", "{case}");
            continue;
        };
        assert_eq!(claim["claim"]["phase"], "revoked", "{case}: {claim:#}");
        let task = pair.owner_task(&fixture.task);
        assert_eq!(task["status"], "backlog", "{case}: {task:#}");
        let note = task["history"]
            .as_array()
            .unwrap()
            .iter()
            .rfind(|event| event["event"] == "claim_released")
            .and_then(|event| event["note"].as_str())
            .unwrap_or_default();
        assert!(
            note.contains("known_red_contradicted")
                && note.contains(&format!("base {} {verdict}", fixture.base)),
            "{case}: the owner's verdict is the release reason: {task:#}"
        );
        assert_eq!(follower.settlement_phase, "settled", "{case}");
        let refusal = follower.refusal.expect("closed as obsolete");
        assert!(
            refusal.contains("known_red_contradicted") && refusal.contains("already ended"),
            "{case}: {refusal}"
        );
        assert_eq!(
            pair.follower.pending_pull_settlements().unwrap().count,
            0,
            "{case}: the slot is free"
        );
        fixture.assert_no_blocked();
    }
}

/// A base run that cannot finish within settlement's budget is cut once,
/// then retries reuse its durable inconclusive outcome, including on a newly
/// opened owner session. Partial failed-test output never admits a handoff.
///
/// The budget is a generous ceiling for reaching the base, which host load can
/// stretch. The cut comes from the supervisor's clock once the stalled base
/// reports it is running, so no wall time is waited out [ORB-16402].
#[test]
#[cfg(unix)]
fn a_slow_known_red_base_run_converges_to_one_cut_over_settle_retries() {
    use orbit_common::test_env::{SupervisorSeam, with_supervision_clock, with_supervisor_seam};
    use std::{cell::Cell, rc::Rc, time::Instant};

    if !isolated(
        module_path!(),
        "a_slow_known_red_base_run_converges_to_one_cut_over_settle_retries",
    ) {
        return;
    }
    let red = RedCheck::new();
    let fixture = CleanLeaf::red(&red);
    fixture.attach("automation-coverage.json", json!({"findings": []}));
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let leaf_runs = red.runs();

    // No timing race or load: the base prints the claimed failure and reports
    // readiness, then blocks on a FIFO with no writer until the supervisor cuts
    // its process group. The cut happens when the supervisor's clock passes the
    // attempt's limit, which this test advances only at that readiness point.
    let budget = Duration::from_secs(600);
    let fifo = red._dir.path().join("wait");
    let ready = red._dir.path().join("ready");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        &red.script,
        format!(
            "echo ran >> '{}'\necho 'test suite::broken ... FAILED'\ntouch '{}'\nread ignored < '{}'\nexit 3\n",
            red.runs.display(),
            ready.display(),
            fifo.display(),
        ),
    )
    .unwrap();
    let offset = Rc::new(Cell::new(Duration::ZERO));
    let (clock_offset, wait_offset) = (Rc::clone(&offset), Rc::clone(&offset));
    let seam = SupervisorSeam {
        wait: Some(Box::new(move |child| {
            if ready.exists() {
                std::fs::remove_file(&ready).unwrap();
                // Advancing the clock here takes effect on the supervisor's
                // next deadline check, which follows this poll.
                wait_offset.set(budget);
                return Ok(None);
            }
            child.try_wait()
        })),
        ..SupervisorSeam::default()
    };
    let settle = |owner: &OrbitRuntime| {
        owner
            .run_tool_with_context_and_role(
                "orbit.drain.claim.settle",
                json!({
                    "claim_id": handoff.claim_id, "run_id": fixture.leaf,
                    "settlement": ClaimMutation::AcceptHandoff(handoff.clone()),
                }),
                Role::Admin,
                ToolContext {
                    session_context: ToolSessionContext {
                        caller_machine_id: Some(FOLLOWER.to_string()),
                        process_machine_id: Some(OWNER.to_string()),
                        transport: Some(McpTransport::SshMcp),
                        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                        ..ToolSessionContext::default()
                    },
                    ..ToolContext::default()
                },
            )
            .expect_err("a cut base never confirms known red")
            .to_string()
    };
    let owner = fixture
        .pair
        .wire
        .owner
        .clone()
        .with_claim_known_red_budget(budget);
    let error = with_supervision_clock(
        move || Instant::now() + clock_offset.get(),
        || with_supervisor_seam(seam, || settle(&owner)),
    );
    assert!(
        error.contains("known_red_unverified") && error.contains("comparison deadline"),
        "{error}"
    );
    assert_eq!(
        offset.get(),
        budget,
        "the stalled base reached readiness before its deadline cut"
    );
    assert_eq!(
        red.runs(),
        leaf_runs + 1,
        "the owner starts just one base run"
    );

    // A command now able to complete still must not restart on settlement
    // retry. Reopening the runtime rules out a process-local refusal cache.
    std::fs::write(
        &red.script,
        format!(
            "echo ran >> '{}'\necho 'test suite::broken ... ok'\nexit 0\n",
            red.runs.display(),
        ),
    )
    .unwrap();
    for _ in 0..2 {
        let reopened =
            OrbitRuntime::from_roots(&owner.paths().global_dir, &owner.paths().orbit_dir)
                .unwrap()
                .with_automation_machine_identity(Some(OWNER.to_string()))
                .with_claim_known_red_budget(budget);
        assert_eq!(settle(&reopened), error);
        assert_eq!(red.runs(), leaf_runs + 1, "settle retries reuse the cut");
    }
    assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
    assert!(owner.accepted_task_handoff(&handoff.claim_id).is_err());
    assert!(
        fixture.pair.owner_task(&fixture.task)["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["event"] != orbit_types::workflow::KNOWN_RED_VALIDATION_EVENT)
    );
    fixture.assert_no_blocked();
}

/// A stand-in `cargo` the follower's validation PATH resolves first, for an
/// already-resolved replay [ORB-15747]. It records each argv and prints what
/// it was last told to, from outside both checkouts so neither tree changes.
#[cfg(unix)]
struct Replay {
    _dir: TempDir,
    bin: PathBuf,
    script: PathBuf,
    runs: PathBuf,
}

#[cfg(unix)]
impl Replay {
    /// The incident's selector: a test that skips inside the agent sandbox.
    const SELECTOR: &'static str =
        "cargo test -p orbit-cli --test process cli_ctrl_c_during_proc_spawn_reports_interrupt";
    const TEST: &'static str = "cli_ctrl_c_during_proc_spawn_reports_interrupt";

    fn new() -> Self {
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
    fn config(&self) -> String {
        format!(
            "[workflow.validation_env]\nlogin_shell = false\npath = [\"{}\"]\n",
            self.bin.display()
        )
    }

    fn set(&self, body: &str) {
        std::fs::write(&self.script, body).unwrap();
    }

    /// libtest's report of `tests` passing tests.
    fn passes(&self, tests: u64) {
        self.set(&format!(
            "echo 'test {} ... ok'\necho 'test result: ok. {tests} passed; 0 failed; 0 ignored; \
             0 measured; 0 filtered out; finished in 0.01s'\n",
            Self::TEST
        ));
    }

    /// The named test still fails natively.
    fn fails(&self) {
        self.set(&format!(
            "echo 'test {test} ... FAILED'\necho 'interrupt was not reported'\necho 'test \
             result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.01s'\nexit 101\n",
            test = Self::TEST
        ));
    }

    /// The test returns without exercising its path, even natively.
    fn skips(&self) {
        self.set(&format!(
            "echo 'skipping: {} needs a terminal'\necho 'test result: ok. 1 passed; 0 failed; \
             0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\n",
            Self::TEST
        ));
    }

    /// Each argv the stand-in was run with.
    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.runs)
            .map(|runs| runs.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// The command line the claim host runs for [`Self::SELECTOR`].
    fn host_command() -> String {
        format!("{} -- --nocapture", Self::SELECTOR)
    }
}

/// [ORB-15747] A claimed leaf that finds its base already resolves the task,
/// shown only by a test that skips inside the agent sandbox, names that test
/// as its already-resolved outcome. Its claim host replays it on the base
/// outside the agent sandbox, the passing log joins the handoff beside the
/// required command's, and the owner completes the task without a PR.
/// Before, an empty diff gave the affected-test gate nothing to run, and the
/// task blocked until an operator ran the test by hand.
#[cfg(unix)]
fn a_claimed_already_resolved_base_replays_its_named_tests_and_completes_without_a_pr() {
    for job in NO_DIFF_ROUTES {
        let replay = Replay::new();
        let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
        let handoff = fixture.run_clean_pipeline(job);
        assert_eq!(
            replay.runs(),
            vec![format!(
                "test -p orbit-cli --test process {} -- --nocapture",
                Replay::TEST
            )],
            "{job}: the named test replays once, through the host's grammar"
        );
        assert_eq!(handoff.candidate.candidate, handoff.candidate.base, "{job}");
        assert_eq!(handoff.candidate.base.commit, fixture.base, "{job}");
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string(), Replay::host_command()],
            "{job}: the replay is recorded beside the required command"
        );
        let artifacts =
            RuntimeHost::get_task_artifacts(&fixture.pair.wire.owner, &fixture.task).unwrap();
        let replayed = artifacts
            .iter()
            .find(|artifact| artifact.path == handoff.validation[1].path)
            .expect("the owner holds the replay log the handoff pins");
        let log: HandoffValidationLog = serde_json::from_slice(&replayed.content).unwrap();
        assert_eq!(log.tested_head, fixture.base, "{job}");
        assert!(
            log.output.contains("test result: ok. 1 passed"),
            "{job}: {log:?}"
        );

        let settled = fixture.settle(&handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        let accepted = fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .unwrap();
        engine_action(
            &fixture.pair.wire.owner,
            "handoff_land",
            &json!({"handoff_id": accepted.handoff_id}),
        )
        .unwrap();
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done", "{job}");
        fixture.assert_no_blocked();
    }
}

/// [ORB-15747] A named test that still fails natively fails the step with
/// its output, before any handoff, and the leaf's settlement blocks the task
/// on the owner with that output; the full log is on the owner's task.
#[cfg(unix)]
fn a_failing_already_resolved_replay_blocks_the_task_with_its_output() {
    let replay = Replay::new();
    replay.fails();
    let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
    let input = fixture.validate_input();
    let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::Execution(_))
            && message.contains("test_failed")
            && message.contains("interrupt was not reported"),
        "{message}"
    );
    assert_eq!(replay.runs().len(), 1);
    assert!(
        fixture.pair.admission(&fixture.leaf).settlement.is_none(),
        "a failed replay records no handoff"
    );
    let artifacts =
        RuntimeHost::get_task_artifacts(&fixture.pair.wire.owner, &fixture.task).unwrap();
    let failed = artifacts
        .iter()
        .find(|artifact| artifact.path.ends_with("/already-resolved-0.failed.json"))
        .expect("the failed replay's log is on the owner's task");
    assert!(message.contains(&failed.path), "{message}");
    let log: Value = serde_json::from_slice(&failed.content).unwrap();
    assert_eq!(log["exit_code"], 101, "{log}");
    assert_eq!(log["tested_head"], fixture.base.as_str(), "{log}");
    assert!(
        log["output"]
            .as_str()
            .is_some_and(|output| output.contains("interrupt was not reported")),
        "{log}"
    );

    // The leaf ends on that step's failure, and its settlement blocks the task.
    fixture.pair.leaf_fails_with(&fixture.leaf, &message);
    let pass = fixture
        .pair
        .pass_over(&fixture.drain, json!({"window_expired": true}));
    assert_eq!(
        fixture.pair.owner_status(&fixture.task),
        "blocked",
        "{pass}"
    );
    let task = fixture.pair.owner_task(&fixture.task).to_string();
    assert!(
        task.contains("test result: FAILED. 0 passed; 1 failed"),
        "the owner's task names the failing output: {task}"
    );
}

/// [ORB-15747] An already-resolved outcome that would prove nothing is
/// refused rather than settled: no selectors, a selector outside the `cargo
/// test` grammar (refused before anything runs), a replay that runs no test
/// or skips its path, and a declaration on a candidate that changes its base.
#[test]
#[cfg(unix)]
fn an_already_resolved_claim_that_runs_no_named_test_is_refused() {
    if !isolated(
        module_path!(),
        "an_already_resolved_claim_that_runs_no_named_test_is_refused",
    ) {
        return;
    }
    for (selectors, refusal) in [
        (vec![], "names no test selectors"),
        (vec!["make ci-test-affected"], "command_not_allowed"),
        (
            vec!["cargo test -p orbit-cli --test process x;touch ran"],
            "shell_metacharacter",
        ),
        (vec![Replay::SELECTOR, Replay::SELECTOR], "twice"),
    ] {
        let replay = Replay::new();
        let fixture = CleanLeaf::resolved(&replay, &selectors);
        let input = fixture.validate_input();
        let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)) && error.to_string().contains(refusal),
            "{selectors:?}: {error}"
        );
        assert!(replay.runs().is_empty(), "{selectors:?}: nothing replays");
        let artifacts =
            RuntimeHost::get_task_artifacts(&fixture.pair.wire.owner, &fixture.task).unwrap();
        assert!(
            artifacts
                .iter()
                .all(|artifact| !artifact.path.starts_with("validation/")),
            "{selectors:?}: refused before any command runs"
        );
        assert!(fixture.pair.admission(&fixture.leaf).settlement.is_none());
    }

    for (behave, refusal) in [
        (Replay::passes as fn(&Replay, u64), "no_tests_ran"),
        (|replay: &Replay, _| replay.skips(), "self_skipped"),
    ] {
        let replay = Replay::new();
        behave(&replay, 0);
        let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
        let input = fixture.validate_input();
        let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)) && error.to_string().contains(refusal),
            "{refusal}: {error}"
        );
        assert_eq!(replay.runs().len(), 1, "{refusal}");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "{refusal}: no handoff"
        );
    }

    // Off the NoDiff route, the candidate changed its base.
    let replay = Replay::new();
    let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
    let error = engine_action(&fixture.bound, "claim_validate", &fixture.input).unwrap_err();
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)) && error.to_string().contains("changes it"),
        "{error}"
    );
    assert!(replay.runs().is_empty());
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    verified_no_diff_and_already_satisfied_base_complete_without_a_pr_scenarios,
    verified_no_diff_and_already_satisfied_base_complete_without_a_pr,
    a_follower_claims_no_diff_expected_work_files_findings_and_completes_without_a_pr
);

crate::scenario_table!(
    a_claimed_no_diff_expected_review_without_a_report_settles_like_the_owner_scenarios,
    an_owner_accepts_a_tagged_skip_that_still_carries_logs_and_checks_them,
    a_claimed_no_diff_expected_task_that_changed_the_worktree_is_refused
);

crate::scenario_table!(
    owner_refuses_a_no_diff_expected_skip_for_an_untagged_task_scenarios,
    owner_refuses_a_no_diff_expected_skip_for_an_untagged_task,
    a_claimed_delivery_review_files_regression_findings_and_hands_off_the_rest,
    a_published_candidates_string_findings_are_normalized_at_handoff
);

crate::scenario_table!(
    no_diff_retains_owner_completion_authority_scenarios,
    no_diff_retains_owner_completion_authority,
    owner_refuses_changed_no_diff_evidence_base_or_missing_required_checks_without_blocking,
    claimed_no_diff_requires_a_verifier_checkpoint_and_passing_logs
);

crate::scenario_table!(
    a_claimed_no_diff_chore_completes_after_its_base_advances_scenarios,
    a_claimed_no_diff_chore_completes_after_its_base_advances,
    a_claimed_no_diff_chore_that_changed_its_worktree_is_refused_by_name
);

crate::scenario_table!(
    a_claimed_no_diff_handoff_replays_a_baseline_only_deferred_gate_scenarios,
    a_claimed_no_diff_handoff_replays_a_baseline_only_deferred_gate,
    a_claimed_no_diff_replay_that_still_defers_is_refused,
    a_claimed_no_diff_replay_that_ran_no_tests_is_refused
);

crate::scenario_table!(
    a_claimed_no_diff_handoff_without_a_deferral_replays_nothing_scenarios,
    a_claimed_no_diff_handoff_without_a_deferral_replays_nothing,
    a_replica_lacking_the_owners_baseline_gate_accepts_and_replays_its_deferral
);

crate::scenario_table!(
    a_deferral_of_a_replica_only_gate_is_refused_naming_the_owners_gates_scenarios,
    a_deferral_of_a_replica_only_gate_is_refused_naming_the_owners_gates,
    a_known_red_handoff_the_owners_base_run_contradicts_ends_the_claim
);

#[cfg(unix)]
crate::scenario_table!(
    a_claimed_already_resolved_base_replays_its_named_tests_and_completes_without_a_pr_scenarios,
    a_claimed_already_resolved_base_replays_its_named_tests_and_completes_without_a_pr,
    a_failing_already_resolved_replay_blocks_the_task_with_its_output
);
