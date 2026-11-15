# orbit-mcp

Speak MCP: stdio framing, advertised-name translation, structured responses, tool discovery, TCP listener, SSH stdio proxy, federated mux. A protocol crate, not a runtime — workspace resolution, validation, auditing, and authorization stay behind the `McpHost` trait in `orbit-core`.

- Internal deps are pinned by `tests/boundary/dep_boundary.rs` (`orbit-common`, `orbit-registry`, `orbit-tools`, `orbit-types`). Need more? Add a host method, not a manifest edge.
- `rmcp` appears only in this crate; translate its types at the adapter edge, never re-export.
- Advertised names are shipped contract; sanitization keeps canonical names inside the Cursor ∩ VS Code character set. A post-sanitization collision is a hard error, never a silent rename.
- The federated mux is the one place Orbit is an MCP client. Membership comes only from the operator's host file (`orbit host`, read through `orbit-registry`); the accepting machine is an implicit local destination. No caching between calls of anything a destination said (snapshots, tool surfaces, health): every routed call runs live discovery and identity confirmation. The only thing carried over is the transport — at most one idle routed SSH session (`ssh` child plus remote `orbit mcp serve`) parked per destination for up to 60 s, reused only after a bounded `ping` health check and never when its last answer was lost or anything it sent is still unread (`RoutedSessionPool`). Opening a fresh session to a destination closes its parked one first. Listing always opens fresh sessions.
- Routing is fail-closed: unknown selectors are refused (`unknown_selector`), never guessed. The accepting machine always resolves its own workspaces and dispatches through Core.
