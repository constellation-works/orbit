//! A backend the host spawns on an agent's behalf runs under the plugin
//! profile ∩ the calling agent's profile, widened only by its own
//! `{{plugin_state}}` (design `docs/design/plugins/2_agent_call_broker.md`
//! §5). The live tests spawn through the real confinement: Landlock on Linux,
//! `sandbox-exec` on macOS.

use orbit_exec::{EnvironmentMode, ExecRequest, Sandbox, StdinMode};
use orbit_types::policy::{ResolvedFsProfile, compile_glob_regex};

use super::super::super::backend::{BrokeredCaller, PluginSandboxProfile};
use super::*;

/// A checkout, a plugin whose grants reach into it, and a synthetic agent
/// profile that excludes part of what the plugin was granted.
struct Fixture {
    _temp: tempfile::TempDir,
    worktree: PathBuf,
    spec: PluginBackendSpec,
}

impl Fixture {
    fn new(read: &[&str], write: &[&str]) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        // Physical, so a macOS `/var` alias cannot split a rule from the path
        // it is decided against.
        let base = temp.path().canonicalize().expect("canonical tempdir");
        let worktree = base.join("checkout");
        for dir in ["allowed", "denied", "gen/out", "private", "sub"] {
            std::fs::create_dir_all(worktree.join(dir)).expect("worktree dir");
        }
        for (file, contents) in [
            ("public.txt", "public"),
            ("secret.txt", "secret"),
            ("sub/.env", "TOKEN=1"),
            ("private/key.txt", "key"),
            ("denied/existing.txt", "existing"),
        ] {
            std::fs::write(worktree.join(file), contents).expect("worktree file");
        }
        let root = base.join("plugin");
        std::fs::create_dir_all(&root).expect("plugin root");
        let permissions = PluginPermissions {
            fs: PluginFsPermissions {
                read: read.iter().map(|root| (*root).into()).collect(),
                write: write.iter().map(|root| (*root).into()).collect(),
            },
            ..PluginPermissions::default()
        };
        let spec = (*spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])).clone();
        Self {
            _temp: temp,
            worktree,
            spec,
        }
    }

    fn caller(&self, read: &[&str], modify: &[&str]) -> BrokeredCaller {
        BrokeredCaller {
            worktree: self.worktree.clone(),
            fs_profile: ResolvedFsProfile {
                name: "implementer".to_string(),
                read: read.iter().map(|rule| (*rule).to_string()).collect(),
                modify: modify.iter().map(|rule| (*rule).to_string()).collect(),
            },
            proc_allowed_programs: Vec::new(),
        }
    }

    fn brokered(&self, caller: &BrokeredCaller) -> PluginSandboxProfile {
        self.spec
            .brokered_sandbox_profile(caller)
            .expect("brokered profile")
    }

    /// Run `script` under `profile` through the platform confinement and
    /// return what it printed.
    fn run(&self, profile: &PluginSandboxProfile, script: &str) -> String {
        let request = ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            current_dir: None,
            timeout_ms: Some(10_000),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                (
                    "WT".to_string(),
                    self.worktree.to_string_lossy().into_owned(),
                ),
                (
                    "STATE".to_string(),
                    self.spec.state_dir.to_string_lossy().into_owned(),
                ),
            ]),
            debug: false,
        };
        let child = profile.spawn(&request).expect("spawn confined backend");
        let output = child.wait_with_output().expect("wait for backend");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

const WRITE_PROBE: &str = "for target in \"$WT/allowed/new.txt\" \"$WT/denied/new.txt\" \
     \"$WT/denied/existing.txt\" \"$STATE/token.json\"; do \
     if echo x > \"$target\" 2>/dev/null; then echo \"wrote $target\"; fi; done";

const READ_PROBE: &str = "for target in \"$WT/public.txt\" \"$WT/secret.txt\" \"$WT/sub/.env\" \
     \"$WT/private/key.txt\"; do \
     if cat \"$target\" >/dev/null 2>&1; then echo \"read $target\"; fi; done; \
     if ls \"$WT\" >/dev/null 2>&1; then echo listed; fi";

/// Last-match-wins over absolute rules in the profile grammar: how the
/// seatbelt compiler orders the clauses it emits for them.
fn seatbelt_allows(rules: &[String], path: &Path) -> bool {
    let path = path.to_string_lossy();
    rules
        .iter()
        .rev()
        .find_map(|rule| {
            let (negated, body) = rule
                .strip_prefix('!')
                .map_or((false, rule.as_str()), |body| (true, body));
            compile_glob_regex(body)
                .expect("rule compiles")
                .is_match(&path)
                .then_some(!negated)
        })
        .unwrap_or(false)
}

#[test]
fn a_write_root_the_agent_may_not_write_is_dropped_but_plugin_state_is_kept() {
    let fixture = Fixture::new(
        &[],
        &[
            "{{workspace}}/allowed",
            "{{workspace}}/denied",
            "{{plugin_state}}",
        ],
    );
    let caller = fixture.caller(&["**"], &["**", "!denied/**"]);
    let brokered = fixture.brokered(&caller);
    let plain = fixture
        .spec
        .sandbox_profile(Some(&fixture.worktree))
        .expect("plain profile");

    assert!(
        plain.write.contains(&fixture.worktree.join("denied")),
        "the non-brokered profile keeps every granted root"
    );
    assert_eq!(
        brokered.write,
        vec![
            fixture.worktree.join("allowed"),
            fixture.spec.state_dir.clone()
        ],
        "only the root the agent may write survives, beside the plugin's own state"
    );
}

