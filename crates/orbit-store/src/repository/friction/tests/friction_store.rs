//! Read/write behaviour of the SQLite friction store (ORB-10680).

use chrono::{TimeZone, Utc};
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_types::record::FrictionStatus;
use serde_json::json;

use super::super::queries::DECODED_RECORDS;
use super::super::{FrictionListFilter, FrictionStore, FrictionUpdateParams};
use super::support::{add_params, at, done_task, friction_store, store};

#[test]
fn add_allocates_workspace_local_monthly_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let may = Utc.with_ymd_and_hms(2026, 5, 31, 23, 59, 0).unwrap();
    let june = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

    let first = frictions
        .add(add_params(TEST_CODEX_MODEL, may, &["tooling"]))
        .expect("first add");
    let second = frictions
        .add(add_params(TEST_CODEX_MODEL, may, &["docs"]))
        .expect("second add");
    let next_month = frictions
        .add(add_params(TEST_CODEX_MODEL, june, &["build"]))
        .expect("next month add");

    assert_eq!(first.record.id, "F2026-05-001");
    assert_eq!(second.record.id, "F2026-05-002");
    assert_eq!(next_month.record.id, "F2026-06-001");
}

/// Identity is `(workspace_id, friction_id)`: two workspaces allocate the same
/// ID independently and neither can see or overwrite the other's record.
#[test]
fn identical_friction_ids_in_two_workspaces_stay_distinct() {
    let temp = tempfile::tempdir().expect("tempdir");
    let shared = store(temp.path());
    let one = FrictionStore::open(shared.clone(), "ws_one", temp.path().join("ws_one"))
        .expect("first workspace");
    let two = FrictionStore::open(shared, "ws_two", temp.path().join("ws_two"))
        .expect("second workspace");

    let mut first = add_params(TEST_CODEX_MODEL, at(4, 9), &["tooling"]);
    first.body = "First workspace report".to_string();
    let mut second = add_params("claude", at(4, 10), &["docs"]);
    second.body = "Second workspace report".to_string();

    let first = one.add(first).expect("add in ws_one");
    let second = two.add(second).expect("add in ws_two");
    assert_eq!(first.record.id, second.record.id);

    one.update(
        &first.record.id,
        FrictionUpdateParams {
            status: Some(FrictionStatus::Resolved),
            tags: None,
            title: None,
            body: None,
            resolved_by_task: None,
            updated_at: at(5, 9),
        },
    )
    .expect("resolve in ws_one");

    let isolated = two
        .show(&second.record.id)
        .expect("show in ws_two")
        .expect("record present");
    assert_eq!(isolated.record.status, FrictionStatus::Open);
    assert_eq!(isolated.record.body, "Second workspace report");
    assert_eq!(one.list(&FrictionListFilter::default()).unwrap().len(), 1);
    assert_eq!(two.list(&FrictionListFilter::default()).unwrap().len(), 1);
    assert_eq!(
        one.foreign_owners_of(&first.record.id)
            .expect("owners from one"),
        vec!["ws_two".to_string()]
    );
    assert_eq!(
        two.foreign_owners_of(&second.record.id)
            .expect("owners from two"),
        vec!["ws_one".to_string()]
    );
}

/// The bound this task exists to establish: a fixed-size page decodes exactly
/// that many rows regardless of how many records the workspace retains.
#[test]
fn a_fixed_size_page_decodes_only_the_requested_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    for index in 0..120 {
        let status = if index % 3 == 0 {
            FrictionStatus::Resolved
        } else {
            FrictionStatus::Open
        };
        let stored = frictions
            .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
            .expect("seed record");
        if status == FrictionStatus::Resolved {
            frictions
                .update(
                    &stored.record.id,
                    FrictionUpdateParams {
                        status: Some(status),
                        tags: None,
                        title: None,
                        body: None,
                        resolved_by_task: None,
                        updated_at: at(2, 0),
                    },
                )
                .expect("resolve seed");
        }
    }

    DECODED_RECORDS.with(|count| count.set(0));
    let page = frictions
        .list(&FrictionListFilter {
            status: Some(FrictionStatus::Open),
            limit: Some(10),
            ..FrictionListFilter::default()
        })
        .expect("bounded page");

    assert_eq!(page.len(), 10);
    assert_eq!(
        DECODED_RECORDS.with(|count| count.get()),
        10,
        "a 10-row page must not decode the other 110 records"
    );
    assert!(
        page.iter()
            .all(|stored| stored.record.status == FrictionStatus::Open)
    );
}

