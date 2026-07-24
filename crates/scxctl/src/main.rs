mod cli;

use clap::Parser;
use cli::{Cli, Commands};
use colored::Colorize;
use scx_loader::{dbus::LoaderClientProxyBlocking, SchedMode, SupportedSched};
use std::process::exit;
use zbus::blocking::Connection;

fn cmd_get(scx_loader: &LoaderClientProxyBlocking) -> Result<(), Box<dyn std::error::Error>> {
    let current_scheduler: String = scx_loader.current_scheduler()?;

    if current_scheduler.as_str() == "unknown" {
        println!("no scx scheduler running");
    } else {
        let sched = SupportedSched::try_from(current_scheduler.as_str())?;
        let current_args: Vec<String> = scx_loader.current_scheduler_args()?;

        if current_args.is_empty() {
            let sched_mode: SchedMode = scx_loader.scheduler_mode()?;
            let mode_configured = mode_is_configured(scx_loader, &sched, sched_mode);
            report_mode_result("running", &sched, sched_mode, mode_configured);
        } else {
            println!(
                "running {sched:?} with arguments \"{}\"",
                current_args.join(" ")
            );
        }
    }
    Ok(())
}

fn cmd_list(scx_loader: &LoaderClientProxyBlocking) -> Result<(), Box<dyn std::error::Error>> {
    let supported_scheds = scx_loader
        .supported_schedulers()?
        .iter()
        .map(|s| remove_scx_prefix(s))
        .collect::<Vec<String>>();
    println!("supported schedulers: {supported_scheds:?}");
    Ok(())
}

fn cmd_modes(
    scx_loader: &LoaderClientProxyBlocking,
    sched_name: &str,
    show_args: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sched: SupportedSched = validate_sched(scx_loader, sched_name)?;

    if show_args {
        let mode_args: Vec<(SchedMode, Vec<String>)> =
            scx_loader.scheduler_mode_args(sched.clone())?;
        println!("configuration for {sched:?}:");
        for (mode, args) in mode_args {
            if args.is_empty() {
                if mode == SchedMode::Auto {
                    println!("  {mode:?}: (uses {sched:?}'s own defaults)");
                } else {
                    println!("  {mode:?}: (not configured, uses {sched:?}'s own defaults)");
                }
            } else {
                println!("  {mode:?}: {}", args.join(" "));
            }
        }
    } else {
        let modes: Vec<SchedMode> = scx_loader.scheduler_modes(sched.clone())?;
        println!("modes configured for {sched:?}: {modes:?}");
        println!(
            "(unlisted modes run with {sched:?}'s own defaults; use --show-args to see them all)"
        );
    }
    Ok(())
}

/// Checks whether `mode` has configured arguments for `sched`, warning the
/// user if it doesn't, and returns whether it does.
///
/// `scx_loader` itself only logs the "no configured args" case server-side
/// (e.g. to the systemd journal), which an interactive `scxctl` user would
/// never see. This makes the same check client-side, using the
/// `SchedulerModes` method, so the person running `scxctl start`/`switch`
/// actually finds out that no mode-specific arguments will be applied,
/// instead of scxctl implying that the selected mode has a dedicated
/// configuration when it does not.
/// Returns whether `mode` has configured arguments for `sched`.
///
/// `Auto` always counts as configured (it *is* the scheduler's own
/// defaults), and query failures count as configured too (fail-open), so
/// callers never block or mislead on a transient D-Bus error.
fn mode_is_configured(
    scx_loader: &LoaderClientProxyBlocking,
    sched: &SupportedSched,
    mode: SchedMode,
) -> bool {
    if mode == SchedMode::Auto {
        return true;
    }
    scx_loader
        .scheduler_modes(sched.clone())
        .map_or(true, |modes| modes.contains(&mode))
}

