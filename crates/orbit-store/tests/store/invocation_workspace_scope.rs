//  Run ids are minted per workspace, so two workspaces submitting in the same
//  minute hold the same id. An invocation recorded under one workspace's run
//  never reaches the other's run listing or reliability counts.

/// A store where workspaces `ws_a` and `ws_b` each hold a run with the same
/// id. Retries once if the two submissions straddle a minute boundary.
fn colliding_runs() -> (Store, String) {
    for _ in 0..3 {
        let store = Store::open_in_memory().expect("memory store");
        let submit = |workspace: &str| {
            workspace_job_run_store(store.clone(), workspace)
                .insert_job_run("job", 1, Utc::now(), None, None)
                .expect("insert run")
                .run_id
        };
        let a = submit("ws_a");
        let b = submit("ws_b");
        if a == b {
            return (store, a);
        }
    }
    panic!("two workspaces submitting in one minute mint the same run id");
}

fn record_invocation(store: &Store, workspace_id: &str, run_id: &str, activity_id: &str) {
    store
        .insert_invocation_trace_record(
            workspace_id,
            &InvocationInsertParams {
                job_run_id: run_id.to_string(),
                account: None,
                activity_id: activity_id.to_string(),
                agent: "claude".to_string(),
                provider: None,
                model: None,
                task_ids: Vec::new(),
                trace: InvocationTrace::default(),
            },
        )
        .expect("record invocation");
}

#[test]
fn colliding_run_ids_keep_invocations_in_their_own_workspace() {
    let (store, run_id) = colliding_runs();
    record_invocation(&store, "ws_b", &run_id, "implement_one");
    let since = Utc::now() - invocation_workspace_scope_Duration::hours(1);
    let until = Utc::now() + invocation_workspace_scope_Duration::hours(1);

    let run_listing = |workspace: &str| {
        store
            .list_invocation_records(&InvocationQuery {
                workspace_id: Some(workspace.to_string()),
                job_run_id: Some(run_id.clone()),
                ..InvocationQuery::default()
            })
            .expect("list run invocations")
    };
    assert!(
        run_listing("ws_a").is_empty(),
        "ws_a's run listing read ws_b's invocation"
    );
    assert_eq!(run_listing("ws_b").len(), 1);

    assert!(
        store
            .count_invocations_by_activity("ws_a", since, until)
            .expect("count ws_a")
            .is_empty(),
        "ws_a's reliability counts included ws_b's invocation"
    );
    let b_counts = store
        .count_invocations_by_activity("ws_b", since, until)
        .expect("count ws_b");
    assert_eq!(b_counts.len(), 1);
    assert_eq!(b_counts[0].invocation_count, 1);

    let coverage = |workspace: &str| {
        store
            .count_invocation_job_runs(workspace, since, until, &["implement_one".to_string()])
            .expect("count run coverage")
    };
    assert_eq!(coverage("ws_a").total_job_runs, 0);
    assert_eq!(coverage("ws_b").matching_job_runs, 1);
}

