//! Tier-normalised implementer scores through the public runtime and HTTP API.

use chrono::{Duration, Utc};
use orbit_core::TaskStatus;
use orbit_core::TaskTypeDefinition;
use orbit_core::application::agents::{
    AgentQuery, AgentRole, agent_by_type_scorecard, agent_detail, agent_scorecard,
};
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{DefectSeverity, RegressionEscape, RegressionNature, TaskComplexity};
use serde_json::{Value, json};

use super::agents::{
    Owned, WS, add, append, as_f64, comment, implementation_run, regression, retag,
    sited_regression,
};
use super::support::{Fixture, isolated, json_ok};

fn stat_value(row: &Value, key: &str) -> Value {
    row["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stat| stat["key"] == key)
        .unwrap()["value"]
        .clone()
}

fn row<'a>(card: &'a Value, crew: &str) -> &'a Value {
    card["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["crew"] == crew)
        .unwrap()
}

#[test]
fn implementers_compare_actual_outcomes_with_the_same_tier_mix() {
    isolated(
        "agents_implementers::implementers_compare_actual_outcomes_with_the_same_tier_mix",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let start = now - Duration::hours(3);
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let mut low_ids = Vec::new();
            for (crew, complexity) in [
                ("hard", TaskComplexity::Hard),
                ("hard-peer", TaskComplexity::Hard),
                ("low", TaskComplexity::Low),
                ("low-peer", TaskComplexity::Low),
            ] {
                for index in 0..40 {
                    let landed = if crew.starts_with("hard") { 30 } else { 36 };
                    let status = if index < landed {
                        TaskStatus::Done
                    } else {
                        TaskStatus::Backlog
                    };
                    let id = add(
                        &fixture,
                        Owned {
                            title: "tier outcome",
                            orchestrator: Some("opus"),
                            origin: None,
                            complexity,
                            status,
                            relations: vec![],
                        },
                    );
                    implementation_run(
                        &fixture,
                        &format!("jrun-{crew}-{index}"),
                        &id,
                        crew,
                        Some("model"),
                        start,
                        now + Duration::hours(1),
                    );
                    append(
                        &fixture,
                        &id,
                        start,
                        "started",
                        None,
                        Some(TaskStatus::InProgress),
                    );
                    if index < if crew.starts_with("hard") { 2 } else { 1 } {
                        append(
                            &fixture,
                            &id,
                            now - Duration::hours(2),
                            "workflow_run_failed",
                            Some("validation failed: 3 assertions"),
                            Some(TaskStatus::Blocked),
                        );
                        append(
                            &fixture,
                            &id,
                            now - Duration::minutes(110),
                            "operator_requeued",
                            None,
                            Some(TaskStatus::Backlog),
                        );
                        implementation_run(
                            &fixture,
                            &format!("jrun-retry-{crew}-{index}"),
                            &id,
                            crew,
                            Some("model"),
                            now - Duration::minutes(100),
                            now + Duration::hours(1),
                        );
                        append(
                            &fixture,
                            &id,
                            now - Duration::minutes(100),
                            "started",
                            None,
                            Some(TaskStatus::InProgress),
                        );
                    }
                    append(
                        &fixture,
                        &id,
                        now - Duration::hours(1),
                        "status_changed",
                        None,
                        Some(status),
                    );
                    if index < if crew.starts_with("hard") { 2 } else { 1 } {
                        regression(
                            &fixture,
                            &id,
                            RegressionNature::DefectInNew,
                            RegressionEscape::NoTest,
                        );
                    }
                    let rejects = if crew.starts_with("hard") { 20 } else { 0 };
                    let verdict = if index < rejects {
                        "reject"
                    } else if index < rejects + 4 {
                        "accept_with_fixes"
                    } else {
                        "accept"
                    };
                    comment(
                        &fixture,
                        &id,
                        &format!(
                            "before-PR review settled attempt `rv-{crew}-{index}`: verdict **{verdict}**\n- Reviewer: crew `reviewer` (codex / review-model)"
                        ),
                    );
                    if crew == "low" {
                        low_ids.push(id);
                    }
                }
            }
            let facts = fixture.runtime.agent_scan().unwrap();
            let card = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            assert_eq!(
                as_f64(&row(&card, "hard")["raw"]),
                as_f64(&row(&card, "low")["raw"]),
                "matching each tier's fleet rates must erase tier-mix bias"
            );
            for crew in ["hard", "hard-peer", "low", "low-peer"] {
                let r = row(&card, crew);
                assert_eq!(
                    as_f64(&r["counts"]["weighted_landed"]),
                    if crew.starts_with("hard") { 30.0 } else { 36.0 },
                    "landings count one per task at every complexity"
                );
                assert_eq!(
                    as_f64(&r["raw"]),
                    100.0,
                    "a fleet-average row starts at 100"
                );
                assert!(as_f64(&r["counts"]["crew_rework"]) > 0.0);
                assert!(as_f64(&r["counts"]["escaped_regressions"]) > 0.0);
                for term in card["terms"].as_array().unwrap() {
                    let key = term["key"].as_str().unwrap();
                    assert_eq!(
                        as_f64(&r["expected"][key]),
                        as_f64(&r["counts"][key]),
                        "all terms use the same tier population when every task was reviewed"
                    );
                }
            }
            // Add twenty rejects to low: twenty vs the fleet's ten per forty.
            for (index, id) in low_ids.iter().take(20).enumerate() {
                comment(
                    &fixture,
                    id,
                    &format!(
                        "before-landing review settled attempt `extra-{index}`: verdict **reject**\n- Reviewer: crew `reviewer` (codex / review-model)"
                    ),
                );
            }
            let next = fixture.runtime.agent_scan().unwrap();
            let card = agent_scorecard(
                &next,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            assert!(
                as_f64(&row(&card, "hard")["score"]) > as_f64(&row(&card, "low")["score"]),
                "fleet-average hard work outranks excessive low-tier rejects"
            );
            let detail = agent_detail(
                &next,
                AgentRole::Implementers,
                &query,
                "low",
                Some("low-model"),
                std::time::Duration::ZERO,
            )
            .unwrap();
            let sum: f64 = detail["terms"]
                .as_array()
                .unwrap()
                .iter()
                .map(|term| as_f64(&term["contribution"]))
                .sum();
            assert!(
                (100.0 + sum - as_f64(&detail["row"]["raw"])).abs() < 0.01,
                "detail contributions reconcile with the scored row"
            );
            let server = fixture.server(false);
            let http = json_ok(server.get(&format!(
                "/api/agents/implementers/detail?{WS}&crew=low&model=low-model&min_support=1"
            )));
            assert_eq!(http["row"], detail["row"]);
            assert_eq!(as_f64(&http["row"]["expected"]["review_rejects"]), 10.0);
            // Extra implementer verdicts cannot change the owners' score formula.
            for role in [AgentRole::Reviewers, AgentRole::Pilots] {
                let other = agent_scorecard(&next, role, &query, std::time::Duration::ZERO);
                for r in other["rows"].as_array().unwrap() {
                    let raw: f64 = other["terms"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|t| {
                            as_f64(&t["weight"]) * as_f64(&r["per_100"][t["key"].as_str().unwrap()])
                        })
                        .sum();
                    assert!(
                        (raw - as_f64(&r["raw"])).abs() < 0.02,
                        "other roles retain their uncentred score formula"
                    );
                    assert!(r.get("expected").is_none());
                }
            }
        },
    );
}

