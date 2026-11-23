# Report a run diagnosis

## Report Format

```markdown
<run_id> failed in <step_id>/<activity_name>.

Primary cause: <one sentence>.

Evidence:
- Task(s): <ids>
- Run state: <state>, started <timestamp>, finished <timestamp or still running>
- First failed event: <event type / step / activity>
- Key error: <short error text>
- Relevant stdout/stderr/audit source: <command, blob ref, or file path>

Current state:
- Process: <not running | running pid/pgid | killed>
- Task: <status and important metadata>
- Branch/PR: <if relevant>

Next step: <specific recommended action>
```

Keep the report short unless the human asks for a full forensic trace.

## Validation Checklist

| Validation checklist |
|---|
| Before finalizing a diagnosis, verify: you matched the right run id and task id(s); you identified the first failed step, not just the last logged error; you checked recovery events when present; you checked stdout/stderr blobs for the failing invocation when available; you checked live process state for runs marked `running`; you separated root cause from downstream fallout; you recorded friction ([friction.md](../../orbit/references/friction.md)) if Orbit diagnostics or recovery behavior were misleading. |
