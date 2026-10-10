//! Consent plan: resolved programs, toolchain roots, and the paths a build may read.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::credential_safe_location;
use orbit_exec::{PLUGIN_BUILD_DIR_CAP_BYTES, PLUGIN_BUILD_FETCH_PORT};
use orbit_types::plugin::{PluginBuildOutput, PluginBuildSpec, format_plugin_build_argv};

use super::super::backend::resolve_declared_program;
use super::super::source::{MAX_UNPACKED_BYTES, ResolvedCommit};
use super::env::{PluginBuildHostEnv, Toolchain, invoked_path, toolchain_for};

/// One `spec.build.programs` entry as resolved at consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuildProgram {
    pub name: String,
    /// Where it was found: the path the phase executes, so a toolchain proxy
    /// that dispatches on its own name (`cargo` → rustup) keeps that name.
    pub invoked: PathBuf,
    /// The canonical executable, recorded and re-checked before each phase.
    pub canonical: PathBuf,
}

/// What a build will run, shown verbatim at consent (§3.8).
#[derive(Debug, Clone)]
pub struct PluginBuildPlan {
    /// The `git+` source, credentials removed.
    pub source: String,
    pub commit: String,
    pub committed_at: i64,
    pub fetch: Option<Vec<String>>,
    pub command: Vec<String>,
    pub programs: Vec<ResolvedBuildProgram>,
    pub toolchain_roots: Vec<PathBuf>,
    pub outputs: Vec<PluginBuildOutput>,
    pub timeout_ms: u64,
    /// `PATH` directories ahead of `/usr/bin:/bin`.
    pub(super) path_dirs: Vec<PathBuf>,
    /// Locators the phases receive, already decided.
    pub(super) locators: Vec<(String, String)>,
}

impl PluginBuildPlan {
    /// Every path the sandbox makes readable besides the system runtime.
    pub fn readable(&self) -> Vec<PathBuf> {
        let mut readable: BTreeSet<PathBuf> = BTreeSet::new();
        for program in &self.programs {
            readable.insert(program.invoked.clone());
            readable.insert(program.canonical.clone());
        }
        readable.extend(self.toolchain_roots.iter().cloned());
        readable.into_iter().collect()
    }

    /// The plan as the consent text prints it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "  source:   {:?}", self.source);
        let _ = writeln!(out, "  commit:   {}", self.commit);
        for program in &self.programs {
            let _ = writeln!(
                out,
                "  program:  {:?} -> {:?}",
                program.name,
                program.canonical.display()
            );
        }
        for root in &self.toolchain_roots {
            let _ = writeln!(out, "  toolchain root (read-only): {:?}", root.display());
        }
        if let Some(fetch) = &self.fetch {
            let _ = writeln!(
                out,
                "  phase fetch: {}  [network: outbound TCP to port {PLUGIN_BUILD_FETCH_PORT} only]",
                format_plugin_build_argv(fetch)
            );
        }
        let _ = writeln!(
            out,
            "  phase build: {}  [network: none]",
            format_plugin_build_argv(&self.command)
        );
        let _ = writeln!(
            out,
            "  limits:   {} s per phase; build directory {} GiB; outputs {} MiB",
            self.timeout_ms / 1000,
            PLUGIN_BUILD_DIR_CAP_BYTES / (1024 * 1024 * 1024),
            MAX_UNPACKED_BYTES / (1024 * 1024)
        );
        for output in &self.outputs {
            let _ = writeln!(out, "  output:   {:?} -> {:?}", output.from, output.to);
        }
        let _ = write!(
            out,
            "  writable: only the build directory; reads: system runtime, the programs and \
             toolchain roots above"
        );
        out
    }
}

pub(super) fn quote_argv(argv: &[String]) -> String {
    format_plugin_build_argv(argv)
}

