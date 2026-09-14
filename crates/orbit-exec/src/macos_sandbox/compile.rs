use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::Provider;

/// Compile a [`ResolvedFsProfile`] into SBPL text suitable for
/// `sandbox-exec -f`.
///
/// `provider` is the canonical provider name of the CLI this profile will
/// confine (`claude`, `codex`, ...). It only selects the credential-store
/// carve-out described in [`macos_login_keychain_access`]; every other
/// clause is provider-independent. An unknown name compiles with the full
/// default credential denylist, so a typo fails closed.
///
/// The emitted profile:
/// - denies everything by default;
/// - allows broad reads (`file-read*`) for agent CLI compatibility, then
///   appends default read denies for well-known credential locations so those
///   paths still lose under SBPL's last-match-wins evaluation, then re-allows
///   the confined provider's own credential store if it lives in one of those
///   locations, and only then emits the activity's own negated `read` rules —
///   so a policy-authored `denyRead` outranks the provider carve-out;
/// - allows the syscall classes agent CLIs rely on (process, signal, mach,
///   ipc, sysctl, iokit) and unrestricted network — agents call out to
///   provider APIs;
/// - allows pseudo-tty allocation (`(allow pseudo-tty)`), needed by
///   `openpty`/`posix_openpt` alongside the `/dev/ptmx` and `/dev/ttys*`
///   file access already covered by the broad read allow and the `/dev`
///   write grant below;
/// - allows writes inside the resolved `modify` scope plus a small set of
///   well-known scratch areas (`/tmp`, `/private/tmp`,
///   `/private/var/folders`, `~/Library/Caches`, and the HOME-derived Orbit
///   JSONL log directory) that tools and the filesystem layer expect to write to;
/// - allows writes inside Cargo's shared download caches
///   (`$CARGO_HOME/registry`, `$CARGO_HOME/git`, and the two
///   `.package-cache*` locks) so a build can populate the host registry, for a
///   profile that already grants some write — see
///   [`emit_cargo_download_cache_write_allows`];
/// - emits resolved `read` / `modify` rules in order, including explicit
///   `(deny ...)` clauses for negated entries and narrow host-policy or
///   runtime re-allows after their enclosing deny, preserving SBPL's
///   last-match-wins evaluation.
///
/// Paths in `rules.modify` are emitted as-is. Callers must resolve
/// workspace-relative globs to absolute paths before invoking this
/// function — a relative `subpath` is meaningless to the kernel.
pub fn compile_macos_sandbox_profile(
    rules: &ResolvedFsProfile,
    provider: &str,
) -> Result<String, OrbitError> {
    let home = std::env::var_os("HOME");
    let codex_home = std::env::var_os("CODEX_HOME");
    let claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
    let grok_home = std::env::var_os("GROK_HOME");
    let copilot_home = std::env::var_os("COPILOT_HOME");
    let xdg_cache_home = std::env::var_os("XDG_CACHE_HOME");
    let pi_coding_agent_dir = std::env::var_os("PI_CODING_AGENT_DIR");
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME");
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    let xdg_state_home = std::env::var_os("XDG_STATE_HOME");
    let opencode_config_dir = std::env::var_os("OPENCODE_CONFIG_DIR");
    let cargo_home = std::env::var_os("CARGO_HOME");
    compile_macos_sandbox_profile_with_env(
        rules,
        provider,
        SandboxCompileEnv {
            home: home.as_deref(),
            codex_home: codex_home.as_deref(),
            claude_config_dir: claude_config_dir.as_deref(),
            grok_home: grok_home.as_deref(),
            copilot_home: copilot_home.as_deref(),
            xdg_cache_home: xdg_cache_home.as_deref(),
            pi_coding_agent_dir: pi_coding_agent_dir.as_deref(),
            xdg_data_home: xdg_data_home.as_deref(),
            xdg_config_home: xdg_config_home.as_deref(),
            xdg_state_home: xdg_state_home.as_deref(),
            opencode_config_dir: opencode_config_dir.as_deref(),
            cargo_home: cargo_home.as_deref(),
        },
    )
}

