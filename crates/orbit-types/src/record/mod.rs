//! Domain contracts for this Orbit types module.

mod adr;
mod audit;
mod crew_discovery;
mod error;
mod event;
mod friction;
pub use error::RecordError;

#[cfg(test)]
mod tests;

pub use adr::{Adr, AdrStatus, LegacyValidation, legacy_id_for, validate_adr_id};
pub use audit::Audit;
pub use crew_discovery::{CREW_DISCOVERY_SCHEMA_VERSION, CrewDiscoveryEntryV1, CrewDiscoveryV1};
pub use event::OrbitEvent;
pub use friction::{FrictionEntry, FrictionFrontmatter, FrictionRecord, FrictionStatus};
