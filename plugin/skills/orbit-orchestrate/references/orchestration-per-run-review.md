# Select review for one delivery run

## Per-run review

`orbit run ship --review before-landing` and `orbit run auto --review
before-landing` (MCP `orbit_workflow` action `ship` and `orbit_workflow` action `auto` `start`
with `review: "before-landing"`) add a second-agent review before landing to
one run. Use it when the user asks for review on specific work and the
workspace applies none. It only adds review and never changes configuration.

`--review before-pr` is deprecated but still honoured as before-PR with a
bounded warning; prefer `before-landing` for new requests:

- If workspace policy already applies the same layer, the option is accepted
  unchanged. If policy applies the other layer, it is refused, naming the key
  and its source.
- It is refused before anything is submitted when
  the effective reviewer pool resolves no crew (`workflow.purpose_crews.pre_landing_review`
  when non-empty, falling back to `workflow.purpose_crews.review`), on local-only
  delivery (`--mode local`, or a local ship mode for a drain), and with `--pull`,
  where the owner decides review for its claims.
- A drain with the option holds tasks routed to local-only delivery in the
  backlog instead of starting them. Child runs inherit the layer, and a resume
  keeps it.
- `orbit run show` prints the captured layer and its origin on the `Review:`
  line (`.run.review_layer` in JSON).
- After-landing review stays the `delivery-code-review` auto-task.