/// Env inputs that influence per-provider state-directory allowances in the
/// compiled SBPL profile. Threaded through a struct so tests can pin every
/// override without juggling a long parameter list.
#[derive(Default, Clone, Copy)]
pub(super) struct SandboxCompileEnv<'a> {
    pub(super) home: Option<&'a OsStr>,
    pub(super) codex_home: Option<&'a OsStr>,
    pub(super) claude_config_dir: Option<&'a OsStr>,
    pub(super) grok_home: Option<&'a OsStr>,
    pub(super) copilot_home: Option<&'a OsStr>,
    pub(super) xdg_cache_home: Option<&'a OsStr>,
    pub(super) pi_coding_agent_dir: Option<&'a OsStr>,
    pub(super) xdg_data_home: Option<&'a OsStr>,
    pub(super) xdg_config_home: Option<&'a OsStr>,
    pub(super) xdg_state_home: Option<&'a OsStr>,
    pub(super) opencode_config_dir: Option<&'a OsStr>,
    pub(super) cargo_home: Option<&'a OsStr>,
}

pub(super) fn compile_macos_sandbox_profile_with_env(
    rules: &ResolvedFsProfile,
    provider: &str,
    env: SandboxCompileEnv<'_>,
) -> Result<String, OrbitError> {
    let SandboxCompileEnv {
        home,
        codex_home,
        claude_config_dir,
        grok_home,
        copilot_home,
        xdg_cache_home,
        pi_coding_agent_dir,
        xdg_data_home,
        xdg_config_home,
        xdg_state_home,
        opencode_config_dir,
        cargo_home,
    } = env;
    let mut out = String::new();
    out.push_str("(version 1)\n");
    out.push_str("(deny default)\n");

    out.push_str("(allow file-read*)\n");
    out.push_str("(allow process*)\n");
    out.push_str("(allow signal)\n");
    out.push_str("(allow ipc-posix*)\n");
    out.push_str("(allow mach*)\n");
    out.push_str("(allow system-fsctl)\n");
    out.push_str("(allow system-socket)\n");
    // Codex's own seatbelt profile allows these provenance-related MAC
    // syscalls. Without them, macOS can fail Codex startup with a bare
    // `Operation not permitted`; revisit this if future macOS versions move
    // or rename the private Sandbox/67 operation.
    out.push_str("(allow system-mac-syscall (mac-policy-name \"vnguard\"))\n");
    out.push_str(
        "(allow system-mac-syscall (require-all (mac-policy-name \"Sandbox\") (mac-syscall-number 67)))\n",
    );
    out.push_str("(allow network*)\n");
    out.push_str("(allow sysctl*)\n");
    out.push_str("(allow iokit*)\n");
    // `openpty`/`posix_openpt` need the `pseudo-tty` operation in addition to
    // `/dev/ptmx` and `/dev/ttys*` file access; without it allocation fails
    // with EPERM even though the broad `file-read*` allow and the `/dev`
    // `file-write*` subpath below already cover those device files. A PTY is
    // a local IPC primitive scoped to the calling process's own descriptors —
    // granting the operation adds no filesystem or network reach beyond what
    // this profile already grants. [ORB-12470]
    out.push_str("(allow pseudo-tty)\n");

    out.push_str("(allow file-write* (subpath \"/tmp\"))\n");
    out.push_str("(allow file-write* (subpath \"/private/tmp\"))\n");
    out.push_str("(allow file-write* (subpath \"/private/var/folders\"))\n");
    out.push_str("(allow file-write* (subpath \"/dev\"))\n");
    if let Some(home) = super::provider_dirs::non_empty_env_path(home) {
        let home = home.display().to_string();
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}/Library/Caches\"))\n",
            super::sbpl_filter::sbpl_escape(&home)
        ));
        // The agent CLI inherits the sandbox into its `orbit mcp serve` child
        // (and any other `orbit ...` calls it makes). Logging initializes
        // before the child can resolve Orbit's runtime roots, so the profile
        // carries the one HOME-derived path that must be writable up front.
        // Runtime-specific store/artifact paths are appended by orbit-core's
        // sandbox resolver instead of granting the whole HOME/.orbit tree.
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}/.orbit/state/logs\"))\n",
            super::sbpl_filter::sbpl_escape(&home)
        ));
    }
    if profile_grants_write(rules)
        && let Some(cargo_home) = cargo_home_dir(home, cargo_home)
    {
        emit_cargo_download_cache_write_allows(&cargo_home, &mut out);
    }
    // Per-provider state directories. Each `backend: cli` agent CLI writes
    // setup state (sessions, settings, history, etc.) before it reads
    // Orbit's envelope. Active provider is not threaded through SBPL
    // compilation, and per-provider allowances do not widen attack surface,
    // so emit narrow allows for every supported provider's state dir
    // unconditionally.
    for state_dir in
        super::provider_dirs::provider_state_dirs(home, codex_home, claude_config_dir, grok_home)
    {
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            super::sbpl_filter::sbpl_escape(&state_dir.display().to_string())
        ));
    }
    // [ORB-10946] Copilot's directories are granted only when Copilot is the
    // provider actually being confined. The comment above explains why the
    // four original providers are emitted unconditionally: their entries are
    // per-tool configuration directories. Copilot's second entry is a
    // package-*extraction* directory — the launcher unpacks and executes code
    // from it — so it is not something an unrelated provider should be handed
    // just because both run under the same profile compiler.
    if Provider::parse(provider).ok() == Some(Provider::Copilot) {
        for state_dir in
            super::provider_dirs::copilot_state_dirs(home, copilot_home, xdg_cache_home)
        {
            out.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                super::sbpl_filter::sbpl_escape(&state_dir.display().to_string())
            ));
        }
    }
    // Cursor's CLI configuration, permissions, and sessions live in
    // `$HOME/.cursor`. On macOS the default login is the login keychain, not
    // that directory; the write grant here is still required for the rest of
    // the CLI state. API-key auth is an explicit environment opt-in and needs
    // no additional path. [ORB-10945] [ORB-12261]
    if Provider::parse(provider).ok() == Some(Provider::Cursor)
        && let Some(state_dir) = super::provider_dirs::cursor_state_dir(home)
    {
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            super::sbpl_filter::sbpl_escape(&state_dir.display().to_string())
        ));
    }
    // Pi's agent directory holds `/login` credentials, settings, saved project
    // trust decisions, installed packages, and sessions. Grant it only to the
    // active Pi executor; API-key auth is an explicit environment opt-in and
    // needs no additional path. [ORB-11296]
    if Provider::parse(provider).ok() == Some(Provider::Pi)
        && let Some(state_dir) = super::provider_dirs::pi_state_dir(home, pi_coding_agent_dir)
    {
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            super::sbpl_filter::sbpl_escape(&state_dir.display().to_string())
        ));
    }
    // OpenCode creates its XDG data/config/state roots during startup, before
    // it reads Orbit's envelope, so they are granted only to the active
    // OpenCode executor. `auth.json` lives in the data root; API-key auth is an
    // explicit environment opt-in and needs no additional path. [ORB-11295]
    if Provider::parse(provider).ok() == Some(Provider::Opencode) {
        for state_dir in
            super::provider_dirs::opencode_state_dirs(super::provider_dirs::OpencodeDirEnv {
                home,
                xdg_data_home,
                xdg_config_home,
                xdg_state_home,
                xdg_cache_home,
                opencode_config_dir,
            })
        {
            out.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                super::sbpl_filter::sbpl_escape(&state_dir.display().to_string())
            ));
        }
    }
    super::provider_dirs::emit_claude_home_json_allows(home, claude_config_dir, &mut out);
    super::provider_dirs::emit_grok_state_file_allows(home, grok_home, &mut out);

    for rule in &rules.modify {
        if let Some(deny_path) = rule.strip_prefix('!') {
            out.push_str(&format!(
                "(deny file-write* {})\n",
                super::sbpl_filter::sbpl_filter_for_deny_rule(deny_path)
            ));
            continue;
        }
        out.push_str(&format!(
            "(allow file-write* {})\n",
            super::sbpl_filter::sbpl_filter_for_allow_rule(rule)
        ));
    }

    // Clause order below is the security contract, not a formatting choice.
    // SBPL is last-match-wins, so the default credential denies come first, the
    // confined provider's own credential carve-out re-allows on top of them,
    // and the activity's negated `read` rules come last — an operator who
    // writes `denyRead` for a credential path gets that denial even when the
    // provider would otherwise be granted it. [ORB-10931]
    emit_default_credential_read_denies(home, cargo_home, &mut out);
    emit_provider_credential_read_reallow(provider, home, &mut out);
    for rule in &rules.read {
        if let Some(deny_path) = rule.strip_prefix('!') {
            out.push_str(&format!(
                "(deny file-read* {})\n",
                super::sbpl_filter::sbpl_filter_for_deny_rule(deny_path)
            ));
        }
    }

    Ok(out)
}

