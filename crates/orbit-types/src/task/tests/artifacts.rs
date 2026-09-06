mod relative_paths {
    use chrono::{TimeZone, Utc};

    use crate::task::artifacts::*;

    #[test]
    fn artifact_path_validation_rejects_absolute_and_parent_paths() {
        assert!(validate_relative_artifact_path("files/result.txt").is_ok());
        assert!(validate_relative_artifact_path("/tmp/result.txt").is_err());
        assert!(validate_relative_artifact_path("../result.txt").is_err());
        assert!(validate_relative_artifact_path("files/../result.txt").is_err());
        assert!(validate_relative_artifact_path(r"files\result.txt").is_err());
        assert!(validate_relative_artifact_path("./result.txt").is_err());
        assert!(validate_relative_artifact_path("   ").is_err());
    }

    #[test]
    fn artifact_manifest_validates_file_metadata() {
        let manifest = ArtifactManifestV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            files: vec![ArtifactManifestFileV2 {
                path: "outputs/report.md".to_string(),
                blob: "files/report.md".to_string(),
                sha256: "a".repeat(64),
                media_type: "text/markdown".to_string(),
                size_bytes: 12,
                created_by: "codex:gpt-5.5".to_string(),
                created_at: Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
            }],
        };
        assert!(manifest.validate().is_ok());

        let mut invalid = manifest;
        invalid.files[0].blob = "../blob".to_string();
        assert!(invalid.validate().is_err());
    }
}

mod envelope {
    use chrono::{TimeZone, Utc};

    use crate::task::artifacts::*;
    use crate::task::{TaskPriority, TaskStatus, TaskType};

    fn valid_envelope_yaml(id: &str) -> String {
        format!(
            r#"schema_version: 1
id: {id}
title: Build the thing
status: backlog
type: feature
priority: medium
created_at: 2026-05-10T12:00:00Z
updated_at: 2026-05-10T12:00:00Z
"#
        )
    }

    fn valid_envelope(id: &str) -> TaskEnvelopeV2 {
        TaskEnvelopeV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            id: id.to_string(),
            title: "Build the thing".to_string(),
            status: TaskStatus::Backlog,
            task_type: TaskType::Feature,
            priority: TaskPriority::Medium,
            complexity: None,
            pr_status: None,
            job_run_id: None,
            crew: None,
            orchestrator: None,
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            context_files: Vec::new(),
            external_refs: Vec::new(),
            created_by: None,
            planned_by: None,
            implemented_by: None,
            created_at: Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn envelope_rejects_old_inline_document_fields() {
        let yaml = format!(
            "{}\ndescription: old inline body\n",
            valid_envelope_yaml("ORB-00001")
        );
        let error = serde_yaml::from_str::<TaskEnvelopeV2>(&yaml).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn envelope_requires_schema_version() {
        let yaml = r#"
id: ORB-00001
title: Build the thing
status: backlog
type: feature
priority: medium
created_at: 2026-05-10T12:00:00Z
updated_at: 2026-05-10T12:00:00Z
"#;
        let error = serde_yaml::from_str::<TaskEnvelopeV2>(yaml).unwrap_err();
        assert!(error.to_string().contains("schema_version"));
    }

    #[test]
    fn envelope_validate_rejects_wrong_schema_version() {
        let mut envelope = valid_envelope("ORB-00001");
        envelope.schema_version = 2;
        assert!(envelope.validate().is_err());
    }

    #[test]
    fn version_one_envelope_defaults_compatible_fields() {
        let envelope = serde_yaml::from_str::<TaskEnvelopeV2>(&valid_envelope_yaml("ORB-00001"))
            .expect("legacy v1 envelope remains readable");
        assert_eq!(envelope.schema_version, TASK_ARTIFACT_SCHEMA_VERSION);
        assert_eq!(envelope.orchestrator, None);
        assert!(envelope.required_tools.is_empty());
    }

    #[test]
    fn version_one_envelope_normalizes_required_tools_deterministically() {
        let yaml = format!(
            "{}\nrequired_tools:\n  - github.run.list\n  - github.auth.status\n  - github.run.list\n",
            valid_envelope_yaml("ORB-00001")
        );
        let envelope =
            serde_yaml::from_str::<TaskEnvelopeV2>(&yaml).expect("deserialize required tools");

        assert_eq!(
            envelope.required_tools,
            vec!["github.auth.status", "github.run.list"]
        );
        let serialized = serde_yaml::to_string(&envelope).expect("serialize required tools");
        assert!(serialized.contains("required_tools:"));
    }

    #[test]
    fn version_one_envelope_round_trips_orchestrator_without_schema_bump() {
        let mut envelope = valid_envelope("ORB-00001");
        envelope.orchestrator = Some("orchestration-crew".to_string());

        let yaml = serde_yaml::to_string(&envelope).expect("serialize envelope");
        assert!(yaml.contains("schema_version: 1"));
        assert!(yaml.contains("orchestrator: orchestration-crew"));
        assert_eq!(
            serde_yaml::from_str::<TaskEnvelopeV2>(&yaml).expect("deserialize envelope"),
            envelope
        );
    }

    #[test]
    fn jsonl_rows_validate_schema_and_required_ids() {
        let event = TaskEventRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            event_id: "EV-0001".to_string(),
            at: Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
            by: "codex:gpt-5.5".to_string(),
            event_type: "created".to_string(),
            note: None,
            from_status: None,
            to_status: Some(TaskStatus::Backlog),
        };
        assert!(event.validate().is_ok());

        let mut invalid_event = event;
        invalid_event.event_id = " ".to_string();
        assert!(invalid_event.validate().is_err());

        let comment = TaskCommentRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            comment_id: "C-0001".to_string(),
            at: Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap(),
            by: "daniel".to_string(),
            body: "Looks good.".to_string(),
        };
        assert!(comment.validate().is_ok());

        let mut invalid_comment = comment;
        invalid_comment.comment_id = String::new();
        assert!(invalid_comment.validate().is_err());
    }
}