/// The same bound holds for the aggregate surface: stats reads no bodies at
/// all, so its decode count over a large corpus is zero.
#[test]
fn stats_decodes_no_records() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    for _ in 0..40 {
        frictions
            .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
            .expect("seed record");
    }

    DECODED_RECORDS.with(|count| count.set(0));
    let stats = frictions.stats(&[]).expect("stats");

    assert_eq!(stats["total"], json!(40));
    assert_eq!(DECODED_RECORDS.with(|count| count.get()), 0);
}

#[test]
fn list_applies_every_filter_and_orders_by_creation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let mut early = add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]);
    early.body = "Worker never claimed the run".to_string();
    let mut late = add_params("claude", at(9, 0), &["docs"]);
    late.body = "Docs drifted from the schema".to_string();
    let early = frictions.add(early).expect("early");
    let late = frictions.add(late).expect("late");

    let all = frictions.list(&FrictionListFilter::default()).expect("all");
    assert_eq!(
        all.iter()
            .map(|stored| stored.record.id.clone())
            .collect::<Vec<_>>(),
        vec![early.record.id.clone(), late.record.id.clone()]
    );

    let by_model = frictions
        .list(&FrictionListFilter {
            model: Some("claude".to_string()),
            ..FrictionListFilter::default()
        })
        .expect("model filter");
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_model[0].record.id, late.record.id);

    let by_tag = frictions
        .list(&FrictionListFilter {
            tag: Some("tooling".to_string()),
            ..FrictionListFilter::default()
        })
        .expect("tag filter");
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].record.id, early.record.id);

    let by_date = frictions
        .list(&FrictionListFilter {
            from: Some(at(5, 0)),
            ..FrictionListFilter::default()
        })
        .expect("date filter");
    assert_eq!(by_date.len(), 1);
    assert_eq!(by_date[0].record.id, late.record.id);

    let by_body = frictions
        .list(&FrictionListFilter {
            q: Some("never claimed".to_string()),
            ..FrictionListFilter::default()
        })
        .expect("body query");
    assert_eq!(by_body.len(), 1);
    assert_eq!(by_body[0].record.id, early.record.id);
}

#[test]
fn the_query_filter_matches_a_stored_title() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let mut add = add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]);
    add.title = Some("Queued runs never reach a worker".to_string());
    frictions.add(add).expect("add with title");

    let matched = frictions
        .list(&FrictionListFilter {
            q: Some("Never Reach".to_string()),
            ..FrictionListFilter::default()
        })
        .expect("query");

    assert_eq!(matched.len(), 1);
}

#[test]
fn list_pages_with_limit_and_offset() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let ids = (0..5)
        .map(|index| {
            frictions
                .add(add_params(TEST_CODEX_MODEL, at(1, index), &["tooling"]))
                .expect("seed")
                .record
                .id
        })
        .collect::<Vec<_>>();

    let page = frictions
        .list(&FrictionListFilter {
            limit: Some(2),
            offset: 2,
            ..FrictionListFilter::default()
        })
        .expect("page");

    assert_eq!(
        page.iter()
            .map(|stored| stored.record.id.clone())
            .collect::<Vec<_>>(),
        ids[2..4].to_vec()
    );
}

/// Titles resolve once, at write time, so the record carries its handle
/// instead of every reader re-deriving one [ORB-10590].
#[test]
fn add_stores_the_authors_title_and_derives_one_otherwise() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let mut titled = add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]);
    titled.title = Some("Queued runs never reach a worker".to_string());
    let mut derived = add_params(TEST_CODEX_MODEL, at(1, 1), &["tooling"]);
    derived.body = "## What happened\n\nThe worker exited before claiming the run.\n\n## Evidence\n\nOne log line.".to_string();

    let titled = frictions.add(titled).expect("add with title");
    let derived = frictions.add(derived).expect("add without title");

    assert_eq!(
        titled.record.title.as_deref(),
        Some("Queued runs never reach a worker")
    );
    assert_eq!(
        derived.record.title.as_deref(),
        Some("The worker exited before claiming the run.")
    );
    let reread = frictions
        .show(&titled.record.id)
        .expect("show")
        .expect("record");
    assert_eq!(reread.record.title, titled.record.title);
}

