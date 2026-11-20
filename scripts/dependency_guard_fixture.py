"""Scratch layout every `check-dependency-direction.sh` fixture needs.

The guard fails closed when a scan path is absent, so each fixture tree must
hold the guard's mandatory roots (empty directories and empty files, which
ripgrep scans without a match or an IO error). Two suites run the guard against
a scratch tree: `test-check-dependency-direction.py` and
`test-ci-fast-guards.py`. Both build that tree here. A new mandatory scan root
in `check-dependency-direction.sh` is then added once, in this file, instead of
in each suite, which broke hosted CI when only one was updated (ORB-25492).

Each suite keeps its own temporary root and its suite-specific files (the guard
copy, the cargo stub, the metadata document). Nothing here touches the guard:
a test that removes one of these paths still observes the production guard
failing closed. Every builder is idempotent and never overwrites an existing
file, so a case may rebuild the layout after it parked or removed a path.
"""

from pathlib import Path

CORE_APPLICATION = "crates/orbit-core/src/application"
VCS = "crates/orbit-engine/src/executor/automation/vcs"

# Directories the architecture scan walks, even in a small manifest fixture.
SCAN_DIRECTORIES = (
    f"{CORE_APPLICATION}/agents",
    f"{CORE_APPLICATION}/experiment",
    f"{CORE_APPLICATION}/contracts",
    f"{CORE_APPLICATION}/task",
    f"{CORE_APPLICATION}/job",
    f"{CORE_APPLICATION}/job/run",
    f"{CORE_APPLICATION}/automation",
    f"{CORE_APPLICATION}/auto_tasks",
    f"{CORE_APPLICATION}/review",
    f"{CORE_APPLICATION}/distributed",
    f"{CORE_APPLICATION}/pull_drain",
    f"{CORE_APPLICATION}/admission",
    f"{CORE_APPLICATION}/workspace_auto",
    f"{CORE_APPLICATION}/backlog_forecast",
    f"{CORE_APPLICATION}/codebase",
    f"{CORE_APPLICATION}/resident",
    f"{CORE_APPLICATION}/search",
    f"{CORE_APPLICATION}/sweep_filing",
    f"{CORE_APPLICATION}/notice",
    f"{CORE_APPLICATION}/evolution",
    f"{CORE_APPLICATION}/landing",
    f"{CORE_APPLICATION}/routines",
    f"{CORE_APPLICATION}/job/pipeline",
    "crates/orbit-core/src/composition",
    "crates/orbit-core/src/runtime",
    "crates/orbit-core/src/adapter",
    "crates/orbit-core/src/bootstrap",
    "crates/orbit-runtime/src",
    "crates/orbit-automation/src",
    "crates/orbit-store/src/contracts",
    "crates/orbit-store/src/driver/file",
    "crates/orbit-store/src/driver/sqlite",
    "crates/orbit-store/src/fs",
    # The vcs module directories the pr_answers and checks guards scan [ORB-25311].
    f"{VCS}/checks",
    f"{VCS}/pr",
    f"{VCS}/base",
    f"{VCS}/claim",
    f"{VCS}/pr_answers",
    # candidate_resume must not path-name pr [ORB-25493].
    f"{VCS}/candidate_resume",
    # The engine dispatch-contract leaf directory [ORB-25491].
    "crates/orbit-engine/src/context",
    # The engine activity_job children the ORB-26097 DECLARED_FAILURE_EVIDENCE_KEY
    # guard scans for a root re-import.
    "crates/orbit-engine/src/activity_job/cli_runner",
    "crates/orbit-engine/src/activity_job/job_executor",
)

# Single-file modules scanned directly. tag_instructions and agent_corpus are
# named without the .rs suffix in the upper-module list [ORB-23019].
SCAN_FILES = (
    f"{CORE_APPLICATION}/tag_instructions.rs",
    f"{CORE_APPLICATION}/agent_corpus.rs",
    # The vcs conventions guard scans these engine files directly [ORB-24097].
    f"{VCS}/conventions.rs",
    f"{VCS}/handoff.rs",
    f"{VCS}/commit/actions.rs",
    f"{VCS}/commit/summary.rs",
    f"{VCS}/commit/already_resolved.rs",
    f"{VCS}/push.rs",
    f"{VCS}/claim/published.rs",
    "crates/orbit-engine/src/context/dispatch_contract.rs",
    # The sandboxed spawn leaf the ORB-26097 executor/automation and
    # sandbox_spawn guards scan directly.
    "crates/orbit-engine/src/sandbox_spawn.rs",
)

CONFIG_FILES = (
    "aliases.rs", "seeding.rs", "config_document.rs", "plugin_enablement.rs",
    "unknown.rs", "config_io.rs", "crew_defaults.rs", "resolved/compatibility.rs",
)
CONFIG_DIRECTORIES = ("crew", "operation", "provider_limit", "layering", "registry")

# The orbit-web module-direction scan [ORB-25492].
WEB_DIRECTORIES = ("state", "lease")
WEB_FILES = (
    "why_stuck.rs", "projections.rs", "connect.rs", "host_tunnels.rs",
    "health.rs", "assets.rs", "run_failure.rs", "shutdown.rs",
    "runtime_memo.rs",
)

# The orbit-tools module-direction scan [ORB-26098]: `builtin` is the mandatory
# root (the guard fails closed without it); crate-root modules are enumerated.
TOOLS_DIRECTORIES = ("builtin",)


def _touch(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_text("")


def create_core_paths(root: Path) -> None:
    """Create the orbit-core, engine, store and runtime scan roots."""
    for relative in SCAN_DIRECTORIES:
        (root / relative).mkdir(parents=True, exist_ok=True)
    for relative in SCAN_FILES:
        _touch(root / relative)


def create_config_paths(root: Path) -> None:
    """Create the orbit-config scan roots; safe to call again after a case edits them."""
    src = root / "crates/orbit-config/src"
    for relative in CONFIG_FILES:
        _touch(src / relative)
    for relative in CONFIG_DIRECTORIES:
        (src / relative).mkdir(parents=True, exist_ok=True)


def create_web_paths(root: Path) -> None:
    """Create the orbit-web scan roots."""
    src = root / "crates/orbit-web/src"
    for relative in WEB_DIRECTORIES:
        (src / relative).mkdir(parents=True, exist_ok=True)
    for relative in WEB_FILES:
        _touch(src / relative)


def create_tools_paths(root: Path) -> None:
    """Create the orbit-tools scan roots."""
    src = root / "crates/orbit-tools/src"
    for relative in TOOLS_DIRECTORIES:
        (src / relative).mkdir(parents=True, exist_ok=True)


def create_scan_layout(root: Path) -> None:
    """Create every mandatory scan root of the guard under `root`."""
    create_core_paths(root)
    create_config_paths(root)
    create_web_paths(root)
    create_tools_paths(root)
