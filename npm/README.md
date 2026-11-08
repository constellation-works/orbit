# @orbit-tools/cli

npm binary proxy for the [Orbit](https://github.com/constellation-works/orbit) CLI.

On install, downloads the matching prebuilt `orbit` binary from
[GitHub Releases](https://github.com/constellation-works/orbit/releases), authenticates
the signed `orbit-checksums.txt` with the package-pinned release trust set,
verifies the archive SHA-256, and exposes it as the `orbit` command.

Downloads retry network errors, HTTP 429 and 5xx responses up to four attempts,
with 4, 8 and 16 seconds between attempts. If a GitHub release download still
fails transiently, the installer tries the GitHub REST asset endpoint. Both
paths require the same signed checksums and archive verification.

## Usage

```bash
# Install globally
npm install -g @orbit-tools/cli
orbit --version
orbit init  # on Linux, also prepares and verifies Bubblewrap for this user

# One-shot via npx
npx -y @orbit-tools/cli mcp serve
```

All arguments are forwarded to the native `orbit` binary.
The npm install lifecycle never requests administrator privileges. On Linux,
`orbit init` is the explicit onboarding step that prepares the host when
needed; a denied or unsupported host receives a precise readiness error.

## Supported platforms

- macOS arm64 / x64
- Linux arm64 / x64

### Windows

Open a WSL2 Linux shell first, then install Orbit inside it:

```bash
npm install -g @orbit-tools/cli
```

Windows via WSL2 has not yet been verified on a Windows host. Native Windows
is not supported; building from source does not change that. Running the npm
command in a native Windows shell fails with `EBADPLATFORM` because the package
supports macOS and Linux only. See the [Windows WSL2 guide](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/windows-wsl2.md) for setup.

## Environment variables

| Variable | Effect |
|---|---|
| `GITHUB_TOKEN` / `GH_TOKEN` | Optional token for the GitHub API download fallback (`GITHUB_TOKEN` takes precedence). Sent only to the API host. Public releases also support unauthenticated fallback. |
| `ORBIT_BINARY` | Path to a local `orbit` binary; bypasses download and trusts that path as the binary source. |
| `ORBIT_RELEASE_PUBLIC_KEY_FILE` | **Deprecated** in favor of `ORBIT_RELEASE_TRUSTED_KEYS_FILE`. Single-key override for the trusted checksum-signing public key; requires `ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1`, logs a deprecation notice when active. |
| `ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` | Required acknowledgement that `ORBIT_RELEASE_PUBLIC_KEY_FILE` replaces the release authenticity trust root. |
| `ORBIT_RELEASE_TRUSTED_KEYS_FILE` | Preferred test/operations override for the full trusted signing-key set, including key IDs, `not_after`, and `revoked_at`; requires `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` and logs when active. |
| `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` | Required acknowledgement that `ORBIT_RELEASE_TRUSTED_KEYS_FILE` replaces the release authenticity trust root. |
| `ORBIT_SKIP_DOWNLOAD=1` | Skip postinstall download (lazy install on first run still works). |

## License

MIT.

<!-- ORB-10117 -->
