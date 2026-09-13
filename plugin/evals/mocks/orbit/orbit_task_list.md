---
expect:
  workspace: string
---
{
  "tasks": [
    { "id": "DANI-41", "title": "Lineage graph: collapse duplicate edges on import", "status": "proposed", "complexity": "medium", "tags": ["import"], "dependencies": [], "created_at": "2026-08-30T09:00:00Z" },
    { "id": "DANI-42", "title": "Dedupe edges when importing a corpus", "status": "proposed", "complexity": "medium", "tags": ["import"], "dependencies": [], "created_at": "2026-09-02T11:30:00Z" },
    { "id": "DANI-44", "title": "Add `nebula export --format dot`", "status": "backlog", "complexity": "low", "tags": ["export"], "dependencies": [], "created_at": "2026-09-05T16:10:00Z" },
    { "id": "DANI-45", "title": "Owner-scoped corpus root in config", "status": "backlog", "complexity": "hard", "tags": ["config"], "dependencies": ["DANI-44"], "created_at": "2026-09-06T10:00:00Z" },
    { "id": "DANI-38", "title": "Rust 1.90 toolchain bump", "status": "done", "complexity": "low", "tags": [], "dependencies": [], "created_at": "2026-08-20T08:00:00Z" }
  ],
  "total": 5,
  "truncated": false
}
