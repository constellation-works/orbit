use std::path::Path;

use serde_json::Value;
use tempfile::tempdir;

use crate::command::CommandOutput;
use crate::tests::env_isolation::EnvGuard;

use super::super::init::WorkspaceInitArgs;

struct IsolatedWorkspace {
    workspace: tempfile::TempDir,
    home: tempfile::TempDir,
    _env: EnvGuard,
}

impl IsolatedWorkspace {
    fn with_prefix(task_prefix: &str) -> Self {
        let workspace = tempdir().expect("workspace tempdir");
        let home = tempdir().expect("home tempdir");
        let global = home.path().join(".orbit");
        std::fs::create_dir_all(&global).expect("create global orbit");
        std::fs::write(
            global.join("host.toml"),
            format!(
                "schema_version = 2\nmachine_id = \"hm_report\"\nhost_id = \"report-host\"\ntask_prefix = \"{task_prefix}\"\n"
            ),
        )
        .expect("write host identity");
        let env = EnvGuard::acquire().home(home.path()).cwd(workspace.path());
        Self {
            workspace,
            home,
            _env: env,
        }
    }
}

fn report_args() -> WorkspaceInitArgs {
    WorkspaceInitArgs {
        name: Some("report".to_string()),
        base_branch: Some("main".to_string()),
        ship_mode: None,
        role: None,
        owner: None,
        task_id_start: None,
        mcp: false,
        inject_agent_rules: false,
        refresh_defaults: false,
        force: true,
    }
}

fn rendered_init_payload(output: CommandOutput) -> (Value, String) {
    let CommandOutput::Payload(payload) = output else {
        panic!("workspace init must return a payload, got {output:?}");
    };
    let (doc, view) = payload.into_view();
    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("workspace init must keep a human view");
    };
    let crate::output::payload::Block::Text(text) = &blocks[0] else {
        panic!("workspace init human view is prose");
    };
    let rendered = crate::output::json::render(&doc, false).expect("render json document");
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or_else(|error| {
        panic!("workspace init json must be one parseable object ({error}): {rendered}")
    });
    assert!(
        parsed.is_object(),
        "workspace init json must be one object, got {parsed}"
    );
    assert!(
        !rendered.contains("workspace '"),
        "human prose leaked into json stdout: {rendered}"
    );
    (parsed, text.clone())
}

#[test]
fn force_init_json_is_one_object_with_identity_and_explicit_mcp() {
    let isolated = IsolatedWorkspace::with_prefix("DANI");
    let output = report_args()
        .execute_without_runtime(None)
        .expect("workspace init --force");
    let (doc, text) = rendered_init_payload(output);

    assert_eq!(doc["id"], "ws_report");
    assert_eq!(doc["name"], "report");
    assert_eq!(
        Path::new(doc["root"].as_str().expect("root")),
        isolated.workspace.path()
    );
    assert_eq!(
        Path::new(doc["orbit_dir"].as_str().expect("orbit_dir")),
        isolated.workspace.path().join(".orbit")
    );
    assert_eq!(doc["mcp"]["status"], "skipped");
    assert!(doc["mcp"]["providers"].is_null());
    assert_eq!(doc["allocator"]["status"], "skipped");
    assert_eq!(doc["rules"]["status"], "skipped");

    assert!(
        text.contains("workspace 'report' initialized"),
        "human view lost workspace identification: {text}"
    );
    assert!(text.contains("id:        ws_report"), "{text}");
    assert!(
        text.contains(&format!(
            "root:      {}",
            isolated.workspace.path().display()
        )),
        "{text}"
    );
    assert!(
        text.contains("mcp:       skipped (pass --mcp to set up integrations)"),
        "{text}"
    );
    assert!(!text.contains("id_start:"), "{text}");
    assert!(!text.contains("rules:"), "{text}");
}

#[test]
fn skipped_optional_actions_are_explicit_in_the_payload() {
    let _isolated = IsolatedWorkspace::with_prefix("DANI");
    let (doc, text) = rendered_init_payload(
        report_args()
            .execute_without_runtime(None)
            .expect("workspace init"),
    );

    assert_eq!(doc["allocator"]["status"], "skipped");
    assert!(doc["allocator"]["next"].is_null());
    assert!(doc["allocator"]["changed"].is_null());
    assert_eq!(doc["mcp"]["status"], "skipped");
    assert!(doc["mcp"]["providers"].is_null());
    assert_eq!(doc["rules"]["status"], "skipped");
    assert!(doc["rules"]["outcomes"].is_null());
    assert!(text.contains("mcp:       skipped"), "{text}");
}