#[test]
fn update_sets_and_clears_the_stored_title() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let stored = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
        .expect("seed record");

    let retitled = frictions
        .update(
            &stored.record.id,
            FrictionUpdateParams {
                status: None,
                tags: None,
                title: Some(Some("Queued runs never reach a worker".to_string())),
                body: None,
                resolved_by_task: None,
                updated_at: at(2, 0),
            },
        )
        .expect("set title");
    assert_eq!(
        retitled.record.title.as_deref(),
        Some("Queued runs never reach a worker")
    );

    let cleared = frictions
        .update(
            &stored.record.id,
            FrictionUpdateParams {
                status: None,
                tags: None,
                title: Some(None),
                body: None,
                resolved_by_task: None,
                updated_at: at(3, 0),
            },
        )
        .expect("clear title");
    assert_eq!(cleared.record.title, None);
}

#[test]
fn resolution_records_the_resolving_task_and_reopening_clears_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let stored = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
        .expect("seed record");

    let resolved = frictions
        .resolve_by_task(&stored.record.id, "ORB-00042", at(2, 0))
        .expect("resolve by task");
    assert_eq!(resolved.record.status, FrictionStatus::Resolved);
    assert_eq!(resolved.record.resolved_at, Some(at(2, 0)));
    assert_eq!(
        resolved.record.resolved_by_task.as_deref(),
        Some("ORB-00042")
    );

    // A later resolution against a different task replaces the first one;
    // the original resolution instant is kept.
    let re_resolved = frictions
        .resolve_by_task(&stored.record.id, "ORB-00043", at(2, 12))
        .expect("re-resolve by another task");
    assert_eq!(
        re_resolved.record.resolved_by_task.as_deref(),
        Some("ORB-00043")
    );
    assert_eq!(re_resolved.record.resolved_at, Some(at(2, 0)));

    let reopened = frictions
        .update(
            &stored.record.id,
            FrictionUpdateParams {
                status: Some(FrictionStatus::Open),
                tags: None,
                title: None,
                body: None,
                resolved_by_task: None,
                updated_at: at(3, 0),
            },
        )
        .expect("reopen");
    assert_eq!(reopened.record.resolved_at, None);
    assert_eq!(reopened.record.resolved_by_task, None);

    let plain = frictions
        .resolve(&stored.record.id, at(4, 0))
        .expect("resolve");
    assert_eq!(plain.record.resolved_at, Some(at(4, 0)));
    assert_eq!(plain.record.resolved_by_task, None);
}

/// Task completion claims a resolution only when nobody has resolved the
/// record yet; an explicit `resolve_by_task` still replaces the task.
#[test]
fn auto_resolution_yields_to_an_existing_resolution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let stored = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
        .expect("seed record");

    let first = frictions
        .auto_resolve_by_task(&stored.record.id, "ORB-00001", at(2, 0))
        .expect("auto-resolve open record")
        .expect("record exists");
    assert_eq!(first.record.status, FrictionStatus::Resolved);
    assert_eq!(first.record.resolved_by_task.as_deref(), Some("ORB-00001"));

    let second = frictions
        .auto_resolve_by_task(&stored.record.id, "ORB-00002", at(3, 0))
        .expect("auto-resolve resolved record")
        .expect("record exists");
    assert_eq!(second.record.resolved_by_task.as_deref(), Some("ORB-00001"));
    assert_eq!(second.record.resolved_at, Some(at(2, 0)));

    assert!(
        frictions
            .auto_resolve_by_task("F2026-01-999", "ORB-00003", at(3, 0))
            .expect("missing record is not an error")
            .is_none()
    );
}

/// The taxonomy stayed a file; record persistence moving does not move it.
#[test]
fn tag_validation_uses_the_taxonomy_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    let error = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["surprise-tag"]))
        .expect_err("unknown tag fails");
    assert!(error.to_string().contains("valid tags"), "{error}");

    std::fs::write(
        temp.path().join("ws_one").join("tags.yaml"),
        "surprise-tag: allowed\n",
    )
    .expect("rewrite taxonomy");

    frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["surprise-tag"]))
        .expect("new taxonomy tag succeeds");
    let tags = frictions.tags().expect("tags");
    assert!(tags.contains(&"surprise-tag".to_string()), "{tags:?}");
    assert!(tags.contains(&"tooling".to_string()), "{tags:?}");
}

#[test]
fn a_tag_absent_from_defaults_and_the_workspace_file_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    let error = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["invented-tag"]))
        .expect_err("invented tag must stay rejected");
    let message = error.to_string();
    assert!(
        message.contains("unknown friction tag(s): invented-tag"),
        "{message}"
    );
}

