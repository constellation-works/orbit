# orbit-mcp

Speak MCP: stdio framing, advertised-name translation, structured responses, tool discovery, TCP listener, SSH stdio proxy, federated mux. A protocol crate, not a runtime — workspace resolution, validation, auditing, and authorization stay behind the `McpHost` trait in `orbit-core`.

- Internal deps are pinned by `tests/dep_boundary.rs` (`orbit-common`, `orbit-registry`, `orbit-tools`, `orbit-types`). Need more? Add a host method, not a manifest edge.
- `rmcp` appears only in this crate; translate its types at the adapter edge, never re-export.
- Advertised names are shipped contract; sanitization keeps canonical names inside the Cursor ∩ VS Code character set. A post-sanitization collision is a hard error, never a silent rename.
- The federated mux is the one place Orbit is an MCP client. Membership comes only from the operator's destinations file; the accepting machine is an implicit local destination. No caching between calls.
- Routing is fail-closed: unknown selectors are refused (`unknown_selector`), never guessed. The accepting machine always resolves its own workspaces and dispatches through Core.
