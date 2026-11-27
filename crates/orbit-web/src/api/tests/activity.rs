use crate::api::router;
use crate::api::tests::test_support;
use crate::state::{DashboardState, RefusalReason};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, TimeZone, Utc};
use orbit_core::test_support::{FixtureRuntime, fixture_core};
use std::net::IpAddr;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn remote_access_http_snapshot_excludes_out_of_order_expired_activity() {
    if !test_support::enter_isolated_child(
        module_path!(),
        "remote_access_http_snapshot_excludes_out_of_order_expired_activity",
    ) {
        return;
    }

    let runtime = Arc::new(FixtureRuntime::in_memory().expect("build runtime"));
    let state = DashboardState::single(Arc::new(fixture_core(&runtime)));
    let access = state.remote_access();
    let device = Some(IpAddr::from([100, 64, 0, 1]));
    let now = Utc
        .timestamp_opt(Utc::now().timestamp(), 0)
        .single()
        .expect("current timestamp is valid");

    access.record_admitted_at(device, "a@example.com", now);
    access.record_admitted_at(device, "a@example.com", now - Duration::seconds(901));
    access.record_admitted_at(device, "a@example.com", now + Duration::milliseconds(30));

    access.record_refused_at(RefusalReason::NoIdentity, None, now);
    access.record_refused_at(
        RefusalReason::NoIdentity,
        None,
        now - Duration::hours(24) - Duration::minutes(1),
    );
    access.record_refused_at(RefusalReason::NoIdentity, None, now + Duration::seconds(30));

    let request = Request::builder()
        .uri("/remote-access")
        .header(header::HOST, "localhost:7878")
        .body(Body::empty())
        .expect("request");
    let response = router(state.clone())
        .with_state(state)
        .oneshot(request)
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let report = test_support::body_json(response).await;
    let devices = report["connections"]["devices"]
        .as_array()
        .expect("connection devices");
    assert_eq!(devices.len(), 1, "{report}");
    assert_eq!(devices[0]["requests_15m"], 2, "{report}");
    assert_eq!(report["refusals"]["total"], 2, "{report}");
}