/// Whether `provider`'s CLI reads its own credentials from the macOS login
/// Keychain, and therefore cannot run under the default Keychain read deny.
///
/// Claude Code, Copilot CLI, and Cursor Agent CLI do: each keeps its login
/// session in a login-keychain item (`Claude Code-credentials`,
/// `github-copilot-app`, `cursor-access-token` / `cursor-refresh-token`) and
/// does not persist a readable token under its state directory by default.
/// Codex, Gemini, and Grok keep credentials in plain files under their own
/// state directories, which are already granted, so they keep the deny. Names
/// that do not resolve to a canonical [`Provider`] keep the deny too — the
/// carve-out fails closed. [ORB-10929] [ORB-12261]
///
/// Internal: callers outside this crate want [`macos_login_keychain_access`],
/// which answers the question the compiled profile actually settles.
fn provider_reads_macos_login_keychain(provider: &str) -> bool {
    matches!(
        Provider::parse(provider).ok(),
        Some(Provider::Claude | Provider::Copilot | Provider::Cursor)
    )
}

/// What the compiled profile decides about `provider` reading the *user* login
/// keychain directory (`$HOME/Library/Keychains`).
///
/// This mirrors the clause order emitted by
/// [`compile_macos_sandbox_profile`], so a caller can explain a failing run
/// without re-deriving SBPL semantics. [ORB-10931]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacosLoginKeychainAccess {
    /// The profile grants the read: the provider owns credentials there and no
    /// activity rule takes it back.
    Allowed,
    /// The provider keeps the default credential deny — it does not store
    /// credentials in the keychain, so it never receives the carve-out.
    DeniedByDefaultPolicy,
    /// `HOME` did not resolve, so the compiler had no path to re-allow and the
    /// default deny stands.
    HomeUnresolved,
    /// The activity's own negated `read` rule denies the keychain directory.
    /// It is emitted after the carve-out, so last-match-wins gives it priority.
    DeniedByActivityRule {
        /// The rule as authored in the resolved profile, `!`-prefix included.
        rule: String,
    },
}

