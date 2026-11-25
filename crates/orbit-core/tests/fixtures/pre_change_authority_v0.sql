-- Pre-ledger `authority.db` fixture (unnumbered `ensure_schema`, no `schema_meta`),
-- carrying one pre-attempt legacy certificate and one attempt-bound certificate.
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

INSERT INTO rebase_recovery_certificate
    (run_id, step_id, workspace_path, head_sha, base_sha, payload_digest, issued_at)
VALUES (
    'jrun-20261004-0001-1',
    'sync_base',
    '/fixture/workspace',
    '4444444444444444444444444444444444444444',
    '3333333333333333333333333333333333333333',
    '7e9eec983733424a5976c2e9d395c9d5b213f5a10c5205657e0f6f84f714d97c',
    '2026-10-04T22:04:41Z'
);

INSERT INTO rebase_recovery_attempt
    (run_id, step_id, attempt, workspace_path, head_sha_before, target_base_sha, reserved_at)
VALUES (
    'jrun-20261015-0002-1',
    'sync_base',
    1,
    '/fixture/workspace',
    '1111111111111111111111111111111111111111',
    '3333333333333333333333333333333333333333',
    '2026-10-15T12:00:00Z'
);

INSERT INTO rebase_recovery_attempt_certificate
    (run_id, step_id, attempt, workspace_path, head_sha, base_sha, payload_digest, issued_at)
VALUES (
    'jrun-20261015-0002-1',
    'sync_base',
    1,
    '/fixture/workspace',
    '5555555555555555555555555555555555555555',
    '3333333333333333333333333333333333333333',
    '8cc8b1c42cbdada920ffd7eaedb6b1d20160bb7b0df08a5009d280906aef199f',
    '2026-10-15T12:01:00Z'
);
