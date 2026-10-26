//! Section-gap drafts published by a follower must reach a repair, not a
//! permanently stopped owner landing (ORB-18534).
use super::*;
use orbit_engine::{HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate, PrConfig};
use orbit_types::workflow::{DeliveryConventions, PR_SECTIONS_ARTIFACT, ScreenshotSection};

struct DraftLanding<'a> {
    follower: &'a DeliveryHost,
    context: HandoffLandingContext,
    updates: Mutex<Vec<HandoffLandingUpdate>>,
}

impl orbit_engine::test_support::NullRuntimeHost for DraftLanding<'_> {
    fn pr_config(&self) -> PrConfig {
        RuntimeHost::pr_config(self.follower)
    }

    fn handoff_landing_context(&self, _: &str) -> Res<HandoffLandingContext> {
        Ok(self.context.clone())
    }

    fn get_task_history(&self, id: &str) -> Res<Vec<TaskHistoryEntry>> {
        RuntimeHost::get_task_history(self.follower, id)
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Res<()> {
        locked(&self.updates).push(update.clone());
        Ok(())
    }
}

fn hand_off<'a>(fx: &Fixture, follower: &'a DeliveryHost, spent: bool) -> DraftLanding<'a> {
    follower.ship_pr();
    let mut input = fx.claim_input();
    input["pull_request"] = json!(PR_NUMBER);
    let validated = action_ok(follower, "claim_validate", &input);
    input["candidate"] = validated["candidate"].clone();
    input["validation"] = validated["validation"].clone();
    assert_eq!(
        action_ok(follower, "claim_handoff", &input)["handed_off"],
        true
    );
    follower.set_status(TASK_ID, TaskStatus::Review);
    DraftLanding {
        follower,
        context: HandoffLandingContext {
            handoff_id: "draft-handoff".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-landing".into(),
            workspace_path: fx.repo.clone(),
            unresolved_merge_intent: None,
            owed_hosted_checks: vec!["test".into()],
            automatic_repair_spent: spent,
            candidate: follower.handoffs().last().unwrap().candidate.clone(),
        },
        updates: Mutex::default(),
    }
}

fn land(host: &DraftLanding<'_>) -> Res<Value> {
    execute_deterministic_action(
        host,
        "handoff_land",
        &json!({}),
        &json!({
            "handoff_id": "draft-handoff", "run_id": RUN_ID, "max_wait_seconds": 0,
        }),
        false,
        &HashMap::new(),
        None,
    )
}

#[test]
fn follower_section_gap_draft_returns_for_repair_then_lands() {
    isolated(
        "draft_handoff::follower_section_gap_draft_returns_for_repair_then_lands",
        |sandbox| {
            for case in ["missing", "configured", "manual"] {
                let (fx, follower) = fx_host(sandbox);
                // Keep the bare fixture remote while giving observation the
                // same repository identity the substitute forge reports.
                fs::create_dir(fx.forge.join("orbit")).unwrap();
                let remote = fx.forge.join("orbit/test.git");
                std::os::unix::fs::symlink(fx.forge.join("remote.git"), &remote).unwrap();
                fx.git(&["remote", "set-url", "origin", path_str(&remote)]);
                // A required agent section the implementer left empty is the
                // section gap. Screenshots never count as missing (ORB-18535).
                follower.set_conventions(DeliveryConventions {
                    agent_sections: vec!["Rollback".into()],
                    required_sections: vec!["Rollback".into()],
                    screenshot_section: ScreenshotSection::Off,
                    draft: case == "configured",
                    ..DeliveryConventions::default()
                });
                let opened = open_ok(&fx, &follower);
                assert_eq!(opened["draft"], true);
                assert_eq!(opened["missing_required_sections"], json!(["Rollback"]));
                assert!(
                    fx.forge_state("pr-body")
                        .unwrap()
                        .contains("Missing required section")
                );
                if case == "manual" {
                    // A human draft quoting the generated body is not an
                    // unfilled section eligible for automatic repair.
                    fx.write_forge(
                        "pr-body",
                        format!(
                            "## Human notes\n\n```markdown\n{}\n```\n",
                            fx.forge_state("pr-body").unwrap()
                        ),
                    );
                }

                let owner = hand_off(&fx, &follower, false);
                let error = land(&owner).expect_err("draft cannot merge").to_string();
                assert_eq!(follower.status(TASK_ID), TaskStatus::Review);
                assert_eq!(fx.remote_tip(BASE), fx.base_sha);
                assert!(fx.forge_state("merge-requests").is_none());
                let updates = locked(&owner.updates);
                assert_eq!(updates.len(), 1);
                assert_eq!(
                    updates[0].step,
                    HandoffLandingStep::Stop {
                        repairable: case == "missing"
                    },
                    "{error}"
                );
                if case != "missing" {
                    assert!(
                        error.contains("still a draft")
                            && error.contains(if case == "configured" {
                                "pr.draft is configured"
                            } else {
                                "no section-gap recovery evidence"
                            })
                            && error.contains("next action"),
                        "{error}"
                    );
                    continue;
                }
                assert!(
                    error.contains("Rollback") && error.contains("repair"),
                    "{error}"
                );
                assert!(updates[0].evidence.contains("Rollback"));
                drop(updates);

                // A repair that still omits the section must keep its candidate
                // for the next run, rather than become another silent stop.
                let spent = hand_off(&fx, &follower, true);
                let error = land(&spent).unwrap_err().to_string();
                assert_eq!(
                    locked(&spent.updates)[0].step,
                    HandoffLandingStep::StopForRequeue,
                    "{error}"
                );

                // The repair leaf supplies the section and reuses pr_open's
                // existing guarded body refresh / ready transition.
                follower.put_artifact(
                    TASK_ID,
                    PR_SECTIONS_ARTIFACT,
                    r#"{"sections":{"Rollback":"The change can be reverted."}}"#,
                );
                assert_eq!(open_ok(&fx, &follower)["pr_reused"], true);
                assert!(fx.forge_state("pr-draft").is_none());
                assert!(
                    !fx.forge_state("pr-body")
                        .unwrap()
                        .contains("Missing required section")
                );
                let repaired = hand_off(&fx, &follower, true);
                assert_eq!(land(&repaired).unwrap()["phase"], "landed");
                assert_eq!(fx.merge_requests(), squash(&fx.candidate));
                assert_ne!(fx.remote_tip(BASE), fx.base_sha);
                assert_eq!(
                    locked(&repaired.updates).last().unwrap().step,
                    HandoffLandingStep::Complete
                );
            }
        },
    );
}
