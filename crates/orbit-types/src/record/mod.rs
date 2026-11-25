//! Durable record contracts (`OrbitEvent`, `Audit`, friction, notice, evolution, and crew discovery); imports `identity`, `telemetry`, and `workflow` within `orbit-types` only (I/O-free).

mod audit;
mod crew_discovery;
mod error;
mod event;
mod evolution;
pub use evolution::*;
mod friction;
mod notice;
pub use error::RecordError;

pub use audit::Audit;
pub use crew_discovery::{CREW_DISCOVERY_SCHEMA_VERSION, CrewDiscoveryEntryV1, CrewDiscoveryV1};
pub use event::OrbitEvent;
pub use friction::{FrictionFrontmatter, FrictionRecord, FrictionStatus};
pub use notice::*;
