-- Pre-move ledgered v1 `authority.db` fixture (exact schema written by the build
-- before `authority.db` moved under `orbit-store`), carrying one certified attempt.
CREATE TABLE IF NOT EXISTS schema_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

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

INSERT INTO schema_meta (key, value, updated_at) VALUES
    ('migration.v0001', 'recovery_authority_baseline', '2026-11-24T00:00:00Z'),
    ('migration.compat', '{"format":1,"version":1,"breaking":[],"read_only":[]}', '2026-11-24T00:00:00Z');

INSERT INTO rebase_recovery_attempt
    (run_id, step_id, attempt, workspace_path, head_sha_before, target_base_sha, reserved_at)
VALUES (
    'jrun-20261124-0003-1',
    'sync_base',
    1,
    '/fixture/workspace',
    '1111111111111111111111111111111111111111',
    '3333333333333333333333333333333333333333',
    '2026-11-24T12:00:00Z'
);

INSERT INTO rebase_recovery_attempt_certificate
    (run_id, step_id, attempt, workspace_path, head_sha, base_sha, payload_digest, issued_at)
VALUES (
    'jrun-20261124-0003-1',
    'sync_base',
    1,
    '/fixture/workspace',
    '6666666666666666666666666666666666666666',
    '3333333333333333333333333333333333333333',
    '387c67eea443fa353a42949074de0becf2083c0dfc030f92ef5d987cb501c372',
    '2026-11-24T12:01:00Z'
);
