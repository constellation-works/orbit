use orbit_core::app::task::Outcome;

use super::show;
use crate::command::CommandOut;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    show::render(outcome)
}