mod ids {
    use crate::task::artifacts::*;

    #[test]
    fn validates_and_formats_prefix_and_width_agnostic_task_ids() {
        assert!(is_valid_orb_task_id("ORB-00000"));
        assert!(is_valid_orb_task_id("ORB-99999"));
        assert!(is_valid_orb_task_id("ORB-100000"));
        assert!(is_valid_orb_task_id("DE-7"));
        assert!(is_valid_orb_task_id("ALPHA-123456789"));
        assert!(!is_valid_orb_task_id("orb-00001"));
        assert!(!is_valid_orb_task_id("A-00001"));
        assert!(!is_valid_orb_task_id("ADR-0001"));
        assert_eq!(format_orb_task_id(42).unwrap(), "ORB-00042");
        assert_eq!(format_orb_task_id(100_000).unwrap(), "ORB-100000");
        assert_eq!(format_task_id("DE", 42).unwrap(), "DE-00042");
        assert_eq!(task_id_prefix("DE-00042"), Some("DE"));
        assert_eq!(parse_task_number("ORB-100000"), Some(100_000));
        assert!(validate_orb_task_id("ORB-12345").is_ok());
        assert!(validate_orb_task_id("ORB-1234").is_ok());
    }
}

mod relations {
    use crate::task::artifacts::*;