#[test]
fn add_accepts_later_defaults_on_a_pre_default_taxonomy_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let files_root = temp.path().join("ws_one");
    std::fs::create_dir_all(&files_root).expect("files root");
    std::fs::write(
        files_root.join("tags.yaml"),
        "build: \"make/fmt/lint friction\"\ndocs: docs\nlifecycle: lifecycle\n\
         naming: naming\nother: fallback\npolicy: policy\n\
         skill-guidance: skill\ntooling: tools\n",
    )
    .expect("stale taxonomy");

    let frictions = friction_store(temp.path(), "ws_one");
    let stored = frictions
        .add(add_params(
            TEST_CODEX_MODEL,
            at(1, 0),
            &["automation", "build"],
        ))
        .expect("later default tags must be accepted after merge");
    assert_eq!(
        stored.record.tags,
        vec!["automation".to_string(), "build".to_string()]
    );
}

#[test]
fn empty_and_whitespace_tags_default_to_other() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    let empty = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &[]))
        .expect("empty tags");
    assert_eq!(empty.record.tags, vec!["other".to_string()]);

    let whitespace = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 1), &["  ", "\t"]))
        .expect("whitespace tags");
    assert_eq!(whitespace.record.tags, vec!["other".to_string()]);
}

#[test]
fn stats_report_status_counts_and_family_rates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    let open = frictions
        .add(add_params("grok", at(1, 0), &["tooling"]))
        .expect("open record");
    let triaged = frictions
        .add(add_params("grok", at(1, 1), &["tooling"]))
        .expect("triaged record");
    frictions
        .update(
            &triaged.record.id,
            FrictionUpdateParams {
                status: Some(FrictionStatus::Triaged),
                tags: None,
                title: None,
                body: None,
                resolved_by_task: None,
                updated_at: at(2, 0),
            },
        )
        .expect("triage");
    frictions
        .resolve(&open.record.id, Utc::now())
        .expect("resolve now");

    let stats = frictions.stats(&[done_task("T1", "codex")]).expect("stats");

    assert_eq!(stats["total"], json!(2));
    assert_eq!(stats["open"], json!(0));
    assert_eq!(stats["triaged"], json!(1));
    assert_eq!(stats["resolved"], json!(1));
    assert_eq!(stats["resolved_this_month"], json!(1));
    assert_eq!(
        stats["by_family"]["grok"]["frictions_per_10_tasks"],
        json!("n/a")
    );
    assert_eq!(
        stats["by_family"]["codex"]["frictions_per_10_tasks"],
        json!(0.0)
    );
    assert_eq!(stats["by_tag"]["tooling"]["grok"]["frictions"], json!(2));
}

#[test]
fn stats_render_zero_rows_for_known_families_without_records() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    let stats = frictions.stats(&[]).expect("stats");

    assert_eq!(stats["by_family"]["grok"]["frictions"], json!(0));
    assert_eq!(stats["by_family"]["grok"]["tasks_done"], json!(0));
    assert_eq!(
        stats["by_family"]["grok"]["frictions_per_10_tasks"],
        json!("n/a")
    );
}

/// The scoreboard's per-family counter comes from a windowed SQL aggregate,
/// not a corpus scan.
#[test]
fn reported_by_model_windows_in_sql() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");
    frictions
        .add(add_params("codex", at(1, 0), &["tooling"]))
        .expect("old record");
    frictions
        .add(add_params("codex", at(20, 0), &["tooling"]))
        .expect("recent record");
    frictions
        .add(add_params("claude", at(20, 1), &["tooling"]))
        .expect("other model");

    let lifetime = frictions.reported_by_model(None).expect("lifetime");
    assert_eq!(lifetime.len(), 2);
    assert_eq!(
        lifetime
            .iter()
            .find(|entry| entry.model == "codex")
            .map(|entry| entry.count),
        Some(2)
    );

    let windowed = frictions
        .reported_by_model(Some(at(15, 0)))
        .expect("windowed");
    assert_eq!(
        windowed
            .iter()
            .find(|entry| entry.model == "codex")
            .map(|entry| entry.count),
        Some(1)
    );
}

/// ADR-0345: a record written after cutover reports no backing file rather
/// than a path nothing could open.
#[test]
fn a_record_written_after_cutover_has_no_legacy_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    let stored = frictions
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
        .expect("add");

    assert_eq!(stored.path, None);
    assert_eq!(
        frictions
            .show(&stored.record.id)
            .expect("show")
            .expect("record")
            .path,
        None
    );
}

#[test]
fn show_rejects_a_malformed_id_and_reports_a_missing_record() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    assert!(frictions.show("not-an-id").is_err());
    assert!(
        frictions
            .show("F2026-05-001")
            .expect("well-formed id")
            .is_none()
    );
}
