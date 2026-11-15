// `<crew>@auto` pool members admit only with bands for their crew, only in
// the pools that resolve them at admission, and resolve through the admitted
// policy.

fn auto_effort_load(body: &str) -> Result<ResolvedConfig, String> {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    fs::write(global.path().join("config.toml"), body).expect("global config");
    ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .map_err(|error| error.to_string())
}

const AUTO_EFFORT_CREW: &str = "[crews.sol]\nmodel = \"gpt-5.5\"\nprovider = \"codex\"\n";

const AUTO_EFFORT_BANDS: &str = "[[workflow.auto_effort.bands.sol]]\neffort = \"low\"\n\
     complexity = [\"low\"]\n\n[[workflow.auto_effort.bands.sol]]\neffort = \"high\"\n\
     complexity = [\"hard\", \"xhard\"]\n";

fn auto_effort_body(workflow: &str, bands: &str) -> String {
    format!("[workflow]\ndefault_crew = \"sol\"\n{workflow}\n{bands}\n{AUTO_EFFORT_CREW}")
}

#[test]
fn auto_effort_members_admit_only_with_bands_in_resolving_pools() {
    for (workflow, case) in [
        ("low_complexity_crews = [\"sol@auto\"]", "complexity pool"),
        (
            "[workflow.purpose_crews]\ntask_pilot = [\"sol@auto:3\", \"sol:1\"]",
            "task-pilot pool",
        ),
        (
            "[workflow.purpose_crews]\nreview = [\"sol@auto\"]",
            "review pool",
        ),
    ] {
        auto_effort_load(&auto_effort_body(workflow, AUTO_EFFORT_BANDS))
            .unwrap_or_else(|error| panic!("{case} with bands must admit: {error}"));
        let error = auto_effort_load(&auto_effort_body(workflow, ""))
            .expect_err("an @auto member without bands has nothing to resolve from");
        assert!(
            error.contains("sol@auto") && error.contains("workflow.auto_effort.bands"),
            "{case}: {error}"
        );
    }
    for (workflow, case) in [
        (
            "[workflow.purpose_crews]\nauto_fix = [\"sol@auto\"]",
            "auto-fix pool",
        ),
        (
            "[workflow.purpose_crews]\nfinal_recovery = [\"sol@auto\"]",
            "final recovery pool",
        ),
        ("default_crew = \"sol@auto\"", "default crew"),
    ] {
        let body = if workflow.starts_with("default_crew") {
            format!("[workflow]\n{workflow}\n{AUTO_EFFORT_BANDS}\n{AUTO_EFFORT_CREW}")
        } else {
            auto_effort_body(workflow, AUTO_EFFORT_BANDS)
        };
        let error =
            auto_effort_load(&body).expect_err("only pools resolved at admission may name @auto");
        assert!(error.contains("@auto"), "{case}: {error}");
    }
}

#[test]
fn auto_effort_refuses_unusable_bands_and_shares() {
    for (bands, (case, reason)) in [
        (
            "[[workflow.auto_effort.bands.sol]]\neffort = \"high\"\n",
            ("a band without a condition", "sets no condition"),
        ),
        (
            "[[workflow.auto_effort.bands.ghost]]\neffort = \"high\"\ncomplexity = [\"hard\"]\n",
            ("bands for an unknown crew", "names no crew"),
        ),
        (
            "[[workflow.auto_effort.bands.sol]]\neffort = \"bogus\"\ncomplexity = [\"hard\"]\n",
            ("an unknown effort", "bogus"),
        ),
    ] {
        let body = auto_effort_body(
            "low_complexity_crews = [\"sol@auto\"]",
            &format!("{AUTO_EFFORT_BANDS}{bands}"),
        );
        let error = auto_effort_load(&body).expect_err(case);
        assert!(error.contains(reason), "{case}: {error}");
    }
}

#[test]
fn auto_effort_policy_resolves_bands_fallback_and_escalation() {
    use orbit_types::identity::{AutoEffortBasis, AutoEffortSignal, ReasoningEffort};
    use orbit_types::task::TaskComplexity;

    let config = auto_effort_load(&auto_effort_body(
        "low_complexity_crews = [\"sol@auto\"]",
        AUTO_EFFORT_BANDS,
    ))
    .expect("bands admit");
    let policy = config.snapshot.auto_effort();
    let sol = config.crews.get("sol").expect("sol crew");
    let filed = |tier| AutoEffortSignal {
        complexity: Some(tier),
        lines: None,
        files: None,
    };

    let hard = policy.resolve(sol, &filed(TaskComplexity::Hard), None);
    assert_eq!(hard.resolved, "sol@high");
    assert_eq!(
        (hard.basis, hard.band, hard.explored),
        (AutoEffortBasis::Band, Some(1), false)
    );
    assert_eq!(hard.requested, "sol@auto");

    let none = policy.resolve(sol, &AutoEffortSignal::default(), Some(0));
    assert_eq!(none.basis, AutoEffortBasis::Fallback);
    assert!(!none.resolved.ends_with("@auto"), "{}", none.resolved);

    // A ticket under the share explores another named effort and keeps the
    // policy's choice on record; one above it runs the policy.
    let explored = policy.resolve(sol, &filed(TaskComplexity::Low), Some(0));
    assert!(explored.explored, "{explored:?}");
    assert_ne!(explored.resolved, "sol@low");
    assert_eq!(explored.policy, "sol@low");
    let kept = policy.resolve(sol, &filed(TaskComplexity::Low), Some(999_999));
    assert_eq!((kept.resolved.as_str(), kept.explored), ("sol@low", false));

    let piloted = policy.resolve(sol, &filed(TaskComplexity::Low), None);
    let low = Some(TaskComplexity::Low);
    assert_eq!(
        policy.pilot_escalation(sol, &piloted, low, Some(TaskComplexity::Hard), Some("high")),
        Some(ReasoningEffort::High),
        "a raised tier mapped to a higher effort escalates"
    );
    assert_eq!(
        policy.pilot_escalation(sol, &piloted, low, low, Some("low")),
        Some(ReasoningEffort::High),
        "low confidence escalates to the highest band effort"
    );
    assert_eq!(
        policy.pilot_escalation(sol, &piloted, low, low, Some("high")),
        None,
        "a confident pilot at its filed tier stands"
    );
    let escalated = policy.escalated(sol, &piloted.signal, ReasoningEffort::High);
    assert_eq!(
        policy.pilot_escalation(
            sol,
            &escalated,
            low,
            Some(TaskComplexity::XHard),
            Some("low")
        ),
        None,
        "an escalated pilot never escalates again"
    );
}

#[test]
fn auto_effort_is_never_a_concrete_crew_reference() {
    let config = auto_effort_load(&auto_effort_body("", "")).expect("plain config");
    let error = orbit_types::identity::resolve_crew("sol@auto", &config.crews)
        .expect_err("@auto names no effort a run can execute at");
    assert!(error.to_string().contains("@auto"), "{error}");
}
