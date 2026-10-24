//! The retained drain entry points that admit workspace delivery work
//! [ORB-12500]. Job submission names the one it serves, and the distributed
//! drain decides admission for it.

/// A retained entry point that admits workspace delivery work [ORB-12500].
///
/// Every one of these existed before the distributed drain and keeps its own
/// surface, schedule and enablement. What they no longer keep is a private
/// idea of what the host is already doing: they all ask
/// [`OrbitRuntime::drain_entry_admission`](crate::OrbitRuntime::drain_entry_admission), which reads one occupancy, one
/// claim ledger and one destination-authority rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainEntryPoint {
    /// `orbit run auto`, and the seeded `ship_sweep` routine and
    /// `workspace_ship_pipeline` wrapper that invoke the drain beneath it.
    OwnerDrain,
    /// `orbit run ship`, `orbit.workflow.ship`, and the dashboard endpoint —
    /// the explicit shipment surfaces, with or without named tasks.
    ExplicitShip,
    /// Whole-run replay from the CLI or dashboard.
    Replay,
    /// The independent registry-driven `orbit run ship-sweep` CLI, which
    /// dispatches per workspace without a workspace runtime of its own.
    ShipSweep,
}

impl DrainEntryPoint {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DrainEntryPoint::OwnerDrain => "orbit.workflow.auto",
            DrainEntryPoint::ExplicitShip => "orbit.workflow.ship",
            DrainEntryPoint::Replay => "orbit.job.replay",
            DrainEntryPoint::ShipSweep => "orbit.run.ship-sweep",
        }
    }
}
