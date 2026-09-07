# OCM

OCM manages OpenClaw environments, runtimes, and services. This package contains
a small Node launcher and platform-specific Rust binaries, not a JavaScript
implementation or an install-time downloader. Rust is not required.

Supported: macOS ARM64/x64 and Linux x64 with glibc. Requires Node.js
`^22.15.0 || >=24.0.0` and optional dependencies.

```sh
npm install --global @openclaw/ocm
ocm --help
```

Update a global installation with `npm install --global @openclaw/ocm@latest`,
using the same Node installation and npm prefix. For a project-local installation,
run `npm install @openclaw/ocm@latest` in that project. `ocm self update` refuses
to overwrite npm-owned files; `ocm self update --check` reports GitHub binary
releases, not npm availability. Use `npm view @openclaw/ocm version` to check npm.

`npx --yes @openclaw/ocm@latest --help` is suitable for one-off commands. Its
temporary cache cannot own a background service. Install globally or in a durable
project before installing or refreshing services.

An npm update does not restart the running OCM daemon or managed gateways.
Inspect `ocm service status`, then schedule an explicit refresh:

```sh
ocm service refresh-daemon --acknowledge-gateway-restarts
```

Keep the installation path stable while services use it. Changing Node versions,
prefixes, or deleting a local project requires rebinding the daemon from the new
durable installation before removing the old one.

Each package's `release.json` records its signed source tag, commit, and GitHub
asset digests. Native payloads also record the executable SHA-256. npm provenance
identifies the protected packaging workflow; these embedded fields identify the
native release source and are covered by the package digest.

Project documentation: https://github.com/openclaw/ocm
