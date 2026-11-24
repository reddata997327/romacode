# Roxterm terminal-core integration

RomaCode vendors the `rox-protocol`, `rox-graphics`, and `rox-vt` crates from the user-supplied `roxterm-1.0.0-patched.zip`. The vendored manifests were adapted for this workspace, and `rox-vt::Screen::live_rows()` was added so RomaCode can render only the current viewport without cloning scrollback on every draw.

RomaCode uses `portable-pty` directly for local shell sessions. It intentionally does not use Roxterm's daemon, RPC/server, CLI, runbook agent, journal, tunnel/capture services, separate wgpu/winit viewer, or its PTY wrapper that generates shell-integration files and sources shell rc files.

The supplied Roxterm workspace manifests declare `MIT`, but the archive contained no `LICENSE`, `COPYING`, `NOTICE`, author declaration, or source history. This integration records that declaration but cannot independently verify it. Before redistribution outside the intended private project, verify the source rights and provide the actual required license and attribution notices.
