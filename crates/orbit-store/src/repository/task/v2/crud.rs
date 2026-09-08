use super::*;

impl TaskV2Store {
    pub(crate) fn create_task(&self, params: TaskCreateParams) -> Result<Task, OrbitError> {
        self.create_task_with_key(params, None)
    }
    pub(crate) fn create_task_with_key(
        &self,
        params: TaskCreateParams,
        key: Option<&str>,
    ) -> Result<Task, OrbitError> {
        if params.title.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task title must not be empty".to_string(),
            ));
        }
        if params.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }
        let relations = relations_from_create_params(&params)?;
        self.registry
            .validate_new_task_relation_targets(&self.workspace_id, &relations)?;

        let now = Utc::now();
        let id = if let Some(key) = key {
            let bytes =
                serde_json::to_vec(&params).map_err(|e| OrbitError::Store(e.to_string()))?;
            self.registry.reserve_task_action(
                &self.workspace_id,
                key,
                &format!("{:x}", Sha256::digest(bytes)),
            )?
        } else {
            self.registry.allocate_task_id(&self.workspace_id)?
        };
        self.registry
            .validate_task_relations(&self.workspace_id, &id, &relations)?;
        let comments = params
            .comments
            .iter()
            .enumerate()
            .map(|(index, comment)| orbit_types::task::TaskCommentRowV2 {
                schema_version: orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION,
                comment_id: format!("C-{number:04}", number = index + 1),
                at: comment.at,
                by: comment.by.clone(),
                body: comment.message.clone(),
            })
            .collect();
        let bundle = TaskBundleV2 {
            envelope: orbit_types::task::TaskEnvelopeV2 {
                schema_version: orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION,
                id: id.clone(),
                title: params.title,
                status: params.status,
                task_type: params.task_type,
                priority: params.priority,
                complexity: params.complexity,
                pr_status: None,
                job_run_id: None,
                crew: params.crew,
                orchestrator: params.orchestrator,
                relations,
                tags: normalize_task_tags(params.tags),
                required_tools: orbit_types::task::normalize_required_tools(params.required_tools),
                context_files: params.context_files,
                external_refs: params.external_refs,
                created_by: params.created_by,
                planned_by: params.planned_by,
                implemented_by: params.implemented_by,
                created_at: now,
                updated_at: now,
            },
            description: params.description,
            acceptance: render_acceptance(&params.acceptance_criteria),
            plan: params.plan,
            execution_summary: params.execution_summary,
            events: vec![orbit_types::task::TaskEventRowV2 {
                schema_version: orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: "EV-0001".to_string(),
                at: now,
                by: params.actor,
                event_type: "created".to_string(),
                note: None,
                from_status: None,
                to_status: Some(params.status),
            }],
            comments,
            artifact_manifest: None,
        };

        if key.is_some() {
            let bundle = self.bundle_store.create_or_recover_action_bundle(&bundle)?;
            self.replace_index_best_effort(&bundle.envelope, "idempotent task creation");
            return self.task_from_bundle(bundle);
        }
        self.bundle_store.create_bundle(&bundle)?;
        self.replace_index_best_effort(&bundle.envelope, "task creation");
        self.task_from_bundle(bundle)
    }

    /// Materialize tasks on the lightweight bundle path: no artifact hashing.
    pub(crate) fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        if let Some(tasks) = self.indexed_tasks(TaskIndexFilter {
            status: None,
            priority: None,
            job_run_id: None,
            tags: Vec::new(),
        })? {
            return Ok(tasks);
        }

        let mut tasks = self
            .bundle_store
            .list_bundles()?
            .into_iter()
            .map(|bundle| self.task_from_bundle(bundle))
            .collect::<Result<Vec<_>, _>>()?;
        sort_by_created_desc_id_asc(&mut tasks, |task| &task.created_at, |task| &task.id);
        Ok(tasks)
    }

    pub(crate) fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        let mut tasks = match self.indexed_tasks(TaskIndexFilter {
            status,
            priority,
            job_run_id: job_run_id.map(ToOwned::to_owned),
            tags: Vec::new(),
        })? {
            Some(tasks) => tasks,
            None => self.list_tasks()?,
        };
        tasks.retain(|task| {
            status.is_none_or(|value| task.status == value)
                && priority.is_none_or(|value| task.priority == value)
                && parent_id.is_none_or(|value| task.parent_id() == Some(value))
                && job_run_id.is_none_or(|value| task.job_run_id.as_deref() == Some(value))
                && external_ref.is_none_or(|value| {
                    task.external_refs.iter().any(|candidate| {
                        candidate.system == value.system && candidate.id == value.id
                    })
                })
                && has_external_ref_system.is_none_or(|value| {
                    task.external_refs
                        .iter()
                        .any(|candidate| candidate.system == value)
                })
        });
        Ok(tasks)
    }

    pub(crate) fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        let required_tags = normalize_task_tags(tags.to_vec());
        if required_tags.is_empty() {
            return self.list_tasks();
        }
        if let Some(tasks) = self.indexed_tasks(TaskIndexFilter {
            status: None,
            priority: None,
            job_run_id: None,
            tags: required_tags.clone(),
        })? {
            return Ok(tasks);
        }
        let mut tasks = self.list_tasks()?;
        tasks.retain(|task| {
            required_tags
                .iter()
                .all(|required| task.tags.iter().any(|tag| tag == required))
        });
        Ok(tasks)
    }

    pub(crate) fn get_task(&self, id: &str) -> Result<Option<Task>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        match self.bundle_store.read_bundle(id) {
            Ok(bundle) => self.task_from_bundle(bundle).map(Some),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError> {
        self.search_tasks_filtered(query, &[])
    }

    pub(crate) fn search_tasks_filtered(
        &self,
        query: &str,
        tags: &[String],
    ) -> Result<Vec<Task>, OrbitError> {
        // Candidate materialization is lightweight; artifact content search
        // below may still open matching text blobs on demand.
        let lowered = query.to_lowercase();
        let bundles = self.candidate_bundles_by_tags(tags)?;
        self.search_bundles(bundles, &lowered)
    }

    /// The bundles `list_tasks_by_tags` would materialize, in the same order,
    /// handed over whole so a caller that also needs the sidecars does not
    /// read each bundle again.
    fn candidate_bundles_by_tags(&self, tags: &[String]) -> Result<Vec<TaskBundleV2>, OrbitError> {
        let required_tags = normalize_task_tags(tags.to_vec());
        if let Some(bundles) = self.indexed_bundles(TaskIndexFilter {
            status: None,
            priority: None,
            job_run_id: None,
            tags: required_tags.clone(),
        })? {
            return Ok(bundles);
        }
        let mut bundles = self.bundle_store.list_bundles()?;
        bundles.retain(|bundle| {
            required_tags
                .iter()
                .all(|required| bundle.envelope.tags.iter().any(|tag| tag == required))
        });
        sort_by_created_desc_id_asc(
            &mut bundles,
            |bundle| &bundle.envelope.created_at,
            |bundle| &bundle.envelope.id,
        );
        Ok(bundles)
    }

    pub(crate) fn delete_task(&self, id: &str) -> Result<bool, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.bundle_store.delete_bundle(id)
    }
}