#[test]
fn requested_allocator_uses_the_host_task_prefix_and_records_unchanged_reseeds() {
    let _isolated = IsolatedWorkspace::with_prefix("DANI");
    let mut args = report_args();
    args.task_id_start = Some(20_000);

    let (first, first_text) = rendered_init_payload(
        args.execute_without_runtime(None)
            .expect("seed allocator on first init"),
    );
    assert_eq!(first["allocator"]["status"], "seeded");
    assert_eq!(first["allocator"]["next"], "DANI-20000");
    assert_eq!(first["allocator"]["changed"], true);
    assert!(
        first_text.contains("id_start:  allocator seeded to DANI-20000"),
        "{first_text}"
    );

    let mut reseed = report_args();
    reseed.task_id_start = Some(20_000);
    let (second, second_text) = rendered_init_payload(
        reseed
            .execute_without_runtime(None)
            .expect("re-seed matching allocator"),
    );
    assert_eq!(second["allocator"]["status"], "unchanged");
    assert_eq!(second["allocator"]["next"], "DANI-20000");
    assert_eq!(second["allocator"]["changed"], false);
    assert!(
        second_text.contains("id_start:  allocator already at DANI-20000 (unchanged)"),
        "{second_text}"
    );
}

#[test]
fn requested_mcp_records_configured_providers() {
    let isolated = IsolatedWorkspace::with_prefix("DANI");
    std::fs::create_dir_all(isolated.workspace.path().join(".claude")).expect("create .claude");
    std::fs::create_dir_all(isolated.workspace.path().join(".gemini")).expect("create .gemini");
    std::fs::create_dir_all(isolated.workspace.path().join(".grok")).expect("create .grok");
    std::fs::create_dir_all(isolated.home.path().join(".codex")).expect("create global .codex");
    std::fs::write(
        isolated.home.path().join(".codex").join("config.toml"),
        "model = \"gpt-5.4\"\n",
    )
    .expect("write global codex config");

    let mut configured = report_args();
    configured.mcp = true;
    let (doc, text) = rendered_init_payload(
        configured
            .execute_without_runtime(None)
            .expect("workspace init --mcp"),
    );
    assert_eq!(doc["mcp"]["status"], "configured");
    let providers = doc["mcp"]["providers"]
        .as_array()
        .expect("configured mcp providers");
    for expected in ["claude", "codex", "gemini", "grok"] {
        assert!(
            providers.iter().any(|provider| provider == expected),
            "missing {expected} in {providers:?}"
        );
    }
    assert!(
        text.contains("mcp:       ") && text.contains("operator-authorized"),
        "{text}"
    );
}

#[test]
fn requested_mcp_records_none_detected_when_no_providers_exist() {
    let _isolated = IsolatedWorkspace::with_prefix("DANI");
    let mut none_detected = report_args();
    none_detected.mcp = true;
    let (doc, text) = rendered_init_payload(
        none_detected
            .execute_without_runtime(None)
            .expect("workspace init --mcp with no providers"),
    );
    assert_eq!(doc["mcp"]["status"], "none_detected");
    assert_eq!(doc["mcp"]["providers"], serde_json::json!([]));
    assert!(
        text.contains("mcp:       no providers auto-detected"),
        "{text}"
    );
}

#[test]
fn requested_rule_injection_records_created_files() {
    let isolated = IsolatedWorkspace::with_prefix("DANI");
    let mut injected = report_args();
    injected.inject_agent_rules = true;
    let (doc, text) = rendered_init_payload(
        injected
            .execute_without_runtime(None)
            .expect("workspace init --inject-agent-rules"),
    );
    assert_eq!(doc["rules"]["status"], "injected");
    let outcomes = doc["rules"]["outcomes"]
        .as_array()
        .expect("injected rule outcomes");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["file"], "CLAUDE.md");
    assert_eq!(outcomes[0]["action"], "created");
    assert_eq!(outcomes[1]["file"], "AGENTS.md");
    assert_eq!(outcomes[1]["action"], "created");
    assert!(
        text.contains("rules:     CLAUDE.md: created with Orbit rules block"),
        "{text}"
    );
    assert!(
        isolated.workspace.path().join("CLAUDE.md").is_file(),
        "rule injection must write CLAUDE.md"
    );
    assert!(
        isolated.workspace.path().join("AGENTS.md").is_file(),
        "rule injection must write AGENTS.md"
    );
}

#[test]
fn payload_detail_uses_the_existing_renderer_contract() {
    let _isolated = IsolatedWorkspace::with_prefix("DANI");
    let output = report_args()
        .execute_without_runtime(None)
        .expect("workspace init");
    let CommandOutput::Payload(payload) = output else {
        panic!("global --format json has no document unless init returns Payload");
    };
    let (doc, view) = payload.into_view();
    assert!(doc.is_object(), "detail document must be one object: {doc}");
    assert!(
        matches!(view, crate::output::payload::View::Blocks(_)),
        "human form must stay on the renderer Blocks path"
    );
}
