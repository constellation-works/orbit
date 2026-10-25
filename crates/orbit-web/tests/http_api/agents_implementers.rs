//! Tier-normalised implementer scores through the public runtime and HTTP API.

use chrono::{Duration, Utc};
use orbit_core::CrewPerformanceType;
use orbit_core::TaskStatus;
use orbit_core::application::agents::{
    AgentQuery, AgentRole, agent_by_type_scorecard, agent_detail, agent_scorecard,
};
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{RegressionEscape, RegressionNature, TaskComplexity};
use serde_json::{Value, json};

use super::agents::{
    Owned, WS, add, append, as_f64, comment, implementation_run, regression, retag,
};
use super::support::{Fixture, isolated, json_ok};

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
                ("low", TaskComplexity::Low),
                ("low-peer", TaskComplexity::Low),
            ] {
                for index in 0..40 {
                    let landed = if crew == "hard" { 30 } else { 36 };
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
                    if index < if crew == "hard" { 2 } else { 1 } {
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
                    if index < if crew == "hard" { 2 } else { 1 } {
                        regression(
                            &fixture,
                            &id,
                            RegressionNature::DefectInNew,
                            RegressionEscape::NoTest,
                        );
                    }
                    let rejects = if crew == "hard" { 20 } else { 0 };
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
            for crew in ["hard", "low", "low-peer"] {
                let r = row(&card, crew);
                assert_eq!(
                    as_f64(&r["counts"]["weighted_landed"]),
                    if crew == "hard" { 30.0 } else { 36.0 },
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
                        "all terms use the same tier-task denominator"
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
                Some("model"),
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
                "/api/agents/implementers/detail?{WS}&crew=low&model=model&min_support=1"
            )));
            assert_eq!(http["row"], detail["row"]);
            assert_eq!(as_f64(&http["row"]["expected"]["review_rejects"]), 10.0);
            // Extra implementer verdicts cannot change the owners' score formula.
            for role in [
                AgentRole::Orchestrators,
                AgentRole::Reviewers,
                AgentRole::Pilots,
            ] {
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
                    if crew != "dense" {
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
            assert_eq!(
                as_f64(&card["fleet_average"]),
                100.0,
                "pooling conserves the fleet's total expected outcomes"
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
            let types = vec![CrewPerformanceType {
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
            let types = vec![CrewPerformanceType {
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