#[test]
fn implementers_pool_sparse_tiers_without_amplifying_single_task_outliers() {
    isolated(
        "agents_implementers::implementers_pool_sparse_tiers_without_amplifying_single_task_outliers",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let query = AgentQuery::parse(Some("30d"), None).unwrap();
            let empty = fixture
                .runtime
                .agent_role_scorecard(AgentRole::Implementers, &query)
                .unwrap();
            assert_eq!(empty["rows"], json!([]));
            for (crew, complexity, tasks) in [
                ("dense", TaskComplexity::Low, 40),
                ("sparse", TaskComplexity::XHard, 1),
                ("unknown", TaskComplexity::Unassessed, 1),
            ] {
                for index in 0..tasks {
                    let id = add(
                        &fixture,
                        Owned {
                            title: "sparse outcome",
                            orchestrator: None,
                            origin: None,
                            complexity,
                            status: TaskStatus::Done,
                            relations: vec![],
                        },
                    );
                    let start = now - Duration::hours(2);
                    implementation_run(
                        &fixture,
                        &format!("jrun-{crew}-{index}"),
                        &id,
                        crew,
                        (crew != "unknown").then_some("model"),
                        start,
                        now + Duration::hours(1),
                    );
                    append(
                        &fixture,
                        &id,
                        start,
                        "started",
                        None,
                        Some(TaskStatus::InProgress),
                    );
                    append(
                        &fixture,
                        &id,
                        now - Duration::hours(1),
                        "status_changed",
                        None,
                        Some(TaskStatus::Done),
                    );
                    if crew == "dense" {
                        // The reference must contain measured clean reviews;
                        // unreviewed work is outside the review population.
                        comment(
                            &fixture,
                            &id,
                            &format!(
                                "before-PR review settled attempt `rv-{crew}-{index}`: verdict **accept**\n- Reviewer: crew `reviewer` (codex / review-model)"
                            ),
                        );
                    } else {
                        for review in 0..5 {
                            comment(
                                &fixture,
                                &id,
                                &format!(
                                    "before-PR review settled attempt `rv-{crew}-{review}`: verdict **reject**\n- Reviewer: crew `reviewer` (codex / review-model)"
                                ),
                            );
                        }
                    }
                }
            }
            let card = fixture
                .runtime
                .agent_role_scorecard(AgentRole::Implementers, &query)
                .unwrap();
            for crew in ["sparse", "unknown"] {
                let r = row(&card, crew);
                let expectation = as_f64(&r["expected"]["review_rejects"]);
                assert!(
                    expectation > 0.0 && expectation < 1.0,
                    "rare-tier outliers share the largest tier's denominator"
                );
                assert!(as_f64(&r["raw"]).is_finite());
                assert_eq!(r["counts"]["weighted_landed"], 1);
                assert_eq!(r["below_support"], true);
                assert_eq!(r["rank"], Value::Null);
            }
            assert_eq!(row(&card, "unknown")["model"], "model unknown");
            // One reviewed delivery is below the review terms' minimum
            // support: the rejects are counted and rated, but score nothing,
            // so the dense row's clean reviews are no longer offset.
            for crew in ["sparse", "unknown"] {
                assert_eq!(
                    row(&card, crew)["provisional_terms"],
                    json!(["first_pass_accepts", "review_rejects", "reviewer_repairs"])
                );
            }
            assert!(
                row(&card, "dense")["provisional_terms"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        },
    );
}

#[test]
fn by_type_cells_classify_n_and_shrink_empty_cells_to_the_crew_mean() {
    isolated(
        "agents_implementers::by_type_cells_classify_n_and_shrink_empty_cells_to_the_crew_mean",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let start = now - Duration::hours(3);
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            for (crew, tagged) in [("alpha", true), ("beta", false)] {
                for index in 0..3 {
                    let id = fixture
                        .runtime
                        .add_task(TaskAddParams {
                            title: "by-type fixture".into(),
                            description: "by-type fixture".into(),
                            complexity: TaskComplexity::Low,
                            tags: if tagged {
                                vec!["audit-work".to_string()]
                            } else {
                                Vec::new()
                            },
                            ..Default::default()
                        })
                        .unwrap()
                        .id
                        .to_string();
                    implementation_run(
                        &fixture,
                        &format!("jrun-{crew}-{index}"),
                        &id,
                        crew,
                        Some("model"),
                        start,
                        now + Duration::hours(1),
                    );
                    append(
                        &fixture,
                        &id,
                        start,
                        "started",
                        None,
                        Some(TaskStatus::InProgress),
                    );
                }
            }
            let types = vec![TaskTypeDefinition {
                name: "review".into(),
                tags: vec!["audit-work".into()],
                paths: Vec::new(),
            }];
            let facts = fixture.runtime.agent_scan().unwrap();
            let card = agent_by_type_scorecard(&facts, &query, &types, std::time::Duration::ZERO);
            assert_eq!(card["types"][0]["name"], "review", "{card}");
            let cells = card["cells"].as_array().unwrap();
            let cell = |crew: &str, type_name: &str| {
                cells
                    .iter()
                    .find(|cell| cell["crew"] == crew && cell["type"] == type_name)
                    .unwrap_or_else(|| panic!("cell {crew}/{type_name} in {card}"))
            };
            assert_eq!(cell("alpha", "review")["n"], 3);
            assert_eq!(cell("alpha", "other")["n"], 0);
            assert_eq!(cell("beta", "other")["n"], 3);
            assert_eq!(cell("beta", "review")["n"], 0);
            // An empty cell scores exactly its crew mean.
            for crew in ["alpha", "beta"] {
                let empty = if crew == "alpha" {
                    cell(crew, "other")
                } else {
                    cell(crew, "review")
                };
                assert_eq!(empty["score"], empty["crew_mean"], "{empty}");
                assert!(as_f64(&empty["score"]).is_finite());
            }
            // With no configured types every task is `other`.
            let bare = agent_by_type_scorecard(&facts, &query, &[], std::time::Duration::ZERO);
            assert_eq!(bare["types"].as_array().unwrap().len(), 0);
            assert!(
                bare["cells"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|cell| cell["type"] == "other"),
                "{bare}"
            );
        },
    );
}

