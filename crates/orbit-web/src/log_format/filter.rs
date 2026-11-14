//! Dashboard query parsing for shared process-log filters.

use crate::parse::parse_since;
use orbit_cmd::log_format::{Filters, LevelFilter};
use orbit_core::OrbitError;

pub(crate) fn filters_from_query_parts(
    target: Option<String>,
    level: Option<String>,
    since: Option<&str>,
) -> Result<Filters, OrbitError> {
    let min_level = match level.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(LevelFilter::parse_query(raw)?),
        None => None,
    };
    let since = since.map(parse_since).transpose()?;
    Ok(Filters::new(target, min_level, since))
}
