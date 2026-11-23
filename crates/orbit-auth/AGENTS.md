# orbit-auth

Orbit's credential code, so one crate carries the security review. It holds the lease listener's bearer **lease tokens** ([path-leases design §7](../../docs/design/path-leases/2_design.md#7-authority-and-authentication)).

## Rules

- Any Rust code that creates, hashes, checks or stores a lease token lives here. Surfaces wrap this crate (`orbit-web`'s lease listener wraps verification, the client-address rule and the limiter); none reimplements it. Path leases are not configurable [ORB-22808], so no command mints tokens.
- The token format is `orbl_` + 256 random bits from `getrandom`, hashed with SHA-256 and checked with the shared `orbit_common::security::token` minting and constant-time comparison primitives.
- The listener's client-address rule is the only reader of forwarding headers.
- Internal dependencies are `orbit-common` and `orbit-types` only: no HTTP framework, runtime handle or Orbit store. The token store is a separate file opened through `orbit_common::storage::sqlite`, never a table in Orbit's main store.
- The store is never created here. Root provisions it in the listener user's `0700` Orbit home; a user without one is refused with how to run as the listener user. Opening refuses a store or home other users can access or own.
- STD-05: a secret appears once, in the mint result. Never put it, or its hash, in an error, log, audit row or `Debug` output; `LeaseTokenSecret` has no `Display` and a redacting `Debug`. The `orbl_` pattern in `orbit_common::security::redaction` is the backstop, not the control.
- Schema changes append to `store::schema::MIGRATIONS`; never edit a shipped entry.
- What each module owns: [architecture page](../../docs/architecture/orbit-auth.md#ownership-by-module).
