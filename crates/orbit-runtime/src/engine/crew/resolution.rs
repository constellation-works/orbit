use crate::RuntimeKernel;
use orbit_common::OrbitError;
use orbit_types::identity::{Crew, HumanCrew, find_human_crew, human_crew_refusal, resolve_crew};
use orbit_types::task::Task;
use orbit_types::workflow::activity_job::ProviderSource;

/// Select the crew *name* to dispatch by the Constellation provider-resolution
/// precedence (ORB-10091, contract §3): explicit > task_config >
/// workspace_default > environment_default > system_default. Empty /
/// whitespace-only values are skipped (not a selection). Returns the chosen
/// name and the tier it came from; `None` only when no tier supplies a value.
/// Pure and table-tested so the precedence cannot drift from the shared
/// contract or from the `Provider::resolve` surface it mirrors.
fn select_crew_name<'a>(
    explicit: Option<&'a str>,
    task_config: Option<&'a str>,
    workspace_default: Option<&'a str>,
    environment_default: Option<&'a str>,
    system_default: Option<&'a str>,
) -> Option<(&'a str, ProviderSource)> {
    [
        (explicit, ProviderSource::Explicit),
        (task_config, ProviderSource::TaskConfig),
        (workspace_default, ProviderSource::WorkspaceDefault),
        (environment_default, ProviderSource::EnvironmentDefault),
        (system_default, ProviderSource::SystemDefault),
    ]
    .into_iter()
    .find_map(|(value, source)| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| (value, source))
    })
}

impl RuntimeKernel {
    pub fn validate_crew_name(&self, crew: Option<&str>) -> Result<(), OrbitError> {
        self.canonical_crew_name(crew).map(|_| ())
    }

    /// The human crew `crew` names, if it names one.
    pub fn human_crew(&self, crew: Option<&str>) -> Option<&HumanCrew> {
        find_human_crew(crew, self.context.settings().human_crews())
    }

    /// Whether `crew` names a human crew (`[crews.<name>] kind = "human"`):
    /// operator work that no drain, follower or task pilot picks up.
    pub fn is_human_crew(&self, crew: Option<&str>) -> bool {
        self.human_crew(crew).is_some()
    }

    /// Canonicalize the crew a task is assigned. A task, unlike a dispatch,
    /// may name a human crew; anything else must name a configured agent
    /// crew, as [`Self::canonical_crew_name`] requires.
    pub fn canonical_task_crew_name(
        &self,
        crew: Option<&str>,
    ) -> Result<Option<String>, OrbitError> {
        match self.human_crew(crew) {
            Some(human) => Ok(Some(human.name.clone())),
            None => self.canonical_crew_name(crew),
        }
    }

    /// The crew a backlog task would actually dispatch as, or the reason it
    /// cannot be determined on this host. Used by auto-drain admission so an
    /// excluded task is reported against its *effective* crew rather than the
    /// raw string on the task.
    pub fn effective_task_crew(&self, task: &Task) -> Result<Crew, OrbitError> {
        self.resolve_crew_for_task(None, task.crew.as_deref())
    }

    /// Resolve a user-supplied crew name to the exact alias stored in the
    /// active named-crew registry. Blank optional values remain unset.
    pub fn canonical_crew_name(&self, crew: Option<&str>) -> Result<Option<String>, OrbitError> {
        Ok(self.canonical_crew(crew)?.map(|crew| crew.name))
    }

    /// Resolve a user-supplied crew name to its entry in the active
    /// named-crew registry. Blank optional values remain unset.
    pub fn canonical_crew(&self, crew: Option<&str>) -> Result<Option<Crew>, OrbitError> {
        let Some(crew) = crew.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        resolve_crew(crew, self.context.settings().crews())
            .map(Some)
            .map_err(Into::into)
    }

