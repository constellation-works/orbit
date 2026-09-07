-- Frozen prior-v5 schema from 2c143e01b^:crates/orbit-store/src/driver/sqlite/task_registry/schema.rs.
-- Keep independent of current schema setup: this database never had task_action_keys.
CREATE TABLE IF NOT EXISTS allocator_state (
    authority TEXT PRIMARY KEY,
    next_number INTEGER NOT NULL CHECK(next_number >= 0),
    task_prefix TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_bindings (
    workspace_id TEXT PRIMARY KEY,
    slug TEXT NOT NULL,
    repo_fingerprint TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_checkout_bindings (
    workspace_id TEXT PRIMARY KEY,
    repo_root TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    orbit_dir TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_workspace_checkout_bindings_paths
    ON workspace_checkout_bindings(repo_root, workspace_path, orbit_dir);

CREATE TABLE IF NOT EXISTS task_bundle_bindings (
    task_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    canonical_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_bundle_bindings_workspace
    ON task_bundle_bindings(workspace_id, task_id);

CREATE TABLE IF NOT EXISTS task_bundle_index (
    task_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    status TEXT NOT NULL,
    priority TEXT NOT NULL,
    job_run_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    terminal_month TEXT,
    complexity TEXT,
    FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
    FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_created
    ON task_bundle_index(workspace_id, created_at DESC, task_id ASC);
CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_status
    ON task_bundle_index(workspace_id, status, created_at DESC, task_id ASC);
CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_priority
    ON task_bundle_index(workspace_id, priority, created_at DESC, task_id ASC);

CREATE TABLE IF NOT EXISTS task_bundle_tags (
    task_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY(task_id, tag),
    FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
    FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_bundle_tags_workspace_tag
    ON task_bundle_tags(workspace_id, tag, task_id);

CREATE TABLE IF NOT EXISTS task_bundle_relations (
    source_task_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    target_task_id TEXT NOT NULL,
    PRIMARY KEY(source_task_id, relation_type, target_task_id),
    FOREIGN KEY(source_task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE,
    FOREIGN KEY(workspace_id) REFERENCES workspace_bindings(workspace_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_task_bundle_relations_workspace_type_target
    ON task_bundle_relations(workspace_id, relation_type, target_task_id, source_task_id);

CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_job_run
            ON task_bundle_index(workspace_id, job_run_id, created_at DESC, task_id ASC);
CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_terminal
            ON task_bundle_index(workspace_id, terminal_month, task_id);
CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_complexity
            ON task_bundle_index(workspace_id, complexity, status);
PRAGMA user_version = 5;
