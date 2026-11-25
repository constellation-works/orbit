-- Frozen pre-ledger schema, including the optional first-use binding tables.
CREATE TABLE IF NOT EXISTS rebase_recovery_certificate (
                     run_id         TEXT NOT NULL,
                     step_id        TEXT NOT NULL,
                     workspace_path TEXT NOT NULL,
                     head_sha       TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     payload_digest TEXT NOT NULL,
                     issued_at      TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id)
                 ) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS rebase_recovery_attempt (
                     run_id          TEXT NOT NULL,
                     step_id         TEXT NOT NULL,
                     attempt         INTEGER NOT NULL,
                     workspace_path  TEXT NOT NULL,
                     head_sha_before TEXT NOT NULL,
                     target_base_sha TEXT NOT NULL,
                     reserved_at     TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id, attempt)
                 ) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS rebase_recovery_attempt_certificate (
                     run_id         TEXT NOT NULL,
                     step_id        TEXT NOT NULL,
                     attempt        INTEGER NOT NULL,
                     workspace_path TEXT NOT NULL,
                     head_sha       TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     payload_digest TEXT NOT NULL,
                     issued_at      TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id, attempt)
                 ) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS worker_process_binding (
            pid INTEGER NOT NULL, identity TEXT NOT NULL, binding_json TEXT NOT NULL,
            PRIMARY KEY(pid, identity)
        ) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS worker_namespace_binding (
            namespace_key TEXT PRIMARY KEY, binding_json TEXT NOT NULL
        ) WITHOUT ROWID;