    /// Resolve the crew a dispatch will run, refusing a disabled one or a
    /// workspace default whose executor cannot run on this host.
    ///
    /// Every path that launches a provider — task start, run and activity
    /// dispatch, pool candidates — resolves through here, so
    /// a crew with `enabled = false` is refused with the table and command
    /// that enable it. There is no fallback to another crew: an operator who
    /// disabled a provider did not ask Orbit to spend a different one.
    pub fn resolve_crew_for_task(
        &self,
        cli_override: Option<&str>,
        task_crew: Option<&str>,
    ) -> Result<Crew, OrbitError> {
        let (crew, source) = self.select_configured_crew(cli_override, task_crew)?;
        if crew.enabled {
            // Explicit names are also used to enumerate pools before their OS
            // filter runs. Keep those assignments intact; only the automatic
            // default fallback is refused here, at the seam shared by task
            // draws, activity draws and direct dispatch.
            if source == ProviderSource::WorkspaceDefault {
                let limits = self.executor_host_os_limits()?;
                let requirement =
                    orbit_types::task::TaskOsRequirement::for_crew(&[], Some(&crew.name), &limits)
                        .map_err(OrbitError::InvalidInput)?;
                if let Some(wait) = requirement.unsatisfied_reason(self.host_os()) {
                    return Err(OrbitError::PolicyDenied(format!(
                        "workflow.default_crew `{}` {wait}; configure a default or pool with an \
                         executor that supports this host, or run on a supported host",
                        crew.name,
                    )));
                }
            }
            return Ok(crew);
        }
        let origin = match source {
            ProviderSource::TaskConfig => "task.crew selects ",
            ProviderSource::WorkspaceDefault => "workflow.default_crew selects ",
            _ => "",
        };
        Err(OrbitError::InvalidInput(format!(
            "{origin}{}",
            orbit_config::disabled_crew_message(&crew.name)
        )))
    }

    /// Look up the crew a task names, or the configured default, without the
    /// enabled check. For read surfaces, material fingerprints and startup
    /// validation of deterministic jobs: these describe the configured crew,
    /// disabled or not, and must never launch an agent.
    pub fn lookup_crew_for_task(
        &self,
        cli_override: Option<&str>,
        task_crew: Option<&str>,
    ) -> Result<Crew, OrbitError> {
        self.select_configured_crew(cli_override, task_crew)
            .map(|(crew, _)| crew)
    }

    /// A task's or template's crew, or `None` when it names the retired
    /// `system` crew [ORB-15663]. A task minted with that name before the
    /// system lane was retired, or a template still naming it, routes like an
    /// unassigned one — through the complexity pool for its complexity —
    /// instead of failing on a crew this host no longer defines.
    pub fn crew_unless_retired<'a>(&self, crew: Option<&'a str>) -> Option<&'a str> {
        match crew {
            Some(name)
                if orbit_config::names_retired_system_crew(
                    name,
                    self.context.settings().crews(),
                ) =>
            {
                tracing::warn!(crew = name, "{}", orbit_config::RETIRED_SYSTEM_CREW_WARNING);
                None
            }
            other => other,
        }
    }

    fn select_configured_crew(
        &self,
        cli_override: Option<&str>,
        task_crew: Option<&str>,
    ) -> Result<(Crew, ProviderSource), OrbitError> {
        // Runtime precedence is explicit > task_config > the default projected
        // by RuntimeConfig. That projection has already resolved workspace >
        // environment > system-default precedence, so lower tiers are not
        // re-read here and cannot drift during a run.
        let selection = select_crew_name(
            cli_override,
            self.crew_unless_retired(task_crew),
            self.context.settings().default_crew(),
            None,
            None,
        );

        let Some((selected, source)) = selection else {
            return Err(OrbitError::InvalidInput(
                "no crew selected; set [workflow].default_crew, task.crew, or pass crew"
                    .to_string(),
            ));
        };
        // A human crew is never an agent: refuse it by name rather than as an
        // undefined crew, and never fall back to another crew for it.
        if self.is_human_crew(Some(selected)) {
            let subject = match source {
                ProviderSource::TaskConfig => "task.crew",
                _ => "the selected crew",
            };
            return Err(OrbitError::PolicyDenied(human_crew_refusal(
                subject,
                selected.trim(),
            )));
        }
        let crew = resolve_crew(selected, self.context.settings().crews())?;
        Ok((crew, source))
    }

    pub fn resolve_and_log_crew_for_task_start(
        &self,
        task_id: &str,
        crew_override: Option<&str>,
        task_crew: Option<&str>,
    ) -> Result<Crew, OrbitError> {
        let crew = self.resolve_crew_for_task(crew_override, task_crew)?;
        tracing::info!(
            task_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for task start",
        );
        Ok(crew)
    }
}
