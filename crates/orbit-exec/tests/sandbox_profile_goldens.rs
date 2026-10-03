//! Goldens of the compiled sandbox for representative activity policies.
//!
//! Each case is a resolved filesystem profile shaped like the one the
//! runtime hands the spawn path, compiled through this crate's public
//! entry points exactly as the spawn path compiles it:
//!
//! - `macos/<case>.sbpl`: [`compile_macos_sandbox_profile`] followed by the
//!   agent mask ([`append_macos_subpath_mask`]) or, for a plugin backend, the
//!   read boundary and network clause. The SBPL compiler is plain text
//!   generation, so these goldens are checked on every platform.
//! - `linux/<case>.txt` (Linux only, because the credential masks read the
//!   host mount table): the Bubblewrap argv from
//!   [`compile_linux_bwrap_argv_with_authority`], or the Landlock grants a
//!   plugin backend is confined by.
//!
//! The cases: a leaf worker writing its managed worktree, a reviewer with no
//! source writes, a plugin backend, the provider credential carve-out, and a
//! global runtime store redirected by a symlink. A redirected store compiles
//! to its target on both platforms: that is why the runtime resolver must
//! drop one before compiling.
//!
//! The fixture renders in a child of this binary with a cleared environment
//! and a fixture `HOME`, so provider and Cargo overrides on the host cannot
//! leak into the output. Fixture paths render as `<ROOT>`.
//!
//! Regenerate with `make goldens UPDATE=1` (or
//! `ORBIT_UPDATE_SANDBOX_GOLDENS=1 cargo test -p orbit-exec --test
//! sandbox_profile_goldens`) and explain every diff in the PR: a policy
//! change is meant to arrive as a reviewed golden diff.

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::print_stderr, clippy::unwrap_used)]
#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_exec::{
    MacosNetworkAccess, append_macos_network_access, append_macos_read_boundary,
    append_macos_subpath_mask, compile_macos_sandbox_profile,
};
use orbit_types::policy::ResolvedFsProfile;

const UPDATE_ENV: &str = "ORBIT_UPDATE_SANDBOX_GOLDENS";
/// Set on the child only: where it writes the rendered cases.
const OUTPUT_ENV: &str = "ORBIT_TEST_SANDBOX_GOLDEN_OUTPUT";
/// Set on the child only: the fixture root, which is also its `HOME` parent.
const ROOT_ENV: &str = "ORBIT_TEST_SANDBOX_GOLDEN_ROOT";
const CHILD_TEST: &str = "render_compiled_profiles_in_a_pinned_environment";
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

#[test]
fn compiled_sandbox_profiles_match_their_goldens() {
    // Under `/tmp` on every platform so the Linux scratch-anchor mounts, which
    // depend on where the fixture sits relative to `/tmp`, render the same on
    // every host whatever its `TMPDIR`.
    let root = tempfile::tempdir_in("/tmp").expect("fixture root");
    let output = tempfile::tempdir().expect("render output");
    run_child(root.path(), output.path());

    let mut rendered = Vec::new();
    for entry in std::fs::read_dir(output.path()).expect("rendered cases") {
        let entry = entry.expect("rendered case");
        let platform = entry.file_name().to_string_lossy().into_owned();
        for case in std::fs::read_dir(entry.path()).expect("platform cases") {
            let case = case.expect("case");
            rendered.push(format!("{platform}/{}", case.file_name().to_string_lossy()));
        }
    }
    rendered.sort();
    assert!(
        !rendered.is_empty(),
        "the child rendered no cases from {}",
        output.path().display()
    );

    let mut drifted = Vec::new();
    for name in &rendered {
        let actual = std::fs::read_to_string(output.path().join(name)).expect("rendered case");
        if !golden_matches(name, &actual) {
            drifted.push(name.clone());
        }
    }
    assert!(
        drifted.is_empty(),
        "{drifted:?} drifted from the checked-in sandbox goldens (diffs above). If the policy \
         change is intended, regenerate with `make goldens UPDATE=1` and explain the diff in \
         the PR."
    );

    // A golden whose case was removed would otherwise go stale unnoticed.
    for platform in rendered_platforms(&rendered) {
        let dir = golden_dir().join(&platform);
        for entry in std::fs::read_dir(&dir).expect("golden dir") {
            let name = format!(
                "{platform}/{}",
                entry.expect("golden").file_name().to_string_lossy()
            );
            assert!(
                rendered.contains(&name),
                "{name} has no case any more; delete it (or regenerate with `make goldens \
                 UPDATE=1`)"
            );
        }
    }
}

