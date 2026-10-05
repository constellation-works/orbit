---
type: pattern
summary: "Newtype Wrapper"
last_validated: 2026-10-04
---
# Newtype Wrapper

Wrap a primitive (typically `String` or a numeric type) in a one-field struct with a private inner value and a *fallible* constructor that validates. Downstream code accepts the newtype and trusts the value is well-formed — invariants are enforced once, at the boundary.

```rust
pub struct Wrapper(Inner);

impl Wrapper {
    pub fn new(value: Inner) -> Result<Self, Error> {
        validate(&value)?;
        Ok(Self(value))
    }
    pub fn as_inner(&self) -> &Inner { &self.0 }
}
```

Sometimes phrased as "parse, don't validate": validate once at construction so no downstream caller has to.

## When to reach for it

- **A primitive carries protocol meaning.** Git ref names, content-addressed hashes, semantic-version strings, IDs — each has a well-formedness contract that callers shouldn't be re-checking.
- **The same value is passed through many layers.** Once a function receives `&str` it has no idea whether the caller validated. A typed wrapper carries the proof.
- **You're tempted to write `is_valid_X(&str) -> bool`.** That's the smell: validate-then-use leaves room for time-of-check-time-of-use bugs and forces every callsite to remember the check.

## When NOT to

- **The value never leaves one function.** Local strings don't need typing; validate inline.
- **The "primitive" is already typed.** `PathBuf`, `Uuid`, `chrono::DateTime` already wear their domain.
- **The primitive really is free-form.** A user-facing note, comment, or description has no contract to enforce.

`PluginSecretValue` (`crates/orbit-core/src/runtime/plugin/secrets.rs`) is a current production example: its `new` constructor rejects empty values, values above `MAX_PLUGIN_SECRET_BYTES`, and NUL bytes.

---

**Related: parsed sum-type form.** `Selector` (`crates/orbit-common/src/fs/selector.rs`) applies the same principle to a structured input string: `FromStr` parses `dir:`/`file:`/`symbol:` prefixes into an enum with a typed `SelectorParseError`. The enum variants have `pub` fields (a deliberate ergonomics trade for pattern matching at use sites), so the invariant is enforced by convention — every callsite uses `parse()` — rather than by visibility. Reach for this when the input has multiple legitimate shapes and pattern-matching the parsed result outweighs airtight construction.