fn check_mode_configured(
    scx_loader: &LoaderClientProxyBlocking,
    sched: &SupportedSched,
    mode: SchedMode,
) -> bool {
    let is_configured = mode_is_configured(scx_loader, sched, mode);
    if !is_configured {
        eprintln!(
            "{} {sched:?} has no configured arguments for {mode:?} mode; it will run with its own defaults",
            "warning:".yellow().bold()
        );
    }
    is_configured
}

fn cmd_start(
    scx_loader: &LoaderClientProxyBlocking,
    sched_name: &str,
    mode_name: Option<SchedMode>,
    args: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify scx_loader is not running a scheduler
    let current_scheduler = scx_loader.current_scheduler()?;
    if current_scheduler != "unknown" {
        eprintln!(
            "{} scx scheduler already running, use '{}' instead of '{}'",
            "error:".red().bold(),
            "switch".bold(),
            "start".bold()
        );
        eprintln!("\nFor more information, try '{}'", "--help".bold());
        exit(1);
    }

    let sched: SupportedSched = validate_sched(scx_loader, sched_name)?;
    let mode: SchedMode = mode_name.unwrap_or(SchedMode::Auto);
    if let Some(args) = args {
        scx_loader.start_scheduler_with_args(sched.clone(), &args)?;
        println!("started {sched:?} with arguments \"{}\"", args.join(" "));
    } else {
        let mode_configured = check_mode_configured(scx_loader, &sched, mode);
        scx_loader.start_scheduler(sched.clone(), mode)?;
        report_mode_result("started", &sched, mode, mode_configured);
    }
    Ok(())
}

/// Prints the outcome of a start/switch operation, noting whether the
/// requested mode actually had configured arguments applied or the
/// scheduler fell back to its own defaults.
fn report_mode_result(
    action: &str,
    sched: &SupportedSched,
    mode: SchedMode,
    mode_configured: bool,
) {
    if mode_configured {
        println!("{action} {sched:?} in {mode:?} mode");
    } else {
        println!("{action} {sched:?} with its own defaults");
    }
}

/// Resolves which mode a `switch` should use. The current mode is fetched
/// lazily via `fetch_current_mode` so that callers only pay for the D-Bus
/// round-trip when it's actually needed (no explicit mode was requested and
/// we're not switching to a different scheduler).
fn resolve_switch_mode<E>(
    requested_mode: Option<SchedMode>,
    switching_scheduler: bool,
    fetch_current_mode: impl FnOnce() -> Result<SchedMode, E>,
) -> Result<SchedMode, E> {
    match requested_mode {
        Some(mode) => Ok(mode),
        None if switching_scheduler => Ok(SchedMode::Auto),
        None => fetch_current_mode(),
    }
}

fn cmd_switch(
    scx_loader: &LoaderClientProxyBlocking,
    sched_name: Option<&str>,
    mode_name: Option<SchedMode>,
    args: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify scx_loader is running a scheduler
    let current_sched_name = scx_loader.current_scheduler()?;
    if current_sched_name == "unknown" {
        eprintln!(
            "{} no scx scheduler running, use '{}' instead of '{}'",
            "error:".red().bold(),
            "start".bold(),
            "switch".bold()
        );
        eprintln!("\nFor more information, try '{}'", "--help".bold());
        exit(1);
    }

    let current_sched = SupportedSched::try_from(current_sched_name.as_str())?;

    // Whether this switch is actually changing to a different scheduler, as
    // opposed to just changing the mode of the one already running. Resolved
    // alongside `sched` so the `None` branch (no `-s` given) can move
    // `current_sched` straight through instead of cloning it just to satisfy
    // a comparison whose answer is already known to be `false`.
    let (sched, switching_scheduler): (SupportedSched, bool) = match sched_name {
        Some(sched_name) => {
            let sched = validate_sched(scx_loader, sched_name)?;
            let switching_scheduler = sched != current_sched;
            (sched, switching_scheduler)
        }
        None => (current_sched, false),
    };

    let mode = resolve_switch_mode(mode_name, switching_scheduler, || {
        scx_loader.scheduler_mode()
    })?;
    if let Some(args) = args {
        scx_loader.switch_scheduler_with_args(sched.clone(), &args)?;
        println!(
            "switched to {sched:?} with arguments \"{}\"",
            args.join(" ")
        );
    } else {
        let mode_configured = check_mode_configured(scx_loader, &sched, mode);
        scx_loader.switch_scheduler(sched.clone(), mode)?;
        report_mode_result("switched to", &sched, mode, mode_configured);
    }
    Ok(())
}