/// Re-executed by [`compiled_sandbox_profiles_match_their_goldens`] with a
/// cleared environment. Does nothing when run any other way.
#[test]
#[ignore = "child of compiled_sandbox_profiles_match_their_goldens"]
fn render_compiled_profiles_in_a_pinned_environment() {
    let (Some(output), Some(root)) = (std::env::var_os(OUTPUT_ENV), std::env::var_os(ROOT_ENV))
    else {
        return;
    };
    let fixture = Fixture::create(Path::new(&root));
    let output = PathBuf::from(output);
    let macos = output.join("macos");
    std::fs::create_dir_all(&macos).expect("macos output");
    for case in fixture.cases() {
        let text = fixture.render_macos(&case);
        std::fs::write(macos.join(format!("{}.sbpl", case.name)), text).expect("write sbpl");
    }
    #[cfg(target_os = "linux")]
    {
        let linux = output.join("linux");
        std::fs::create_dir_all(&linux).expect("linux output");
        for case in fixture.cases() {
            let text = fixture.render_linux(&case);
            std::fs::write(linux.join(format!("{}.txt", case.name)), text).expect("write plan");
        }
    }
}

fn run_child(root: &Path, output: &Path) {
    let root = root.canonicalize().expect("canonical fixture root");
    let logs = tempfile::tempdir().expect("child logs");
    let stdout_path = logs.path().join("stdout.log");
    let stderr_path = logs.path().join("stderr.log");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root.join("home"))
        .env(OUTPUT_ENV, output)
        .env(ROOT_ENV, &root)
        .args([
            "--exact",
            CHILD_TEST,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(&root)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path).expect("stdout log"))
        .stderr(std::fs::File::create(&stderr_path).expect("stderr log"));
    let mut child = ChildGuard(command.spawn().expect("spawn renderer"));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("poll renderer") {
            break Some(status);
        }
        if started.elapsed() > CHILD_DEADLINE {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(child);
    let read = |path: &Path| {
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|mut file| file.read_to_string(&mut text))
            .expect("read child log");
        text
    };
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let status = status
        .unwrap_or_else(|| panic!("renderer ran past {CHILD_DEADLINE:?}:\n{stdout}\n{stderr}"));
    assert!(status.success(), "renderer failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the child must run the renderer itself:\n{stdout}"
    );
}

/// Kills and reaps the renderer however the parent leaves.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sandbox_profile_goldens")
}

fn rendered_platforms(rendered: &[String]) -> Vec<String> {
    let mut platforms: Vec<String> = rendered
        .iter()
        .filter_map(|name| {
            name.split_once('/')
                .map(|(platform, _)| platform.to_string())
        })
        .collect();
    platforms.dedup();
    platforms
}

/// Compare with (or, under [`UPDATE_ENV`], overwrite) one golden. Prints a
/// line diff on drift so every mismatching case shows in one run.
fn golden_matches(name: &str, actual: &str) -> bool {
    let path = golden_dir().join(name);
    if std::env::var(UPDATE_ENV).as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().expect("golden parent")).expect("golden dir");
        std::fs::write(&path, actual)
            .unwrap_or_else(|error| panic!("write golden {}: {error}", path.display()));
        return true;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    if expected == actual {
        return true;
    }
    eprintln!("--- {name} (golden)\n+++ {name} (compiled)");
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    for line in &expected_lines {
        if !actual_lines.contains(line) {
            eprintln!("-{line}");
        }
    }
    for line in &actual_lines {
        if !expected_lines.contains(line) {
            eprintln!("+{line}");
        }
    }
    false
}

