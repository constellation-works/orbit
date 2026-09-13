---
expect:
  id: /^[A-Z]+-\d+$/
---
{
  "id": "{{input.id}}",
  "title": "Detect stale launchd unit running an older orbit binary",
  "status": "{{input.status}}",
  "complexity": "medium",
  "crew": "kepler",
  "updated_at": "2026-09-12T10:00:00Z",
  "history": [
    { "at": "2026-09-12T10:00:00Z", "by": "{{input.model}}", "from": "in-review", "to": "{{input.status}}", "note": "{{input.note}}" }
  ]
}
