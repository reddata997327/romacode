//! `phone` — run the legacy in-process handset simulator for development only.
//!
//!     phone serve [--port N] [--display WxH] [--density D] [--data-dir P]
//!
//! This synthetic runtime is not Android and does not execute APKs. It listens
//! on 127.0.0.1:<port> with an ADB-like protocol for legacy tests only.
//!
//!     mandroid device 127.0.0.1:5555
//!     mandroid screen --target 127.0.0.1:5555 --screenshot home.png

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "phone",
    about = "Legacy RomaCode handset simulator (not an Android OS; APKs do not execute)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Run the legacy simulator protocol server until the process is stopped.
    Serve {
        /// Preferred ADB port (default 5555; steps +2 when busy).
        #[arg(long, default_value_t = 5555)]
        port: u16,
        /// Display panel size WxH (default 720x1560).
        #[arg(long)]
        display: Option<String>,
        /// Display density dpi (default 420).
        #[arg(long, default_value_t = 420)]
        density: u32,
        /// Data directory (default ~/.romacode/android).
        #[arg(long)]
        data_dir: Option<String>,
        /// Quiet: suppress the startup banner.
        #[arg(long)]
        quiet: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve {
            port,
            display,
            density,
            data_dir,
            quiet,
        } => {
            let display = display
                .map(|d| -> anyhow::Result<(u32, u32)> {
                    let (w, h) = d
                        .split_once('x')
                        .ok_or_else(|| anyhow::anyhow!("--display expects WxH"))?;
                    let w: u32 = w.parse()?;
                    let h: u32 = h.parse()?;
                    // Cap dimensions before they reach the pixel allocator: a
                    // typo like `--display 10000x10000` would otherwise try to
                    // allocate ~400 MB per framebuffer and trip the u32 multiply
                    // overflow check inside `Canvas::new`. 4096 matches the
                    // hardware ceiling of every shipping Android panel.
                    const MAX_SIDE: u32 = 4096;
                    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                        anyhow::bail!(
                            "--display {w}x{h} out of range (min 1x1, max {MAX_SIDE}x{MAX_SIDE})"
                        );
                    }
                    Ok((w, h))
                })
                .transpose()?;
            let mut cfg = phone::PhoneConfig {
                port,
                density,
                ..Default::default()
            };
            if let Some((w, h)) = display {
                cfg.display = (w, h);
            }
            if let Some(d) = data_dir {
                cfg.data_dir = std::path::PathBuf::from(d);
            }
            let phone = phone::boot(cfg)?;
            if !quiet {
                eprintln!(
                    "RomaCode legacy simulator (not Android) — serial {} — local ADB-like server on 127.0.0.1:{} — data {}",
                    phone.serial,
                    phone.port,
                    phone.state.lock().unwrap().data_dir.display()
                );
            }
            // State is persisted every few seconds by the compositor, so a
            // SIGINT/SIGTERM from the terminal ends the run cleanly. On
            // graceful shutdown (romacode embeds the phone via the library
            // API), PhoneHandle::shutdown flushes and joins both threads.
            loop {
                std::thread::sleep(std::time::Duration::from_secs(600));
            }
        }
    }
}