/// One activity policy and the provider whose CLI it confines.
struct Case {
    name: &'static str,
    provider: &'static str,
    profile: ResolvedFsProfile,
    /// The directory the child starts in.
    cwd: PathBuf,
    managed_worktree: bool,
    kind: CaseKind,
}

enum CaseKind {
    /// An agent CLI: the plugin mask is appended last.
    Agent,
    /// The same profile compiled for two providers, to show the credential
    /// carve-out one of them gets.
    ProviderPair(&'static str),
    /// A plugin backend: read boundary and no network.
    PluginBackend,
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    global: PathBuf,
    repo: PathBuf,
    worktree: PathBuf,
    inspection: PathBuf,
    redirected_global: PathBuf,
    plugin_root: PathBuf,
    plugin_state: PathBuf,
}

impl Fixture {
    fn create(root: &Path) -> Self {
        let root = root.canonicalize().expect("canonical root");
        let home = root.join("home");
        let global = home.join(".orbit");
        let repo = root.join("repo");
        let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-golden");
        let inspection = root.join("inspection");
        let redirected_global = root.join("redirected/.orbit");
        let plugin_root = global.join("plugins/demo");
        let plugin_state = global.join("state/plugins/demo");
        for dir in [
            home.join(".ssh"),
            home.join(".aws"),
            home.join(".cargo/registry"),
            home.join(".cargo/git"),
            global.join("state/logs"),
            global.join("state/audit"),
            global.join("state/plugin-secrets"),
            global.join("state/plugin-callbacks"),
            global.join("state/plugin-broker/masked"),
            global.join("plugins/.grants"),
            global.join("tasks"),
            global.join("cache"),
            repo.join(".orbit/auto_tasks"),
            repo.join(".orbit/tmp"),
            repo.join("src"),
            worktree.join("src"),
            worktree.join(".orbit/tmp"),
            inspection.join("src"),
            redirected_global.join("state/logs"),
            redirected_global.join("state/audit"),
            redirected_global.join("tasks"),
            root.join("outside"),
            plugin_root.clone(),
            plugin_state.clone(),
        ] {
            std::fs::create_dir_all(&dir).expect("fixture dir");
        }
        std::fs::write(home.join(".cargo/credentials.toml"), "").expect("cargo token");
        std::fs::write(global.join("orbit.db"), "").expect("global db");
        std::fs::write(redirected_global.join("orbit.db"), "").expect("redirected db");
        std::fs::write(plugin_root.join("backend.sh"), "#!/bin/sh\n").expect("backend");
        std::os::unix::fs::symlink(root.join("outside"), redirected_global.join("cache"))
            .expect("redirect the host cache store");
        Self {
            root,
            home,
            global,
            repo,
            worktree,
            inspection,
            redirected_global,
            plugin_root,
            plugin_state,
        }
    }

    /// Rules for the global runtime stores every agent's nested `orbit`
    /// child writes, plus the implementer-only host cache.
    fn runtime_stores(global: &Path, implementer: bool) -> Vec<String> {
        let global = global.display();
        let mut rules = vec![
            format!("{global}/state/logs/**"),
            format!("{global}/state/audit/**"),
            format!("{global}/orbit.db*"),
            format!("{global}/tasks/**"),
        ];
        if implementer {
            rules.push(format!("{global}/cache/**"));
        }
        rules
    }

    fn dotenv_denies(root: &Path) -> Vec<String> {
        ["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"]
            .iter()
            .map(|pattern| format!("!{}/{pattern}", root.display()))
            .collect()
    }

