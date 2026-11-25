//! Names and documentation for the fixed environment variables Orbit reads.
//!
//! Each entry also names its typed accessor on [`Env`], the one capture of the
//! process environment. Accessors of test hooks (`ORBIT_TEST_*`, marked
//! `#[test_support]`) exist only in tests and `test-support` builds.
//!
//! Callers retain their own Unicode, parsing, precedence and fallback rules.
//! The CLI golden suite renders the reference from this same list.

use std::ffi::OsStr;

use crate::env::Env;

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
    ($(
        $(#[$hook:ident])?
        const $name:ident: $accessor:ident = ($meaning:literal, $default:literal $(, $flag:literal)? $(,)?);
    )+) => {
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

        impl Env {
            $(
                env_vars!(@accessor [$($hook)?]
                    #[doc = concat!("The captured value of `", stringify!($name), "`: ", $meaning)]
                    pub fn $accessor(&self) -> Option<&OsStr> {
                        self.get($name)
                    }
                );
            )+
        }
    };
    (@flag $flag:literal) => { Some($flag) };
    (@flag) => { None };
    (@accessor [test_support] $($item:tt)+) => {
        #[cfg(any(test, feature = "test-support"))]
        $($item)+
    };
    (@accessor [] $($item:tt)+) => { $($item)+ };
}