/// One landed task for `crew` with `tags`, its attempt three hours ago.
fn landed_with_tags(fixture: &Fixture, run: &str, crew: &str, tags: Vec<String>) -> String {
    let now = Utc::now();
    let start = now - Duration::hours(3);
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "no-diff exclusion fixture".into(),
            description: "no-diff exclusion fixture".into(),
            complexity: TaskComplexity::Low,
            status: Some(TaskStatus::Done),
            tags,
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    implementation_run(
        fixture,
        run,
        &id,
        crew,
        Some("model"),
        start,
        now - Duration::hours(1),
    );
    append(
        fixture,
        &id,
        start,
        "started",
        None,
        Some(TaskStatus::InProgress),
    );
    append(
        fixture,
        &id,
        now - Duration::hours(1),
        "status_changed",
        None,
        Some(TaskStatus::Done),
    );
    id
}

#[test]
fn implementers_leave_no_diff_expected_work_out_but_count_untagged_no_diff_outcomes() {
    isolated(
        "agents_implementers::implementers_leave_no_diff_expected_work_out_but_count_untagged_no_diff_outcomes",
        || {
            use orbit_core::application::agents::Owner;

            let fixture = Fixture::new();
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            // Two ordinary landed tasks: the baseline row.
            for index in 0..2 {
                let id =
                    landed_with_tags(&fixture, &format!("jrun-base-{index}"), "solo", Vec::new());
                comment(
                    &fixture,
                    &id,
                    &format!(
                        "before-PR review settled attempt `rv-base-{index}`: verdict **accept**\n- Reviewer: crew `reviewer` (codex / review-model)"
                    ),
                );
            }
            let facts = fixture.runtime.agent_scan().unwrap();
            let before = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            let base = row(&before, "solo").clone();
            assert_eq!(base["units"], 2, "{before}");
            assert_eq!(base["counts"]["weighted_landed"], 2);

            // A task filed as no-diff-expected, landed with a reject and an
            // escaped regression: none of it is implementer work.
            let tagged = landed_with_tags(
                &fixture,
                "jrun-tagged",
                "solo",
                vec!["no-diff-expected".to_string()],
            );
            comment(
                &fixture,
                &tagged,
                "before-PR review settled attempt `rv-tagged`: verdict **reject**\n- Reviewer: crew `reviewer` (codex / review-model)",
            );
            regression(
                &fixture,
                &tagged,
                RegressionNature::DefectInNew,
                RegressionEscape::NoTest,
            );
            let facts = fixture.runtime.agent_scan().unwrap();
            assert!(
                facts.charges().iter().all(|charge| charge.task_id != tagged
                    || charge.owner != Owner::Role(AgentRole::Implementers)),
                "a no-diff-expected task charges no implementer row"
            );
            assert!(
                facts
                    .credits()
                    .iter()
                    .all(|credit| credit.task_id != tagged),
                "a no-diff-expected task earns no landed credit"
            );
            let after = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            assert_eq!(
                row(&after, "solo"),
                &base,
                "adding no-diff-expected work leaves units, counts, expectations and score unchanged"
            );

            // An untagged task that ends with no diff still counts in full,
            // and so does a task retagged no-diff-expected only after its
            // attempt started.
            landed_with_tags(&fixture, "jrun-verified-no-diff", "solo", Vec::new());
            let late = landed_with_tags(&fixture, "jrun-late-tag", "solo", Vec::new());
            retag(&fixture, &late, vec!["no-diff-expected".to_string()]);
            let facts = fixture.runtime.agent_scan().unwrap();
            let card = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            let solo = row(&card, "solo");
            assert_eq!(solo["units"], 4, "{card}");
            assert_eq!(solo["counts"]["weighted_landed"], 4);

            // The by-type cells share the exclusion: a type that matches
            // only on no-diff-expected has no counted task, and the
            // late-tagged task keeps the type its attempt was admitted with.
            let types = vec![TaskTypeDefinition {
                name: "review".into(),
                tags: vec!["no-diff-expected".into()],
                paths: Vec::new(),
            }];
            let card = agent_by_type_scorecard(&facts, &query, &types, std::time::Duration::ZERO);
            let cells = card["cells"].as_array().unwrap();
            let cell = |type_name: &str| {
                cells
                    .iter()
                    .find(|cell| cell["crew"] == "solo" && cell["type"] == type_name)
                    .unwrap_or_else(|| panic!("cell {type_name} in {card}"))
            };
            assert_eq!(cell("review")["n"], 0, "{card}");
            assert_eq!(cell("other")["n"], 4, "{card}");
        },
    );
}

