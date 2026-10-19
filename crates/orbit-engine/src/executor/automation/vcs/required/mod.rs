//! Running a required validation command and keeping its bounded output and
//! command-reported evidence.

pub(super) mod capture;
pub(super) mod command;
pub(super) mod evidence;

#[cfg(test)]
mod tests;
