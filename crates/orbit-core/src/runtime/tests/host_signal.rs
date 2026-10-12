//! The shutdown horizon is an instant, not a direction: equality holds, one
//! second past it admits, and a past timestamp holds.

use chrono::{DateTime, Utc};

use crate::runtime::host_signal::ScheduledShutdown;

fn shutdown(at: DateTime<Utc>) -> ScheduledShutdown {
    ScheduledShutdown {
        mode: "reboot".into(),
        scheduled_at: at,
        source: "fixture".into(),
    }
}

#[test]
fn the_hold_includes_the_horizon_and_a_later_instant_admits() {
    let now = Utc::now();
    let horizon = chrono::Duration::try_minutes(30).expect("30 minutes fits");
    assert!(shutdown(now + horizon).holds_admission(now, 30));
    assert!(!shutdown(now + horizon + chrono::Duration::seconds(1)).holds_admission(now, 30));
    assert!(shutdown(now - chrono::Duration::seconds(1)).holds_admission(now, 30));
}
