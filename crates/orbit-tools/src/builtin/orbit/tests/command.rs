//! [ORB-10711, ADR-0351] The self-dispatch guard on `orbit.command.exec`.
//!
//! Mirrors `orbit.workflow.ship`'s guard test: a managed run's leaf agent must
//! not reach this tool, because it could otherwise invoke the CLI to bypass
//! every other tool-specific policy. The guard reads `task_scope().run_id`
//! before the host is ever resolved, so a mock host is enough to prove it —
//! no runtime, claim, or process spawn required.
//!
//! Working-directory confinement is owned by `confine_workspace_cwd` and the
//! runtime chokepoint that calls it. The six cases below pin that helper
//! contract here, next to the tool that advertises it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::fs::cwd::confine_workspace_cwd;
use serde_json::json;
use tempfile::TempDir;

use crate::builtin::orbit::command::OrbitCommandExecTool;
use crate::{
    OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, Tool, ToolContext,
};

struct ManagedHost;

impl OrbitToolHost for ManagedHost {
    fn execute(
        &self,
        _action: OrbitBuiltinAction,
        _input: serde_json::Value,
        _agent: Option<String>,
        _model: Option<String>,
        _reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<serde_json::Value, OrbitError> {
        panic!("the guard must refuse before the host is ever reached");
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope {
            run_id: Some("jrun-managed".to_string()),
            ..OrbitTaskScope::default()
        }
    }
}

#[test]
fn managed_run_rejects_command_exec_before_host_resolution() {
    let context = ToolContext {
        orbit_host: Some(Arc::new(ManagedHost)),
        ..ToolContext::default()
    };

    let error = OrbitCommandExecTool
        .execute(
            &context,
            json!({"argv": ["git", "status"], "working_directory": "/tmp"}),
        )
        .expect_err("managed run must not execute remote commands");

    assert!(
        matches!(error, OrbitError::CapabilityDenied(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("managed runs cannot execute"));
}

#[test]
fn schema_requires_argv_and_working_directory() {
    let schema = OrbitCommandExecTool.schema();
    let required: Vec<&str> = schema
        .parameters
        .iter()
        .filter(|parameter| parameter.required)
        .map(|parameter| parameter.name.as_str())
        .collect();

    assert!(required.contains(&"argv"));
    assert!(required.contains(&"working_directory"));

    let working_directory = schema
        .parameters
        .iter()
        .find(|parameter| parameter.name == "working_directory")
        .expect("working_directory parameter");
    assert!(
        working_directory.description.contains("Absolute"),
        "{}",
        working_directory.description
    );
    assert!(
        working_directory
            .description
            .contains("inside this workspace's checkout"),
        "{}",
        working_directory.description
    );
}

fn checkout_fixture() -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("tempdir");
    let checkout = root.path().join("repo");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let checkout = checkout.canonicalize().expect("canonicalize checkout");
    (root, checkout)
}

fn confine(
    requested: &str,
    checkout: &Path,
    extra_roots: &[PathBuf],
) -> Result<PathBuf, OrbitError> {
    confine_workspace_cwd(
        "working_directory",
        requested,
        "ws_fixture",
        checkout,
        extra_roots,
    )
}

fn invalid_input(error: OrbitError) -> String {
    match error {
        OrbitError::InvalidInput(message) => message,
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[test]
fn working_directory_inside_root_is_ok() {
    let (_root, checkout) = checkout_fixture();
    let nested = checkout.join("src");
    std::fs::create_dir_all(&nested).expect("nested dir");

    let resolved =
        confine(&nested.display().to_string(), &checkout, &[]).expect("inside the checkout");
    assert_eq!(
        resolved,
        nested.canonicalize().expect("canonicalize nested")
    );
}

#[test]
fn working_directory_inside_a_linked_worktree_is_ok() {
    let (root, checkout) = checkout_fixture();
    let worktree = root.path().join("worktrees/jrun-fixture");
    std::fs::create_dir_all(&worktree).expect("linked worktree");
    let worktree = worktree.canonicalize().expect("canonicalize worktree");

    let resolved = confine(
        &worktree.display().to_string(),
        &checkout,
        &[worktree.parent().expect("worktrees dir").to_path_buf()],
    )
    .expect("inside a linked worktree");
    assert_eq!(resolved, worktree);
}

#[test]
fn working_directory_in_a_sibling_checkout_is_refused() {
    let (root, checkout) = checkout_fixture();
    let sibling = root.path().join("sibling");
    std::fs::create_dir_all(&sibling).expect("sibling checkout");

    let message = invalid_input(
        confine(&sibling.display().to_string(), &checkout, &[]).expect_err("sibling checkout"),
    );
    assert!(
        message.contains("outside workspace 'ws_fixture' checkout"),
        "{message}"
    );
    assert!(
        message.contains(&checkout.display().to_string()),
        "{message}"
    );
}

#[test]
fn working_directory_in_home_is_refused() {
    let (_root, checkout) = checkout_fixture();
    let home = orbit_common::fs::path::home_dir().expect("home directory");
    let home = if home.exists() {
        home.canonicalize().expect("canonicalize home")
    } else {
        home
    };
    assert!(
        !home.starts_with(&checkout),
        "HOME must lie outside the fixture checkout"
    );

    let message =
        invalid_input(confine(&home.display().to_string(), &checkout, &[]).expect_err("$HOME"));
    assert!(message.contains("outside workspace"), "{message}");
}

#[cfg(unix)]
#[test]
fn working_directory_symlink_that_escapes_is_refused() {
    let (root, checkout) = checkout_fixture();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let link = checkout.join("escape");
    std::os::unix::fs::symlink(&outside, &link).expect("escaping symlink");

    let message = invalid_input(
        confine(&link.display().to_string(), &checkout, &[]).expect_err("escaping symlink"),
    );
    assert!(message.contains("outside workspace"), "{message}");
}

#[test]
fn relative_working_directory_is_refused() {
    let (_root, checkout) = checkout_fixture();
    let message = invalid_input(confine("src", &checkout, &[]).expect_err("relative path"));
    assert!(message.contains("must be an absolute path"), "{message}");
    assert!(message.contains("'src'"), "{message}");
}
