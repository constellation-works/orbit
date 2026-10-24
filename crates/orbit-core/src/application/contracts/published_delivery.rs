//! The publication a deferred gate grant carries for a resumed published
//! delivery. Admission decides the grant and job's gate reports it in its
//! step output, so neither owns the shape [ORB-17938].

use serde::Serialize;

/// Publication metadata carried only by a deferred gate grant.
#[derive(Serialize)]
pub(crate) struct PublishedDelivery {
    source_run_id: String,
    pull_request: Option<String>,
}

impl PublishedDelivery {
    pub(crate) fn new(source_run_id: String, pull_request: Option<String>) -> Self {
        Self {
            source_run_id,
            pull_request,
        }
    }
}
