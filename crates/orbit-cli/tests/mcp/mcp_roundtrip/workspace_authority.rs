//! Which generation authority may migrate a workspace layout that recorded
//! none, through real `orbit` processes over private homes and checkouts.
//!
//! Every row builds two private authorities: A, whose home created the
//! checkout and whose client is live, and B, an unrelated home that also
//! registered the checkout. Before any candidate runs, every resolved path is
//! asserted to sit under its fixture's temp root and outside the real home.
use super::*;
use orbit_common::fs::generation::{
    Access, GenerationGuard, GenerationUpdate, Participant, ParticipantRole, QUIESCE_TIMEOUT_ENV,
    executable_generation,
};

const RECORD: &str = "state/generation-authority.json";
const LEDGER: &str = "state/generation-authorities.json";

struct TwoAuthorities {
    protected: McpWorkspace,
    foreign: McpWorkspace,
    authority_a: PathBuf,
    authority_b: PathBuf,
    workspace_root: PathBuf,
    current: u32,
    old_layout: u32,
}

impl TwoAuthorities {
    /// A's checkout, also registered by B while its layout was current.
    fn init() -> Self {
        let protected = McpWorkspace::init();
        let foreign = McpWorkspace::init_with_workspace_name("foreign-authority");
        protected.run_ok(&["task", "list"]);
        cli_ok(
            &protected.work,
            &foreign.home,
            &["workspace", "init", "--name", "mcp-roundtrip"],
        );
        let current = orbit_core::composition::compiled_compatibility()
            .workspace_layout
            .version;
        let fixture = Self {
            authority_a: protected.home.join(".orbit"),
            authority_b: foreign.home.join(".orbit"),
            workspace_root: protected.work.join(".orbit"),
            protected,
            foreign,
            current,
            old_layout: current - 1,
        };
        fixture.assert_private();
        fixture
    }

    fn assert_private(&self) {
        let real_authority =
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".orbit"));
        let real_authority = real_authority.and_then(|real| real.canonicalize().ok());
        for (path, temp) in [
            (&self.authority_a, self.protected._temp.path()),
            (&self.workspace_root, self.protected._temp.path()),
            (&self.authority_b, self.foreign._temp.path()),
        ] {
            let resolved = path.canonicalize().expect("fixture path resolves");
            let temp = temp.canonicalize().expect("fixture temp resolves");
            assert!(
                resolved.starts_with(&temp),
                "{} escapes its fixture",
                resolved.display()
            );
            if let Some(real) = real_authority.as_ref() {
                assert!(
                    !resolved.starts_with(real),
                    "{} is a live authority",
                    resolved.display()
                );
            }
        }
        assert_ne!(self.authority_a, self.authority_b);
    }

    /// The legacy shape: a layout migration pending and no recorded
    /// authority, as every workspace last opened before the record existed.
    fn rewind_unrecorded(&self) {
        remove_if_present(&self.workspace_root.join(RECORD));
        self.rewind();
    }

    fn rewind(&self) {
        std::fs::write(self.marker(), format!("{}\n", self.old_layout)).expect("rewind marker");
    }

    fn marker(&self) -> PathBuf {
        self.workspace_root.join("state/layout.version")
    }

    fn marker_version(&self) -> String {
        std::fs::read_to_string(self.marker()).expect("layout marker")
    }

    fn record(&self) -> Option<Vec<u8>> {
        std::fs::read(self.workspace_root.join(RECORD)).ok()
    }

    fn recorded_authority(&self) -> Option<PathBuf> {
        let record: Value = serde_json::from_slice(&self.record()?).expect("record JSON");
        record["authority"].as_str().map(PathBuf::from)
    }

    /// A's client, compiled before the newest layout migration.
    fn join_old_client(&self, authority: &Path) -> GenerationGuard {
        let mut identity = orbit_core::composition::compiled_compatibility();
        identity.workspace_layout.version = self.old_layout;
        identity.workspace_layout.writer_floor = self.old_layout;
        identity.workspace_layout.reader_floor = self.old_layout;
        let digest = executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit")))
            .expect("fixture executable digest");
        let participant = Participant {
            digest: &digest,
            identity: &identity,
            role: ParticipantRole::McpServe,
            access: Access::Write,
            handover: None,
            in_activity: false,
        };
        GenerationGuard::join(authority, &participant, Duration::ZERO, || Ok(0))
            .expect("old client joins its authority")
    }

    /// `orbit task add` from `cwd` under `home`, with any extra global args.
    fn task_add(&self, cwd: &Path, home: &Path, global: &[&str]) -> std::process::Output {
        McpWorkspace::orbit_command(cwd, home)
            .env(QUIESCE_TIMEOUT_ENV, "0")
            .args(global)
            .args([
                "task",
                "add",
                "--title",
                "Layout probe",
                "--complexity",
                "low",
                "--json",
            ])
            .output()
            .expect("task add")
    }

    fn adopt(&self, cwd: &Path, home: &Path, claimed: &Path) -> std::process::Output {
        McpWorkspace::orbit_command(cwd, home)
            .env(QUIESCE_TIMEOUT_ENV, "0")
            .args(["migrate", "--adopt-authority"])
            .arg(claimed)
            .arg("--json")
            .output()
            .expect("adopt authority")
    }
}

