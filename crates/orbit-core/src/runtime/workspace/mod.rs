//! The workspace a runtime serves: its registry-neutral binding, the catalog
//! seam for reads that span workspaces, and the exclusive workspace claim.

pub(crate) mod binding;
pub mod catalog;
pub mod claim;

#[cfg(test)]
mod tests;
