# Environment variables

The fixed names Orbit reads are listed below. Managed launchers supply the
activity, session and worker variables when a run needs them. Test overrides
apply only in the builds or fixtures described in their rows.

A few lookups use names determined at runtime. The CLI executor derives
`ORBIT_V2_CLI_<PROVIDER>` by uppercasing the provider name. A nonblank value
selects the executable in place of the registered command or provider-name
fallback. Crew and service credential names come from configuration, and
environment forwarding and redaction may inspect a process environment as a
whole.

<!-- BEGIN GENERATED ENVIRONMENT REFERENCE -->
| Variable | Meaning | Default when unset | Flag relationship |
|---|---|---|---|
| `CARGO_DENY_DB_PATH` | Primary advisory database location for `scripts/cargo-deny.sh`; a non-empty value takes precedence over `ORBIT_CARGO_DENY_DB_PATH`. Orbit sets it for a sandboxed worker that may write. | If unset or empty, the wrapper uses `ORBIT_CARGO_DENY_DB_PATH`; if both are unset or empty, cargo-deny uses its own default location. | — |
| `CARGO_HOME` | Cargo home for toolchain discovery, build caches and credential paths. | The user's `~/.cargo` where Orbit derives a Cargo home. | — |
| `CARGO_MANIFEST_DIR` | Workspace location for the engine round-trip example. | The manifest directory recorded at compile time. | — |
| `CLAUDE_CONFIG_DIR` | Claude Code's configuration and state directory. | `~/.claude` where Orbit derives the directory. | — |
| `CLICOLOR_FORCE` | Terminal color preference. It cannot enable color on non-terminals or bypass `NO_COLOR` or `TERM=dumb`. | Unset. | — |
| `CODEX_CA_CERTIFICATE` | PEM CA bundle for Codex under the macOS Seatbelt sandbox. A nonempty value takes precedence over `SSL_CERT_FILE`; Orbit validates the selected path is a readable file and fails the launch otherwise. When both are unset or empty, Orbit supplies `/etc/ssl/cert.pem` through this variable. Bare macOS launches, other providers, and Linux are unaffected. | Unset. On a macOS Seatbelt Codex launch, Orbit supplies `/etc/ssl/cert.pem` when both this and `SSL_CERT_FILE` are unset or empty. | — |
| `CODEX_HOME` | Codex configuration and state directory. | `~/.codex` where Orbit derives the directory. | — |
| `COLUMNS` | Preferred terminal width in columns. | Terminal width query, or no truncation when neither supplies a width. | — |
| `CONSTELLATION_DEFAULT_PROVIDER` | Provider used to resolve the default workflow crew below explicit config. | The built-in default crew. | — |
| `COPILOT_HOME` | Copilot configuration and state directory. | `~/.copilot` where Orbit derives the directory. | — |
| `DBUS_SESSION_BUS_ADDRESS` | Session bus address forwarded to GitHub CLI for access to the login keyring. | Unset. | — |
| `GH_CONFIG_DIR` | GitHub CLI configuration directory forwarded to governed GitHub CLI processes. | GitHub CLI uses its own defaults. Quota accounting tries `XDG_CONFIG_HOME` then `HOME`. | — |
| `GH_ENTERPRISE_TOKEN` | GitHub Enterprise authentication token forwarded to GitHub CLI. | Unset. | — |
| `GH_HOST` | GitHub host for quota accounting and GitHub CLI forwarding. | `github.com` for quota accounting. GitHub CLI resolves its own host. | — |
| `GH_TOKEN` | GitHub authentication token forwarded to GitHub CLI. | Unset. | — |
| `GITHUB_ENTERPRISE_TOKEN` | Alternate GitHub Enterprise authentication token forwarded to GitHub CLI. | Unset. | — |
| `GITHUB_TOKEN` | Alternate GitHub authentication token forwarded to GitHub CLI. | Unset. | — |
| `GIT_COMMON_DIR` | Git common-directory override. Its presence makes Orbit defer worktree discovery to Git. | Unset. | — |
| `GIT_DIR` | Git directory override. Its presence makes Orbit defer worktree discovery to Git. | Unset. | — |
| `GIT_WORK_TREE` | Git working-tree override. Its presence makes Orbit defer worktree discovery to Git. | Unset. | — |
| `GOROOT` | Go installation root forwarded to plugin builds. | Unset. | — |
| `GROK_CLAUDE_MCPS_ENABLED` | Set to `0` or `false` to use native home-level Grok MCP registration instead of Claude-compatible registration. | Grok's Claude compatibility setting. | — |
| `GROK_HOME` | Grok configuration and state directory. | `~/.grok` where Orbit derives the directory. | — |
| `HOME` | Process home directory used to resolve Orbit and provider paths and compose child environments. | Unset if the OS or launcher does not supply it. Some lookups also try `USERPROFILE`. | — |
| `HOSTNAME` | Optional host attribution recorded in audit events. | Unset. | — |
| `JAVA_HOME` | Java installation root forwarded to plugin builds. | Unset. | — |
| `LANG` | Locale inherited by host-side Git and GitHub CLI children. | Unset if the OS or launcher does not supply it. | — |
| `LC_ALL` | Locale override inherited by host-side Git and GitHub CLI children. | Unset if the OS or launcher does not supply it. | — |
| `LD_LIBRARY_PATH` | Existing Linux library search path appended after browser kit libraries. | Unset. | — |
| `LLVM_PROFILE_FILE` | LLVM coverage output path template forwarded to pipeline workers. | Unset. | — |
| `LOGNAME` | Login name used as a fallback for host identity and forwarded to host-side VCS children. | Unset if the OS or launcher does not supply it. | — |
| `NO_COLOR` | A non-empty value disables terminal styling. | Unset. | — |
| `ORBIT_ACTIVITY_BUILD_BUDGET_DIR` | Managed directory for build-slot wait records used to extend the activity deadline. | Unset. | — |
| `ORBIT_ACTIVITY_DEADLINE_UNIX_MS` | Managed activity deadline as Unix epoch milliseconds. | Unset when no managed deadline is supplied. | — |
| `ORBIT_ACTIVITY_DIR` | Colon-separated activity catalog directories loaded ahead of global and workspace catalogs. | `ORBIT_V2_CATALOG_DIR`, then the normal catalog paths. | — |
| `ORBIT_ACTIVITY_FS_PROFILE` | Filesystem profile for the current managed activity. | Unset. | — |
| `ORBIT_ACTIVITY_ID` | Current activity identifier for managed attribution. | Unset. | — |
| `ORBIT_ACTIVITY_MCP_TOOLSETS` | Managed MCP toolsets advertised for this activity. | Use the flags or host configuration. | A dispatch stamp overrides `--role` and `--toolset` for MCP serve and listen. |
| `ORBIT_ACTIVITY_NAME` | Name of the managed activity used in tool-policy diagnostics. | `unnamed activity` when needed for a diagnostic. | — |
| `ORBIT_ACTIVITY_RESIDENT_CONTINUITY` | The dispatcher supplies `rebuilt` when a resident conversation is rebuilt after a failed resume. | Unset. | — |
| `ORBIT_ACTIVITY_TIMEOUT_MS` | Activity wall-clock budget in milliseconds, used to read build-slot wait records. | Unset. | — |
| `ORBIT_ACTIVITY_TOOLS` | Comma-separated callable tools stamped by the managed dispatcher. | Unset when no list is stamped. | — |
| `ORBIT_ACTIVITY_TOOLS_DENY` | Comma-separated disallowed tools used with managed deny-mode tool policy. | Unset. The dispatcher allowlist supplies the fallback policy. | — |
| `ORBIT_ACTIVITY_TOOL_POLICY` | Managed tool policy marker. `deny` selects the deny policy when its list is present. | The managed dispatcher allowlist. | — |
| `ORBIT_ACTOR` | Identity label used for attribution when supplied by the process. | Derived from the caller identity. | — |
| `ORBIT_ADOPT_PIPELINE_RUN` | Identifies the pipeline run a replacement worker adopts during handover. | Unset. | — |
| `ORBIT_AGENT_MODEL` | Model label stamped for agent attribution. | Unset. | Explicit attribution flags such as `orbit tool run --model` take precedence. |
| `ORBIT_AGENT_NAME` | Agent or provider label stamped for agent attribution. | Unset. | Explicit attribution via `orbit tool run --model` or the deprecated `--agent` takes precedence. |
| `ORBIT_APPLICATION_CREDENTIAL_FD` | Inherited descriptor for the managed application credential. | Unset. | — |
| `ORBIT_ATTENTION_WAIT_TOKEN` | Session grant for `orbit attention wait`. | Unset. The wait requires a token. | — |
| `ORBIT_BACKEND` | Retired backend selector retained for compatibility diagnostics. | Unset. | — |
| `ORBIT_BIN` | Explicit Orbit executable pinned for tools launched by an agent. | The dispatching Orbit executable. | — |
| `ORBIT_BROWSER_KIT_ENV` | Path of the headless browser kit's `env.sh`, which a check sources before it launches Chromium. | Unset. Set only for a worker whose host has a verified kit. | — |
| `ORBIT_BROWSER_KIT_SCREENSHOT` | Screenshot path that `orbit config browser-kit verify` hands to its smoke test. | Unset. Set only for the smoke test. | — |
| `ORBIT_BUILD_BUDGET` | Whether to enforce host build-slot admission, `0` or `1`. | `1`. | — |
| `ORBIT_BUILD_BUDGET_DIR` | Directory holding host build-slot state. | `~/.orbit/cache/build-budget`. | — |
| `ORBIT_BUILD_SLOTS` | Host build-slot count from 1 through 128. | The budget directory's `slots` file, or `2` when absent. | — |
| `ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES` | Capture limit in bytes per stream for a provider CLI. | `1048576`. | — |
| `ORBIT_COMMAND_EVIDENCE_DIR` | Directory in which a required validation command records command evidence. | Unset. Set only for a required validation command. | — |
| `ORBIT_COMMAND_EVIDENCE_INVOCATION` | Invocation identifier that a required validation command attaches to its command evidence. | Unset. Set only for a required validation command. | — |
| `ORBIT_DASHBOARD_HANDOVER` | Marks a replacement dashboard process so it skips opening a browser. | Unset. | Same browser behavior as `orbit web serve --no-open`. |
| `ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES` | Capture limit in bytes per stream for a supervised command. | `1048576`. | — |
| `ORBIT_EXTERNAL_TOOL_TIMEOUT_MS` | Timeout override in milliseconds for an external tool. | 15 seconds. | — |
| `ORBIT_FORMAT` | CLI output mode. Accepted values are `auto`, `table`, `plain`, `json` and `ndjson`. | `auto`. | Overridden by `--format`, legacy `--json` or command-local `--ops`. |
| `ORBIT_INSTALL_DIR` | Directory containing the managed Orbit installation. | `~/.orbit/bin`. | — |
| `ORBIT_INSTALL_REPO` | GitHub repository from which `orbit update` fetches releases. | `constellation-works/orbit`. | — |
| `ORBIT_JOB_DIR` | Colon-separated extra job catalog directories. | `ORBIT_V2_JOB_DIR`, then the normal catalog paths. | — |
| `ORBIT_LENT_GENERATION_FENCE` | Generation fence inherited by a child during an update. | Unset. | — |
| `ORBIT_LOG_PATH` | Path read by `orbit log tail` and the dashboard (`orbit-web`) via `resolve_log_path`. | The global Orbit JSONL log. | Overridden by `orbit log tail --path`. |
| `ORBIT_MANAGED_RUN_CONTEXT` | Marks a process launched by the managed dispatcher. Some consumers also require a run or session identity. | Unset. | — |
| `ORBIT_MCP_RESUME` | Resume descriptor passed to a replacement stdio MCP session during handover. | Unset. The server starts a fresh stdio session. | — |
| `ORBIT_OPERATOR` | Explicit, audited operator authority override for a local caller. | Unset. Normal caller capability rules apply. | — |
| `ORBIT_PLUGIN` | Name of the plugin whose backend is running. | Unset. | — |
| `ORBIT_PLUGIN_BROKER` | Socket path for a managed plugin broker. | Unset. | — |
| `ORBIT_PLUGIN_CALLBACK` | Retired plugin callback credential detected to reject old callback invocations. | Unset. | — |
| `ORBIT_PLUGIN_CALLBACK_FD` | Inherited descriptor for the plugin callback channel. | Unset. | — |
| `ORBIT_PROC_ALLOWED_PROGRAMS` | Comma-separated callable program names stamped for managed subprocess execution. | Unset. | — |
| `ORBIT_PROC_DISALLOWED_PROGRAMS` | Comma-separated program names disallowed by managed deny-mode subprocess policy. | Unset. | — |
| `ORBIT_PROC_PROGRAM_POLICY` | Managed subprocess policy marker. `deny` enables the disallow list. | Unset. The existing allowed-program rules apply. | — |
| `ORBIT_REGISTRY_ROOT` | Absolute registry root supplied by the dispatcher for a managed invocation. | Normal global registry resolution. | An explicit `--root` can select a different root. |
| `ORBIT_RELEASE_PUBLIC_KEY_FILE` | Retired release-key override. A release trust file cannot be configured alongside it. | Unset. | — |
| `ORBIT_RELEASE_TRUSTED_KEYS_FILE` | File containing the replacement trusted release signing key set, replacing the shipped release keys. | Shipped release keys only. | — |
| `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE` | Set to `1` to acknowledge the replacement release signing trust file. | Unset. A replacement trust file requires acknowledgement. | — |
| `ORBIT_REQUIRE_SANDBOX_EXEC` | Set to `1` to require macOS Seatbelt for applicable plugin and host checks. | Unset. | — |
| `ORBIT_ROOT` | Explicit Orbit config and state root, or checkout containing an initialized `.orbit` directory. | Normal workspace discovery and global `~/.orbit`. | Overridden by `--root`. |
| `ORBIT_RUN_ID` | Job-run identifier used for managed dispatch and audit correlation. | Unset. | — |
| `ORBIT_SCRATCH_DIR` | Worker scratch directory, `<checkout>/.orbit/tmp`, which task artifact upload accepts. | Unset. Set only for a managed worker. | — |
| `ORBIT_SESSION_ID` | Invocation or session identifier used for managed dispatch and audit correlation. | Unset. | — |
| `ORBIT_SKIP_HOST_PREREQUISITES` | Skip Linux host package and AppArmor setup during initialization. | Off. Cargo test configuration sets it to `1`. | Fallback for `orbit init --skip-host-prerequisites`. |
| `ORBIT_STEP_INDEX` | Numeric index of the managed activity step for audit correlation. | Unset. | — |
| `ORBIT_STOPPED_DESCENDANT_THRESHOLD_MS` | Time a supervised descendant must stay stopped before it can be terminated. | 10 minutes. | — |
| `ORBIT_TASK_ACTOR_KIND` | Actor kind stamped on a managed task. An `agent` value activates agent-specific policy checks. | Unset. | — |
| `ORBIT_TASK_ID` | Task identifier used for managed dispatch and audit correlation. | Unset. | — |
| `ORBIT_TEST_AGENT_JSONL_GATE` | Debug-build rendezvous for an agent JSONL write fixture. | Unset. Writes proceed without a gate. | — |
| `ORBIT_TEST_CONFIG_WRITE_GATE` | Test-support rendezvous directory for a config write fixture. | Unset. Writes proceed without a gate. | — |
| `ORBIT_TEST_ENV_SCRUBBED` | Marker that a re-executed test process already scrubbed its environment. | Unset. | — |
| `ORBIT_TEST_EVENTS_DRAIN_RECHECK_INTERVAL_MS` | Test-support interval in milliseconds for dashboard drain-phase checks. | 15 seconds. | — |
| `ORBIT_TEST_EVENTS_HOST_RESOURCES_INTERVAL_MS` | Test-support interval in milliseconds for dashboard host-resource samples. | 5 seconds. | — |
| `ORBIT_TEST_EVENTS_SCAN_INTERVAL_MS` | Test-support interval in milliseconds for dashboard workspace event scans. | 2 seconds. | — |
| `ORBIT_TEST_FORECAST_COMPUTE_BUDGET_MS` | Test-support compute budget for backlog arrivals, in milliseconds. | 20 seconds. | — |
| `ORBIT_TEST_FORECAST_MAX_GENERATED` | Test-support cap on generated backlog items. | `10000`. | — |
| `ORBIT_TEST_HOSTS_PUBLISH_GATE` | Test-support rendezvous directory for host-list publication. | Unset. Publication proceeds without a gate. | — |
| `ORBIT_TEST_HOST_FORWARD_ATTACH_TIMEOUT_MS` | Timeout in milliseconds for attaching to a remote dashboard. Honored only in a debug build with the `test-support` feature or in a unit-test binary. | 5 seconds. | — |
| `ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS` | Interval in milliseconds for stdio MCP lifecycle checks. Honored only in a debug build with the `test-support` feature or in a unit-test binary. | 2 seconds. | — |
| `ORBIT_TEST_MERGEABILITY_WAIT` | Set to `virtual` to exercise mergeability polling without real delays. Honored only in a debug build with the `test-support` feature or in a unit-test binary. | Real polling delays. A release build, or a debug build without `test-support` and outside a unit-test binary, always polls in real time. | — |
| `ORBIT_TEST_PIPELINE_WAIT_VIRTUAL` | Set to `1` to use a virtual clock for pipeline wait fixtures. Honored only in a debug build with the `test-support` feature or in a unit-test binary. | Real clock. A release build, or a debug build without `test-support` and outside a unit-test binary, always uses the real clock. | — |
| `ORBIT_TEST_RELAXED_DURABILITY` | Set to `1` in debug builds to skip durable syncs in tests. | Durable syncs enabled. Workspace Cargo config sets it to `1`. | — |
| `ORBIT_TEST_RELEASE_API_BASE` | GitHub release API base override for builds with the release API test feature. | The GitHub API. | — |
| `ORBIT_TEST_SHARED_IMAGE_DIGESTS` | Debug-build directory for sharing executable digests among test processes. | Unset. Workspace Cargo config supplies `target/test-image-digests`. | — |
| `ORBIT_TEST_SOURCE_ELAPSED_MS` | Frozen elapsed milliseconds for source inspection in test-support builds. | Real elapsed time. | — |
| `ORBIT_TOOL_AGENT_NAME` | Name of the agent that called an external tool. | Unset when the call has no agent. | — |
| `ORBIT_TOOL_ALLOWED_TOOLS` | Comma-separated tools the caller of an external tool is allowed. | Unset when the list is empty. | — |
| `ORBIT_TOOL_CWD` | Working directory an external tool runs in. | Always set for an external tool. | — |
| `ORBIT_TOOL_MODEL_NAME` | Name of the model that called an external tool. | Unset when the call has no model. | — |
| `ORBIT_TOOL_NAME` | Name of the external tool being run. | Always set for an external tool. | — |
| `ORBIT_TOOL_PROC_ALLOWED_PROGRAMS` | Comma-separated programs the caller of an external tool may start with `proc.spawn`. | Unset when the list is empty. | — |
| `ORBIT_TOOL_WORKSPACE_ROOT` | Workspace root of the call that runs an external tool. | Unset when the call has no workspace root. | — |
| `ORBIT_UPDATE_PROBE_TIMEOUT_SECS` | Timeout in seconds for version and contract probes during an update. | 30 seconds. | — |
| `ORBIT_UPDATE_RELEASE_DIR` | Local release mirror directory used in preference to the network. | GitHub Releases. | — |
| `ORBIT_UPGRADE_QUIESCE_SECS` | Time in seconds for live participants to yield during a breaking upgrade. | 120 seconds. | — |
| `ORBIT_V2_CATALOG_DIR` | Legacy activity catalog directory list used when `ORBIT_ACTIVITY_DIR` is missing or not Unicode. | Normal global and workspace activity catalogs. | — |
| `ORBIT_V2_JOB_DIR` | Legacy job catalog directory list used when `ORBIT_JOB_DIR` is missing or not Unicode. | Normal workspace and global job catalogs. | — |
| `ORBIT_VALIDATION_BASE` | Base revision supplied by the validation runner. | Unset. | — |
| `ORBIT_VALIDATION_GATE` | Name or command supplied by the validation runner for the current check. | Unset. | — |
| `ORBIT_VALIDATION_SELECTION` | Selection that a validation command must test exactly, so a base rerun tests what the candidate run tested. | Unset. The command chooses its own selection. | — |
| `ORBIT_VALIDATION_SUMMARY` | Path at which validation writes its structured summary. | Unset. | — |
| `ORBIT_WORKER_CONTAINMENT_STRICT` | Set to `1` to carry strict worker containment to pipeline descendants. | The configured worker containment policy applies. | `--strict-worker-containment` also enables strict containment. |
| `ORBIT_WORKER_CONTEXT_REQUIRED` | Marks a worker that must receive the managed coordination context. | Unset. | — |
| `ORBIT_WORKSPACE` | Logical workspace selector supplied by the managed dispatcher. | Normal workspace resolution when no managed selector applies. | An explicit `--workspace` takes precedence. |
| `ORBIT_WORKSPACE_CLAIM_TOKEN` | Claim credential supplied to an admitted workspace claimant. | Unset. | — |
| `ORBIT_WORKTREE_ROOT` | Parent directory override for managed worktrees. | The workspace's managed worktree directory. | — |
| `PATH` | Program search path used for executable discovery and child execution. | Unset if the OS or launcher does not supply it. Individual launchers may supply a fallback. | — |
| `PATHEXT` | Windows executable extensions tried when searching for programs. | `.COM;.EXE;.BAT;.CMD`. | — |
| `PLAYWRIGHT_BROWSERS_PATH` | Directory of the headless browser kit's browsers, which Playwright reads. | Unset. Orbit sets it for a worker whose host has a verified kit. Host setup also sets it for the Chromium download, and the kit's `env.sh`, which smoke verification sources, exports it whether or not the kit is verified. | — |
| `PLAYWRIGHT_MODULE` | The headless browser kit's Playwright ES module, which a check imports `chromium` from. | Unset. Orbit sets it for a worker whose host has a verified kit. The kit's `env.sh`, which host setup writes and smoke verification sources, exports it whether or not the kit is verified. | — |
| `PYTHONDONTWRITEBYTECODE` | Python bytecode-write switch. Orbit sets `1` for a validation command that does not set it. | Orbit sets `1` for validation commands. | — |
| `RUSTUP_HOME` | Rustup installation and toolchain cache root forwarded to plugin builds. | Unset. | — |
| `RUSTUP_TOOLCHAIN` | Rust toolchain selection forwarded to plugin builds. | Unset. | — |
| `RUST_LOG` | Tracing directives for Orbit logging. | `warn` for CLI stderr. The CLI JSONL log also keeps provider supervision and MCP configuration at `info`. | — |
| `SHELL` | Login shell location forwarded to host-side Git and GitHub CLI children. | Unset if the OS or launcher does not supply it. | — |
| `SSH_AUTH_SOCK` | SSH agent socket forwarded to host-side Git children. | Unset. | — |
| `SSH_CONNECTION` | OpenSSH connection metadata from which Orbit extracts the caller IP. | Unset. | — |
| `SSL_CERT_FILE` | PEM CA bundle that TLS clients read. For Codex under the macOS Seatbelt sandbox, a nonempty value applies when `CODEX_CA_CERTIFICATE` is unset or empty; Orbit validates the selected path is a readable file and fails the launch otherwise. When both are unset or empty, Orbit supplies `/etc/ssl/cert.pem` through `CODEX_CA_CERTIFICATE`. Bare macOS launches, other providers, and Linux are unaffected. | Unset. On a macOS Seatbelt Codex launch, the `/etc/ssl/cert.pem` fallback applies when both this and `CODEX_CA_CERTIFICATE` are unset or empty. | — |
| `SUDO_GID` | Original group ID used by Linux host setup when running under sudo. | Unset. | — |
| `SUDO_UID` | Original user ID used by Linux host setup when running under sudo. | Unset. | — |
| `TERM` | Terminal type. `dumb` disables terminal styling. | Unset if the OS or launcher does not supply it. | — |
| `TMPDIR` | Temporary directory forwarded to host-side Git and GitHub CLI children. | Unset if the OS or launcher does not supply it. | — |
| `TZ` | Timezone used for script events and routine schedule display. | UTC for job scripts. Routine display consults the host timezone files and always reports the local offset. | — |
| `USER` | Host login name used for attribution and forwarded to host-side VCS children. | Unset if the OS or launcher does not supply it. | — |
| `USERNAME` | Host login name supplied by Windows or the launcher. | Unset if the OS or launcher does not supply it. | — |
| `USERPROFILE` | Windows home directory used when supported home lookups cannot use `HOME`. | Unset if the OS or launcher does not supply it. | — |
| `XDG_CACHE_HOME` | XDG cache root used for Copilot's launcher cache. | `~/.cache` where Orbit derives the cache root. | — |
| `XDG_CONFIG_HOME` | XDG configuration root used for provider state and forwarded to GitHub CLI. | `~/.config` where Orbit derives the configuration root. | — |
| `XDG_DATA_HOME` | XDG data root used for Muse state. | `~/.local/share` where Orbit derives the data root. | — |
| `XDG_RUNTIME_DIR` | XDG runtime directory used to locate IPC sockets and forwarded to GitHub CLI. | Unset. | — |
<!-- END GENERATED ENVIRONMENT REFERENCE -->