    #[test]
    fn relation_validation_rejects_duplicate_and_self_edges() {
        let duplicate = vec![
            TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: "ORB-00002".to_string(),
            },
            TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: "ORB-00002".to_string(),
            },
        ];
        assert!(validate_task_relations_for_source("ORB-00001", &duplicate, &[]).is_err());

        let self_edge = vec![TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: "ORB-00001".to_string(),
        }];
        assert!(validate_task_relations_for_source("ORB-00001", &self_edge, &[]).is_err());

        let self_cross_artifact_edge = vec![TaskRelation {
            relation_type: TaskRelationType::Produces,
            target: "ORB-00001".to_string(),
        }];
        assert!(
            validate_task_relations_for_source("ORB-00001", &self_cross_artifact_edge, &[])
                .is_err()
        );
    }

    #[test]
    fn task_relation_produces_and_resolves_yaml_round_trip() {
        let relations = vec![
            TaskRelation {
                relation_type: TaskRelationType::Produces,
                target: "F2026-05-007".to_string(),
            },
            TaskRelation {
                relation_type: TaskRelationType::Resolves,
                target: "L-0001".to_string(),
            },
        ];

        let yaml = serde_yaml::to_string(&relations).unwrap();
        assert!(yaml.contains("type: produces"));
        assert!(yaml.contains("type: resolves"));

        let decoded: Vec<TaskRelation> = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(decoded, relations);
    }

    #[test]
    fn produces_and_resolves_accept_cross_artifact_targets() {
        // Supported target families: task (ORB-), friction (FYYYY-MM-NNN), ADR (ADR-NNNN).
        for relation_type in [TaskRelationType::Produces, TaskRelationType::Resolves] {
            for target in ["ORB-00002", "F2026-05-007", "ADR-0001"] {
                let relations = vec![TaskRelation {
                    relation_type,
                    target: target.to_string(),
                }];
                assert!(
                    validate_task_relations_for_source("ORB-00001", &relations, &[]).is_ok(),
                    "{relation_type:?} should accept {target}"
                );
            }
        }
    }

    #[test]
    fn produces_and_resolves_reject_unsupported_targets() {
        // "L-0001" is a retired learning-subsystem id; it must not be treated
        // as an active relation contract even though it once was.
        for relation_type in [TaskRelationType::Produces, TaskRelationType::Resolves] {
            for target in ["L-0001", "not-an-id"] {
                let relations = vec![TaskRelation {
                    relation_type,
                    target: target.to_string(),
                }];
                assert!(
                    validate_task_relations_for_source("ORB-00001", &relations, &[]).is_err(),
                    "{relation_type:?} should reject {target}"
                );
            }
        }
    }

    #[test]
    fn legacy_relation_types_reject_non_task_targets() {
        for relation_type in [
            TaskRelationType::BlockedBy,
            TaskRelationType::ChildOf,
            TaskRelationType::SpawnedFrom,
            TaskRelationType::RegressionFrom,
            TaskRelationType::Supersedes,
            TaskRelationType::RelatedTo,
        ] {
            for target in ["F2026-05-007", "L-0001", "ADR-0001", "not-an-id"] {
                let relations = vec![TaskRelation {
                    relation_type,
                    target: target.to_string(),
                }];
                assert!(
                    validate_task_relations_for_source("ORB-00001", &relations, &[]).is_err(),
                    "{relation_type:?} should reject {target}"
                );
            }
        }
    }

    #[test]
    fn duplicate_cross_artifact_relations_are_rejected() {
        let relations = vec![
            TaskRelation {
                relation_type: TaskRelationType::Resolves,
                target: "F2026-05-007".to_string(),
            },
            TaskRelation {
                relation_type: TaskRelationType::Resolves,
                target: "F2026-05-007".to_string(),
            },
        ];
        assert!(validate_task_relations_for_source("ORB-00001", &relations, &[]).is_err());
    }

    #[test]
    fn relation_validation_rejects_blocking_and_hierarchy_cycles() {
        let existing = vec![TaskRelationEdge {
            source: "ORB-00002".to_string(),
            relation_type: TaskRelationType::BlockedBy,
            target: "ORB-00001".to_string(),
        }];
        let relations = vec![TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: "ORB-00002".to_string(),
        }];
        assert!(validate_task_relations_for_source("ORB-00001", &relations, &existing).is_err());

        let existing = vec![TaskRelationEdge {
            source: "ORB-00002".to_string(),
            relation_type: TaskRelationType::ChildOf,
            target: "ORB-00003".to_string(),
        }];
        let relations = vec![TaskRelation {
            relation_type: TaskRelationType::ChildOf,
            target: "ORB-00002".to_string(),
        }];
        assert!(validate_task_relations_for_source("ORB-00003", &relations, &existing).is_err());
    }

    #[test]
    fn relation_validation_allows_non_cyclic_related_edges() {
        let existing = vec![TaskRelationEdge {
            source: "ORB-00002".to_string(),
            relation_type: TaskRelationType::RelatedTo,
            target: "ORB-00001".to_string(),
        }];
        let relations = vec![TaskRelation {
            relation_type: TaskRelationType::RelatedTo,
            target: "ORB-00002".to_string(),
        }];
        assert!(validate_task_relations_for_source("ORB-00001", &relations, &existing).is_ok());
    }
}

mod presentation {
    use crate::task::{
        ArtifactPresentation, artifact_presentation, image_bytes_match_media_type,
        inline_safe_artifact_media_type, is_inline_image_media_type,
        normalized_artifact_media_type,
    };

    const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn png_bytes() -> Vec<u8> {
        let mut bytes = PNG_HEADER.to_vec();
        bytes.extend_from_slice(b"synthetic pixel payload");
        bytes
    }