/// Resolve `spec`'s programs and toolchain roots for a build of `commit`.
///
/// `forbidden` are the paths no root may be or contain besides `$HOME` and
/// the credential paths: the global Orbit root and the workspace roots.
pub fn plan_plugin_build(
    spec: &PluginBuildSpec,
    source: &str,
    commit: &ResolvedCommit,
    env: &PluginBuildHostEnv,
    forbidden: &[PathBuf],
) -> Result<PluginBuildPlan, OrbitError> {
    let mut programs = Vec::new();
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    let mut path_dirs: Vec<PathBuf> = Vec::new();
    let mut rustup_home: Option<PathBuf> = None;
    for name in &spec.programs {
        let canonical = resolve_declared_program(name, env.path.as_deref()).map_err(|reason| {
            OrbitError::InvalidInput(format!(
                "spec.build.programs: `{name}` did not resolve: {reason}; nothing was built"
            ))
        })?;
        let invoked = invoked_path(name, env.path.as_deref()).unwrap_or_else(|| canonical.clone());
        if let Some(dir) = invoked.parent()
            && !path_dirs.iter().any(|known| known == dir)
        {
            path_dirs.push(dir.to_path_buf());
        }
        match toolchain_for(&invoked, &canonical, env) {
            Toolchain::System => {}
            Toolchain::Prefix(prefix) => {
                roots.insert(prefix);
            }
            Toolchain::Rustup {
                home,
                toolchains,
                settings,
                default_bin,
            } => {
                roots.insert(toolchains);
                if let Some(settings) = settings {
                    roots.insert(settings);
                }
                if let Some(bin) = default_bin
                    && !path_dirs.contains(&bin)
                {
                    path_dirs.push(bin);
                }
                rustup_home = Some(home);
            }
        }
        programs.push(ResolvedBuildProgram {
            name: name.clone(),
            invoked,
            canonical,
        });
    }
    let roots: Vec<PathBuf> = roots.into_iter().collect();
    let mut checked: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    for program in &programs {
        checked.push(&program.invoked);
        checked.push(&program.canonical);
    }
    refuse_unsafe_roots(&checked, env, forbidden)?;

    let mut locators = Vec::new();
    if let Some(home) = &rustup_home {
        locators.push(("RUSTUP_HOME".to_string(), home.display().to_string()));
        if let Some(toolchain) = env.locator("RUSTUP_TOOLCHAIN")
            && !toolchain.contains('/')
        {
            locators.push(("RUSTUP_TOOLCHAIN".to_string(), toolchain.to_string()));
        }
    }
    for name in ["GOROOT", "JAVA_HOME"] {
        if let Some(value) = env.locator(name)
            && let Ok(canonical) = Path::new(value).canonicalize()
            && roots.iter().any(|root| canonical.starts_with(root))
        {
            locators.push((name.to_string(), canonical.display().to_string()));
        }
    }

    let plan = PluginBuildPlan {
        source: credential_safe_location(source),
        commit: commit.id.clone(),
        committed_at: commit.committed_at,
        fetch: spec.fetch.clone(),
        command: spec.command.clone(),
        programs,
        toolchain_roots: roots,
        outputs: spec.outputs.clone(),
        timeout_ms: spec.effective_timeout_ms(),
        path_dirs,
        locators,
    };
    recheck_build_paths(&plan)?;
    Ok(plan)
}

/// Resolve the consented paths again before each phase. A toolchain root
/// must itself be physical: binding a link could reveal a different tree
/// from the one checked against the forbidden roots.
pub(super) fn recheck_build_paths(plan: &PluginBuildPlan) -> Result<(), OrbitError> {
    for program in &plan.programs {
        if program.invoked.canonicalize().ok().as_deref() != Some(&program.canonical) {
            return Err(OrbitError::PolicyDenied(format!(
                "build program `{}` no longer resolves to {} as shown at consent; nothing was built",
                program.name,
                program.canonical.display()
            )));
        }
    }
    for root in &plan.toolchain_roots {
        if root.canonicalize().ok().as_deref() != Some(root.as_path()) {
            return Err(OrbitError::PolicyDenied(format!(
                "build toolchain root {} is missing or is not a physical path as shown at consent; \
                 use a toolchain installed in its own physical directory",
                root.display()
            )));
        }
    }
    Ok(())
}

/// A root or program path is never `$HOME` and never contains it; it is
/// never the global Orbit root, a workspace or a credential path, and is
/// neither inside nor around one of them (§3.2).
fn refuse_unsafe_roots(
    paths: &[&Path],
    env: &PluginBuildHostEnv,
    forbidden: &[PathBuf],
) -> Result<(), OrbitError> {
    // (path, what it is, whether a root inside it is refused too)
    let mut sensitive: Vec<(PathBuf, &str, bool)> = Vec::new();
    if let Some(home) = &env.home {
        sensitive.push((canonical_or_self(home), "your home directory", false));
    }
    for path in forbidden {
        sensitive.push((canonical_or_self(path), "an Orbit root or workspace", true));
    }
    for path in orbit_exec::default_credential_read_denies() {
        sensitive.push((canonical_or_self(&path), "a credential path", true));
    }
    for path in paths {
        for (sensitive_path, what, inside_refused) in &sensitive {
            let around = sensitive_path.starts_with(path);
            let inside = *inside_refused && path.starts_with(sensitive_path);
            if around || inside {
                let relation = if around {
                    "is or contains"
                } else {
                    "is inside"
                };
                return Err(OrbitError::PolicyDenied(format!(
                    "the build would read {}, which {relation} {what} ({}); a build toolchain \
                     must be installed in its own directory. Nothing was built",
                    path.display(),
                    sensitive_path.display()
                )));
            }
        }
    }
    Ok(())
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
