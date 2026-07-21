// SPDX-License-Identifier: GPL-2.0

mod app;
mod backend;
mod kernel;
mod logs;
mod ui;

use anyhow::{bail, Context, Result};

use app::{make_backend, App, BackendKind};

const USAGE: &str = "\
scxtui — TUI for managing sched_ext schedulers

USAGE:
    scxtui [--backend <loader|service>]

OPTIONS:
    --backend <loader|service>  Force a backend instead of auto-detection
                                (loader = scx_loader over D-Bus,
                                 service = scx.service via systemctl)
    -h, --help                  Show this help
";

fn main() -> Result<()> {
    let requested = parse_args()?;

    // Backend resolution happens *before* entering raw mode, so failures
    // print as normal error messages instead of garbling the terminal.
    // Auto-detection prefers the loader and falls back to scx.service.
    let mut fallback_note = None;
    let (kind, backend) = match requested {
        Some(kind) => (kind, make_backend(kind)?),
        None => match make_backend(BackendKind::Loader) {
            Ok(backend) => (BackendKind::Loader, backend),
            Err(loader_err) => match make_backend(BackendKind::Service) {
                Ok(backend) => {
                    fallback_note = Some(format!(
                        "scx_loader unavailable ({loader_err:#}) — using the scx.service backend"
                    ));
                    (BackendKind::Service, backend)
                }
                Err(service_err) => bail!(
                    "no usable backend found.\n  scx_loader: {loader_err:#}\n  scx.service: {service_err:#}"
                ),
            },
        },
    };

    let mut app = App::new(kind, backend)?;
    if let Some(note) = fallback_note {
        app.notify(&note);
    }

    let terminal = ratatui::init();
    let result = app.run(terminal);
    ratatui::restore();
    result
}

fn parse_args() -> Result<Option<BackendKind>> {
    let mut kind = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => {
                let value = args.next().context("--backend requires a value")?;
                kind = Some(parse_backend(&value)?);
            }
            _ if arg.starts_with("--backend=") => {
                kind = Some(parse_backend(&arg["--backend=".len()..])?);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}\n\n{USAGE}"),
        }
    }
    Ok(kind)
}

fn parse_backend(value: &str) -> Result<BackendKind> {
    match value {
        "loader" => Ok(BackendKind::Loader),
        "service" => Ok(BackendKind::Service),
        other => bail!("unknown backend {other:?} (expected \"loader\" or \"service\")"),
    }
}