fn cmd_stop(scx_loader: &LoaderClientProxyBlocking) -> Result<(), Box<dyn std::error::Error>> {
    scx_loader.stop_scheduler()?;
    println!("stopped");
    Ok(())
}

fn cmd_restart(scx_loader: &LoaderClientProxyBlocking) -> Result<(), Box<dyn std::error::Error>> {
    scx_loader.restart_scheduler()?;
    println!("restarted");
    Ok(())
}

fn cmd_restore(scx_loader: &LoaderClientProxyBlocking) -> Result<(), Box<dyn std::error::Error>> {
    // Check if a default scheduler is configured
    let default_scheduler = scx_loader.default_scheduler()?;
    if default_scheduler == "unknown" {
        eprintln!("{} no default scheduler configured", "error:".red().bold());
        eprintln!(
            "\nSet '{}' in your config file to use this command",
            "default_sched".bold()
        );
        exit(1);
    }

    scx_loader.restore_default()?;

    // Fetch the default mode for display
    let default_mode: SchedMode = scx_loader.default_mode()?;
    let sched = SupportedSched::try_from(default_scheduler.as_str())?;
    let mode_configured = mode_is_configured(scx_loader, &sched, default_mode);
    report_mode_result(
        "restored default scheduler",
        &sched,
        default_mode,
        mode_configured,
    );

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let conn = Connection::system()?;
    let scx_loader = LoaderClientProxyBlocking::new(&conn)?;

    match cli.command {
        Commands::Get => cmd_get(&scx_loader)?,
        Commands::List => cmd_list(&scx_loader)?,
        Commands::Modes { args } => cmd_modes(&scx_loader, &args.sched, args.show_args)?,
        Commands::Start { args } => cmd_start(&scx_loader, &args.sched, args.mode, args.args)?,
        Commands::Switch { args } => {
            cmd_switch(&scx_loader, args.sched.as_deref(), args.mode, args.args)?;
        }
        Commands::Stop => cmd_stop(&scx_loader)?,
        Commands::Restart => cmd_restart(&scx_loader)?,
        Commands::Restore => cmd_restore(&scx_loader)?,
    }

    Ok(())
}

/*
 * Utilities
 */

const SCHED_PREFIX: &str = "scx_";

fn ensure_scx_prefix(input: &str) -> String {
    if input.starts_with(SCHED_PREFIX) {
        return input.to_string();
    }
    format!("{SCHED_PREFIX}{input}")
}

fn remove_scx_prefix(input: &str) -> String {
    if let Some(strip_input) = input.strip_prefix(SCHED_PREFIX) {
        return strip_input.to_string();
    }
    input.to_string()
}

/// Why a user-supplied scheduler name failed to resolve.
#[derive(Debug, PartialEq)]
enum SchedNameError {
    /// The name isn't on the list of schedulers reported by `scx_loader`.
    UnknownName,
    /// `scx_loader` reports the scheduler as supported, but this `scxctl`
    /// build doesn't have a matching `SupportedSched` variant (e.g. a newer
    /// `scx_loader` paired with an older `scxctl`).
    UnsupportedByClient,
}

