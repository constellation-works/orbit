//! Directory regression sites cover descendant keys introduced by other
//! sites, including paths neither culprit declared in its footprint.

use orbit_core::OrbitRuntime;
use orbit_core::TaskStatus;
use orbit_core::application::codebase::CodebaseQuery;
use orbit_core::application::task::{TaskAddParams, TaskWriteScreen};
use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    TaskRelation, TaskRelationType,
};
use serde_json::json;

#[test]
fn directory_sites_cover_site_only_descendants() {
    if !super::dispatch_admission::isolated(
        "codebase_regressions::directory_sites_cover_site_only_descendants",
    ) {
        return;
    }
    for (site, child) in [
        ("file:src/area/a.rs", "src/area/a.rs"),
        ("symbol:src/area/a.rs#run:function", "src/area/a.rs"),
        ("dir:src/area/nested", "src/area/nested/"),
    ] {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let mut culprits = Vec::new();
        for (title, sites) in [
            ("directory culprit", vec!["dir:src/area"]),
            ("child culprit", vec![site, "file:src/area-other/a.rs"]),
        ] {
            let culprit = runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "Site-only descendant fixture".into(),
                    context_files: vec!["file:src/hub.rs".into()],
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                })
                .unwrap();
            runtime
                .add_task_screened(
                    TaskAddParams {
                        title: format!("Fix {title}"),
                        description: "Site-only descendant fixture".into(),
                        relations: vec![TaskRelation {
                            relation_type: TaskRelationType::RegressionFrom,
                            target: culprit.id.clone(),
                            regression: Some(RegressionClassification {
                                nature: RegressionNature::BrokeExisting,
                                escape: RegressionEscape::NoTest,
                                severity: DefectSeverity::Medium,
                                severity_source: SeveritySource::Filer,
                                sites: sites.into_iter().map(str::to_string).collect(),
                            }),
                        }],
                        ..Default::default()
                    },
                    &TaskWriteScreen::unbound(true),
                    None,
                    None,
                )
                .unwrap();
            culprits.push(culprit.id.to_string());
        }

        for (rollup, min_support, path) in [
            ("file", 1, child),
            ("file", 2, child),
            (
                "dir",
                2,
                if site.starts_with("dir:") {
                    child
                } else {
                    "src/area/"
                },
            ),
        ] {
            let query = CodebaseQuery::parse(Some("all"), Some(min_support), Some(rollup)).unwrap();
            let analytics = runtime.codebase_analytics(&query).unwrap();
            let rows = analytics["regressions"]["rows"].as_array().unwrap();
            let row = rows
                .iter()
                .find(|row| row["path"] == path)
                .unwrap_or_else(|| {
                    panic!("{site}, {rollup}, support {min_support}: missing {path}: {analytics}")
                });
            assert_eq!(
                (&row["numerator"], &row["denominator"]),
                (&json!(2), &json!(2)),
                "{site}: directory attribution adds an undeclared culprit once: {row}"
            );
            let regressed = row["regressed_tasks"].as_array().unwrap();
            assert_eq!(regressed.len(), 2, "distinct culprits: {row}");
            for (culprit, expected_site) in culprits.iter().zip(["dir:src/area", site]) {
                let task = regressed
                    .iter()
                    .find(|task| task["task_id"] == *culprit)
                    .unwrap();
                assert_eq!(task["sites"], json!([expected_site]), "{row}");
            }
            let mut selectors: Vec<_> = row["sites"]
                .as_array()
                .unwrap()
                .iter()
                .map(|site| site["selector"].clone())
                .collect();
            selectors.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            let mut expected = vec![json!("dir:src/area"), json!(site)];
            expected.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            assert_eq!(selectors, expected, "both matching sites: {row}");
            if rollup == "file" && min_support == 1 {
                let sibling = rows
                    .iter()
                    .find(|row| row["path"] == "src/area-other/a.rs")
                    .unwrap();
                assert_eq!(
                    (&sibling["numerator"], &sibling["denominator"]),
                    (&json!(1), &json!(1)),
                    "directory containment respects path boundaries: {sibling}"
                );
            }
        }
    }
}
