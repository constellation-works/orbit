# @orbit-tools/cli

npm binary proxy for the [Orbit](https://github.com/constellation-works/orbit) CLI.

On install, downloads the matching prebuilt `orbit` binary from
[GitHub Releases](https://github.com/constellation-works/orbit/releases), authenticates
the signed `orbit-checksums.txt` with the package-pinned release trust set,
verifies the archive SHA-256, and exposes it as the `orbit` command.

## Usage

```bash
# Install globally
npm install -g @orbit-tools/cli
orbit --version

# One-shot via npx
npx -y @orbit-tools/cli mcp serve
```

All arguments are forwarded to the native `orbit` binary.

## Supported platforms

- macOS arm64 / x64
- Linux arm64 / x64

Windows is not currently published. Use WSL or build from source.

## Environment variables

| Variable | Effect |
|---|---|
| `ORBIT_BINARY` | Path to a local `orbit` binary; bypasses download and trusts that path as the binary source. |
| `ORBIT_RELEASE_PUBLIC_KEY_FILE` | **Deprecated** in favor of `ORBIT_RELEASE_TRUSTED_KEYS_FILE`. Single-key override for the trusted checksum-signing public key; requires `ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1`, logs a deprecation notice when active. |
| `ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` | Required acknowledgement that `ORBIT_RELEASE_PUBLIC_KEY_FILE` replaces the release authenticity trust root. |
| `ORBIT_RELEASE_TRUSTED_KEYS_FILE` | Preferred test/operations override for the full trusted signing-key set, including key IDs, `not_after`, and `revoked_at`; requires `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` and logs when active. |
| `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` | Required acknowledgement that `ORBIT_RELEASE_TRUSTED_KEYS_FILE` replaces the release authenticity trust root. |
| `ORBIT_SKIP_DOWNLOAD=1` | Skip postinstall download (lazy install on first run still works). |

## License

MIT.

<!-- ORB-10117 -->
