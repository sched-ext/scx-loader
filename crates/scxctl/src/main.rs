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
            println!("running {sched:?} in {sched_mode:?} mode");
        } else {
            println!(
                "running {sched:?} with arguments \"{}\"",
                current_args.join(" ")
            );
        }
    }
    Ok(())
}

fn cmd_list(scx_loader: &LoaderClientProxyBlocking) {
    match scx_loader.supported_schedulers() {
        Ok(sl) => {
            let supported_scheds = sl
                .iter()
                .map(|s| remove_scx_prefix(s))
                .collect::<Vec<String>>();
            println!("supported schedulers: {supported_scheds:?}");
        }
        Err(e) => {
            eprintln!("scheduler list failed: {e}");
            exit(1);
        }
    }
}

fn cmd_modes(
    scx_loader: &LoaderClientProxyBlocking,
    sched_name: String,
    show_args: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sched: SupportedSched = validate_sched(scx_loader, sched_name);

    if show_args {
        let mode_args: Vec<(SchedMode, Vec<String>)> =
            scx_loader.scheduler_mode_args(sched.clone())?;
        println!("configuration for {sched:?}:");
        for (mode, args) in mode_args {
            if args.is_empty() {
                println!("  {mode:?}: (not configured, runs with {sched:?}'s own defaults)");
            } else {
                println!("  {mode:?}: {}", args.join(" "));
            }
        }
    } else {
        let modes: Vec<SchedMode> = scx_loader.scheduler_modes(sched.clone())?;
        println!("modes configured for {sched:?}: {modes:?}");
        println!("(unlisted modes run with {sched:?}'s own defaults; use --show-args to see them all)");
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
/// actually finds out that the mode they picked has no effect, instead of
/// scxctl claiming a mode was applied when nothing about it actually changed
/// scheduler behavior.
fn check_mode_configured(
    scx_loader: &LoaderClientProxyBlocking,
    sched: &SupportedSched,
    mode: SchedMode,
) -> bool {
    if mode == SchedMode::Auto {
        return true;
    }

    // If the query itself fails, don't block the actual start/switch on it;
    // just skip the warning and let scx_loader do what it would do anyway.
    let Ok(configured_modes) = scx_loader.scheduler_modes(sched.clone()) else {
        return true;
    };

    let is_configured = configured_modes.contains(&mode);
    if !is_configured {
        println!(
            "{} {sched:?} has no configured arguments for {mode:?} mode; it will run with its own defaults",
            "warning:".yellow().bold()
        );
    }
    is_configured
}

fn cmd_start(
    scx_loader: &LoaderClientProxyBlocking,
    sched_name: String,
    mode_name: Option<SchedMode>,
    args: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify scx_loader is not running a scheduler
    if scx_loader.current_scheduler().unwrap() != "unknown" {
        println!(
            "{} scx scheduler already running, use '{}' instead of '{}'",
            "error:".red().bold(),
            "switch".bold(),
            "start".bold()
        );
        println!("\nFor more information, try '{}'", "--help".bold());
        exit(1);
    }

    let sched: SupportedSched = validate_sched(scx_loader, sched_name);
    let mode: SchedMode = mode_name.unwrap_or(SchedMode::Auto);
    if let Some(args) = args {
        scx_loader.start_scheduler_with_args(sched.clone(), &args.clone())?;
        println!("started {sched:?} with arguments \"{}\"", args.join(" "));
    } else if check_mode_configured(scx_loader, &sched, mode) {
        scx_loader.start_scheduler(sched.clone(), mode)?;
        println!("started {sched:?} in {mode:?} mode");
    } else {
        scx_loader.start_scheduler(sched.clone(), mode)?;
        println!("started {sched:?} (running with default scheduler arguments)");
    }
    Ok(())
}

fn cmd_switch(
    scx_loader: &LoaderClientProxyBlocking,
    sched_name: Option<String>,
    mode_name: Option<SchedMode>,
    args: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify scx_loader is running a scheduler
    if scx_loader.current_scheduler().unwrap() == "unknown" {
        println!(
            "{} no scx scheduler running, use '{}' instead of '{}'",
            "error:".red().bold(),
            "start".bold(),
            "switch".bold()
        );
        println!("\nFor more information, try '{}'", "--help".bold());
        exit(1);
    }

    let current_sched_name = scx_loader.current_scheduler().unwrap();
    let sched: SupportedSched = match sched_name {
        Some(sched_name) => validate_sched(scx_loader, sched_name),
        None => SupportedSched::try_from(current_sched_name.as_str()).unwrap(),
    };

    // Whether this switch is actually changing to a different scheduler, as
    // opposed to just changing the mode of the one already running.
    let target_sched_name: &str = sched.clone().into();
    let switching_scheduler = target_sched_name != current_sched_name;

    let mode: SchedMode = match mode_name {
        Some(mode_name) => mode_name,
        // Only inherit the currently active mode when switching within the
        // same scheduler. Switching to a *different* scheduler without an
        // explicit -m should start it fresh in Auto mode, rather than
        // silently carrying over a mode picked for the scheduler being
        // switched away from (which this new scheduler may not even have
        // configured).
        None if switching_scheduler => SchedMode::Auto,
        None => scx_loader.scheduler_mode().unwrap(),
    };
    if let Some(args) = args {
        scx_loader.switch_scheduler_with_args(sched.clone(), &args.clone())?;
        println!(
            "switched to {sched:?} with arguments \"{}\"",
            args.join(" ")
        );
    } else if check_mode_configured(scx_loader, &sched, mode) {
        scx_loader.switch_scheduler(sched.clone(), mode)?;
        println!("switched to {sched:?} in {mode:?} mode");
    } else {
        scx_loader.switch_scheduler(sched.clone(), mode)?;
        println!("switched to {sched:?} (running with default scheduler arguments)");
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
        println!("{} no default scheduler configured", "error:".red().bold());
        println!(
            "\nSet '{}' in your config file to use this command",
            "default_sched".bold()
        );
        exit(1);
    }

    scx_loader.restore_default()?;

    // Fetch the default mode for display
    let default_mode: SchedMode = scx_loader.default_mode()?;
    let sched = SupportedSched::try_from(default_scheduler.as_str())?;
    println!("restored default scheduler {sched:?} in {default_mode:?} mode");

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let conn = Connection::system()?;
    let scx_loader = LoaderClientProxyBlocking::new(&conn)?;

    match cli.command {
        Commands::Get => cmd_get(&scx_loader)?,
        Commands::List => cmd_list(&scx_loader),
        Commands::Modes { args } => cmd_modes(&scx_loader, args.sched, args.show_args)?,
        Commands::Start { args } => cmd_start(&scx_loader, args.sched, args.mode, args.args)?,
        Commands::Switch { args } => cmd_switch(&scx_loader, args.sched, args.mode, args.args)?,
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

fn ensure_scx_prefix(input: String) -> String {
    if !input.starts_with(SCHED_PREFIX) {
        return format!("{SCHED_PREFIX}{input}");
    }
    input
}

fn remove_scx_prefix(input: &str) -> String {
    if let Some(strip_input) = input.strip_prefix(SCHED_PREFIX) {
        return strip_input.to_string();
    }
    input.to_string()
}

fn validate_sched(scx_loader: &LoaderClientProxyBlocking, sched: String) -> SupportedSched {
    let raw_supported_scheds: Vec<String> = scx_loader.supported_schedulers().unwrap();
    let supported_scheds: Vec<String> = raw_supported_scheds
        .iter()
        .map(|s| remove_scx_prefix(s))
        .collect();
    if !supported_scheds.contains(&sched) && !raw_supported_scheds.contains(&sched) {
        println!(
            "{} invalid value '{}' for '{}'",
            "error:".red().bold(),
            &sched.yellow(),
            "--sched <SCHED>".bold()
        );
        println!("supported schedulers: {supported_scheds:?}");
        println!("\nFor more information, try '{}'", "--help".bold());
        exit(1);
    }

    SupportedSched::try_from(ensure_scx_prefix(sched).as_str()).unwrap()
}