    #[test]
    fn media_type_normalization_drops_parameters_and_case() {
        assert_eq!(
            normalized_artifact_media_type("image/PNG; charset=binary").as_deref(),
            Some("image/png")
        );
        assert_eq!(normalized_artifact_media_type("   ").as_deref(), None);
    }

    #[test]
    fn inline_allowlist_covers_raster_images_and_excludes_active_content() {
        for media_type in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            assert!(
                is_inline_image_media_type(media_type),
                "{media_type} should be inline-renderable"
            );
        }
        // Active content: both are viewable formats, both host script.
        assert_eq!(inline_safe_artifact_media_type("image/svg+xml"), None);
        assert_eq!(inline_safe_artifact_media_type("text/html"), None);
        assert!(!is_inline_image_media_type("text/plain"));
    }

    #[test]
    fn image_signatures_gate_the_declared_media_type() {
        assert!(image_bytes_match_media_type("image/png", &png_bytes()));
        assert!(image_bytes_match_media_type(
            "image/jpeg",
            &[0xFF, 0xD8, 0xFF, 0xE0]
        ));
        assert!(image_bytes_match_media_type("image/gif", b"GIF89a...."));
        assert!(image_bytes_match_media_type(
            "image/webp",
            b"RIFF\0\0\0\0WEBPVP8 "
        ));
        assert!(!image_bytes_match_media_type(
            "image/webp",
            b"RIFF\0\0\0\0AVI "
        ));
        assert!(!image_bytes_match_media_type("image/svg+xml", b"<svg/>"));
    }

    #[test]
    fn presentation_classifies_text_images_and_opaque_payloads() {
        assert_eq!(
            artifact_presentation("image/png", &png_bytes()),
            ArtifactPresentation::Image
        );
        assert_eq!(
            artifact_presentation("text/plain", b"hello"),
            ArtifactPresentation::Text
        );
        // SVG is never rendered inline, however well-formed it is.
        assert_eq!(
            artifact_presentation(
                "image/svg+xml",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
            ),
            ArtifactPresentation::Opaque
        );
        assert_eq!(
            artifact_presentation("application/octet-stream", &[0x00, 0x01]),
            ArtifactPresentation::Opaque
        );
    }

    #[test]
    fn a_declared_image_whose_bytes_do_not_match_is_not_presented_as_an_image() {
        // `payload.png` carrying markup: the extension picked the media type,
        // so only the signature check stops a renderer from trusting it.
        assert_eq!(
            artifact_presentation("image/png", b"<html><script>alert(1)</script></html>"),
            ArtifactPresentation::Opaque
        );
    }

    #[test]
    fn a_declared_text_artifact_with_invalid_utf8_is_opaque_rather_than_lossy() {
        assert_eq!(
            artifact_presentation("text/plain", &[0xFF, 0xFE, 0x00]),
            ArtifactPresentation::Opaque
        );
    }
}

mod textual_policy {
    use crate::task::{
        ArtifactPresentation, artifact_presentation, inline_safe_artifact_media_type,
        is_textual_artifact_media_type,
    };

    #[test]
    fn retrieval_treats_markdown_as_text_even_though_the_http_route_will_not_inline_it() {
        // Two different questions: "may a browser render this from a URL?" and
        // "may a caller be handed this as a string?". Markdown answers no and
        // yes, and conflating them would base64-encode the commonest artifact.
        assert_eq!(inline_safe_artifact_media_type("text/markdown"), None);
        assert!(is_textual_artifact_media_type("text/markdown"));
        assert_eq!(
            artifact_presentation("text/markdown", b"# heading\n"),
            ArtifactPresentation::Text
        );
    }

    #[test]
    fn active_content_is_excluded_from_the_textual_set() {
        for media_type in ["text/html", "image/svg+xml", "application/xhtml+xml"] {
            assert!(
                !is_textual_artifact_media_type(media_type),
                "{media_type} hosts script and must not be classified as plain text"
            );
        }
    }

    #[test]
    fn structured_text_formats_round_trip_as_text() {
        for media_type in [
            "application/json",
            "application/yaml",
            "application/toml",
            "text/csv",
            "text/plain",
        ] {
            assert!(is_textual_artifact_media_type(media_type), "{media_type}");
        }
    }
}