fn remove_if_present(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("private fixture file {}: {error}", path.display()),
    }
}

/// Every file under `root` with its bytes, without following symlinks; with
/// `skip_admission`, except the generation admission files a workspace data
/// directory keeps for itself while it is an explicit root.
fn tree_bytes(root: &Path, skip_admission: bool) -> BTreeSet<(PathBuf, Vec<u8>)> {
    let mut files = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read fixture tree") {
            let entry = entry.expect("fixture entry");
            if skip_admission
                && dir == root
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".generation")
            {
                continue;
            }
            let kind = entry.file_type().expect("fixture entry type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.insert((
                    entry.path(),
                    std::fs::read(entry.path()).expect("fixture file"),
                ));
            }
        }
    }
    files
}

fn assert_refused_with(output: &std::process::Output, needle: &str) {
    assert!(!output.status.success(), "unexpected success: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(needle),
        "expected {needle:?}: {output:?}"
    );
}

/// Whether some participant of `authority` is live: an update cannot take it.
fn has_live_participant(authority: &Path) -> bool {
    GenerationUpdate::acquire(authority).is_err()
}

/// [ORB-20272] Neither the workspace data directory as an explicit `--root`
/// nor an unrelated authority that registered the checkout may migrate an
/// unrecorded workspace layout under A's live client, which the pre-fix
/// build allowed. Nothing in the workspace, A's authority or A's participant
/// changes.
fn explicit_root_and_registered_foreign_authority_cannot_migrate_an_unrecorded_workspace() {
    let fixture = TwoAuthorities::init();
    fixture.rewind_unrecorded();
    let old_client = fixture.join_old_client(&fixture.authority_a);
    let root = fixture.workspace_root.to_string_lossy().into_owned();
    let attempts: [(&str, &Path, &Path, Vec<&str>); 2] = [
        (
            "explicit --root",
            &fixture.foreign.home,
            &fixture.foreign.home,
            vec!["--root", &root],
        ),
        (
            "B-registered checkout",
            &fixture.protected.work,
            &fixture.foreign.home,
            Vec::new(),
        ),
    ];
    for (name, cwd, home, global) in attempts {
        let before = (
            tree_bytes(&fixture.workspace_root, true),
            tree_bytes(&fixture.authority_a, false),
        );
        let output = fixture.task_add(cwd, home, &global);
        assert_eq!(
            fixture.marker_version(),
            format!("{}\n", fixture.old_layout),
            "{name}: advanced the layout under A's live client: {output:?}"
        );
        assert_eq!(
            (
                tree_bytes(&fixture.workspace_root, true),
                tree_bytes(&fixture.authority_a, false),
            ),
            before,
            "{name}: must refuse before any workspace or authority A write"
        );
        assert_refused_with(&output, "pending layout upgrade");
        assert!(
            has_live_participant(&fixture.authority_a),
            "{name}: A's client must stay live"
        );
    }
    drop(old_client);
}