/// Resolve [`MacosLoginKeychainAccess`] for a profile that would be compiled
/// with the same `provider`, `home`, and `rules`.
///
/// Activity-rule coverage is judged from the longest non-glob prefix of each
/// negated `read` rule, which bounds what the emitted `(subpath ...)` or
/// `(regex ...)` deny clause can match. That is exact for the literal and
/// trailing-`**` rules operators actually write, and conservative for interior
/// globs: it may report a denial the kernel would only partially apply, but it
/// never reports a keychain as reachable once a rule has denied it.
pub fn macos_login_keychain_access(
    provider: &str,
    home: Option<&OsStr>,
    rules: &ResolvedFsProfile,
) -> MacosLoginKeychainAccess {
    if !provider_reads_macos_login_keychain(provider) {
        return MacosLoginKeychainAccess::DeniedByDefaultPolicy;
    }
    let Some(home) = super::provider_dirs::non_empty_env_path(home) else {
        return MacosLoginKeychainAccess::HomeUnresolved;
    };
    let keychains = home.join(USER_KEYCHAINS_SUBPATH);
    for rule in &rules.read {
        let Some(deny_path) = rule.strip_prefix('!') else {
            continue;
        };
        if super::sbpl_filter::deny_rule_reaches_path(deny_path, &keychains) {
            return MacosLoginKeychainAccess::DeniedByActivityRule { rule: rule.clone() };
        }
    }
    MacosLoginKeychainAccess::Allowed
}

