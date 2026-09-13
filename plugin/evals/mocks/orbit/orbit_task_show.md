---
expect:
  id: /^[A-Z]+-\d+$/
---
{
  "id": "{{input.id}}",
  "title": "Gate command_exec cwd to the task worktree",
  "description": "`orbit_command_exec` accepts any absolute cwd; confine it to the run's worktree or a registered workspace root.",
  "acceptance_criteria": [
    "A cwd outside the worktree returns a refusal error naming the boundary",
    "cargo test -p orbit-mcp passes with a new confinement test"
  ],
  "status": "in-progress",
  "priority": "high",
  "complexity": "medium",
  "type": "bug",
  "crew": "kepler",
  "resolved_crew": "kepler",
  "orchestrator": "sol",
  "tags": ["qa-sweep-0.21.0", "security"],
  "context_files": ["file:crates/orbit-mcp/src/tools/command_exec.rs"],
  "dependencies": [],
  "pr_status": null,
  "created_by": "claude",
  "implemented_by": "claude",
  "created_at": "2026-09-11T14:22:08Z",
  "updated_at": "2026-09-12T08:05:41Z",
  "comments": [
    { "at": "2026-09-12T08:05:41Z", "by": "kepler", "text": "Picked up; writing the confinement check first." }
  ]
}
