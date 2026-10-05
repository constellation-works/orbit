<!-- orbit-managed:start -->
## Orbit Workflow Rules

- **Task before work.** File an Orbit task before non-trivial code changes. Use the `orbit` skill (or `orbit.task.add`). Don't invent task IDs — `orbit.task.add` allocates them.
- **Tool surface over file edits.** Use `orbit.task.*` for task state. Read repository documentation with the available file tools. Decisions are git-reviewed entries in the design docs for the feature they govern; never edit files under `.orbit/` directly.
- **Route via the `orbit` skill.** Start sessions by reading the `orbit` skill (`<orbit-root>/skills/orbit/SKILL.md`). It is the entry point, and its reference table covers task authoring and execution, review, search, friction, workflows, and setup.
<!-- orbit-managed:end -->
