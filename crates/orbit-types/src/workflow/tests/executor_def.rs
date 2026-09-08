mod sandbox_kind_platform {
    use super::super::super::executor_def::ExecutorSandboxKind;
    use crate::resource::ExecutorResource;

    #[test]
    fn sandbox_choices_round_trip_and_legacy_absence_remains_unspecified() {
        let base = "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: codex\nspec:\n  executor_type: direct_agent\n";
        for (setting, expected) in [
            ("", None),
            ("  sandbox: null\n", None),
            ("  sandbox: off\n", Some(ExecutorSandboxKind::Off)),
            (
                "  sandbox: linux-bwrap\n",
                Some(ExecutorSandboxKind::LinuxBwrap),
            ),
            (
                "  sandbox: macos-sandbox-exec\n",
                Some(ExecutorSandboxKind::MacosSandboxExec),
            ),
        ] {
            let resource: ExecutorResource = serde_yaml::from_str(&format!("{base}{setting}"))
                .expect("supported executor resource");
            assert_eq!(resource.spec.sandbox, expected);
            let yaml = serde_yaml::to_string(&resource).expect("serialize YAML");
            let decoded: ExecutorResource = serde_yaml::from_str(&yaml).expect("read YAML");
            assert_eq!(decoded, resource);
            let json = serde_json::to_string(&resource).expect("serialize JSON");
            let decoded: ExecutorResource = serde_json::from_str(&json).expect("read JSON");
            assert_eq!(decoded, resource);
            if expected == Some(ExecutorSandboxKind::Off) {
                assert_eq!(
                    serde_json::to_value(&resource).expect("JSON")["spec"]["sandbox"],
                    "off"
                );
            }
        }
        for malformed in ["false", "disabled", "[]", "{}", "42"] {
            let error = serde_yaml::from_str::<ExecutorResource>(&format!(
                "{base}  sandbox: {malformed}\n"
            ))
            .expect_err("malformed choice must not turn sandboxing off");
            assert!(error.to_string().contains("sandbox"), "{error}");
        }
    }

    #[test]
    fn target_os_matches_std_env_consts_naming() {
        assert_eq!(
            ExecutorSandboxKind::MacosSandboxExec.target_os(),
            Some("macos")
        );
        assert_eq!(ExecutorSandboxKind::LinuxBwrap.target_os(), Some("linux"));
        assert_eq!(ExecutorSandboxKind::Off.target_os(), None);
        for platform in ["macos", "linux", "windows"] {
            assert!(ExecutorSandboxKind::Off.is_available_on(platform));
        }
    }

    #[test]
    fn linux_bwrap_round_trips_as_concrete_wire_value() {
        let serialized =
            serde_json::to_string(&ExecutorSandboxKind::LinuxBwrap).expect("serialize");
        assert_eq!(serialized, "\"linux-bwrap\"");
        let parsed: ExecutorSandboxKind = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(parsed, ExecutorSandboxKind::LinuxBwrap);
    }

    #[test]
    fn is_available_on_matches_only_its_target_os() {
        assert!(ExecutorSandboxKind::MacosSandboxExec.is_available_on("macos"));
        assert!(!ExecutorSandboxKind::MacosSandboxExec.is_available_on("linux"));
        assert!(!ExecutorSandboxKind::MacosSandboxExec.is_available_on("windows"));
        assert!(ExecutorSandboxKind::LinuxBwrap.is_available_on("linux"));
        assert!(!ExecutorSandboxKind::LinuxBwrap.is_available_on("macos"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn is_available_on_current_platform_is_true_on_macos() {
        assert!(ExecutorSandboxKind::MacosSandboxExec.is_available_on("macos"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn is_available_on_current_platform_is_false_off_macos() {
        assert!(!ExecutorSandboxKind::MacosSandboxExec.is_available_on("linux"));
    }
}

mod external_variant {
    use super::super::super::executor_def::*;
    use crate::resource::ExecutorResource;

    #[test]
    fn external_executor_type_roundtrips_as_external_wire_string() {
        // enum -> wire string
        let serialized = serde_json::to_string(&ExecutorType::External).expect("serialize");
        assert_eq!(serialized, "\"external\"");

        // wire string -> enum
        let parsed: ExecutorType =
            serde_json::from_str("\"external\"").expect("deserialize external");
        assert_eq!(parsed, ExecutorType::External);

        // as_str / Display agree on the wire value
        assert_eq!(ExecutorType::External.as_str(), "external");
        assert_eq!(ExecutorType::External.to_string(), "external");
    }

    #[test]
    fn external_executor_def_roundtrips_through_resource_spec() {
        let resource: ExecutorResource = serde_yaml::from_str(
            r#"
schemaVersion: 2
kind: Executor
metadata:
  name: acme-harness
spec:
  executor_type: external
  command: acme-harness
  args:
    - run
"#,
        )
        .expect("parse executor yaml");
        let def = ExecutorDef::from_resource_spec(
            resource.metadata.name,
            resource.spec.clone(),
            resource.spec.created_at,
            resource.spec.updated_at,
        );

        assert_eq!(def.executor_type, ExecutorType::External);
        assert_eq!(def.command.as_deref(), Some("acme-harness"));

        let serialized = serde_yaml::to_string(&def).expect("serialize executor def");
        assert!(
            serialized.contains("executor_type: external"),
            "serialized external def should carry the wire type: {serialized}"
        );
    }
}

mod model_pair_override {
    use super::super::super::executor_def::*;
    use crate::resource::ExecutorResource;

    fn def_from_yaml(yaml: &str) -> ExecutorDef {
        let resource: ExecutorResource = serde_yaml::from_str(yaml).expect("parse executor yaml");
        ExecutorDef::from_resource_spec(
            resource.metadata.name,
            resource.spec.clone(),
            resource.spec.created_at,
            resource.spec.updated_at,
        )
    }

    #[test]
    fn roundtrips_model_pair_override_without_removed_models_key() {
        let def = def_from_yaml(
            r#"
schemaVersion: 2
kind: Executor
metadata:
  name: gemini
spec:
  executor_type: direct_agent
  command: gemini
  args:
    - -m
    - gemini-3.1-pro
  model_pair_override:
    strong: gemini-3.1-pro
    weak: gemini-3-flash
  model_flag: "-m"
"#,
        );

        assert_eq!(
            def.model_pair_override(),
            Some(&ModelPairOverride {
                strong: "gemini-3.1-pro".to_string(),
                weak: "gemini-3-flash".to_string(),
            })
        );
        assert_eq!(def.model_flag.as_deref(), Some("-m"));

        let serialized = serde_yaml::to_string(&def).expect("serialize executor def");
        assert!(
            serialized.contains("model_pair_override:"),
            "serialized executor def should use new key: {serialized}"
        );
        assert!(
            serialized.contains("model_flag: -m"),
            "serialized executor def should include model flag: {serialized}"
        );
        assert!(
            !serialized.contains("models:"),
            "serialized executor def should not use removed key: {serialized}"
        );
    }

    #[test]
    fn models_key_does_not_override_model_pair() {
        let def = def_from_yaml(
            r#"
schemaVersion: 2
kind: Executor
metadata:
  name: gemini
spec:
  executor_type: direct_agent
  command: gemini
  models:
    strong: gemini-3.1-pro
    weak: gemini-3-flash
"#,
        );

        assert!(
            def.model_pair_override().is_none(),
            "removed `models` key must not populate model_pair_override"
        );
    }
}
