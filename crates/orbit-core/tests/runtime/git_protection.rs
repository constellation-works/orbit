//! Git protection at sandbox preparation, through the composed runtime.

#![allow(missing_docs)]
#![cfg(any(target_os = "linux", target_os = "macos"))]

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

/// Git's interrupted writes leave a temporary name beside the final one in
/// `objects/` (ORB-14928). A leaf must still start with those pairs in the
/// store, while every way a metadata inode can be reached from outside the
/// store stays a refusal at sandbox preparation.
#[test]
fn git_protection_tolerates_object_store_leftovers_and_refuses_outside_aliases() {
    if !crate::dispatch_admission::isolated(
        "git_protection::git_protection_tolerates_object_store_leftovers_and_refuses_outside_aliases",
    ) {
        return;
    }

    let sandbox = if cfg!(target_os = "linux") {
        ExecutorSandboxKind::LinuxBwrap
    } else {
        ExecutorSandboxKind::MacosSandboxExec
    };
    for (case, refusal, remedy) in [
        ("loose-object-and-temp", false, false),
        ("pack-index-and-temps", false, false),
        ("temp-across-fan-out-directories", false, false),
        ("hook-aliased-outside", true, false),
        ("config-aliased-outside", true, false),
        ("object-aliased-outside", true, false),
        ("temp-aliased-outside", true, true),
        ("temp-aliased-beside-metadata", true, true),
    ] {
        let root = tempfile::tempdir().expect("fixture roots");
        let repo_root = root
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("repo");
        let workspace = repo_root.join(".orbit");
        std::fs::create_dir_all(&workspace).expect("workspace root");
        let runtime = OrbitRuntime::from_roots(&root.path().join("home/.orbit"), &workspace)
            .expect("build runtime");
        let git = repo_root.join(".git");
        let objects = git.join("objects");
        for directory in ["61", "ab", "pack"] {
            std::fs::create_dir_all(objects.join(directory)).expect("object store");
        }
        std::fs::create_dir_all(git.join("hooks")).expect("hooks");
        let outside = repo_root.parent().unwrap().join("outside");
        std::fs::write(&outside, "host state").expect("outside file");
        let object = objects.join("61/5bfcb2ad167392037266f1f2dfec4546b37dd6");
        std::fs::write(&object, "object").expect("loose object");
        match case {
            "loose-object-and-temp" => {
                std::fs::hard_link(&object, objects.join("61/tmp_obj_Ab12Cd")).unwrap();
            }
            "pack-index-and-temps" => {
                let pack = objects.join("pack/pack-1.pack");
                let index = objects.join("pack/pack-1.idx");
                std::fs::write(&pack, "pack").unwrap();
                std::fs::write(&index, "idx").unwrap();
                std::fs::hard_link(&pack, objects.join("pack/tmp_pack_Xy98Zw")).unwrap();
                std::fs::hard_link(&index, objects.join("pack/tmp_idx_Xy98Zw")).unwrap();
            }
            "temp-across-fan-out-directories" => {
                std::fs::hard_link(&object, objects.join("ab/tmp_obj_Ab12Cd")).unwrap();
            }
            "hook-aliased-outside" => {
                std::fs::hard_link(&outside, git.join("hooks/pre-commit")).unwrap();
            }
            "config-aliased-outside" => {
                std::fs::hard_link(&outside, git.join("config")).unwrap();
            }
            "object-aliased-outside" => {
                std::fs::hard_link(&outside, objects.join("61/aliased")).unwrap();
            }
            "temp-aliased-outside" => {
                // The pair is intact, but the inode is also reachable outside.
                std::fs::hard_link(&object, objects.join("61/tmp_obj_Ab12Cd")).unwrap();
                std::fs::hard_link(&object, repo_root.join("alias")).unwrap();
            }
            "temp-aliased-beside-metadata" => {
                // Outside `objects/` the original rule applies to every name.
                std::fs::hard_link(&outside, git.join("tmp_obj_Ab12Cd")).unwrap();
            }
            _ => unreachable!(),
        }
        runtime
            .upsert_executor_def(&ExecutorDef {
                name: "claude".to_string(),
                executor_type: ExecutorType::DirectAgent,
                command: Some("claude".to_string()),
                args: vec![],
                stdout_format: None,
                model_pair_override: None,
                model_flag: None,
                timeout_seconds: None,
                auth_probe: None,
                env: Default::default(),
                sandbox: Some(sandbox),
                allow_fallback: false,
                created_at: None,
                updated_at: None,
            })
            .expect("sandboxed executor");
        let resolved = runtime.resolve_executor_sandbox("claude", None, Some(&repo_root));
        match (resolved, refusal) {
            (Ok(Some(_)), false) => {}
            (Err(error), true) => {
                let message = error.to_string();
                assert!(
                    message.contains("hard-linked metadata entry"),
                    "{case}: {message}"
                );
                assert_eq!(
                    message.contains("interrupted Git write"),
                    remedy,
                    "{case}: {message}"
                );
            }
            (other, _) => panic!("{case}: unexpected outcome {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "host state",
            "{case}"
        );
    }
}