#[test]
fn implementers_review_terms_ignore_review_coverage() {
    isolated(
        "agents_implementers::implementers_review_terms_ignore_review_coverage",
        || {
            let fixture = Fixture::new();
            // Review terms score from 30 reviewed deliveries up, so the
            // reviewed crews are sized to clear that minimum support.
            for (crew, tasks, reviewed) in [
                ("dense", 64, 64),
                ("sparse", 64, 32),
                ("unreviewed", 8, 0),
                ("reference", 64, 64),
            ] {
                for index in 0..tasks {
                    let id = landed_with_tags(
                        &fixture,
                        &format!("jrun-{crew}-{index}"),
                        crew,
                        Vec::new(),
                    );
                    if index < reviewed {
                        let verdict = if crew == "reference" || index % 4 < 2 {
                            "accept"
                        } else if index % 4 == 2 {
                            "reject"
                        } else {
                            "accept_with_fixes"
                        };
                        comment(
                            &fixture,
                            &id,
                            &format!(
                                "before-PR review settled attempt `rv-{crew}-{index}`: verdict **{verdict}**\n- Reviewer: crew `reviewer` (codex / review-model)"
                            ),
                        );
                    }
                }
            }
            let export = fixture
                .runtime
                .agent_per_task_export(&AgentQuery::parse(Some("30d"), Some(1)).unwrap())
                .unwrap();
            for (crew, reviewed) in [("dense", 64), ("sparse", 32), ("unreviewed", 0)] {
                let count: u64 = export["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|task| task["crew"] == crew)
                    .map(|task| task["reviewed_units"].as_u64().unwrap())
                    .sum();
                assert_eq!(
                    count, reviewed,
                    "the per-task export preserves the scored review population"
                );
            }
            let server = fixture.server(false);
            let card = json_ok(server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&window=30d&min_support=1"
            )));
            let detail = |crew| {
                json_ok(server.get(&format!("/api/agents/implementers/detail?{WS}&window=30d&min_support=1&crew={crew}&model={crew}-model")))
            };
            let dense = detail("dense");
            let sparse = detail("sparse");
            let unreviewed = detail("unreviewed");
            for key in ["first_pass_accepts", "review_rejects", "reviewer_repairs"] {
                let term = |detail: &Value| {
                    detail["terms"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|term| term["key"] == key)
                        .unwrap()
                        .clone()
                };
                assert_eq!(
                    term(&dense)["contribution"],
                    term(&sparse)["contribution"],
                    "equal reviewed outcomes must have equal contributions: {key}"
                );
                assert_ne!(
                    as_f64(&term(&dense)["contribution"]),
                    0.0,
                    "the reference crew makes the coverage comparison non-neutral"
                );
                for (crew, units) in [("dense", 64), ("sparse", 32), ("unreviewed", 0)] {
                    assert_eq!(row(&card, crew)["term_units"][key], units);
                    assert_eq!(
                        card["terms"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|term| term["key"] == key)
                            .unwrap()["unit"],
                        "reviewed deliveries"
                    );
                }
                assert_eq!(
                    row(&card, "dense")["per_100"][key],
                    row(&card, "sparse")["per_100"][key]
                );
                assert_eq!(
                    as_f64(&row(&card, "dense")["expected"][key]),
                    2.0 * as_f64(&row(&card, "sparse")["expected"][key])
                );
                assert_eq!(row(&card, "unreviewed")["reviewed_units"], 0);
                assert!(
                    row(&card, "unreviewed")["per_100"][key].is_null(),
                    "no reviewed deliveries is not a measured zero"
                );
                assert!(term(&unreviewed)["per_100"].is_null());
                assert_eq!(term(&unreviewed)["units"], 0);
                assert_eq!(term(&unreviewed)["contribution"], 0);
                assert_eq!(row(&card, "unreviewed")["expected"][key], 0);
            }
            assert_eq!(row(&card, "dense")["raw"], row(&card, "sparse")["raw"]);
            assert_eq!(row(&card, "unreviewed")["raw"], 100);
            let matrix = json_ok(server.get(&format!(
                "/api/agents/implementers/by-type?{WS}&window=30d&min_support=1"
            )));
            let cell = |crew| {
                matrix["cells"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|cell| cell["crew"] == crew && cell["n"].as_u64() > Some(0))
                    .unwrap()
                    .clone()
            };
            assert_eq!(
                cell("dense")["raw"],
                cell("sparse")["raw"],
                "tier scores must also ignore review coverage"
            );
            assert_eq!(cell("unreviewed")["raw"], 100);
        },
    );
}

