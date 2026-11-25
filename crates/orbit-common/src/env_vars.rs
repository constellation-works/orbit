//! Names and documentation for the fixed environment variables Orbit reads.
//!
//! Callers retain their own Unicode, parsing, precedence and fallback rules.
//! The CLI golden suite renders the reference from this same list.

/// Documentation for one fixed process environment variable.
pub struct EnvVarDoc {
    /// The name used for the process environment lookup.
    pub name: &'static str,
    /// What the variable controls.
    pub meaning: &'static str,
    /// The behavior when the variable is unset.
    pub default: &'static str,
    /// How the variable relates to an existing CLI flag, if any.
    pub flag: Option<&'static str>,
}

macro_rules! env_vars {
    ($(const $name:ident = ($meaning:literal, $default:literal $(, $flag:literal)? $(,)?);)+) => {
        $(
            #[doc = concat!($meaning, "\n\n# Default when unset\n\n", $default $(, "\n\n", $flag)?)]
            pub const $name: &str = stringify!($name);
        )+

        /// Fixed process environment variables and their reference entries.
        pub const ENVIRONMENT_VARIABLES: &[EnvVarDoc] = &[
            $(
                EnvVarDoc {
                    name: $name,
                    meaning: $meaning,
                    default: $default,
                    flag: env_vars!(@flag $($flag)?),
                },
            )+
        ];
    };
    (@flag $flag:literal) => { Some($flag) };
    (@flag) => { None };
}