/// Re-allow the confined provider's own credential store after
/// [`emit_default_credential_read_denies`], so last-match-wins grants it.
///
/// Without this, a sandboxed Claude, Copilot, or Cursor CLI cannot see its
/// Keychain item and reports a fake login failure (Claude: OAuth expiry;
/// Copilot: no authentication information; Cursor: authentication required) —
/// an authentication failure no re-login can clear, because the credential is
/// present and simply unreadable. The carve-out is deliberately narrow:
/// - it applies only to those providers, so a Codex or Grok agent still cannot
///   read any keychain;
/// - it covers only the *user* keychain directory; `/Library/Keychains` and
///   `/System/Library/Keychains` stay denied for every provider;
/// - it grants reads only. Nothing here makes the login keychain writable, so a
///   sandboxed run can use a refreshed token in memory but cannot persist it
///   back to the keychain; re-authentication stays an unsandboxed operation.
///
/// Reading the keychain file is not the same as reading its secrets: item
/// contents stay encrypted and gated by their own per-item ACLs, which is why
/// the grant is scoped to the provider that owns the item it needs.
///
/// Precedence: this clause sits between the default credential denies and the
/// activity's own negated `read` rules. Nothing an activity declares can
/// *widen* the grant — it depends only on the confined provider — and an
/// activity that denies the keychain directory (or any ancestor of it) narrows
/// it back, because its deny is emitted afterwards and last-match-wins.
/// [`macos_login_keychain_access`] reports which of those cases a given profile
/// lands in. [ORB-10931]
fn emit_provider_credential_read_reallow(provider: &str, home: Option<&OsStr>, out: &mut String) {
    if !provider_reads_macos_login_keychain(provider) {
        return;
    }
    let Some(home) = super::provider_dirs::non_empty_env_path(home) else {
        return;
    };
    let keychains = format!("{}/{USER_KEYCHAINS_SUBPATH}", home.display());
    out.push_str(&format!(
        "(allow file-read* (subpath \"{}\"))\n",
        super::sbpl_filter::sbpl_escape(&keychains)
    ));
}

/// HOME-relative path of the per-user keychain directory. Shared by the default
/// deny and the provider re-allow so the two clauses cannot drift.
const USER_KEYCHAINS_SUBPATH: &str = "Library/Keychains";

fn emit_default_credential_read_denies(
    home: Option<&OsStr>,
    cargo_home: Option<&OsStr>,
    out: &mut String,
) {
    if let Some(home) = super::provider_dirs::non_empty_env_path(home) {
        let home = home.display().to_string();
        for suffix in [
            ".ssh",
            ".aws",
            ".config/gh",
            USER_KEYCHAINS_SUBPATH,
            "Library/Application Support/Google/Chrome",
            "Library/Application Support/Chromium",
            "Library/Application Support/BraveSoftware/Brave-Browser",
            "Library/Application Support/Firefox",
        ] {
            emit_read_deny_subpath(&format!("{home}/{suffix}"), out);
        }
    }

    // Cargo's crates.io publish token. It is a file at the `$CARGO_HOME` root
    // rather than inside a granted subdirectory, so it needs its own clause:
    // `registry`/`git` are writable (see
    // [`emit_cargo_download_cache_write_allows`]) while the token beside them
    // is unreadable. Both spellings are denied — cargo reads the legacy
    // extensionless `credentials` as well as `credentials.toml`. [ORB-12469]
    if let Some(cargo_home) = cargo_home_dir(home, cargo_home) {
        for name in CARGO_CREDENTIAL_FILE_NAMES {
            emit_read_deny_literal(&format!("{}/{name}", cargo_home.display()), out);
        }
    }

    for path in ["/Library/Keychains", "/System/Library/Keychains"] {
        emit_read_deny_subpath(path, out);
    }
}

/// Whether the resolved profile grants any write at all. A profile whose
/// `modify` rules are all negated confines the CLI to a read-only filesystem,
/// and no convenience grant may quietly turn it into a writer.
fn profile_grants_write(rules: &ResolvedFsProfile) -> bool {
    rules.modify.iter().any(|rule| !rule.starts_with('!'))
}

