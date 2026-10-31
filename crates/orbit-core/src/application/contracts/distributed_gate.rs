//! The build-time gate on the distributed drain's mutating entry points.
//! `distributed` refuses them while it is closed, and `review`'s handoff
//! console reports it.

/// Whether the mutating distributed entry points are reachable from any public
/// surface.
///
/// It is a constant rather than configuration on purpose: the feature must not
/// become reachable, or unreachable, because an operator set a key or an
/// environment variable. [ORB-13625] opened it once the routed follower peer,
/// the owner's pull/bind/settle tools and `orbit run auto --pull` landed
/// together; every gated entry point still names it, so closing the feature
/// again is one deliberate source change.
pub const DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED: bool = true;