#[test]
fn stored_sol_costs_preserve_history_and_start_current_rates_at_verified_observation() {
    if !provider_ledger_isolated(
        "stored_sol_costs_preserve_history_and_start_current_rates_at_verified_observation",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = root.path().join("sol.db");
    let store = Store::open(&path).unwrap();
    // Observed 2026-08-22T04:00:04.449861486Z: replay the same usage at
    // selected boundaries, without treating an invocation total as context size.
    let observed = TokenUsage {
        input: 13_424_851,
        cache_read: 13_090_560,
        output: 41_263,
        ..Default::default()
    };
    let old_cost = (334_291.0 * 5.0 + 13_090_560.0 * 0.5 + 41_263.0 * 30.0) / 1e6;
    let current_cost = (334_291.0 * 4.0 + 13_090_560.0 * 0.4 + 41_263.0 * 20.0) / 1e6;
    let mut cases = Vec::new();
    for (at, expected) in [
        ("2026-07-16T23:59:59.999999999Z", None),
        ("2026-07-17T00:00:00Z", Some(old_cost)),
        ("2026-07-29T23:59:59.999999999Z", Some(old_cost)),
        ("2026-07-30T00:00:00Z", Some(old_cost)),
        // No calendar-date convention has authorized historical repricing.
        ("2026-08-20T23:59:59.999999999Z", Some(old_cost)),
        ("2026-08-21T00:00:00Z", Some(old_cost)),
        ("2026-08-22T04:00:04.449861486Z", Some(old_cost)),
        ("2026-09-21T23:59:59.999999999Z", Some(old_cost)),
        ("2026-09-22T00:00:00Z", None),
        ("2026-10-23T03:09:14.111506999Z", None),
        ("2026-10-23T03:09:14.111507Z", Some(current_cost)),
        ("2026-10-23T03:09:14.111507001Z", Some(current_cost)),
        ("2026-11-22T00:00:00Z", Some(current_cost)),
        ("2027-01-01T00:00:00Z", Some(current_cost)),
    ] {
        cases.push((at, observed.clone(), expected));
    }
    let current = "2026-10-23T03:09:14.111507Z";
    for (usage, expected) in [
        // Ordinary input and every cache bucket are charged exactly once.
        (
            TokenUsage {
                input: 1_000_000,
                ..Default::default()
            },
            Some(4.0),
        ),
        (
            TokenUsage {
                input: 1_000_000,
                cache_read: 1_000_000,
                ..Default::default()
            },
            Some(0.4),
        ),
        (
            TokenUsage {
                input: 1_000_000,
                cache_create: 1_000_000,
                ..Default::default()
            },
            Some(5.0),
        ),
        // Legacy malformed 1h data retains a nonzero fallback, not a 12h rate.
        (
            TokenUsage {
                input: 1_000_000,
                cache_create_1h: 1_000_000,
                ..Default::default()
            },
            Some(5.0),
        ),
        (
            TokenUsage {
                output: 1_000_000,
                ..Default::default()
            },
            Some(20.0),
        ),
        (
            TokenUsage {
                input: 1,
                cache_read: 2,
                ..Default::default()
            },
            None,
        ),
        (
            TokenUsage {
                input: 1,
                cache_create: 1,
                cache_create_1h: 1,
                ..Default::default()
            },
            None,
        ),
    ] {
        cases.push((current, usage, expected));
    }
    let conn = Connection::open(&path).unwrap();
    for (index, (at, usage, _)) in cases.iter().enumerate() {
        let activity = format!("sol-{index}");
        store
            .insert_invocation_trace_record(
                "ws_sol",
                &InvocationInsertParams {
                    job_run_id: "pricing-replay".to_string(),
                    account: None,
                    activity_id: activity.clone(),
                    agent: "codex".to_string(),
                    provider: Some("openai".to_string()),
                    model: Some("gpt-5.6-sol".to_string()),
                    task_ids: Vec::new(),
                    trace: InvocationTrace {
                        usage: usage.clone(),
                        provider_cost_usd: Some(42.125),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        // Timestamp setup is private to this disposable fixture; assertions
        // exercise the public stored-invocation read boundary.
        conn.execute(
            "UPDATE invocations SET ts = ?1 WHERE activity_id = ?2",
            rusqlite::params![at, activity],
        )
        .unwrap();
    }
    let rows = store
        .list_invocation_records(&InvocationQuery {
            workspace_id: Some("ws_sol".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), cases.len());
    for (index, (at, usage, expected)) in cases.iter().enumerate() {
        let row = rows
            .iter()
            .find(|row| row.activity_id == format!("sol-{index}"))
            .unwrap();
        match (row.derived_cost_usd, expected) {
            (Some(actual), Some(expected)) => assert!(
                (actual - expected).abs() < 1e-10,
                "Sol stored-cost regression at {at}: {usage:?}: {actual} != {expected}"
            ),
            (actual, expected) => assert_eq!(
                actual, *expected,
                "Sol stored-cost regression at {at}: {usage:?}"
            ),
        }
        assert_eq!(row.ts, at.parse::<DateTime<Utc>>().unwrap());
        assert_eq!(row.input_tokens, usage.input);
        assert_eq!(row.cache_read_tokens, usage.cache_read);
        assert_eq!(row.cache_create_tokens, usage.cache_create);
        assert_eq!(row.cache_create_1h_tokens, usage.cache_create_1h);
        assert_eq!(row.output_tokens, usage.output);
        assert_eq!(
            row.provider_cost_usd,
            Some(42.125),
            "derived estimates never replace the provider's own cost"
        );
    }
}