/// [ORB-20272] An unrecorded workspace is claimed only by an explicit,
/// fenced adoption: a current-layout open by B records nothing; adoption by
/// any authority refuses while a client of an authority that used the
/// workspace is live; a record is never replaced, and re-running an adoption
/// that already landed is a no-op. Afterwards a breaking migration still
/// waits for every authority that used the workspace, and applies once its
/// clients are gone.
fn adoption_fences_every_authority_that_used_the_workspace() {
    let fixture = TwoAuthorities::init();
    remove_if_present(&fixture.workspace_root.join(RECORD));

    // B opens the current layout: admitted, but it claims nothing.
    cli_ok(
        &fixture.protected.work,
        &fixture.foreign.home,
        &["task", "list"],
    );
    assert_eq!(fixture.record(), None, "a current-layout open recorded B");
    let ledger: Value = serde_json::from_slice(
        &std::fs::read(fixture.workspace_root.join(LEDGER)).expect("authority ledger"),
    )
    .expect("ledger JSON");
    let listed: Vec<PathBuf> = ledger["authorities"]
        .as_array()
        .expect("ledger authorities")
        .iter()
        .map(|path| PathBuf::from(path.as_str().expect("ledger path")))
        .collect();
    for authority in [&fixture.authority_a, &fixture.authority_b] {
        let authority = authority.canonicalize().expect("authority resolves");
        assert!(
            listed.contains(&authority),
            "{listed:?} lacks {}",
            authority.display()
        );
    }

    fixture.rewind();
    let old_client = fixture.join_old_client(&fixture.authority_a);
    let before = tree_bytes(&fixture.workspace_root, false);
    // A's own command waits for its old client first.
    assert_refused_with(
        &fixture.task_add(&fixture.protected.work, &fixture.protected.home, &[]),
        "upgrade admission refused",
    );
    // B's adoption fences A, whose client is live; A's fences itself.
    let by_b = fixture.adopt(
        &fixture.protected.work,
        &fixture.foreign.home,
        &fixture.authority_b,
    );
    assert_refused_with(&by_b, "workspace authority adoption refused");
    let by_a = fixture.adopt(
        &fixture.protected.work,
        &fixture.protected.home,
        &fixture.authority_a,
    );
    assert_refused_with(&by_a, "workspace authority adoption refused");
    // A claim naming an authority other than the process's own.
    let misnamed = fixture.adopt(
        &fixture.protected.work,
        &fixture.foreign.home,
        &fixture.authority_a,
    );
    assert_refused_with(&misnamed, "adoption records only its own authority");
    // An initialized authority that never registered the checkout.
    let unregistered = McpWorkspace::init_with_workspace_name("unregistered-authority");
    let by_unregistered = fixture.adopt(
        &fixture.protected.work,
        &unregistered.home,
        &unregistered.home.join(".orbit"),
    );
    assert_refused_with(&by_unregistered, "has not registered this checkout");
    assert_eq!(tree_bytes(&fixture.workspace_root, false), before);
    assert_eq!(
        fixture.marker_version(),
        format!("{}\n", fixture.old_layout)
    );
    drop(old_client);

    // Even quiescent, not A either: nothing shows it protects the workspace.
    let own = fixture.task_add(&fixture.protected.work, &fixture.protected.home, &[]);
    assert_refused_with(&own, "orbit migrate --adopt-authority");
    assert_eq!(
        fixture.marker_version(),
        format!("{}\n", fixture.old_layout)
    );

    // Quiescent, A adopts; a retry after a lost reply changes nothing.
    let adopted = cli_ok(
        &fixture.protected.work,
        &fixture.protected.home,
        &[
            "migrate",
            "--adopt-authority",
            &fixture.authority_a.to_string_lossy(),
            "--json",
        ],
    );
    let adopted: Value = serde_json::from_slice(&adopted.stdout).expect("adoption JSON");
    assert_eq!(
        adopted["workspace_authority"]["recorded"], true,
        "{adopted}"
    );
    assert_eq!(
        adopted["workspace_authority"]["evidence"], "registered",
        "{adopted}"
    );
    let record = fixture.record().expect("adopted record");
    assert_eq!(
        fixture
            .recorded_authority()
            .map(|path| path.canonicalize().expect("recorded authority resolves")),
        Some(fixture.authority_a.canonicalize().expect("authority A"))
    );
    let retried = cli_ok(
        &fixture.protected.work,
        &fixture.protected.home,
        &[
            "migrate",
            "--adopt-authority",
            &fixture.authority_a.to_string_lossy(),
            "--json",
        ],
    );
    let retried: Value = serde_json::from_slice(&retried.stdout).expect("retry JSON");
    assert_eq!(
        retried["workspace_authority"]["recorded"], false,
        "{retried}"
    );
    // B can neither replace the record nor migrate under it.
    let replaced = fixture.adopt(
        &fixture.protected.work,
        &fixture.foreign.home,
        &fixture.authority_b,
    );
    assert_refused_with(&replaced, "never replaces");
    assert_refused_with(
        &fixture.task_add(&fixture.protected.work, &fixture.foreign.home, &[]),
        "pending layout upgrade",
    );
    assert_eq!(fixture.record(), Some(record));
    assert_eq!(
        fixture.marker_version(),
        format!("{}\n", fixture.old_layout)
    );

    // The recorded authority still waits for its own incompatible client,
    // and for any live client of B, which also used the workspace.
    for (authority, why, refusal) in [
        (&fixture.authority_a, "A's own old client", "did not yield"),
        (
            &fixture.authority_b,
            "a client of B",
            "has also used this workspace",
        ),
    ] {
        let client = fixture.join_old_client(authority);
        let refused = fixture.task_add(&fixture.protected.work, &fixture.protected.home, &[]);
        assert_refused_with(&refused, refusal);
        assert_eq!(
            fixture.marker_version(),
            format!("{}\n", fixture.old_layout),
            "{why}: migrated under a live client"
        );
        drop(client);
    }
    let migrated = fixture.task_add(&fixture.protected.work, &fixture.protected.home, &[]);
    assert!(migrated.status.success(), "{migrated:?}");
    assert_eq!(fixture.marker_version(), format!("{}\n", fixture.current));
}