    fn cases(&self) -> Vec<Case> {
        let worktree = self.worktree.display().to_string();
        let auto_tasks_deny = format!("!{}/.orbit/auto_tasks/**", self.repo.display());

        let mut leaf_modify = vec![
            format!("{worktree}/**"),
            format!("!{worktree}/.orbit/**"),
            format!("{worktree}/.orbit/tmp/**"),
        ];
        leaf_modify.extend(Self::dotenv_denies(&self.worktree));
        leaf_modify.extend(Self::runtime_stores(&self.global, true));
        leaf_modify.push(auto_tasks_deny.clone());

        let mut reviewer_modify = Self::runtime_stores(&self.global, false);
        reviewer_modify.push(auto_tasks_deny.clone());

        let mut redirected_modify = vec![format!("{}/src/**", self.repo.display())];
        redirected_modify.extend(Self::runtime_stores(&self.redirected_global, true));

        let read = |root: &Path| {
            let mut rules = vec![format!("{}/**", root.display())];
            rules.extend(Self::dotenv_denies(root));
            rules
        };

        vec![
            Case {
                name: "leaf_worker",
                provider: "codex",
                profile: profile("implementer", read(&self.worktree), leaf_modify),
                cwd: self.worktree.clone(),
                managed_worktree: true,
                kind: CaseKind::Agent,
            },
            Case {
                name: "reviewer",
                provider: "claude",
                profile: profile("reviewer", read(&self.inspection), reviewer_modify),
                cwd: self.inspection.clone(),
                managed_worktree: false,
                kind: CaseKind::Agent,
            },
            Case {
                name: "plugin_backend",
                provider: "plugin",
                profile: profile(
                    "plugin",
                    vec![
                        format!("{}/**", self.plugin_root.display()),
                        format!("{}/**", self.plugin_state.display()),
                    ],
                    vec![format!("{}/**", self.plugin_state.display())],
                ),
                cwd: self.plugin_state.clone(),
                managed_worktree: false,
                kind: CaseKind::PluginBackend,
            },
            Case {
                name: "provider_credentials",
                provider: "codex",
                profile: profile(
                    "implementer",
                    read(&self.repo),
                    vec![format!("{}/src/**", self.repo.display())],
                ),
                cwd: self.repo.clone(),
                managed_worktree: false,
                kind: CaseKind::ProviderPair("claude"),
            },
            Case {
                name: "redirected_global_runtime_store",
                provider: "claude",
                profile: profile("implementer", read(&self.repo), redirected_modify),
                cwd: self.repo.clone(),
                managed_worktree: false,
                kind: CaseKind::Agent,
            },
        ]
    }

    fn mask_targets(&self) -> Vec<PathBuf> {
        vec![
            self.global.join("state/plugins"),
            self.global.join("state/plugin-secrets"),
        ]
    }

    fn plugin_read_denies(&self) -> Vec<PathBuf> {
        [
            "state/plugin-callbacks",
            "plugins/.grants",
            "state/plugins",
            "state/plugin-secrets",
        ]
        .iter()
        .map(|relative| self.global.join(relative))
        .collect()
    }

    fn render_macos(&self, case: &Case) -> String {
        let compile = |provider: &str| {
            let mut text =
                compile_macos_sandbox_profile(&case.profile, provider).expect("compile SBPL");
            match case.kind {
                CaseKind::Agent | CaseKind::ProviderPair(_) => {
                    append_macos_subpath_mask(&mut text, &self.mask_targets());
                }
                CaseKind::PluginBackend => {
                    append_macos_read_boundary(
                        &mut text,
                        &self.plugin_read_denies(),
                        std::slice::from_ref(&self.plugin_state),
                        &[],
                    );
                    append_macos_network_access(&mut text, MacosNetworkAccess::None);
                }
            }
            format!(";; provider: {provider}\n{text}")
        };
        let mut text = compile(case.provider);
        if let CaseKind::ProviderPair(other) = case.kind {
            text.push('\n');
            text.push_str(&compile(other));
        }
        self.normalize(&text)
    }