/// Regression: a handful of reviewed deliveries rated per 100 reviews once
/// outweighed hundreds of tasks. Below the review minimum support the three
/// review terms are provisional: counted and rated, but left out of the raw.
#[test]
fn two_reviewed_deliveries_do_not_move_the_raw_score() {
    isolated(
        "agents_implementers::two_reviewed_deliveries_do_not_move_the_raw_score",
        || {
            let fixture = Fixture::new();
            for (crew, verdicts) in [
                ("reviewed", vec![Some("reject"), Some("accept_with_fixes")]),
                ("plain", vec![None, None]),
            ] {
                for (index, verdict) in verdicts.into_iter().enumerate() {
                    let id = landed_with_tags(
                        &fixture,
                        &format!("jrun-{crew}-{index}"),
                        crew,
                        Vec::new(),
                    );
                    if let Some(verdict) = verdict {
                        comment(
                            &fixture,
                            &id,
                            &format!(
                                "before-PR review settled attempt `rv-{crew}-{index}`: verdict **{verdict}**\n- Reviewer: crew `reviewer` (codex / review-model)"
                            ),
                        );
                    }
                }
            }
            let card = fixture
                .runtime
                .agent_role_scorecard(
                    AgentRole::Implementers,
                    &AgentQuery::parse(Some("30d"), Some(1)).unwrap(),
                )
                .unwrap();
            assert_eq!(card["review_min_support"], 30);
            let reviewed = row(&card, "reviewed");
            assert_eq!(reviewed["reviewed_units"], 2);
            assert_eq!(
                reviewed["provisional_terms"],
                json!(["first_pass_accepts", "review_rejects", "reviewer_repairs"])
            );
            assert_eq!(reviewed["counts"]["review_rejects"], 1);
            assert_eq!(reviewed["counts"]["reviewer_repairs"], 1);
            assert_eq!(reviewed["per_100"]["review_rejects"], 50);
            assert_eq!(
                reviewed["raw"],
                row(&card, "plain")["raw"],
                "two reviewed deliveries leave the raw score where unreviewed work puts it"
            );
        },
    );
}