#[test]
fn plugin_state_stays_writable_under_an_agent_profile_that_writes_nothing() {
    let fixture = Fixture::new(&[], &["{{workspace}}/allowed", "{{plugin_state}}"]);
    let brokered = fixture.brokered(&fixture.caller(&["**"], &[]));

    assert_eq!(brokered.write, vec![fixture.spec.state_dir.clone()]);
    assert!(brokered.read.contains(&fixture.spec.state_dir));
    assert!(
        seatbelt_allows(
            &brokered.macos_fs_rules().modify,
            &fixture.spec.state_dir.join("token.json")
        ),
        "the seatbelt profile keeps the plugin's state writable"
    );
}

#[test]
fn a_write_root_holding_a_path_the_agent_excludes_is_dropped() {
    let fixture = Fixture::new(&[], &["{{workspace}}/sub", "{{workspace}}/allowed"]);
    let caller = fixture.caller(&["**"], &["**", "!**/.env"]);
    let brokered = fixture.brokered(&caller);

    assert_eq!(
        brokered.write,
        vec![fixture.worktree.join("allowed")],
        "`sub` holds an existing `.env` a kernel write grant could not carve out"
    );
}

#[test]
fn a_root_the_agent_re_allows_beneath_an_exclusion_stays_writable_on_both_platforms() {
    let fixture = Fixture::new(&[], &["{{workspace}}/gen/out"]);
    let caller = fixture.caller(&["**"], &["**", "!gen/**", "gen/out/**", "!**/.env"]);
    let brokered = fixture.brokered(&caller);
    let rules = brokered.macos_fs_rules().modify;

    assert_eq!(brokered.write, vec![fixture.worktree.join("gen/out")]);
    assert!(seatbelt_allows(
        &rules,
        &fixture.worktree.join("gen/out/file.txt")
    ));
    assert!(
        !seatbelt_allows(&rules, &fixture.worktree.join("gen/other.txt")),
        "the re-allow is clipped to the kept root, not the agent's whole grant"
    );
    assert!(
        !seatbelt_allows(&rules, &fixture.worktree.join("gen/out/.env")),
        "a later exclusion still wins inside the kept root"
    );
}

#[test]
fn a_read_root_inside_an_agent_exclusion_is_dropped_and_credentials_are_denied() {
    let fixture = Fixture::new(
        &[
            "{{workspace}}",
            "{{workspace}}/secret.txt",
            "{{workspace}}/private/key.txt",
        ],
        &[],
    );
    let caller = fixture.caller(&["**", "!secret.txt", "!private/**", "!**/.env"], &[]);
    let brokered = fixture.brokered(&caller);

    assert!(brokered.read.contains(&fixture.worktree));
    assert!(!brokered.read.contains(&fixture.worktree.join("secret.txt")));
    assert!(
        !brokered
            .read
            .contains(&fixture.worktree.join("private/key.txt"))
    );
    assert!(
        brokered.readable_denied_files().is_empty(),
        "no granted read re-opens an excluded path"
    );
    for credential in orbit_exec::default_credential_read_denies() {
        assert!(
            brokered.read_denies.contains(&credential),
            "{} is not denied",
            credential.display()
        );
    }
}

#[test]
fn a_declared_program_off_the_callers_allowlist_refuses_the_brokered_profile() {
    let mut fixture = Fixture::new(&[], &[]);
    fixture.spec.programs = vec!["git".into()];
    let mut caller = fixture.caller(&["**"], &["**"]);

    let error = fixture
        .spec
        .brokered_sandbox_profile(&caller)
        .expect_err("an empty caller allowlist admits no program");
    assert!(
        matches!(error, orbit_common::OrbitError::PolicyDenied(_)),
        "{error:?}"
    );

    caller.proc_allowed_programs = vec!["git".into()];
    fixture
        .spec
        .brokered_sandbox_profile(&caller)
        .expect("a program on the caller's allowlist is admitted");
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_brokered_backend_cannot_write_where_the_agent_may_not_but_writes_its_state() {
    require_sandbox();
    let fixture = Fixture::new(
        &[],
        &[
            "{{workspace}}/allowed",
            "{{workspace}}/denied",
            "{{plugin_state}}",
        ],
    );
    let caller = fixture.caller(&["**"], &["**", "!denied/**"]);
    let wt = fixture.worktree.display();
    let state = fixture.spec.state_dir.display();

    let output = fixture.run(&fixture.brokered(&caller), WRITE_PROBE);
    assert_eq!(
        output,
        format!("wrote {wt}/allowed/new.txt\nwrote {state}/token.json"),
        "the brokered backend writes its own state and the agent's writable root only"
    );

    let plain = fixture
        .spec
        .sandbox_profile(Some(&fixture.worktree))
        .expect("plain profile");
    let output = fixture.run(&plain, WRITE_PROBE);
    assert!(
        output.contains(&format!("wrote {wt}/denied/new.txt")),
        "without a caller the plugin's own grant decides: {output}"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn an_agent_read_exclusion_is_unreadable_to_a_brokered_backend_even_when_granted() {
    require_sandbox();
    let fixture = Fixture::new(
        &[
            "{{workspace}}",
            "{{workspace}}/secret.txt",
            "{{workspace}}/private/key.txt",
        ],
        &[],
    );
    let caller = fixture.caller(&["**", "!secret.txt", "!private/**", "!**/.env"], &[]);
    let wt = fixture.worktree.display();

    let output = fixture.run(&fixture.brokered(&caller), READ_PROBE);
    assert_eq!(
        output,
        format!("read {wt}/public.txt\nlisted"),
        "only what the agent may read is readable, and the checkout stays listable"
    );

    let plain = fixture
        .spec
        .sandbox_profile(Some(&fixture.worktree))
        .expect("plain profile");
    let output = fixture.run(&plain, READ_PROBE);
    assert!(
        output.contains(&format!("read {wt}/secret.txt")),
        "without a caller the plugin's own grant decides: {output}"
    );
}