/// [ORB-20272] An unreadable or malformed record refuses every pending
/// migration and every adoption, and nothing replaces it.
fn a_malformed_record_fails_closed() {
    let fixture = TwoAuthorities::init();
    fixture.rewind();
    std::fs::write(fixture.workspace_root.join(RECORD), "{\"authority\":").expect("malform");
    let record = fixture.record();
    assert_refused_with(
        &fixture.task_add(&fixture.protected.work, &fixture.protected.home, &[]),
        "is malformed",
    );
    assert_refused_with(
        &fixture.adopt(
            &fixture.protected.work,
            &fixture.protected.home,
            &fixture.authority_a,
        ),
        "never replaces",
    );
    assert_eq!(fixture.record(), record);
    assert_eq!(
        fixture.marker_version(),
        format!("{}\n", fixture.old_layout)
    );
}

#[test]
fn workspace_authority_adoption() {
    for (name, check) in [
        (
            "explicit_root_and_registered_foreign_authority_cannot_migrate_an_unrecorded_workspace",
            explicit_root_and_registered_foreign_authority_cannot_migrate_an_unrecorded_workspace
                as fn(),
        ),
        (
            "adoption_fences_every_authority_that_used_the_workspace",
            adoption_fences_every_authority_that_used_the_workspace as fn(),
        ),
        (
            "a_malformed_record_fails_closed",
            a_malformed_record_fails_closed as fn(),
        ),
    ] {
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}