env_vars! {
    const CARGO_HOME = (
        "Cargo home for toolchain discovery, build caches and credential paths.",
        "The user's `~/.cargo` where Orbit derives a Cargo home.",
    );
    const CARGO_MANIFEST_DIR = (
        "Workspace location for the engine round-trip example.",
        "The manifest directory recorded at compile time.",
    );
    const CLAUDE_CONFIG_DIR = (
        "Claude Code's configuration and state directory.",
        "`~/.claude` where Orbit derives the directory.",
    );
    const CLICOLOR_FORCE = (
        "Terminal color preference. It cannot enable color on non-terminals or bypass `NO_COLOR` or `TERM=dumb`.",
        "Unset.",
    );
    const CODEX_HOME = (
        "Codex configuration and state directory.",
        "`~/.codex` where Orbit derives the directory.",
    );
    const COLUMNS = (
        "Preferred terminal width in columns.",
        "Terminal width query, or no truncation when neither supplies a width.",
    );
    const CONSTELLATION_DEFAULT_PROVIDER = (
        "Provider used to resolve the default workflow crew below explicit config.",
        "The built-in default crew.",
    );
    const COPILOT_HOME = (
        "Copilot configuration and state directory.",
        "`~/.copilot` where Orbit derives the directory.",
    );
    const DBUS_SESSION_BUS_ADDRESS = (
        "Session bus address forwarded to GitHub CLI for access to the login keyring.",
        "Unset.",
    );
    const GH_CONFIG_DIR = (
        "GitHub CLI configuration directory forwarded to governed GitHub CLI processes.",
        "GitHub CLI uses its own defaults. Quota accounting tries `XDG_CONFIG_HOME` then `HOME`.",
    );
    const GH_ENTERPRISE_TOKEN = (
        "GitHub Enterprise authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GH_HOST = (
        "GitHub host for quota accounting and GitHub CLI forwarding.",
        "`github.com` for quota accounting. GitHub CLI resolves its own host.",
    );
    const GH_TOKEN = (
        "GitHub authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GITHUB_ENTERPRISE_TOKEN = (
        "Alternate GitHub Enterprise authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GITHUB_TOKEN = (
        "Alternate GitHub authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GIT_COMMON_DIR = (
        "Git common-directory override. Its presence makes Orbit defer worktree discovery to Git.",
        "Unset.",
    );
    const GIT_DIR = (
        "Git directory override. Its presence makes Orbit defer worktree discovery to Git.",
        "Unset.",
    );
    const GIT_WORK_TREE = (
        "Git working-tree override. Its presence makes Orbit defer worktree discovery to Git.",
        "Unset.",
    );
    const GOROOT = (
        "Go installation root forwarded to plugin builds.",
        "Unset.",
    );
    const GROK_CLAUDE_MCPS_ENABLED = (
        "Set to `0` or `false` to use native home-level Grok MCP registration instead of Claude-compatible registration.",
        "Grok's Claude compatibility setting.",
    );
    const GROK_HOME = (
        "Grok configuration and state directory.",
        "`~/.grok` where Orbit derives the directory.",
    );
    const HOME = (
        "Process home directory used to resolve Orbit and provider paths and compose child environments.",
        "Unset if the OS or launcher does not supply it. Some lookups also try `USERPROFILE`.",
    );
    const HOSTNAME = (
        "Optional host attribution recorded in audit events.",
        "Unset.",
    );
    const JAVA_HOME = (
        "Java installation root forwarded to plugin builds.",
        "Unset.",
    );
    const LANG = (
        "Locale inherited by host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const LC_ALL = (
        "Locale override inherited by host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const LD_LIBRARY_PATH = (
        "Existing Linux library search path appended after browser kit libraries.",
        "Unset.",
    );
    const LLVM_PROFILE_FILE = (
        "LLVM coverage output path template forwarded to pipeline workers.",
        "Unset.",
    );
    const LOGNAME = (
        "Login name used as a fallback for host identity and forwarded to host-side VCS children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const NO_COLOR = (
        "A non-empty value disables terminal styling.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_BUILD_BUDGET_DIR = (
        "Managed directory for build-slot wait records used to extend the activity deadline.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_DEADLINE_UNIX_MS = (
        "Managed activity deadline as Unix epoch milliseconds.",
        "Unset when no managed deadline is supplied.",
    );
    const ORBIT_ACTIVITY_DIR = (
        "Colon-separated activity catalog directories loaded ahead of global and workspace catalogs.",
        "`ORBIT_V2_CATALOG_DIR`, then the normal catalog paths.",
    );
    const ORBIT_ACTIVITY_FS_PROFILE = (
        "Filesystem profile for the current managed activity.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_ID = (
        "Current activity identifier for managed attribution.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_MCP_TOOLSETS = (
        "Managed MCP toolsets advertised for this activity.",
        "Use the flags or host configuration.",
        "A dispatch stamp overrides `--role` and `--toolset` for MCP serve and listen.",
    );
    const ORBIT_ACTIVITY_NAME = (
        "Name of the managed activity used in tool-policy diagnostics.",
        "`unnamed activity` when needed for a diagnostic.",
    );
    const ORBIT_ACTIVITY_RESIDENT_CONTINUITY = (
        "The dispatcher supplies `rebuilt` when a resident conversation is rebuilt after a failed resume.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_TIMEOUT_MS = (
        "Activity wall-clock budget in milliseconds, used to read build-slot wait records.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_TOOLS = (
        "Comma-separated callable tools stamped by the managed dispatcher.",
        "Unset when no list is stamped.",
    );
    const ORBIT_ACTIVITY_TOOLS_DENY = (
        "Comma-separated disallowed tools used with managed deny-mode tool policy.",
        "Unset. The dispatcher allowlist supplies the fallback policy.",
    );
    const ORBIT_ACTIVITY_TOOL_POLICY = (
        "Managed tool policy marker. `deny` selects the deny policy when its list is present.",
        "The managed dispatcher allowlist.",
    );
    const ORBIT_ACTOR = (
        "Identity label used for attribution when supplied by the process.",
        "Derived from the caller identity.",
    );
    const ORBIT_ADOPT_PIPELINE_RUN = (
        "Identifies the pipeline run a replacement worker adopts during handover.",
        "Unset.",
    );
    const ORBIT_AGENT_MODEL = (
        "Model label stamped for agent attribution.",
        "Unset.",
        "Explicit attribution flags such as `orbit tool run --model` take precedence.",
    );
    const ORBIT_AGENT_NAME = (
        "Agent or provider label stamped for agent attribution.",
        "Unset.",
        "Explicit attribution via `orbit tool run --model` or the deprecated `--agent` takes precedence.",
    );
    const ORBIT_APPLICATION_CREDENTIAL_FD = (
        "Inherited descriptor for the managed application credential.",
        "Unset.",
    );
    const ORBIT_ATTENTION_WAIT_TOKEN = (
        "Session grant for `orbit attention wait`.",
        "Unset. The wait requires a token.",
    );
    const ORBIT_BACKEND = (
        "Retired backend selector retained for compatibility diagnostics.",
        "Unset.",
    );
    const ORBIT_BIN = (
        "Explicit Orbit executable pinned for tools launched by an agent.",
        "The dispatching Orbit executable.",
    );
    const ORBIT_BUILD_BUDGET = (
        "Whether to enforce host build-slot admission, `0` or `1`.",
        "`1`.",
    );
    const ORBIT_BUILD_BUDGET_DIR = (
        "Directory holding host build-slot state.",
        "`~/.orbit/cache/build-budget`.",
    );
    const ORBIT_BUILD_SLOTS = (
        "Host build-slot count from 1 through 128.",
        "The budget directory's `slots` file, or `2` when absent.",
    );
    const ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES = (
        "Capture limit in bytes per stream for a provider CLI.",
        "`1048576`.",
    );
    const ORBIT_DASHBOARD_HANDOVER = (
        "Marks a replacement dashboard process so it skips opening a browser.",
        "Unset.",
        "Same browser behavior as `orbit web serve --no-open`.",
    );
    const ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES = (
        "Capture limit in bytes per stream for a supervised command.",
        "`1048576`.",
    );
    const ORBIT_EXTERNAL_TOOL_TIMEOUT_MS = (
        "Timeout override in milliseconds for an external tool.",
        "15 seconds.",
    );
    const ORBIT_FORMAT = (
        "CLI output mode. Accepted values are `auto`, `table`, `plain`, `json` and `ndjson`.",
        "`auto`.",
        "Overridden by `--format`, legacy `--json` or command-local `--ops`.",
    );
    const ORBIT_INSTALL_DIR = (
        "Directory containing the managed Orbit installation.",
        "`~/.orbit/bin`.",
    );
    const ORBIT_INSTALL_REPO = (
        "GitHub repository from which `orbit update` fetches releases.",
        "`constellation-works/orbit`.",
    );
    const ORBIT_JOB_DIR = (
        "Colon-separated extra job catalog directories.",
        "`ORBIT_V2_JOB_DIR`, then the normal catalog paths.",
    );
    const ORBIT_LENT_GENERATION_FENCE = (
        "Generation fence inherited by a child during an update.",
        "Unset.",
    );
    const ORBIT_LOG_PATH = (
        "Path read by `orbit log tail`.",
        "The global Orbit JSONL log.",
        "Overridden by `orbit log tail --path`.",
    );
    const ORBIT_MANAGED_RUN_CONTEXT = (
        "Marks a process launched by the managed dispatcher. Some consumers also require a run or session identity.",
        "Unset.",
    );
    const ORBIT_MCP_RESUME = (
        "Resume descriptor passed to a replacement stdio MCP session during handover.",
        "Unset. The server starts a fresh stdio session.",
    );
    const ORBIT_OPERATOR = (
        "Explicit, audited operator authority override for a local caller.",
        "Unset. Normal caller capability rules apply.",
    );
    const ORBIT_PLUGIN = (
        "Name of the plugin whose backend is running.",
        "Unset.",
    );
    const ORBIT_PLUGIN_BROKER = (
        "Socket path for a managed plugin broker.",
        "Unset.",
    );
    const ORBIT_PLUGIN_CALLBACK = (
        "Retired plugin callback credential detected to reject old callback invocations.",
        "Unset.",
    );
    const ORBIT_PLUGIN_CALLBACK_FD = (
        "Inherited descriptor for the plugin callback channel.",
        "Unset.",
    );
    const ORBIT_PROC_ALLOWED_PROGRAMS = (
        "Comma-separated callable program names stamped for managed subprocess execution.",
        "Unset.",
    );
    const ORBIT_PROC_DISALLOWED_PROGRAMS = (
        "Comma-separated program names disallowed by managed deny-mode subprocess policy.",
        "Unset.",
    );
    const ORBIT_PROC_PROGRAM_POLICY = (
        "Managed subprocess policy marker. `deny` enables the disallow list.",
        "Unset. The existing allowed-program rules apply.",
    );
    const ORBIT_REGISTRY_ROOT = (
        "Absolute registry root supplied by the dispatcher for a managed invocation.",
        "Normal global registry resolution.",
        "An explicit `--root` can select a different root.",
    );
    const ORBIT_RELEASE_PUBLIC_KEY_FILE = (
        "Retired release-key override. A release trust file cannot be configured alongside it.",
        "Unset.",
    );
    const ORBIT_RELEASE_TRUSTED_KEYS_FILE = (
        "File containing additional trusted release signing keys.",
        "Shipped release keys only.",
    );
    const ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE = (
        "Set to `1` to acknowledge the configured release signing trust change.",
        "Unset. An extra trust file requires acknowledgement.",
    );
    const ORBIT_REQUIRE_SANDBOX_EXEC = (
        "Set to `1` to require macOS Seatbelt for applicable plugin and host checks.",
        "Unset.",
    );
    const ORBIT_ROOT = (
        "Explicit Orbit config and state root, or checkout containing an initialized `.orbit` directory.",
        "Normal workspace discovery and global `~/.orbit`.",
        "Overridden by `--root`.",
    );
    const ORBIT_RUN_ID = (
        "Job-run identifier used for managed dispatch and audit correlation.",
        "Unset.",
    );
    const ORBIT_SESSION_ID = (
        "Invocation or session identifier used for managed dispatch and audit correlation.",
        "Unset.",
    );
    const ORBIT_SKIP_HOST_PREREQUISITES = (
        "Skip Linux host package and AppArmor setup during initialization.",
        "Off. Cargo test configuration sets it to `1`.",
        "Fallback for `orbit init --skip-host-prerequisites`.",
    );
    const ORBIT_STEP_INDEX = (
        "Numeric index of the managed activity step for audit correlation.",
        "Unset.",
    );
    const ORBIT_STOPPED_DESCENDANT_THRESHOLD_MS = (
        "Time a supervised descendant must stay stopped before it can be terminated.",
        "10 minutes.",
    );
    const ORBIT_TASK_ACTOR_KIND = (
        "Actor kind stamped on a managed task. An `agent` value activates agent-specific policy checks.",
        "Unset.",
    );
    const ORBIT_TASK_ID = (
        "Task identifier used for managed dispatch and audit correlation.",
        "Unset.",
    );
    const ORBIT_TEST_AGENT_JSONL_GATE = (
        "Debug-build rendezvous for an agent JSONL write fixture.",
        "Unset. Writes proceed without a gate.",
    );
    const ORBIT_TEST_CONFIG_WRITE_GATE = (
        "Test-support rendezvous directory for a config write fixture.",
        "Unset. Writes proceed without a gate.",
    );
    const ORBIT_TEST_ENV_SCRUBBED = (
        "Marker that a re-executed test process already scrubbed its environment.",
        "Unset.",
    );
    const ORBIT_TEST_EVENTS_DRAIN_RECHECK_INTERVAL_MS = (
        "Test-support interval in milliseconds for dashboard drain-phase checks.",
        "15 seconds.",
    );
    const ORBIT_TEST_EVENTS_HOST_RESOURCES_INTERVAL_MS = (
        "Test-support interval in milliseconds for dashboard host-resource samples.",
        "5 seconds.",
    );
    const ORBIT_TEST_EVENTS_SCAN_INTERVAL_MS = (
        "Test-support interval in milliseconds for dashboard workspace event scans.",
        "2 seconds.",
    );
    const ORBIT_TEST_FORECAST_COMPUTE_BUDGET_MS = (
        "Test-support compute budget for backlog arrivals, in milliseconds.",
        "20 seconds.",
    );
    const ORBIT_TEST_FORECAST_MAX_GENERATED = (
        "Test-support cap on generated backlog items.",
        "`10000`.",
    );
    const ORBIT_TEST_HOSTS_PUBLISH_GATE = (
        "Test-support rendezvous directory for host-list publication.",
        "Unset. Publication proceeds without a gate.",
    );
    const ORBIT_TEST_HOST_FORWARD_ATTACH_TIMEOUT_MS = (
        "Debug-build timeout in milliseconds for attaching to a remote dashboard.",
        "5 seconds.",
    );
    const ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS = (
        "Debug-build interval in milliseconds for stdio MCP lifecycle checks.",
        "2 seconds.",
    );
    const ORBIT_TEST_MERGEABILITY_WAIT = (
        "Set to `virtual` in debug builds to exercise mergeability polling without real delays.",
        "Real polling delays.",
    );
    const ORBIT_TEST_PIPELINE_WAIT_VIRTUAL = (
        "Set to `1` in debug builds to use a virtual clock for pipeline wait fixtures.",
        "Real clock.",
    );
    const ORBIT_TEST_RELAXED_DURABILITY = (
        "Set to `1` in debug builds to skip durable syncs in tests.",
        "Durable syncs enabled. Workspace Cargo config sets it to `1`.",
    );
    const ORBIT_TEST_RELEASE_API_BASE = (
        "GitHub release API base override for builds with the release API test feature.",
        "The GitHub API.",
    );
    const ORBIT_TEST_SHARED_IMAGE_DIGESTS = (
        "Debug-build directory for sharing executable digests among test processes.",
        "Unset. Workspace Cargo config supplies `target/test-image-digests`.",
    );
    const ORBIT_TEST_SOURCE_ELAPSED_MS = (
        "Frozen elapsed milliseconds for source inspection in test-support builds.",
        "Real elapsed time.",
    );
    const ORBIT_UPDATE_PROBE_TIMEOUT_SECS = (
        "Timeout in seconds for version and contract probes during an update.",
        "30 seconds.",
    );
    const ORBIT_UPDATE_RELEASE_DIR = (
        "Local release mirror directory used in preference to the network.",
        "GitHub Releases.",
    );
    const ORBIT_UPGRADE_QUIESCE_SECS = (
        "Time in seconds for live participants to yield during a breaking upgrade.",
        "120 seconds.",
    );
    const ORBIT_V2_CATALOG_DIR = (
        "Legacy activity catalog directory list used when `ORBIT_ACTIVITY_DIR` is missing or not Unicode.",
        "Normal global and workspace activity catalogs.",
    );
    const ORBIT_V2_JOB_DIR = (
        "Legacy job catalog directory list used when `ORBIT_JOB_DIR` is missing or not Unicode.",
        "Normal workspace and global job catalogs.",
    );
    const ORBIT_VALIDATION_BASE = (
        "Base revision supplied by the validation runner.",
        "Unset.",
    );
    const ORBIT_VALIDATION_GATE = (
        "Name or command supplied by the validation runner for the current check.",
        "Unset.",
    );
    const ORBIT_VALIDATION_SUMMARY = (
        "Path at which validation writes its structured summary.",
        "Unset.",
    );
    const ORBIT_WORKER_CONTAINMENT_STRICT = (
        "Set to `1` to carry strict worker containment to pipeline descendants.",
        "The configured worker containment policy applies.",
        "`--strict-worker-containment` also enables strict containment.",
    );
    const ORBIT_WORKER_CONTEXT_REQUIRED = (
        "Marks a worker that must receive the managed coordination context.",
        "Unset.",
    );
    const ORBIT_WORKSPACE = (
        "Logical workspace selector supplied by the managed dispatcher.",
        "Normal workspace resolution when no managed selector applies.",
        "An explicit `--workspace` takes precedence.",
    );
    const ORBIT_WORKSPACE_CLAIM_TOKEN = (
        "Claim credential supplied to an admitted workspace claimant.",
        "Unset.",
    );
    const ORBIT_WORKTREE_ROOT = (
        "Parent directory override for managed worktrees.",
        "The workspace's managed worktree directory.",
    );
    const PATH = (
        "Program search path used for executable discovery and child execution.",
        "Unset if the OS or launcher does not supply it. Individual launchers may supply a fallback.",
    );
    const PATHEXT = (
        "Windows executable extensions tried when searching for programs.",
        "`.COM;.EXE;.BAT;.CMD`.",
    );
    const RUSTUP_HOME = (
        "Rustup installation and toolchain cache root forwarded to plugin builds.",
        "Unset.",
    );
    const RUSTUP_TOOLCHAIN = (
        "Rust toolchain selection forwarded to plugin builds.",
        "Unset.",
    );
    const RUST_LOG = (
        "Tracing directives for Orbit logging.",
        "`warn` for CLI stderr. The CLI JSONL log also keeps provider supervision and MCP configuration at `info`.",
    );
    const SHELL = (
        "Login shell location forwarded to host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const SSH_AUTH_SOCK = (
        "SSH agent socket forwarded to host-side Git children.",
        "Unset.",
    );
    const SSH_CONNECTION = (
        "OpenSSH connection metadata from which Orbit extracts the caller IP.",
        "Unset.",
    );
    const SUDO_GID = (
        "Original group ID used by Linux host setup when running under sudo.",
        "Unset.",
    );
    const SUDO_UID = (
        "Original user ID used by Linux host setup when running under sudo.",
        "Unset.",
    );
    const TERM = (
        "Terminal type. `dumb` disables terminal styling.",
        "Unset if the OS or launcher does not supply it.",
    );
    const TMPDIR = (
        "Temporary directory forwarded to host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const TZ = (
        "Timezone used for script events and routine schedule display.",
        "UTC for job scripts. Routine display consults the host timezone files and always reports the local offset.",
    );
    const USER = (
        "Host login name used for attribution and forwarded to host-side VCS children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const USERNAME = (
        "Host login name supplied by Windows or the launcher.",
        "Unset if the OS or launcher does not supply it.",
    );
    const USERPROFILE = (
        "Windows home directory used when supported home lookups cannot use `HOME`.",
        "Unset if the OS or launcher does not supply it.",
    );
    const XDG_CACHE_HOME = (
        "XDG cache root used for Copilot's launcher cache.",
        "`~/.cache` where Orbit derives the cache root.",
    );
    const XDG_CONFIG_HOME = (
        "XDG configuration root used for provider state and forwarded to GitHub CLI.",
        "`~/.config` where Orbit derives the configuration root.",
    );
    const XDG_DATA_HOME = (
        "XDG data root used for Muse state.",
        "`~/.local/share` where Orbit derives the data root.",
    );
    const XDG_RUNTIME_DIR = (
        "XDG runtime directory used to locate IPC sockets and forwarded to GitHub CLI.",
        "Unset.",
    );
}
