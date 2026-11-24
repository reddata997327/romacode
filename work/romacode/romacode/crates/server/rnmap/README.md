# rnmap (vendored, built-in)

This directory ships the **rnmap 0.4.0** distribution that RomaCode embeds as
a built-in agent tool (`rnmap_scan`). It is installed automatically into the
user data directory on first use — no download, no external dependency.

| file | purpose |
|------|---------|
| `rnmap` | prebuilt Linux x86_64 binary (stripped, dynamically linked against glibc only) |
| `scripts/*.rhai` | 24 service-probe scripts (banner grab, http_*, ssh_*, smb_*, db probes, tls_cert_info, …) passed to `--script-dir` |

Provenance: rnmap 0.4.0 official release, `rnmap-v0.4.0-bin.tar.gz`
(sha256 of the binary:
`d4868e1d9a8c07617fa27cc30250a13122c792928b6f332e90a382e2ac5fb6d4`).
rnmap is a pure-Rust TCP/UDP port scanner with service/OS detection,
traceroute, Rhai scripting and versioned JSON output (`schema_version: 1.0`)
designed for agent parsing. It has no C dependencies.

The engine that drives this binary lives at `crates/server/src/rnmap/`.
Update the vendored files (binary + scripts together, same release) when
bumping versions.