#[test]
fn a_defect_both_review_layers_report_counts_once() {
    isolated(
        "agents_implementers::a_defect_both_review_layers_report_counts_once",
        || {
            let fixture = Fixture::new();
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let both = landed_with_tags(&fixture, "jrun-both", "both", Vec::new());
            let solo = landed_with_tags(&fixture, "jrun-solo", "solo", Vec::new());
            // The pre-landing review rejected `both`'s candidate for a
            // defect in src/a.rs, recording both its range and symbol identity.
            comment(
                &fixture,
                &both,
                "before-landing review settled attempt `rv-both`: verdict **reject**\n- Reviewer: crew `reviewer` (codex / review-model)\n- `F1` [high, open] the parser drops a field\n  - Cited: src/a.rs:10-20, symbol:src/a.rs#parse:fn",
            );
            // Delivery review then files that same defect, and a second
            // one in src/b.rs, against `both`; and the same src/a.rs defect
            // against `solo`, which had no pre-landing review.
            for (culprit, site) in [
                (&both, "symbol:src/a.rs#parse:fn"),
                (&both, "file:src/b.rs"),
                (&solo, "symbol:src/a.rs#parse:fn"),
            ] {
                sited_regression(
                    &fixture,
                    culprit,
                    RegressionNature::DefectInNew,
                    RegressionEscape::NoTest,
                    DefectSeverity::Medium,
                    &[site],
                );
            }
            let card = fixture
                .runtime
                .agent_role_scorecard(AgentRole::Implementers, &query)
                .unwrap();
            let both = row(&card, "both");
            assert_eq!(both["counts"]["review_rejects"], 1, "{both}");
            assert_eq!(
                both["counts"]["escaped_regressions"], 2,
                "the src/a.rs defect counts once, as the pre-landing reject; only the src/b.rs finding adds its medium weight: {both}"
            );
            let solo = row(&card, "solo");
            assert_eq!(solo["counts"]["review_rejects"], 0, "{solo}");
            assert_eq!(
                solo["counts"]["escaped_regressions"], 2,
                "with no pre-landing review the delivery-review finding counts: {solo}"
            );
        },
    );
}