/// Resolve Cargo's home directory the way cargo itself does: `$CARGO_HOME`
/// when the operator admitted it into the child environment, otherwise the
/// documented `$HOME/.cargo` default. `None` when neither resolves, in which
/// case no cargo clause is emitted at all.
fn cargo_home_dir(home: Option<&OsStr>, cargo_home: Option<&OsStr>) -> Option<PathBuf> {
    super::provider_dirs::non_empty_env_path(cargo_home)
        .or_else(|| super::provider_dirs::non_empty_env_path(home).map(|path| path.join(".cargo")))
}

/// Subdirectories of `$CARGO_HOME` a sandboxed build must be able to write.
const CARGO_WRITABLE_CACHE_SUBDIRS: &[&str] = &["registry", "git"];

/// `$CARGO_HOME` lock files a sandboxed build must be able to create and take.
const CARGO_PACKAGE_CACHE_LOCK_FILES: &[&str] = &[".package-cache", ".package-cache-mutate"];

/// Cargo credential file names, both spellings, denied for read.
const CARGO_CREDENTIAL_FILE_NAMES: &[&str] = &["credentials", "credentials.toml"];

/// Allow writes inside Cargo's shared download caches.
///
/// This is the one host-owned tree a sandboxed build must *write* outside its
/// own worktree. `cargo fetch` stores the downloaded `.crate` under
/// `$CARGO_HOME/registry/cache`, unpacks it under `registry/src`, refreshes the
/// index sidecar under `registry/index/<registry>/.cache`, and clones a git
/// dependency under `$CARGO_HOME/git`. With those denied, a worker whose
/// lockfile names a single crate the host has not cached yet dies with
/// `failed to open .../registry/cache/<crate>.crate: Operation not permitted`
/// — and stays silent until then, because a fully warm cache needs no write at
/// all. [ORB-12469]
///
/// The two `.package-cache*` locks are granted as `literal` clauses: they are
/// files at the `$CARGO_HOME` root, and cargo treats a lock it cannot open as
/// a read-only registry and proceeds *unlocked*, so withholding them while the
/// registry is writable would let concurrent workers mutate one shared
/// registry with no serialization.
///
/// Deliberately not granted: `$CARGO_HOME` itself, `$CARGO_HOME/bin` (the
/// host's installed binaries, which stay readable and executable but not
/// replaceable), and the credential files, which
/// [`emit_default_credential_read_denies`] additionally makes unreadable. The
/// caller emits these clauses only for a profile that already grants some
/// write, so a reviewer or other read-only profile keeps an immutable host —
/// the same rule Linux Bubblewrap follows for the identical paths.
/// `$CARGO_HOME/.global-cache` — cargo's cache-GC bookkeeping database — also
/// stays read-only; cargo skips that bookkeeping rather than failing the build.
fn emit_cargo_download_cache_write_allows(cargo_home: &Path, out: &mut String) {
    let cargo_home = cargo_home.display().to_string();
    for subdir in CARGO_WRITABLE_CACHE_SUBDIRS {
        out.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            super::sbpl_filter::sbpl_escape(&format!("{cargo_home}/{subdir}"))
        ));
    }
    for lock in CARGO_PACKAGE_CACHE_LOCK_FILES {
        out.push_str(&format!(
            "(allow file-write* (literal \"{}\"))\n",
            super::sbpl_filter::sbpl_escape(&format!("{cargo_home}/{lock}"))
        ));
    }
}

fn emit_read_deny_subpath(path: &str, out: &mut String) {
    out.push_str(&format!(
        "(deny file-read* (subpath \"{}\"))\n",
        super::sbpl_filter::sbpl_escape(path)
    ));
}

/// Deny reads of exactly one path. Used for a credential *file* that sits
/// beside granted siblings, where `subpath` would be the wrong shape.
fn emit_read_deny_literal(path: &str, out: &mut String) {
    out.push_str(&format!(
        "(deny file-read* (literal \"{}\"))\n",
        super::sbpl_filter::sbpl_escape(path)
    ));
}
