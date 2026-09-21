//! A fixture plugin that contributes everything phase 3 installs: one
//! agent-loop activity, one deterministic activity driving the plugin's own
//! tool through `plugin.tool_call`, one job wiring them, one routine, one
//! auto-task, one skill and a `[plugins.<ns>]` schema.
//!
//! Written as files rather than a constant blob so a test can bend exactly one
//! rule (a cross-plugin routine target, an `enabled: true` schedule, a config
//! value the schema rejects) and assert on the refusal.

use std::path::{Path, PathBuf};

use super::fixture::PluginFixture;

/// Everything a test wants to vary about the fixture plugin.
pub(super) struct DefinitionPlugin<'a> {
    pub(super) namespace: &'a str,
    pub(super) version: &'a str,
    /// Activity name, which is also what a workspace file shadows. Derived
    /// from the namespace so two fixture plugins never collide by accident.
    pub(super) activity: String,
    pub(super) job: String,
    pub(super) routine: &'a str,
    pub(super) auto_task: &'a str,
    /// `target:` of the shipped routine. Defaults to this plugin's own job.
    pub(super) routine_target: Option<&'a str>,
    /// Ship a routine that switches itself on, which §4.5 refuses.
    pub(super) routine_enabled: bool,
    /// Ship an auto-task that switches itself on.
    pub(super) auto_task_enabled: bool,
    /// Ship a `spec.skills[]` directory.
    pub(super) skill: bool,
    /// Ship a `spec.config` schema requiring `index_dir: string`.
    pub(super) config: bool,
}

impl<'a> DefinitionPlugin<'a> {
    pub(super) fn new(namespace: &'a str) -> Self {
        Self {
            namespace,
            version: "1.0.0",
            activity: format!("{namespace}_refresh"),
            job: format!("{namespace}_refresh_pipeline"),
            routine: "refresh",
            auto_task: "reindex",
            routine_target: None,
            routine_enabled: false,
            auto_task_enabled: false,
            skill: true,
            config: true,
        }
    }

    pub(super) fn with_version(mut self, version: &'a str) -> Self {
        self.version = version;
        self
    }

    pub(super) fn targeting(mut self, target: &'a str) -> Self {
        self.routine_target = Some(target);
        self
    }

    pub(super) fn with_enabled_routine(mut self) -> Self {
        self.routine_enabled = true;
        self
    }

    pub(super) fn with_enabled_auto_task(mut self) -> Self {
        self.auto_task_enabled = true;
        self
    }

    /// Write the plugin tree under `fixture.sources` and return its root.
    pub(super) fn write(&self, fixture: &PluginFixture) -> PathBuf {
        let root = fixture.sources.join(self.namespace);
        write_backend(&root);

        let activities = root.join("definitions/activities");
        let jobs = root.join("definitions/jobs");
        let routines = root.join("definitions/routines");
        let auto_tasks = root.join("definitions/auto_tasks");
        for dir in [&activities, &jobs, &routines, &auto_tasks] {
            std::fs::create_dir_all(dir).expect("create definition dir");
        }

        // The agent-loop half of the fixture: a plugin may ship an ordinary
        // agent activity, which is data already.
        write(
            &activities.join("review.yaml"),
            &format!(
                "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: {ns}_review\nspec:\n  \
                 type: agent_loop\n  description: Review what the index found.\n  \
                 prompt: Review the graph index report.\n  input_schema_json:\n    type: object\n  \
                 allowed_tools: []\n",
                ns = self.namespace
            ),
        );
        // The deterministic half: the only way a plugin contributes
        // deterministic behaviour (§4.5).
        write(
            &activities.join("refresh.yaml"),
            &format!(
                "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: {activity}\nspec:\n  \
                 type: deterministic\n  description: Refresh the index through the plugin tool.\n  \
                 input_schema_json:\n    type: object\n  action: plugin.tool_call\n  config:\n    \
                 tool: {ns}.hello\n    input: {{}}\n",
                activity = self.activity,
                ns = self.namespace
            ),
        );
        write(
            &jobs.join("pipeline.yaml"),
            &format!(
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {job}\nspec:\n  state: enabled\n  \
                 kind: workflow\n  max_active_runs: 1\n  steps:\n    - id: refresh\n      \
                 target: activity:{activity}\n",
                job = self.job,
                activity = self.activity
            ),
        );
        let target = self
            .routine_target
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("job:{}", self.job));
        write(
            &routines.join("refresh.yaml"),
            &format!(
                "schemaVersion: 1\nname: {routine}\ndescription: Refresh the index nightly.\n\
                 enabled: {enabled}\ntrigger:\n  cron: \"0 3 * * *\"\ntarget: {target}\n",
                routine = self.routine,
                enabled = self.routine_enabled
            ),
        );
        write(
            &auto_tasks.join("reindex.yaml"),
            &format!(
                "schemaVersion: 1\nname: {name}\ndescription: Reindex the graph.\n\
                 enabled: {enabled}\nschedule:\n  every_minutes: 1440\ntemplate:\n  \
                 title: Reindex the graph\n  description: Rebuild the plugin's index.\n",
                name = self.auto_task,
                enabled = self.auto_task_enabled
            ),
        );

        if self.skill {
            let skill = root.join("skills").join(self.namespace);
            std::fs::create_dir_all(&skill).expect("create skill dir");
            write(
                &skill.join("SKILL.md"),
                "---\nname: graph\ndescription: Use the graph plugin.\n---\n\nAsk the plugin.\n",
            );
        }
        if self.config {
            std::fs::create_dir_all(root.join("schemas")).expect("create schema dir");
            write(
                &root.join("schemas/config.json"),
                "{\n  \"type\": \"object\",\n  \"additionalProperties\": false,\n  \
                 \"properties\": {\n    \"index_dir\": { \"type\": \"string\" },\n    \
                 \"depth\": { \"type\": \"integer\" }\n  }\n}\n",
            );
        }

        write(&root.join("plugin.yaml"), &self.manifest());
        root
    }

    fn manifest(&self) -> String {
        let skills = if self.skill {
            format!("  skills: [skills/{}]\n", self.namespace)
        } else {
            String::new()
        };
        let config = if self.config {
            "  config:\n    schema: schemas/config.json\n    defaults: { index_dir: \".index\" }\n"
                .to_string()
        } else {
            String::new()
        };
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {ns}\n  version: {version}\n  \
             description: Fixture plugin with definitions.\nspec:\n  backend:\n    type: exec\n    \
             command: bin/backend.sh\n  tools:\n    - name: hello\n      description: Say hello.\n      \
             execution_kind: read_only\n      mcp_scope: workspace\n  definitions:\n    \
             activities: [definitions/activities/*.yaml]\n    jobs: [definitions/jobs/*.yaml]\n    \
             routines: [definitions/routines/*.yaml]\n    auto_tasks: [definitions/auto_tasks/*.yaml]\n\
             {skills}{config}",
            ns = self.namespace,
            version = self.version,
        )
    }
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

fn write_backend(root: &Path) {
    let backend = root.join("bin/backend.sh");
    write(
        &backend,
        "#!/bin/sh\ncat > /dev/null\nprintf '{\"ok\":true,\"output\":{\"plugin\":\"%s\"}}\\n' \
         \"$ORBIT_PLUGIN\"\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
}