#[test]
fn same_file_regressions_require_defect_overlap_across_scorecard_consumers() {
    isolated(
        "agents_implementers::same_file_regressions_require_defect_overlap_across_scorecard_consumers",
        || {
            let fixture = Fixture::new();
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            // A line citation cannot identify an opaque symbol without source
            // mapping. Neither that uncertainty nor another symbol is overlap.
            let cases: &[(&str, &str, &[&str], i64)] = &[
                (
                    "mixed",
                    "src/a.rs:10-20",
                    &["symbol:src/a.rs#unrelated_serializer:fn"],
                    2,
                ),
                (
                    "distinct",
                    "symbol:src/a.rs#parse:fn",
                    &["symbol:src/a.rs#unrelated_serializer:fn"],
                    2,
                ),
                (
                    "same",
                    "symbol:src/a.rs#parse:fn",
                    &["symbol:src/a.rs#parse:fn"],
                    0,
                ),
                ("disjoint", "src/a.rs:10-20", &["file:src/a.rs:30-40"], 2),
                ("overlap", "src/a.rs:10-20", &["file:src/a.rs:20-30"], 0),
                ("single-line", "src/a.rs:15", &["file:src/a.rs:10-20"], 0),
                ("imprecise", "src/a.rs", &["symbol:src/a.rs#parse:fn"], 2),
                ("file", "src/a.rs:10-20", &["file:src/a.rs"], 0),
                ("directory", "src/a.rs:10-20", &["dir:src"], 0),
                ("sibling", "src/a.rs:10-20", &["dir:sr"], 2),
                ("other-file", "src/a.rs:10-20", &["file:src/b.rs"], 2),
                ("unsited", "src/a.rs:10-20", &[], 2),
            ];
            for (crew, cited, sites, _) in cases {
                let id = landed_with_tags(&fixture, &format!("jrun-{crew}"), crew, Vec::new());
                comment(
                    &fixture,
                    &id,
                    &format!(
                        "before-landing review settled attempt `rv-{crew}`: verdict **reject**\n- Reviewer: crew `reviewer` (codex / review-model)\n- `F1` [medium, open] the parser drops a field\n  - Cited: {cited}"
                    ),
                );
                sited_regression(
                    &fixture,
                    &id,
                    RegressionNature::DefectInNew,
                    RegressionEscape::NoTest,
                    DefectSeverity::Medium,
                    sites,
                );
            }
            let server = fixture.server(false);
            let card = json_ok(server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&window=30d&min_support=1"
            )));
            let matrix = json_ok(server.get(&format!(
                "/api/agents/implementers/by-type?{WS}&window=30d&min_support=1"
            )));
            let export = fixture.runtime.agent_per_task_export(&query).unwrap();
            let mean = cases.iter().map(|case| case.3 as f64).sum::<f64>() / cases.len() as f64;
            for (crew, _, _, expected) in cases {
                let headline = row(&card, crew);
                assert_eq!(headline["counts"]["review_rejects"], 1, "{crew}");
                assert_eq!(
                    headline["counts"]["escaped_regressions"], *expected,
                    "{crew}: {headline}"
                );
                let task = export["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|task| task["crew"] == *crew)
                    .unwrap();
                assert_eq!(task["counts"], headline["counts"], "{crew}: {task}");
                let cell = matrix["cells"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|cell| cell["crew"] == *crew && cell["type"] == "other")
                    .unwrap();
                // All other terms and task mixes are identical: only the
                // severity-weighted escaped-regression residual changes raw.
                let expected_raw = 100.0 - 0.5 * (*expected as f64 - mean) * 100.0;
                // The public payload rounds scores to two decimal places.
                let expected_raw = (expected_raw * 100.0).round() / 100.0;
                assert_eq!(as_f64(&cell["raw"]), expected_raw, "{crew}: {cell}");
            }
        },
    );
}

