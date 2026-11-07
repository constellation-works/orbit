//! Review's port for dispatching landing after recording handoff authority.
//!
//! Landing consumes the durable outbox above review in the application order.
//! It implements this port on `OrbitRuntime`, keeping review independent of
//! the consumer while dispatching at the point authority is recorded.

/// Dispatch after a durable handoff acceptance or approval.
pub(in crate::application) trait ReviewLandingDispatchPort {
    /// Dispatch pending authorized handoffs. Failures are logged and leave
    /// their durable requests pending for recovery; the decision still succeeds.
    fn dispatch_recorded_landing_authority(&self);
}
