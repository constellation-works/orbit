use super::*;

impl AgentQuery {
    /// Validate a selection. An omitted window is `30d` and an omitted
    /// minimum support 10.
    pub fn parse(window: Option<&str>, min_support: Option<usize>) -> Result<Self, OrbitError> {
        let window = window.unwrap_or(DEFAULT_WINDOW);
        let Some(&(requested, served, hours)) = WINDOWS.iter().find(|(name, _, _)| *name == window)
        else {
            return Err(OrbitError::InvalidInput(format!(
                "window must be one of 1h, 24h, 7d, 30d, 90d or all; got '{window}'"
            )));
        };
        let min_support = min_support.unwrap_or(DEFAULT_MIN_SUPPORT);
        if min_support == 0 {
            return Err(OrbitError::InvalidInput(
                "min_support must be at least 1".to_string(),
            ));
        }
        Ok(Self {
            requested,
            window: served,
            hours,
            min_support,
        })
    }

    /// The window's start; it ends now.
    pub(super) fn since(&self) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::hours(self.hours)
    }

    /// The selection and scan facts every response carries.
    pub(super) fn header(
        &self,
        facts: &AgentFacts,
        since: DateTime<Utc>,
        cache_ttl: Duration,
    ) -> Value {
        json!({
            "window": self.window,
            "requested_window": self.requested,
            "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
            "min_support": self.min_support,
            "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "cache_ttl_seconds": cache_ttl.as_secs(),
            "tasks_scanned": facts.tasks_scanned,
        })
    }
}