env_vars! {
    const CARGO_DENY_DB_PATH: cargo_deny_db_path = (
        "Cargo-deny advisory database location that Orbit sets for a sandboxed worker that may write.",
        "Unset. Cargo-deny uses its own default location.",
    );
    const CARGO_HOME: cargo_home = (
        "Cargo home for toolchain discovery, build caches and credential paths.",
        "The user's `~/.cargo` where Orbit derives a Cargo home.",
    );
    const CARGO_MANIFEST_DIR: cargo_manifest_dir = (
        "Workspace location for the engine round-trip example.",
        "The manifest directory recorded at compile time.",
    );
    const CLAUDE_CONFIG_DIR: claude_config_dir = (
        "Claude Code's configuration and state directory.",
        "`~/.claude` where Orbit derives the directory.",
    );
    const CLICOLOR_FORCE: clicolor_force = (
        "Terminal color preference. It cannot enable color on non-terminals or bypass `NO_COLOR` or `TERM=dumb`.",
        "Unset.",
    );
    const CODEX_CA_CERTIFICATE: codex_ca_certificate = (
        "PEM CA bundle for Codex. On macOS Orbit checks it is readable and supplies the system bundle when neither this nor `SSL_CERT_FILE` is set.",
        "Unset. Orbit uses `/etc/ssl/cert.pem` on macOS.",
    );
    const CODEX_HOME: codex_home = (
        "Codex configuration and state directory.",
        "`~/.codex` where Orbit derives the directory.",
    );
    const COLUMNS: columns = (
        "Preferred terminal width in columns.",
        "Terminal width query, or no truncation when neither supplies a width.",
    );
    const CONSTELLATION_DEFAULT_PROVIDER: constellation_default_provider = (
        "Provider used to resolve the default workflow crew below explicit config.",
        "The built-in default crew.",
    );
    const COPILOT_HOME: copilot_home = (
        "Copilot configuration and state directory.",
        "`~/.copilot` where Orbit derives the directory.",
    );
    const DBUS_SESSION_BUS_ADDRESS: dbus_session_bus_address = (
        "Session bus address forwarded to GitHub CLI for access to the login keyring.",
        "Unset.",
    );
    const GH_CONFIG_DIR: gh_config_dir = (
        "GitHub CLI configuration directory forwarded to governed GitHub CLI processes.",
        "GitHub CLI uses its own defaults. Quota accounting tries `XDG_CONFIG_HOME` then `HOME`.",
    );
    const GH_ENTERPRISE_TOKEN: gh_enterprise_token = (
        "GitHub Enterprise authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GH_HOST: gh_host = (
        "GitHub host for quota accounting and GitHub CLI forwarding.",
        "`github.com` for quota accounting. GitHub CLI resolves its own host.",
    );
    const GH_TOKEN: gh_token = (
        "GitHub authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GITHUB_ENTERPRISE_TOKEN: github_enterprise_token = (
        "Alternate GitHub Enterprise authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GITHUB_TOKEN: github_token = (
        "Alternate GitHub authentication token forwarded to GitHub CLI.",
        "Unset.",
    );
    const GIT_COMMON_DIR: git_common_dir = (
        "Git common-directory override. Its presence makes Orbit defer worktree discovery to Git.",
        "Unset.",
    );
    const GIT_DIR: git_dir = (
        "Git directory override. Its presence makes Orbit defer worktree discovery to Git.",
        "Unset.",
    );
    const GIT_WORK_TREE: git_work_tree = (
        "Git working-tree override. Its presence makes Orbit defer worktree discovery to Git.",
        "Unset.",
    );
    const GOROOT: goroot = (
        "Go installation root forwarded to plugin builds.",
        "Unset.",
    );
    const GROK_CLAUDE_MCPS_ENABLED: grok_claude_mcps_enabled = (
        "Set to `0` or `false` to use native home-level Grok MCP registration instead of Claude-compatible registration.",
        "Grok's Claude compatibility setting.",
    );
    const GROK_HOME: grok_home = (
        "Grok configuration and state directory.",
        "`~/.grok` where Orbit derives the directory.",
    );
    const HOME: home = (
        "Process home directory used to resolve Orbit and provider paths and compose child environments.",
        "Unset if the OS or launcher does not supply it. Some lookups also try `USERPROFILE`.",
    );
    const HOSTNAME: hostname = (
        "Optional host attribution recorded in audit events.",
        "Unset.",
    );
    const JAVA_HOME: java_home = (
        "Java installation root forwarded to plugin builds.",
        "Unset.",
    );
    const LANG: lang = (
        "Locale inherited by host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const LC_ALL: lc_all = (
        "Locale override inherited by host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const LD_LIBRARY_PATH: ld_library_path = (
        "Existing Linux library search path appended after browser kit libraries.",
        "Unset.",
    );
    const LLVM_PROFILE_FILE: llvm_profile_file = (
        "LLVM coverage output path template forwarded to pipeline workers.",
        "Unset.",
    );
    const LOGNAME: logname = (
        "Login name used as a fallback for host identity and forwarded to host-side VCS children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const NO_COLOR: no_color = (
        "A non-empty value disables terminal styling.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_BUILD_BUDGET_DIR: orbit_activity_build_budget_dir = (
        "Managed directory for build-slot wait records used to extend the activity deadline.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_DEADLINE_UNIX_MS: orbit_activity_deadline_unix_ms = (
        "Managed activity deadline as Unix epoch milliseconds.",
        "Unset when no managed deadline is supplied.",
    );
    const ORBIT_ACTIVITY_DIR: orbit_activity_dir = (
        "Colon-separated activity catalog directories loaded ahead of global and workspace catalogs.",
        "`ORBIT_V2_CATALOG_DIR`, then the normal catalog paths.",
    );
    const ORBIT_ACTIVITY_FS_PROFILE: orbit_activity_fs_profile = (
        "Filesystem profile for the current managed activity.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_ID: orbit_activity_id = (
        "Current activity identifier for managed attribution.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_MCP_TOOLSETS: orbit_activity_mcp_toolsets = (
        "Managed MCP toolsets advertised for this activity.",
        "Use the flags or host configuration.",
        "A dispatch stamp overrides `--role` and `--toolset` for MCP serve and listen.",
    );
    const ORBIT_ACTIVITY_NAME: orbit_activity_name = (
        "Name of the managed activity used in tool-policy diagnostics.",
        "`unnamed activity` when needed for a diagnostic.",
    );
    const ORBIT_ACTIVITY_RESIDENT_CONTINUITY: orbit_activity_resident_continuity = (
        "The dispatcher supplies `rebuilt` when a resident conversation is rebuilt after a failed resume.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_TIMEOUT_MS: orbit_activity_timeout_ms = (
        "Activity wall-clock budget in milliseconds, used to read build-slot wait records.",
        "Unset.",
    );
    const ORBIT_ACTIVITY_TOOLS: orbit_activity_tools = (
        "Comma-separated callable tools stamped by the managed dispatcher.",
        "Unset when no list is stamped.",
    );
    const ORBIT_ACTIVITY_TOOLS_DENY: orbit_activity_tools_deny = (
        "Comma-separated disallowed tools used with managed deny-mode tool policy.",
        "Unset. The dispatcher allowlist supplies the fallback policy.",
    );
    const ORBIT_ACTIVITY_TOOL_POLICY: orbit_activity_tool_policy = (
        "Managed tool policy marker. `deny` selects the deny policy when its list is present.",
        "The managed dispatcher allowlist.",
    );
    const ORBIT_ACTOR: orbit_actor = (
        "Identity label used for attribution when supplied by the process.",
        "Derived from the caller identity.",
    );
    const ORBIT_ADOPT_PIPELINE_RUN: orbit_adopt_pipeline_run = (
        "Identifies the pipeline run a replacement worker adopts during handover.",
        "Unset.",
    );
    const ORBIT_AGENT_MODEL: orbit_agent_model = (
        "Model label stamped for agent attribution.",
        "Unset.",
        "Explicit attribution flags such as `orbit tool run --model` take precedence.",
    );
    const ORBIT_AGENT_NAME: orbit_agent_name = (
        "Agent or provider label stamped for agent attribution.",
        "Unset.",
        "Explicit attribution via `orbit tool run --model` or the deprecated `--agent` takes precedence.",
    );
    const ORBIT_APPLICATION_CREDENTIAL_FD: orbit_application_credential_fd = (
        "Inherited descriptor for the managed application credential.",
        "Unset.",
    );
    const ORBIT_ATTENTION_WAIT_TOKEN: orbit_attention_wait_token = (
        "Session grant for `orbit attention wait`.",
        "Unset. The wait requires a token.",
    );
    const ORBIT_BACKEND: orbit_backend = (
        "Retired backend selector retained for compatibility diagnostics.",
        "Unset.",
    );
    const ORBIT_BIN: orbit_bin = (
        "Explicit Orbit executable pinned for tools launched by an agent.",
        "The dispatching Orbit executable.",
    );
    const ORBIT_BROWSER_KIT_ENV: orbit_browser_kit_env = (
        "Path of the headless browser kit's `env.sh`, which a check sources before it launches Chromium.",
        "Unset. Set only for a worker whose host has a verified kit.",
    );
    const ORBIT_BROWSER_KIT_SCREENSHOT: orbit_browser_kit_screenshot = (
        "Screenshot path that `orbit config browser-kit verify` hands to its smoke test.",
        "Unset. Set only for the smoke test.",
    );
    const ORBIT_BUILD_BUDGET: orbit_build_budget = (
        "Whether to enforce host build-slot admission, `0` or `1`.",
        "`1`.",
    );
    const ORBIT_BUILD_BUDGET_DIR: orbit_build_budget_dir = (
        "Directory holding host build-slot state.",
        "`~/.orbit/cache/build-budget`.",
    );
    const ORBIT_BUILD_SLOTS: orbit_build_slots = (
        "Host build-slot count from 1 through 128.",
        "The budget directory's `slots` file, or `2` when absent.",
    );
    const ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES: orbit_cli_runner_output_capture_limit_bytes = (
        "Capture limit in bytes per stream for a provider CLI.",
        "`1048576`.",
    );
    const ORBIT_COMMAND_EVIDENCE_DIR: orbit_command_evidence_dir = (
        "Directory in which a required validation command records command evidence.",
        "Unset. Set only for a required validation command.",
    );
    const ORBIT_COMMAND_EVIDENCE_INVOCATION: orbit_command_evidence_invocation = (
        "Invocation identifier that a required validation command attaches to its command evidence.",
        "Unset. Set only for a required validation command.",
    );
    const ORBIT_DASHBOARD_HANDOVER: orbit_dashboard_handover = (
        "Marks a replacement dashboard process so it skips opening a browser.",
        "Unset.",
        "Same browser behavior as `orbit web serve --no-open`.",
    );
    const ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES: orbit_exec_output_capture_limit_bytes = (
        "Capture limit in bytes per stream for a supervised command.",
        "`1048576`.",
    );
    const ORBIT_EXTERNAL_TOOL_TIMEOUT_MS: orbit_external_tool_timeout_ms = (
        "Timeout override in milliseconds for an external tool.",
        "15 seconds.",
    );
    const ORBIT_FORMAT: orbit_format = (
        "CLI output mode. Accepted values are `auto`, `table`, `plain`, `json` and `ndjson`.",
        "`auto`.",
        "Overridden by `--format`, legacy `--json` or command-local `--ops`.",
    );
    const ORBIT_INSTALL_DIR: orbit_install_dir = (
        "Directory containing the managed Orbit installation.",
        "`~/.orbit/bin`.",
    );
    const ORBIT_INSTALL_REPO: orbit_install_repo = (
        "GitHub repository from which `orbit update` fetches releases.",
        "`constellation-works/orbit`.",
    );
    const ORBIT_JOB_DIR: orbit_job_dir = (
        "Colon-separated extra job catalog directories.",
        "`ORBIT_V2_JOB_DIR`, then the normal catalog paths.",
    );
    const ORBIT_LENT_GENERATION_FENCE: orbit_lent_generation_fence = (
        "Generation fence inherited by a child during an update.",
        "Unset.",
    );
    const ORBIT_LOG_PATH: orbit_log_path = (
        "Path read by `orbit log tail` and the dashboard (`orbit-web`) via `resolve_log_path`.",
        "The global Orbit JSONL log.",
        "Overridden by `orbit log tail --path`.",
    );
    const ORBIT_MANAGED_RUN_CONTEXT: orbit_managed_run_context = (
        "Marks a process launched by the managed dispatcher. Some consumers also require a run or session identity.",
        "Unset.",
    );
    const ORBIT_MCP_RESUME: orbit_mcp_resume = (
        "Resume descriptor passed to a replacement stdio MCP session during handover.",
        "Unset. The server starts a fresh stdio session.",
    );
    const ORBIT_OPERATOR: orbit_operator = (
        "Explicit, audited operator authority override for a local caller.",
        "Unset. Normal caller capability rules apply.",
    );
    const ORBIT_PLUGIN: orbit_plugin = (
        "Name of the plugin whose backend is running.",
        "Unset.",
    );
    const ORBIT_PLUGIN_BROKER: orbit_plugin_broker = (
        "Socket path for a managed plugin broker.",
        "Unset.",
    );
    const ORBIT_PLUGIN_CALLBACK: orbit_plugin_callback = (
        "Retired plugin callback credential detected to reject old callback invocations.",
        "Unset.",
    );
    const ORBIT_PLUGIN_CALLBACK_FD: orbit_plugin_callback_fd = (
        "Inherited descriptor for the plugin callback channel.",
        "Unset.",
    );
    const ORBIT_PROC_ALLOWED_PROGRAMS: orbit_proc_allowed_programs = (
        "Comma-separated callable program names stamped for managed subprocess execution.",
        "Unset.",
    );
    const ORBIT_PROC_DISALLOWED_PROGRAMS: orbit_proc_disallowed_programs = (
        "Comma-separated program names disallowed by managed deny-mode subprocess policy.",
        "Unset.",
    );
    const ORBIT_PROC_PROGRAM_POLICY: orbit_proc_program_policy = (
        "Managed subprocess policy marker. `deny` enables the disallow list.",
        "Unset. The existing allowed-program rules apply.",
    );
    const ORBIT_REGISTRY_ROOT: orbit_registry_root = (
        "Absolute registry root supplied by the dispatcher for a managed invocation.",
        "Normal global registry resolution.",
        "An explicit `--root` can select a different root.",
    );
    const ORBIT_RELEASE_PUBLIC_KEY_FILE: orbit_release_public_key_file = (
        "Retired release-key override. A release trust file cannot be configured alongside it.",
        "Unset.",
    );
    const ORBIT_RELEASE_TRUSTED_KEYS_FILE: orbit_release_trusted_keys_file = (
        "File containing the replacement trusted release signing key set, replacing the shipped release keys.",
        "Shipped release keys only.",
    );
    const ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE: orbit_release_trusted_keys_file_acknowledge_trust_change = (
        "Set to `1` to acknowledge the replacement release signing trust file.",
        "Unset. A replacement trust file requires acknowledgement.",
    );
    const ORBIT_REQUIRE_SANDBOX_EXEC: orbit_require_sandbox_exec = (
        "Set to `1` to require macOS Seatbelt for applicable plugin and host checks.",
        "Unset.",
    );
    const ORBIT_ROOT: orbit_root = (
        "Explicit Orbit config and state root, or checkout containing an initialized `.orbit` directory.",
        "Normal workspace discovery and global `~/.orbit`.",
        "Overridden by `--root`.",
    );
    const ORBIT_RUN_ID: orbit_run_id = (
        "Job-run identifier used for managed dispatch and audit correlation.",
        "Unset.",
    );
    const ORBIT_SCRATCH_DIR: orbit_scratch_dir = (
        "Worker scratch directory, `<checkout>/.orbit/tmp`, which task artifact upload accepts.",
        "Unset. Set only for a managed worker.",
    );
    const ORBIT_SESSION_ID: orbit_session_id = (
        "Invocation or session identifier used for managed dispatch and audit correlation.",
        "Unset.",
    );
    const ORBIT_SKIP_HOST_PREREQUISITES: orbit_skip_host_prerequisites = (
        "Skip Linux host package and AppArmor setup during initialization.",
        "Off. Cargo test configuration sets it to `1`.",
        "Fallback for `orbit init --skip-host-prerequisites`.",
    );
    const ORBIT_STEP_INDEX: orbit_step_index = (
        "Numeric index of the managed activity step for audit correlation.",
        "Unset.",
    );
    const ORBIT_STOPPED_DESCENDANT_THRESHOLD_MS: orbit_stopped_descendant_threshold_ms = (
        "Time a supervised descendant must stay stopped before it can be terminated.",
        "10 minutes.",
    );
    const ORBIT_TASK_ACTOR_KIND: orbit_task_actor_kind = (
        "Actor kind stamped on a managed task. An `agent` value activates agent-specific policy checks.",
        "Unset.",
    );
    const ORBIT_TASK_ID: orbit_task_id = (
        "Task identifier used for managed dispatch and audit correlation.",
        "Unset.",
    );
    #[test_support]
    const ORBIT_TEST_AGENT_JSONL_GATE: orbit_test_agent_jsonl_gate = (
        "Debug-build rendezvous for an agent JSONL write fixture.",
        "Unset. Writes proceed without a gate.",
    );
    #[test_support]
    const ORBIT_TEST_CONFIG_WRITE_GATE: orbit_test_config_write_gate = (
        "Test-support rendezvous directory for a config write fixture.",
        "Unset. Writes proceed without a gate.",
    );
    #[test_support]
    const ORBIT_TEST_ENV_SCRUBBED: orbit_test_env_scrubbed = (
        "Marker that a re-executed test process already scrubbed its environment.",
        "Unset.",
    );
    #[test_support]
    const ORBIT_TEST_EVENTS_DRAIN_RECHECK_INTERVAL_MS: orbit_test_events_drain_recheck_interval_ms = (
        "Test-support interval in milliseconds for dashboard drain-phase checks.",
        "15 seconds.",
    );
    #[test_support]
    const ORBIT_TEST_EVENTS_HOST_RESOURCES_INTERVAL_MS: orbit_test_events_host_resources_interval_ms = (
        "Test-support interval in milliseconds for dashboard host-resource samples.",
        "5 seconds.",
    );
    #[test_support]
    const ORBIT_TEST_EVENTS_SCAN_INTERVAL_MS: orbit_test_events_scan_interval_ms = (
        "Test-support interval in milliseconds for dashboard workspace event scans.",
        "2 seconds.",
    );
    #[test_support]
    const ORBIT_TEST_FORECAST_COMPUTE_BUDGET_MS: orbit_test_forecast_compute_budget_ms = (
        "Test-support compute budget for backlog arrivals, in milliseconds.",
        "20 seconds.",
    );
    #[test_support]
    const ORBIT_TEST_FORECAST_MAX_GENERATED: orbit_test_forecast_max_generated = (
        "Test-support cap on generated backlog items.",
        "`10000`.",
    );
    #[test_support]
    const ORBIT_TEST_HOSTS_PUBLISH_GATE: orbit_test_hosts_publish_gate = (
        "Test-support rendezvous directory for host-list publication.",
        "Unset. Publication proceeds without a gate.",
    );
    #[test_support]
    const ORBIT_TEST_HOST_FORWARD_ATTACH_TIMEOUT_MS: orbit_test_host_forward_attach_timeout_ms = (
        "Debug-build timeout in milliseconds for attaching to a remote dashboard.",
        "5 seconds.",
    );
    #[test_support]
    const ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS: orbit_test_mcp_lifecycle_interval_ms = (
        "Debug-build interval in milliseconds for stdio MCP lifecycle checks.",
        "2 seconds.",
    );
    #[test_support]
    const ORBIT_TEST_MERGEABILITY_WAIT: orbit_test_mergeability_wait = (
        "Set to `virtual` in debug builds to exercise mergeability polling without real delays.",
        "Real polling delays.",
    );
    #[test_support]
    const ORBIT_TEST_PIPELINE_WAIT_VIRTUAL: orbit_test_pipeline_wait_virtual = (
        "Set to `1` in debug builds to use a virtual clock for pipeline wait fixtures.",
        "Real clock.",
    );
    #[test_support]
    const ORBIT_TEST_RELAXED_DURABILITY: orbit_test_relaxed_durability = (
        "Set to `1` in debug builds to skip durable syncs in tests.",
        "Durable syncs enabled. Workspace Cargo config sets it to `1`.",
    );
    #[test_support]
    const ORBIT_TEST_RELEASE_API_BASE: orbit_test_release_api_base = (
        "GitHub release API base override for builds with the release API test feature.",
        "The GitHub API.",
    );
    #[test_support]
    const ORBIT_TEST_SHARED_IMAGE_DIGESTS: orbit_test_shared_image_digests = (
        "Debug-build directory for sharing executable digests among test processes.",
        "Unset. Workspace Cargo config supplies `target/test-image-digests`.",
    );
    #[test_support]
    const ORBIT_TEST_SOURCE_ELAPSED_MS: orbit_test_source_elapsed_ms = (
        "Frozen elapsed milliseconds for source inspection in test-support builds.",
        "Real elapsed time.",
    );
    const ORBIT_TOOL_AGENT_NAME: orbit_tool_agent_name = (
        "Name of the agent that called an external tool.",
        "Unset when the call has no agent.",
    );
    const ORBIT_TOOL_ALLOWED_TOOLS: orbit_tool_allowed_tools = (
        "Comma-separated tools the caller of an external tool is allowed.",
        "Unset when the list is empty.",
    );
    const ORBIT_TOOL_CWD: orbit_tool_cwd = (
        "Working directory an external tool runs in.",
        "Always set for an external tool.",
    );
    const ORBIT_TOOL_MODEL_NAME: orbit_tool_model_name = (
        "Name of the model that called an external tool.",
        "Unset when the call has no model.",
    );
    const ORBIT_TOOL_NAME: orbit_tool_name = (
        "Name of the external tool being run.",
        "Always set for an external tool.",
    );
    const ORBIT_TOOL_PROC_ALLOWED_PROGRAMS: orbit_tool_proc_allowed_programs = (
        "Comma-separated programs the caller of an external tool may start with `proc.spawn`.",
        "Unset when the list is empty.",
    );
    const ORBIT_TOOL_WORKSPACE_ROOT: orbit_tool_workspace_root = (
        "Workspace root of the call that runs an external tool.",
        "Unset when the call has no workspace root.",
    );
    const ORBIT_UPDATE_PROBE_TIMEOUT_SECS: orbit_update_probe_timeout_secs = (
        "Timeout in seconds for version and contract probes during an update.",
        "30 seconds.",
    );
    const ORBIT_UPDATE_RELEASE_DIR: orbit_update_release_dir = (
        "Local release mirror directory used in preference to the network.",
        "GitHub Releases.",
    );
    const ORBIT_UPGRADE_QUIESCE_SECS: orbit_upgrade_quiesce_secs = (
        "Time in seconds for live participants to yield during a breaking upgrade.",
        "120 seconds.",
    );
    const ORBIT_V2_CATALOG_DIR: orbit_v2_catalog_dir = (
        "Legacy activity catalog directory list used when `ORBIT_ACTIVITY_DIR` is missing or not Unicode.",
        "Normal global and workspace activity catalogs.",
    );
    const ORBIT_V2_JOB_DIR: orbit_v2_job_dir = (
        "Legacy job catalog directory list used when `ORBIT_JOB_DIR` is missing or not Unicode.",
        "Normal workspace and global job catalogs.",
    );
    const ORBIT_VALIDATION_BASE: orbit_validation_base = (
        "Base revision supplied by the validation runner.",
        "Unset.",
    );
    const ORBIT_VALIDATION_GATE: orbit_validation_gate = (
        "Name or command supplied by the validation runner for the current check.",
        "Unset.",
    );
    const ORBIT_VALIDATION_SELECTION: orbit_validation_selection = (
        "Selection that a validation command must test exactly, so a base rerun tests what the candidate run tested.",
        "Unset. The command chooses its own selection.",
    );
    const ORBIT_VALIDATION_SUMMARY: orbit_validation_summary = (
        "Path at which validation writes its structured summary.",
        "Unset.",
    );
    const ORBIT_WORKER_CONTAINMENT_STRICT: orbit_worker_containment_strict = (
        "Set to `1` to carry strict worker containment to pipeline descendants.",
        "The configured worker containment policy applies.",
        "`--strict-worker-containment` also enables strict containment.",
    );
    const ORBIT_WORKER_CONTEXT_REQUIRED: orbit_worker_context_required = (
        "Marks a worker that must receive the managed coordination context.",
        "Unset.",
    );
    const ORBIT_WORKSPACE: orbit_workspace = (
        "Logical workspace selector supplied by the managed dispatcher.",
        "Normal workspace resolution when no managed selector applies.",
        "An explicit `--workspace` takes precedence.",
    );
    const ORBIT_WORKSPACE_CLAIM_TOKEN: orbit_workspace_claim_token = (
        "Claim credential supplied to an admitted workspace claimant.",
        "Unset.",
    );
    const ORBIT_WORKTREE_ROOT: orbit_worktree_root = (
        "Parent directory override for managed worktrees.",
        "The workspace's managed worktree directory.",
    );
    const PATH: path = (
        "Program search path used for executable discovery and child execution.",
        "Unset if the OS or launcher does not supply it. Individual launchers may supply a fallback.",
    );
    const PATHEXT: pathext = (
        "Windows executable extensions tried when searching for programs.",
        "`.COM;.EXE;.BAT;.CMD`.",
    );
    const PLAYWRIGHT_BROWSERS_PATH: playwright_browsers_path = (
        "Directory of the headless browser kit's browsers, which Playwright reads.",
        "Unset. Set only for a worker whose host has a verified kit.",
    );
    const PLAYWRIGHT_MODULE: playwright_module = (
        "The headless browser kit's Playwright ES module, which a check imports `chromium` from.",
        "Unset. Set only for a worker whose host has a verified kit.",
    );
    const PYTHONDONTWRITEBYTECODE: pythondontwritebytecode = (
        "Python bytecode-write switch. Orbit sets `1` for a validation command that does not set it.",
        "Orbit sets `1` for validation commands.",
    );
    const RUSTUP_HOME: rustup_home = (
        "Rustup installation and toolchain cache root forwarded to plugin builds.",
        "Unset.",
    );
    const RUSTUP_TOOLCHAIN: rustup_toolchain = (
        "Rust toolchain selection forwarded to plugin builds.",
        "Unset.",
    );
    const RUST_LOG: rust_log = (
        "Tracing directives for Orbit logging.",
        "`warn` for CLI stderr. The CLI JSONL log also keeps provider supervision and MCP configuration at `info`.",
    );
    const SHELL: shell = (
        "Login shell location forwarded to host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const SSH_AUTH_SOCK: ssh_auth_sock = (
        "SSH agent socket forwarded to host-side Git children.",
        "Unset.",
    );
    const SSH_CONNECTION: ssh_connection = (
        "OpenSSH connection metadata from which Orbit extracts the caller IP.",
        "Unset.",
    );
    const SSL_CERT_FILE: ssl_cert_file = (
        "PEM CA bundle that TLS clients read. Orbit accepts it for Codex on macOS when `CODEX_CA_CERTIFICATE` is unset.",
        "Unset.",
    );
    const SUDO_GID: sudo_gid = (
        "Original group ID used by Linux host setup when running under sudo.",
        "Unset.",
    );
    const SUDO_UID: sudo_uid = (
        "Original user ID used by Linux host setup when running under sudo.",
        "Unset.",
    );
    const TERM: term = (
        "Terminal type. `dumb` disables terminal styling.",
        "Unset if the OS or launcher does not supply it.",
    );
    const TMPDIR: tmpdir = (
        "Temporary directory forwarded to host-side Git and GitHub CLI children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const TZ: tz = (
        "Timezone used for script events and routine schedule display.",
        "UTC for job scripts. Routine display consults the host timezone files and always reports the local offset.",
    );
    const USER: user = (
        "Host login name used for attribution and forwarded to host-side VCS children.",
        "Unset if the OS or launcher does not supply it.",
    );
    const USERNAME: username = (
        "Host login name supplied by Windows or the launcher.",
        "Unset if the OS or launcher does not supply it.",
    );
    const USERPROFILE: userprofile = (
        "Windows home directory used when supported home lookups cannot use `HOME`.",
        "Unset if the OS or launcher does not supply it.",
    );
    const XDG_CACHE_HOME: xdg_cache_home = (
        "XDG cache root used for Copilot's launcher cache.",
        "`~/.cache` where Orbit derives the cache root.",
    );
    const XDG_CONFIG_HOME: xdg_config_home = (
        "XDG configuration root used for provider state and forwarded to GitHub CLI.",
        "`~/.config` where Orbit derives the configuration root.",
    );
    const XDG_DATA_HOME: xdg_data_home = (
        "XDG data root used for Muse state.",
        "`~/.local/share` where Orbit derives the data root.",
    );
    const XDG_RUNTIME_DIR: xdg_runtime_dir = (
        "XDG runtime directory used to locate IPC sockets and forwarded to GitHub CLI.",
        "Unset.",
    );
}
