//! Owner-local evolution ledger persistence. No friction operation reaches it.
use orbit_common::OrbitError;
use orbit_types::record::{EvolutionOccurrence, EvolutionRecord};

/// Application lifecycle decision, evaluated under the ledger write transaction.
pub type EvolutionDecision<'a> = dyn Fn(&mut EvolutionRecord) -> Result<(), OrbitError> + 'a;

pub trait EvolutionStoreBackend: Send + Sync {
    /// Atomically corroborate each signature and persist unsigned costs too.
    /// A repeated source key is a no-op, including after a process retry.
    fn record_evolution_costs(
        &self,
        workspace: &str,
        events: &[EvolutionOccurrence],
    ) -> Result<Vec<EvolutionRecord>, OrbitError>;
    fn evolution_records(&self, workspace: &str) -> Result<Vec<EvolutionRecord>, OrbitError>;
    fn evolution_costs(&self, workspace: &str) -> Result<Vec<EvolutionOccurrence>, OrbitError>;
    fn mutate_evolution(
        &self,
        workspace: &str,
        id: &str,
        decide: &EvolutionDecision<'_>,
    ) -> Result<EvolutionRecord, OrbitError>;
}
