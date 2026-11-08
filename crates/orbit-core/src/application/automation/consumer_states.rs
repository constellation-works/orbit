//! Bounded, complete walks of a workspace's automation consumer states.

use std::ops::ControlFlow;

use orbit_common::OrbitError;

const WORKSPACE_CONSUMER_PAGE_SIZE: usize = 100;

/// Read through the empty page that proves exhaustion, unless the visitor
/// breaks early. Validate each whole page before visiting any of its rows so
/// a corrupt later row cannot be hidden by an earlier match.
pub(super) fn for_each_workspace_consumer_state<T, B>(
    prefix: &str,
    mut read_page: impl FnMut(Option<&str>, usize) -> Result<Vec<T>, OrbitError>,
    consumer_key: impl Fn(&T) -> &str,
    mut visit: impl FnMut(T) -> Result<ControlFlow<B>, OrbitError>,
) -> Result<ControlFlow<B>, OrbitError> {
    let mut after: Option<String> = None;
    loop {
        let page = read_page(after.as_deref(), WORKSPACE_CONSUMER_PAGE_SIZE)?;
        if page.is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        for state in &page {
            let key = consumer_key(state);
            if !key.starts_with(prefix) || after.as_deref().is_some_and(|after| key <= after) {
                return Err(OrbitError::Store(
                    "automation state page is outside its prefix or not strictly ordered".into(),
                ));
            }
            after = Some(key.to_string());
        }
        for state in page {
            if let ControlFlow::Break(value) = visit(state)? {
                return Ok(ControlFlow::Break(value));
            }
        }
    }
}
