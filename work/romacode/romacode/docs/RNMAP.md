# rnmap_scan — the built-in network scanner

RomaCode ships the agent an `rnmap_scan` tool by default. It is a structured wrapper around
**rnmap 0.4.0** — a pure-Rust TCP/UDP port scanner with service/version detection, OS
fingerprinting, traceroute, TLS-certificate probing and 24 Rhai service-probe scripts — so the
model can answer questions like "what is listening on this box?" or "is the dev server on
192.168.1.20 reachable?" from real probe results instead of guessing.

The scanner reports versioned JSON (`schema_version: 1.0`); the tool summarises it into a
compact per-host open-port report (or hands over the full report with `mode = "full"`).

## Where the binary comes from

Nothing to install: the rnmap distribution (binary + scripts) is **embedded in the server
binary** and is written to the user data directory on first use
(`<data_dir>/romacode/rnmap/bin/rnmap` + `scripts/`, mode 0755/0644, atomic rename). Resolution
order: `rnmap.binary` override → the installed copy → a sibling of the server executable.
Provenance and hashes: `crates/server/rnmap/README.md`.

## Tool parameters

| Parameter | Meaning |
|---|---|
| `targets` *(required)* | Comma/space list: IP, CIDR (`10.0.0.0/24`), range (`10.0.0.1-50`) or hostname. |
| `ports` / `top_ports` | Port spec (`80`, `1-1000`, `22,80,443`, `T:80,U:53`) or the top N common ports. Default from `rnmap.default_ports`. |
| `scan_type` | `connect` (unprivileged default), `syn`, `ack`, `fin`, `xmas`, `null`, `udp`, `ping` (host discovery only). Raw types need root/CAP_NET_RAW and fall back to connect with a warning otherwise. |
| `timing` | 0 (paranoid) … 5 (insane); default from `rnmap.default_timing` (3). |
| `service`, `os`, `traceroute`, `tls`, `reason` | Feature probes (`-V`, `-O`, `--traceroute`, `--tls`, `--reason`). |
| `scripts` | Rhai probe list or `all` (`banner_grab`, `http_title`, `tls_cert_info`, …). |
| `profile` | `quick`, `web`, `internal`, `full`, `stealth` presets. |
| `max_rate`, `no_ping`, `exclude` | Rate cap, skip host discovery, exclusions. |
| `mode` | `summary` (default) or `full` (complete rnmap JSON report). |

## Safety model

The whole permission policy ships fully open — the scanner runs without asking, for any
target, like every other tool:

* **THE gate:** `rnmap_scan` is in the `net` permission class. Like shell commands it
  **runs without asking** in every mode. The `permissions` rule strings
  (`net(10.*)` style patterns match the target string) and Plan-mode hard-stops are
  accepted for config compatibility but are inert; the approval popup machinery stays
  available for users who explicitly re-enable prompting.
* **Target resolution:** every target is resolved (with a bounded 10 s DNS timeout —
  a dead resolver cannot stall the scan) and classified. Loopback, RFC 1918, CGNAT,
  link-local, IPv6 ULA/link-local and the non-routable documentation/benchmark ranges
  are local; anything else — including public hostnames — is allowed. Unresolvable
  hostnames are rejected outright (never silently skipped).
* **Evasion options:** decoys and packet fragmentation are always permitted.
* **Structured JSON only:** the agent cannot request xml/grepable output, output files,
  resume/watch/diff modes or stdin target lists — no write path, no weird output formats.
* **Caps:** `rnmap.max_targets` (4096), whole-scan timeout `rnmap.timeout_secs` (300 s, 0
  disables it; the child and its process group are killed on overrun or cancel), output
  cap `rnmap.max_output_chars` (20 000 chars, UTF-8-safe truncation).
* Output capture uses unlinked private temp files (the child cannot reopen them by name), the
  same confinement trick as the Bash tool.

## Configuration (`rnmap` section)

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Offer the tool at all. |
| `binary` | *(empty)* | Explicit rnmap executable; empty = installed vendored copy. |
| `default_ports` | `1-1000` | Ports used when the model doesn't pick any. |
| `default_timing` | `3` | Timing template when not requested. |
| `timeout_secs` | `300` | Whole-scan wall clock cap (0 = none). |
| `max_targets` | `4096` | Target count cap per scan. |
| `max_output_chars` | `20000` | Result size cap returned to the model. |
| `allow_external_targets` | `true` | Permit public internet hosts (unrestricted policy: scans are never refused by target class). |
| `allow_evasion` | `true` | Permit decoys / packet fragmentation (unrestricted policy). |
| `allow_scripts` | `true` | Permit the Rhai service-probe scripts. |

The `permissions` rule strings are accepted for config compatibility but are inert: scans
never ask and are never refused by policy.

## Tests

`cargo test -p romacode-server` covers the engine end-to-end (`execute_runs_a_real_local_scan`
installs the vendored binary and scans localhost), the CIDR/hostname classification, the
fully-open security gate (every target, every option) and the summary extraction.
