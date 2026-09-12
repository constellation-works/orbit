use super::super::Provider;
use super::super::provider_sandbox::{
    CODEX_LEAST_RESTRICTIVE_SANDBOX, DEFAULT_PROVIDER_SANDBOX, admit_provider_sandbox_mode,
    format_provider_sandbox, is_least_restrictive_provider_sandbox,
    least_restrictive_provider_sandbox, least_restrictive_provider_sandbox_warning,
    parse_provider_sandbox_label, provider_sandbox_modes,
};

#[test]
fn codex_admits_the_three_cli_sandbox_modes_and_names_danger_full_access() {
    assert_eq!(
        provider_sandbox_modes(Provider::Codex),
        &["read-only", "workspace-write", "danger-full-access"]
    );
    assert_eq!(
        least_restrictive_provider_sandbox(Provider::Codex),
        Some(CODEX_LEAST_RESTRICTIVE_SANDBOX)
    );
    assert!(is_least_restrictive_provider_sandbox(
        Provider::Codex,
        "danger-full-access"
    ));
    assert!(!is_least_restrictive_provider_sandbox(
        Provider::Codex,
        "read-only"
    ));
}

#[test]
fn other_providers_only_admit_default_and_have_no_named_least_restrictive_mode() {
    for provider in [
        Provider::Claude,
        Provider::Gemini,
        Provider::Grok,
        Provider::Copilot,
        Provider::Cursor,
        Provider::Pi,
        Provider::Antigravity,
        Provider::Opencode,
        Provider::Ollama,
        Provider::OpenaiCompat,
    ] {
        assert_eq!(
            provider_sandbox_modes(provider),
            &[DEFAULT_PROVIDER_SANDBOX],
            "{provider}"
        );
        assert_eq!(
            least_restrictive_provider_sandbox(provider),
            None,
            "{provider}"
        );
        assert!(
            !is_least_restrictive_provider_sandbox(provider, DEFAULT_PROVIDER_SANDBOX),
            "{provider}"
        );
    }
}

#[test]
fn admit_refuses_an_unsupported_codex_or_claude_mode() {
    assert_eq!(
        admit_provider_sandbox_mode(Provider::Codex, "read-only").as_deref(),
        Ok("read-only")
    );
    let error = admit_provider_sandbox_mode(Provider::Codex, "unrestricted").expect_err("refuse");
    assert!(error.contains("unrestricted"), "{error}");
    assert!(error.contains("read-only"), "{error}");

    let error = admit_provider_sandbox_mode(Provider::Claude, "read-only").expect_err("refuse");
    assert!(error.contains("claude"), "{error}");
    assert!(error.contains("default"), "{error}");
    assert_eq!(
        admit_provider_sandbox_mode(Provider::Claude, "default").as_deref(),
        Ok("default")
    );
}

#[test]
fn labels_round_trip_and_the_warning_names_the_mode() {
    let label = format_provider_sandbox(Provider::Codex, "danger-full-access");
    assert_eq!(label, "codex:danger-full-access");
    let (provider, mode) = parse_provider_sandbox_label(&label).expect("parse");
    assert_eq!(provider, Provider::Codex);
    assert_eq!(mode, "danger-full-access");
    let warning = least_restrictive_provider_sandbox_warning(&label);
    assert!(warning.contains("codex:danger-full-access"), "{warning}");
    assert!(warning.contains("host integrations"), "{warning}");
}
