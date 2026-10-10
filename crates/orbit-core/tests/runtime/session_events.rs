//! Bounded session observability through the runtime's public event boundary.

use orbit_core::OrbitRuntime;
use orbit_core::runtime::event_bus::SESSION_EVENT_CAPACITY;
use orbit_types::record::OrbitEvent;

#[test]
fn session_events_retain_newest_at_capacity_and_limit_reads() {
    if !super::dispatch_admission::isolated(
        "session_events::session_events_retain_newest_at_capacity_and_limit_reads",
    ) {
        return;
    }
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let reader = runtime.clone();
    assert!(reader.list_session_events(usize::MAX).unwrap().is_empty());

    let total = SESSION_EVENT_CAPACITY * 3 + 17;
    for id in 1..=total {
        runtime
            .record_event(OrbitEvent::ToolExecuted {
                name: format!("fixture.tool.{id}"),
            })
            .expect("record session event");
        if [
            SESSION_EVENT_CAPACITY - 1,
            SESSION_EVENT_CAPACITY,
            SESSION_EVENT_CAPACITY + 1,
            total,
        ]
        .contains(&id)
        {
            let events = reader.list_session_events(usize::MAX).unwrap();
            assert_eq!(events.len(), id.min(SESSION_EVENT_CAPACITY));
            for (offset, event) in events.iter().enumerate() {
                let expected_id = id - offset;
                assert_eq!(event.id, expected_id as i64);
                assert_eq!(event.event_type, "ToolExecuted");
                assert_eq!(
                    event.payload["data"]["name"],
                    format!("fixture.tool.{expected_id}")
                );
            }
        }
    }

    for limit in [0, 1, 7, SESSION_EVENT_CAPACITY, SESSION_EVENT_CAPACITY + 1] {
        let events = reader.list_session_events(limit).unwrap();
        assert_eq!(events.len(), limit.min(SESSION_EVENT_CAPACITY));
        for (offset, event) in events.iter().enumerate() {
            assert_eq!(event.id, (total - offset) as i64);
        }
    }

    // Direct EventLog callers retain the chronological snapshot contract.
    let snapshot = reader.event_log.snapshot();
    assert_eq!(snapshot.len(), SESSION_EVENT_CAPACITY);
    assert_eq!(
        snapshot.first(),
        Some(&OrbitEvent::ToolExecuted {
            name: format!("fixture.tool.{}", total - SESSION_EVENT_CAPACITY + 1),
        })
    );
    assert_eq!(
        snapshot.last(),
        Some(&OrbitEvent::ToolExecuted {
            name: format!("fixture.tool.{total}"),
        })
    );
}
