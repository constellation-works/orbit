---
expect:
  id: /^[A-Z]+-\d+$/
---
{
  "id": "{{input.id}}",
  "title": "Gate command_exec cwd to the task worktree",
  "status": "{{input.status}}",
  "priority": "high",
  "complexity": "medium",
  "type": "bug",
  "crew": "kepler",
  "orchestrator": "sol",
  "tags": ["qa-sweep-0.21.0", "security"],
  "implemented_by": "claude",
  "created_at": "2026-09-11T14:22:08Z",
  "updated_at": "2026-09-13T20:04:12Z",
  "history": [
    { "at": "2026-09-12T08:05:41Z", "by": "kepler", "from": "backlog", "to": "in-progress" },
    { "at": "2026-09-13T20:04:12Z", "by": "{{input.model}}", "from": "in-progress", "to": "{{input.status}}" }
  ],
  "comments": [
    { "at": "2026-09-12T08:05:41Z", "by": "kepler", "text": "Picked up; writing the confinement check first." },
    { "at": "2026-09-13T20:04:12Z", "by": "{{input.model}}", "note": "{{input.note}}", "comment": "{{input.comment}}" }
  ]
}