#[test]
fn delivery_review_regressions_rank_crews_without_pre_landing_review() {
    isolated(
        "agents_implementers::delivery_review_regressions_rank_crews_without_pre_landing_review",
        || {
            let fixture = Fixture::new();
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let mut ids = Vec::new();
            for crew in ["peer-a", "peer-b", "large", "missing"] {
                ids.push(landed_with_tags(
                    &fixture,
                    &format!("jrun-{crew}"),
                    crew,
                    Vec::new(),
                ));
            }
            use orbit_store::contracts::{
                DiffClassCounts, LandedDiffSource, LandedDiffStats, LandedDiffStoreBackend,
            };
            for (index, id) in ids.iter().take(3).enumerate() {
                fixture
                    .runtime
                    .sqlite_store()
                    .unwrap()
                    .record_landed_diff_stats(&LandedDiffStats {
                        workspace_id: "ws_http_fixture".into(),
                        task_id: id.clone(),
                        base_commit: "base".into(),
                        landed_commit: format!("head-{index}"),
                        landed_at: Utc::now() - Duration::minutes(10),
                        recorded_at: Utc::now(),
                        job_run_id: None,
                        source: LandedDiffSource::Landing,
                        crew: None,
                        model: None,
                        complexity: None,
                        task_type: None,
                        code: DiffClassCounts {
                            added: if index == 2 { 150 } else { 50 },
                            ..Default::default()
                        },
                        tests: DiffClassCounts {
                            deleted: 25,
                            ..Default::default()
                        },
                        other: DiffClassCounts {
                            added: 25,
                            ..Default::default()
                        },
                        reviewable: None,
                    })
                    .unwrap();
            }
            regression(
                &fixture,
                &ids[2],
                RegressionNature::DefectInNew,
                RegressionEscape::NoTest,
            );
            // No task has a pre-landing review: the delivery-review finding
            // is the only review evidence, and it must reach the score.
            let card = fixture
                .runtime
                .agent_role_scorecard(AgentRole::Implementers, &query)
                .unwrap();
            assert!(
                card["terms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|term| term["key"] == "escaped_regressions"),
                "escaped regressions are a scored implementer term: {card}"
            );
            assert!(
                as_f64(&row(&card, "large")["score"]) < as_f64(&row(&card, "peer-a")["score"]),
                "a delivery-review finding must rank an unreviewed crew below its clean peer: {card}"
            );
            assert_eq!(row(&card, "large")["counts"]["escaped_regressions"], 1);
            assert_eq!(row(&card, "peer-a")["counts"]["escaped_regressions"], 0);
            assert_eq!(stat_value(row(&card, "large"), "diff_economy"), 2);
            assert_eq!(
                stat_value(row(&card, "missing"), "diff_economy"),
                Value::Null
            );
            // Missing own size uses the known cell median; each rate excludes
            // the row being scored.
            for crew in ["peer-a", "peer-b", "large", "missing"] {
                assert_eq!(
                    as_f64(&row(&card, crew)["expected"]["escaped_regressions"]),
                    if crew == "large" { 0.0 } else { 0.33 },
                    "{crew} expects the other rows’ rate of its expected-size bucket"
                );
                assert_eq!(row(&card, crew)["escaped_regressions_fallback"], false);
            }
            for id in [&ids[0], &ids[1], &ids[3]] {
                regression(
                    &fixture,
                    id,
                    RegressionNature::DefectInNew,
                    RegressionEscape::NoTest,
                );
            }
            let facts = fixture.runtime.agent_scan().unwrap();
            let card = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            for crew in ["peer-a", "peer-b", "large", "missing"] {
                assert_eq!(
                    as_f64(&row(&card, crew)["counts"]["escaped_regressions"]),
                    as_f64(&row(&card, crew)["expected"]["escaped_regressions"]),
                    "fleet-matched expected-size buckets have no residual"
                );
                assert_eq!(
                    row(&card, crew)["score"],
                    row(&card, "peer-a")["score"],
                    "matching regression rates rank alike"
                );
            }
            let matrix = agent_by_type_scorecard(&facts, &query, &[], std::time::Duration::ZERO);
            let server = fixture.server(false);
            let http = json_ok(server.get(&format!(
                "/api/agents/implementers/by-type?{WS}&min_support=1"
            )));
            assert_eq!(
                http["cells"], matrix["cells"],
                "the HTTP tier selector uses the public cell matrix"
            );
            assert_eq!(
                server
                    .get(&format!("/api/agents/implementers/by-type?{WS}&window=bad"))
                    .status(),
                400
            );
        },
    );
}

#[test]
fn escape_expectation_uses_other_rows_when_a_bucket_has_no_peer() {
    isolated(
        "agents_implementers::escape_expectation_uses_other_rows_when_a_bucket_has_no_peer",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let id = add(
                &fixture,
                Owned {
                    title: "isolated tier",
                    orchestrator: None,
                    origin: None,
                    complexity: TaskComplexity::Hard,
                    status: TaskStatus::Done,
                    relations: vec![],
                },
            );
            let start = now - Duration::hours(3);
            implementation_run(
                &fixture,
                "jrun-isolated",
                &id,
                "isolated",
                Some("model"),
                start,
                now - Duration::hours(1),
            );
            append(
                &fixture,
                &id,
                start,
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &id,
                now - Duration::hours(1),
                "status_changed",
                None,
                Some(TaskStatus::Done),
            );
            regression(
                &fixture,
                &id,
                RegressionNature::DefectInNew,
                RegressionEscape::NoTest,
            );
            landed_with_tags(&fixture, "jrun-clean", "clean", Vec::new());
            let server = fixture.server(false);
            let card = json_ok(server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&min_support=1"
            )));
            let isolated = row(&card, "isolated");
            assert_eq!(isolated["counts"]["escaped_regressions"], 1);
            assert_eq!(isolated["expected"]["escaped_regressions"], 0);
            assert_eq!(isolated["escaped_regressions_fallback"], true);
            let detail = json_ok(server.get(&format!(
                "/api/agents/implementers/detail?{WS}&crew=isolated&model=isolated-model&min_support=1"
            )));
            let term = detail["terms"]
                .as_array()
                .unwrap()
                .iter()
                .find(|term| term["key"] == "escaped_regressions")
                .unwrap();
            assert_eq!(term["contribution"], -50);
        },
    );
}