    #[cfg(target_os = "linux")]
    fn render_linux(&self, case: &Case) -> String {
        if let CaseKind::PluginBackend = case.kind {
            return self.render_landlock(case);
        }
        let mask = orbit_exec::LinuxBwrapMask {
            sentinel: self.global.join("state/plugin-broker/masked"),
            targets: self.mask_targets(),
        };
        let plan = orbit_exec::compile_linux_bwrap_argv_with_authority(
            &case.profile,
            "/usr/bin/true",
            &[],
            Some(&case.cwd),
            case.managed_worktree,
            Vec::new(),
            Some(&mask),
        )
        .expect("compile Bubblewrap plan");
        let mut lines: Vec<String> = vec![format!("wrapper: {}", plan.wrapper)];
        let mut after_separator = false;
        for arg in &plan.args {
            let starts_option = !after_separator && arg.starts_with("--");
            if arg == "--" {
                after_separator = true;
            }
            match lines.last_mut() {
                Some(line) if !starts_option && !after_separator => {
                    line.push(' ');
                    line.push_str(arg);
                }
                _ => lines.push(arg.clone()),
            }
        }
        for dropped in &plan.dropped_grants {
            lines.push(format!(
                "dropped grant: {} (anchor {})",
                dropped.rule,
                dropped.anchor.display()
            ));
        }
        self.normalize(&(lines.join("\n") + "\n"))
    }

    /// The Landlock grants under the fixture root. Host grants (the dynamic
    /// loader, `PATH` directories) depend on the machine, not the policy.
    #[cfg(target_os = "linux")]
    fn render_landlock(&self, case: &Case) -> String {
        let boundary = orbit_exec::LandlockBoundary {
            read: vec![self.plugin_root.clone(), self.plugin_state.clone()],
            read_denies: self.plugin_read_denies(),
            read_exclusions: Vec::new(),
            write: vec![self.plugin_state.clone()],
            write_files: Vec::new(),
            deny_tcp: true,
        };
        let request = orbit_exec::ExecRequest {
            program: self.plugin_root.join("backend.sh").display().to_string(),
            args: Vec::new(),
            current_dir: Some(case.cwd.display().to_string()),
            timeout_ms: None,
            stdin_mode: orbit_exec::StdinMode::Null,
            environment_mode: orbit_exec::EnvironmentMode::ClearAndSet(Vec::new()),
            debug: false,
        };
        let grants = orbit_exec::linux_landlock_boundary_grants(&request, &boundary)
            .expect("compile Landlock grants");
        let mut lines = vec![format!("deny_tcp: {}", boundary.deny_tcp)];
        lines.extend(
            grants
                .iter()
                .filter(|grant| grant.path.starts_with(&self.root))
                .map(|grant| format!("{:?} {}", grant.grant, grant.path.display())),
        );
        self.normalize(&(lines.join("\n") + "\n"))
    }

    /// Replace the fixture root with `<ROOT>` and the fixture `HOME` with
    /// `<HOME>`, in plain spelling and both SBPL regex spellings.
    fn normalize(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (path, label) in [(&self.home, "<HOME>"), (&self.root, "<ROOT>")] {
            let plain = path.display().to_string();
            text = text
                .replace(&sbpl_regex_spelling(&plain, true), label)
                .replace(&sbpl_regex_spelling(&plain, false), label)
                .replace(&plain, label);
        }
        text
    }
}

fn profile(name: &str, read: Vec<String>, modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: name.to_string(),
        read,
        modify,
    }
}

/// How the SBPL compiler spells a literal path inside a `(regex ...)`
/// filter: metacharacters escaped, letters case-folded to `[Aa]` for a policy
/// glob (`case_fold`) but not for a provider state file, then the whole string
/// SBPL-escaped.
fn sbpl_regex_spelling(path: &str, case_fold: bool) -> String {
    let mut out = String::new();
    for c in path.chars() {
        if case_fold && c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_uppercase());
            out.push(c.to_ascii_lowercase());
            out.push(']');
        } else {
            if matches!(
                c,
                '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\'
            ) {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out.replace('\\', "\\\\")
}
