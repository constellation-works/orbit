---
type: pattern
summary: "Command Pattern"
last_validated: 2026-09-12
---
# Command Pattern

In this codebase, Command = the `Tool` trait at `crates/orbit-tools/src/lib.rs:286`:

```rust
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError>;
}
```

The registry at `crates/orbit-tools/src/registry.rs:32` stores `Arc<dyn Tool>` keyed by `ToolSchema::name`. Adding a tool means: writing a struct, `impl Tool`, registering in `builtin::register_builtins`. The dispatcher never changes.

## Destructive CLI confirmation

Destructive CLI commands never prompt or read stdin. An irreversible single action exposes
`--confirm` and refuses before mutation when it is absent. A migration or bulk
cleanup command is non-destructive by default (report/dry-run) and uses
`--confirm` to apply. Existing spellings may remain as compatibility aliases
(`--yes` for worktree GC), but new commands
must not introduce another confirmation spelling. Reversible mutations should
instead document and test their recovery command.

Two codebase-specific shapes carry non-obvious lessons; everything else is straightforward `impl Tool`.

## Reference: host-action dispatcher (`OrbitPipelineInvokeTool`)

The most common shape in the `orbit.*` namespace. The struct declares the schema; execution delegates to an `OrbitToolHost` via an action enum. From `crates/orbit-tools/src/builtin/orbit/pipeline/invoke.rs:7`:

```rust
pub struct OrbitPipelineInvokeTool;

impl Tool for OrbitPipelineInvokeTool {
    fn schema(&self) -> ToolSchema { /* params + identity_params() */ }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::PipelineInvoke)
    }
}
```

`execute_host_action` (`orbit/mod.rs:273`) resolves the caller's identity, requires a host on the context, and forwards the action, input, identity, and reservation metadata into the runtime.

Patterns to copy:

- **A new tool of this kind lands in three places.** New struct in `orbit/<area>/<verb>.rs`, new variant in `OrbitBuiltinAction`, new match arm in the host's `execute()`. The dispatcher and registry are untouched.
- **Schema in `orbit-tools`; logic in `orbit-core`.** This is the rule that keeps `orbit-tools` free of runtime / store dependencies per the architecture diagram in `ARCHITECTURE.md`. If your tool needs the task store, the activity-job engine, or sandboxed exec, it must dispatch through the host — don't pull those deps into `orbit-tools`.
- **MCP-only adapters stay in `orbit-mcp`.** Global host/workspace discovery is not a generic `ToolRegistry` command. Its schema and projection live beside MCP framing and are composed with the caller-supplied host; adding one does not add an `OrbitBuiltinAction` or Core `run_tool` arm.

---

**Not Command — same code shape, different role.** The codebase also uses
`Box<dyn Trait>` where every implementation is a parallel algorithm for the
same operation. That is Strategy, not Command: selection is by configuration,
not by a caller naming an operation. The live example is
`AgentRuntimeFactory::build` in
`crates/orbit-agent/src/runtime/factory.rs`, with provider factories producing
the configured `Box<dyn AgentRuntime>`.
