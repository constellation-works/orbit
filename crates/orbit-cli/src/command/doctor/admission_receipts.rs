use clap::Args;
use orbit_core::Core;
use orbit_core::app::doctor::{Command, exec};

use crate::command::CommandOut;

#[derive(Args)]
pub struct AdmissionReceiptsArgs {
    /// Compact idle and settled admission receipts to permanent tombstones; retain full receipts
    /// for unsettled claims and failed or revoked claims with a preserved PR.
    #[arg(long)]
    pub compact: bool,
}

pub(super) fn admission_receipts(core: &Core, compact: bool) -> CommandOut {
    let outcome = exec(core, Command::AdmissionReceipts { compact })?;
    Ok(crate::output::doctor::admission_receipts_payload(outcome).into())
}