/// Resolves a user-supplied scheduler name (with or without the "scx_"
/// prefix) against the list of supported schedulers reported by
/// `scx_loader`.
///
/// Pure by design: the D-Bus call stays in `validate_sched`, so the actual
/// resolution rules can be unit-tested without a running daemon.
fn resolve_sched_name(
    sched: &str,
    raw_supported_scheds: &[String],
) -> Result<SupportedSched, SchedNameError> {
    let known = raw_supported_scheds
        .iter()
        .any(|raw| raw.as_str() == sched || remove_scx_prefix(raw) == sched);
    if !known {
        return Err(SchedNameError::UnknownName);
    }
    SupportedSched::try_from(ensure_scx_prefix(sched).as_str())
        .map_err(|_| SchedNameError::UnsupportedByClient)
}

fn validate_sched(
    scx_loader: &LoaderClientProxyBlocking,
    sched: &str,
) -> Result<SupportedSched, Box<dyn std::error::Error>> {
    let raw_supported_scheds: Vec<String> = scx_loader.supported_schedulers()?;
    match resolve_sched_name(sched, &raw_supported_scheds) {
        Ok(resolved) => Ok(resolved),
        Err(SchedNameError::UnknownName) => {
            let supported_scheds: Vec<String> = raw_supported_scheds
                .iter()
                .map(|s| remove_scx_prefix(s))
                .collect();
            eprintln!(
                "{} invalid value '{}' for '{}'",
                "error:".red().bold(),
                sched.yellow(),
                "--sched <SCHED>".bold()
            );
            eprintln!("supported schedulers: {supported_scheds:?}");
            eprintln!("\nFor more information, try '{}'", "--help".bold());
            exit(1);
        }
        Err(SchedNameError::UnsupportedByClient) => {
            eprintln!(
                "{} scx_loader supports '{}', but this scxctl build does not; update scxctl",
                "error:".red().bold(),
                sched.yellow()
            );
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_to_different_scheduler_defaults_to_auto() {
        let mode = resolve_switch_mode(
            None,
            true,
            || -> Result<SchedMode, Box<dyn std::error::Error>> {
                panic!("current mode should not be fetched when switching scheduler")
            },
        )
        .unwrap();
        assert_eq!(mode, SchedMode::Auto);
    }

    #[test]
    fn switch_within_same_scheduler_keeps_current_mode() {
        let mode: SchedMode = resolve_switch_mode(None, false, || {
            Ok::<_, Box<dyn std::error::Error>>(SchedMode::Gaming)
        })
        .unwrap();
        assert_eq!(mode, SchedMode::Gaming);
    }

    #[test]
    fn explicit_switch_mode_always_wins() {
        let mode = resolve_switch_mode(
            Some(SchedMode::PowerSave),
            true,
            || -> Result<SchedMode, Box<dyn std::error::Error>> {
                panic!("current mode should not be fetched when an explicit mode is given")
            },
        )
        .unwrap();
        assert_eq!(mode, SchedMode::PowerSave);
    }

    fn reported_scheds() -> Vec<String> {
        vec!["scx_bpfland".to_string(), "scx_lavd".to_string()]
    }

    #[test]
    fn resolves_sched_name_with_prefix() {
        assert_eq!(
            resolve_sched_name("scx_lavd", &reported_scheds()),
            Ok(SupportedSched::Lavd)
        );
    }

    #[test]
    fn resolves_sched_name_without_prefix() {
        assert_eq!(
            resolve_sched_name("bpfland", &reported_scheds()),
            Ok(SupportedSched::Bpfland)
        );
    }

    #[test]
    fn rejects_unknown_sched_name() {
        assert_eq!(
            resolve_sched_name("notreal", &reported_scheds()),
            Err(SchedNameError::UnknownName)
        );
    }

    #[test]
    fn rejects_sched_known_to_daemon_but_not_client() {
        // A newer scx_loader can report a scheduler this scxctl build has no
        // SupportedSched variant for. That used to be an unwrap panic; it
        // must resolve to a distinct, actionable error instead.
        let reported = vec!["scx_from_the_future".to_string()];
        assert_eq!(
            resolve_sched_name("from_the_future", &reported),
            Err(SchedNameError::UnsupportedByClient)
        );
    }
}
