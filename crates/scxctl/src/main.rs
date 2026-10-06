mod cli;

use clap::Parser;
use cli::{Cli, Commands};
use colored::Colorize;
use scx_loader::{SchedMode, SupportedSched, config::Sched, dbus::LoaderClientProxyBlocking};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::process::exit;
use zbus::blocking::Connection;
use zbus::names::InterfaceName;

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
                format_scheduler_args(&current_args)
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
    if let Some(raw_args) = args {
        let args = validate_args(&raw_args);
        scx_loader.start_scheduler_with_args(sched.clone(), &args)?;
        println!(
            "started {sched:?} with arguments \"{}\"",
            format_scheduler_args(&args)
        );
    } else {
        check_mode_configured(scx_loader, &sched, mode);
        scx_loader.start_scheduler(sched.clone(), mode)?;
        report_operation_outcome(scx_loader, "start", &sched, mode);
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

/// Loader state from a single `GetAll`, so the three values describe
/// one moment.
#[derive(Debug, PartialEq)]
struct LoaderSnapshot {
    scheduler: String,
    mode: SchedMode,
    args: Vec<String>,
    /// Instance identity from the same `GetAll`; `None` on older daemons.
    generation: Option<String>,
}

/// An uncached `org.freedesktop.DBus.Properties` proxy for the loader.
///
/// The typed proxy caches properties, which is wrong for reads that must
/// reflect the daemon answering *now* - snapshots and generation checks.
fn loader_properties(
    scx_loader: &LoaderClientProxyBlocking,
) -> zbus::Result<zbus::blocking::fdo::PropertiesProxy<'static>> {
    zbus::blocking::fdo::PropertiesProxy::builder(scx_loader.inner().connection())
        .destination("org.scx.Loader")?
        .path("/org/scx/Loader")?
        .build()
}

fn loader_interface() -> InterfaceName<'static> {
    InterfaceName::from_static_str_unchecked("org.scx.Loader")
}

fn read_loader_snapshot(scx_loader: &LoaderClientProxyBlocking) -> Option<LoaderSnapshot> {
    let properties = loader_properties(scx_loader).ok()?;
    let iface = loader_interface();
    // Read until two consecutive answers agree; see scxtui's status() for
    // the rationale. Capped disagreement fails open with the last answer.
    let mut props = properties.get_all(iface.clone()).ok()?;
    for _ in 0..2 {
        let again = properties.get_all(iface.clone()).ok()?;
        if again == props {
            break;
        }
        props = again;
    }
    Some(LoaderSnapshot {
        scheduler: String::try_from(props.remove("CurrentScheduler")?).ok()?,
        mode: SchedMode::try_from(props.remove("SchedulerMode")?).ok()?,
        args: Vec::<String>::try_from(props.remove("CurrentSchedulerArgs")?).ok()?,
        generation: props
            .remove("DaemonGeneration")
            .and_then(|value| String::try_from(value).ok()),
    })
}

/// The one line scxctl prints about loader state: plain observation, no
/// request reference.
fn loader_observation(snapshot: &LoaderSnapshot, mode_configured: bool) -> String {
    if snapshot.scheduler == "unknown" {
        "the loader now reports no scheduler running".to_owned()
    } else if !snapshot.args.is_empty() {
        format!(
            "the loader now reports {} with arguments \"{}\"",
            snapshot.scheduler,
            format_scheduler_args(&snapshot.args)
        )
    } else if mode_configured {
        format!(
            "the loader now reports {} in {:?} mode",
            snapshot.scheduler, snapshot.mode
        )
    } else {
        format!(
            "the loader now reports {} in {:?} mode (no configured arguments; scheduler defaults in effect)",
            snapshot.scheduler, snapshot.mode
        )
    }
}

/// Qualifier only when the same instance answered both calls: bus names
/// are never reused, so an unchanged generation read *after* the
/// follow-up rules out A→B→A.
fn same_instance_confirmed(
    snapshot_generation: Option<&str>,
    generation_after: Option<&str>,
) -> bool {
    matches!((snapshot_generation, generation_after), (Some(a), Some(b)) if a == b)
}

/// Constant within one instance, but the follow-up can be answered by a
/// replacement — truth from two instances is never mixed. Unconfirmable
/// (replacement, read failure, old daemon) fails open and withholds the
/// qualifier.
fn observed_mode_configured(
    scx_loader: &LoaderClientProxyBlocking,
    snapshot: &LoaderSnapshot,
) -> bool {
    if !snapshot.args.is_empty() || snapshot.scheduler == "unknown" {
        return true;
    }
    let Ok(sched) = SupportedSched::try_from(snapshot.scheduler.as_str()) else {
        return true;
    };
    if mode_is_configured(scx_loader, &sched, snapshot.mode) {
        return true;
    }
    let generation_after = scx_loader.daemon_generation().ok();
    !same_instance_confirmed(snapshot.generation.as_deref(), generation_after.as_deref())
}

/// One line claims acceptance, one claims observation, never tied
/// together — a matching snapshot still proves nothing about *this*
/// operation. A failed read downgrades to a warning; the operation
/// already succeeded.
fn report_operation_outcome(
    scx_loader: &LoaderClientProxyBlocking,
    action_request: &str,
    sched: &SupportedSched,
    requested: SchedMode,
) {
    println!("{action_request} request for {sched:?} accepted (requested {requested:?} mode)");
    match read_loader_snapshot(scx_loader) {
        Some(snapshot) => {
            let mode_configured = observed_mode_configured(scx_loader, &snapshot);
            println!("{}", loader_observation(&snapshot, mode_configured));
        }
        None => eprintln!(
            "{} current loader state could not be read",
            "warning:".yellow().bold()
        ),
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
    if let Some(raw_args) = args {
        let args = validate_args(&raw_args);
        scx_loader.switch_scheduler_with_args(sched.clone(), &args)?;
        println!(
            "switched to {sched:?} with arguments \"{}\"",
            format_scheduler_args(&args)
        );
    } else {
        check_mode_configured(scx_loader, &sched, mode);
        scx_loader.switch_scheduler(sched.clone(), mode)?;
        report_operation_outcome(scx_loader, "switch", &sched, mode);
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

/// The scheduler configuration the running daemon resolved, as far as it
/// is visible over D-Bus.
///
/// Shaped like the config file (`scx_loader::config::Config`) so output
/// produced from a matching loader schema parses back as one, with two
/// deliberate differences:
///
/// - every mode carries the arguments the daemon would actually use, so
///   built-in fallbacks are filled in and an empty list means "the
///   scheduler's own defaults" - re-reading the output yields the same
///   behavior, not the same file;
/// - `power_profiles` is absent, because the daemon does not expose it
///   over D-Bus. Leaving it out beats printing a default that may lie.
///
/// `scheds` is a `BTreeMap` so the output is stable and diffable across
/// runs and machines.
#[derive(Debug, PartialEq, Serialize)]
struct ConfigDump {
    default_sched: Option<String>,
    default_mode: SchedMode,
    scheds: BTreeMap<String, Sched>,
}

/// Resolved arguments per mode, as `SchedulerModeArgs` reports them.
type ModeArgs = Vec<(SchedMode, Vec<String>)>;

/// What one read of `DaemonGeneration` established.
///
/// `Unsupported` and `Unreadable` must stay distinct: only a daemon that
/// positively lacks the property may be accepted unconfirmed. Collapsing
/// a failed read into "no generation" would let two failures pass as a
/// legacy daemon and quietly void the fail-closed guarantee.
#[derive(Debug, PartialEq)]
enum Generation {
    /// The daemon reported this generation.
    Present(String),
    /// The daemon predates `DaemonGeneration`: absent from `GetAll`, or
    /// `Get` answered `UnknownProperty`.
    Unsupported,
    /// The property could not be read or decoded; the reason is kept for
    /// the final error.
    Unreadable(String),
}

impl Generation {
    fn from_value(value: zbus::zvariant::OwnedValue) -> Self {
        String::try_from(value).map_or_else(
            |err| Generation::Unreadable(format!("DaemonGeneration has the wrong type: {err}")),
            Generation::Present,
        )
    }

    /// From a `GetAll` answer: a missing key is the legacy daemon.
    fn from_get_all(value: Option<zbus::zvariant::OwnedValue>) -> Self {
        value.map_or(Generation::Unsupported, Generation::from_value)
    }

    /// From a `Get` answer: only `UnknownProperty` is the legacy daemon;
    /// any other error is a failed read.
    fn from_get(result: zbus::fdo::Result<zbus::zvariant::OwnedValue>) -> Self {
        match result {
            Ok(value) => Generation::from_value(value),
            Err(zbus::fdo::Error::UnknownProperty(_)) => Generation::Unsupported,
            Err(err) => Generation::Unreadable(format!("reading DaemonGeneration failed: {err}")),
        }
    }
}

/// One full read of the loader configuration, plus the `DaemonGeneration`
/// seen before and after it.
struct ConfigRead {
    dump: ConfigDump,
    generation_before: Generation,
    generation_after: Generation,
}

/// Why a read cannot be trusted to come from a single daemon instance.
///
/// The configuration is fixed for an instance's lifetime, so an
/// unchanged generation around the whole read rules out a mix of two
/// configs. Both sides `Unsupported` is a daemon that predates
/// `DaemonGeneration`: unconfirmable, but such a daemon cannot tell us
/// otherwise, so the read is accepted. Everything else is rejected - a
/// changed generation, an instance change across the legacy boundary,
/// or any read that failed.
fn generation_mismatch(before: &Generation, after: &Generation) -> Option<String> {
    match (before, after) {
        (Generation::Present(a), Generation::Present(b)) if a == b => None,
        (Generation::Unsupported, Generation::Unsupported) => None,
        (Generation::Unreadable(reason), _) | (_, Generation::Unreadable(reason)) => {
            Some(reason.clone())
        }
        _ => Some("scx_loader was replaced while its configuration was being read".to_owned()),
    }
}

/// Maps the per-mode arguments reported by `SchedulerModeArgs` onto the
/// config file's `Sched` shape. A mode the daemon did not report stays
/// `None`, i.e. omitted from TOML and `null` in JSON.
fn sched_from_mode_args(mode_args: ModeArgs) -> Sched {
    let mut sched = Sched::default();
    for (mode, args) in mode_args {
        let slot = match mode {
            SchedMode::Auto => &mut sched.auto_mode,
            SchedMode::Gaming => &mut sched.gaming_mode,
            SchedMode::PowerSave => &mut sched.powersave_mode,
            SchedMode::LowLatency => &mut sched.lowlatency_mode,
            SchedMode::Server => &mut sched.server_mode,
        };
        *slot = Some(args);
    }
    sched
}

/// Pure assembly of the dump from what the daemon reported, so the
/// shape can be tested without a running loader.
fn build_config_dump(
    default_sched: String,
    default_mode: SchedMode,
    mode_args: Vec<(String, ModeArgs)>,
) -> ConfigDump {
    ConfigDump {
        // "unknown" is the property's sentinel for "not configured".
        default_sched: (default_sched != "unknown").then_some(default_sched),
        default_mode,
        scheds: mode_args
            .into_iter()
            .map(|(name, args)| (name, sched_from_mode_args(args)))
            .collect(),
    }
}

fn take_property<K, T>(
    props: &mut HashMap<K, zbus::zvariant::OwnedValue>,
    name: &str,
) -> Result<T, Box<dyn std::error::Error>>
where
    K: std::borrow::Borrow<str> + std::hash::Hash + Eq,
    T: TryFrom<zbus::zvariant::OwnedValue>,
    T::Error: std::error::Error + 'static,
{
    let value = props
        .remove(name)
        .ok_or_else(|| format!("scx_loader did not report the {name} property"))?;
    Ok(T::try_from(value)?)
}

fn read_config_once(
    scx_loader: &LoaderClientProxyBlocking,
) -> Result<ConfigRead, Box<dyn std::error::Error>> {
    let properties = loader_properties(scx_loader)?;
    let mut props = properties.get_all(loader_interface())?;

    let generation_before = Generation::from_get_all(props.remove("DaemonGeneration"));
    let default_sched: String = take_property(&mut props, "DefaultScheduler")?;
    let default_mode: SchedMode = take_property(&mut props, "DefaultMode")?;
    let supported: Vec<String> = take_property(&mut props, "SupportedSchedulers")?;

    // Called by name rather than through the typed proxy: SchedulerModeArgs
    // takes the scheduler as a plain string on the wire, and a dump should
    // cover every scheduler the daemon knows, including ones this scxctl
    // build has no SupportedSched variant for.
    let mut mode_args = Vec::with_capacity(supported.len());
    for name in supported {
        let args: ModeArgs = scx_loader
            .inner()
            .call("SchedulerModeArgs", &(name.as_str(),))?;
        mode_args.push((name, args));
    }

    let generation_after =
        Generation::from_get(properties.get(loader_interface(), "DaemonGeneration"));

    Ok(ConfigRead {
        dump: build_config_dump(default_sched, default_mode, mode_args),
        generation_before,
        generation_after,
    })
}

/// Total reads before giving up on a daemon that keeps being replaced.
const CONFIG_READ_ATTEMPTS: usize = 3;

/// Reads the configuration, retrying while the daemon instance changes
/// underneath. Unlike the status snapshot, a mixed answer here fails
/// closed: a dump is meant to be authoritative, so a capped
/// disagreement is an error rather than a best guess.
fn read_config(
    scx_loader: &LoaderClientProxyBlocking,
) -> Result<ConfigDump, Box<dyn std::error::Error>> {
    let mut last_mismatch = String::new();
    for _ in 0..CONFIG_READ_ATTEMPTS {
        let read = read_config_once(scx_loader)?;
        match generation_mismatch(&read.generation_before, &read.generation_after) {
            None => return Ok(read.dump),
            Some(reason) => last_mismatch = reason,
        }
    }
    Err(format!(
        "could not confirm that one scx_loader instance answered the whole read \
         ({last_mismatch}); try again"
    )
    .into())
}

fn render_config(dump: &ConfigDump, json: bool) -> Result<String, Box<dyn std::error::Error>> {
    if json {
        Ok(serde_json::to_string_pretty(dump)? + "\n")
    } else {
        Ok(toml::to_string(dump)?)
    }
}

fn cmd_config(
    scx_loader: &LoaderClientProxyBlocking,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let dump = read_config(scx_loader)?;
    print!("{}", render_config(&dump, json)?);
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
        Commands::Config { args } => cmd_config(&scx_loader, args.json)?,
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

/// Formats an argument vector as a shell command line so token boundaries
/// remain visible.
fn format_scheduler_args(args: &[String]) -> String {
    shell_words::join(args)
}

/// Why user-supplied `--args` failed to expand into scheduler arguments.
#[derive(Debug, PartialEq)]
enum ArgsExpandError {
    /// A chunk failed shell-style parsing, e.g. an unclosed quote. This
    /// also covers quotes that span a comma: clap splits on ',' before
    /// quoting is interpreted, so each side of the comma arrives here as
    /// its own unbalanced chunk. The payload is a display-ready message.
    Parse(String),
    /// The input expanded to zero arguments (e.g. `--args '   '`). Passing
    /// an empty argument list to `StartSchedulerWithArgs` would silently
    /// mean something other than what the user typed, so the client
    /// rejects it instead of forwarding it to the daemon.
    Empty,
}

/// Expands the clap-split `--args` chunks into the final argument list
/// passed to `scx_loader`.
///
/// clap first splits the raw input on commas (`value_delimiter(',')`,
/// kept for compatibility with the historical format); each resulting
/// chunk is then shell-split via `shell-words`, and the results are
/// flattened in order. Consequences, deliberately:
///
/// - the historical comma-separated syntax remains supported,
/// - whitespace inside one chunk now separates arguments,
/// - quotes and backslashes are interpreted, not passed through
///   literally (`"--name \"foo bar\""` yields two tokens, the second
///   containing a space),
/// - a quoted region containing a comma cannot survive clap's earlier
///   split and surfaces as a parse error rather than silent garbage.
///
/// Pure by design, mirroring `resolve_sched_name`: no D-Bus and no
/// process exit, so the semantics can be unit-tested — and mirrored by
/// other clients — without a running daemon.
fn expand_scheduler_args(raw: &[String]) -> Result<Vec<String>, ArgsExpandError> {
    let mut expanded = Vec::new();
    for chunk in raw {
        let tokens = shell_words::split(chunk)
            .map_err(|err| ArgsExpandError::Parse(format!("{err} in '{chunk}'")))?;
        expanded.extend(tokens);
    }
    if expanded.is_empty() {
        return Err(ArgsExpandError::Empty);
    }
    Ok(expanded)
}

fn validate_args(raw: &[String]) -> Vec<String> {
    match expand_scheduler_args(raw) {
        Ok(args) => args,
        Err(ArgsExpandError::Parse(msg)) => {
            eprintln!(
                "{} invalid value for '{}': {msg}",
                "error:".red().bold(),
                "--args <ARGS>".bold()
            );
            eprintln!("\nQuotes must be balanced and cannot span a comma");
            exit(1);
        }
        Err(ArgsExpandError::Empty) => {
            eprintln!(
                "{} '{}' expanded to no arguments",
                "error:".red().bold(),
                "--args <ARGS>".bold()
            );
            eprintln!(
                "\nTo run a scheduler with its own defaults, omit '{}'",
                "--args".bold()
            );
            exit(1);
        }
    }
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

    /*
     * Shared --args semantics vector.
     *
     * Both scxctl and scxtui depend on shell-words directly and must
     * agree on these outcomes; there is deliberately no shared helper
     * crate. When touching this table, copy it verbatim into the scxtui
     * custom-args tests (and vice versa).
     *
     * Inputs are the chunks as produced by clap's value_delimiter(','),
     * i.e. already comma-split.
     */
    #[test]
    fn args_expansion_shared_vector_ok() {
        let cases: &[(&[&str], &[&str])] = &[
            // Comma style (the historical documented format): still supported.
            (&["--slice-us", "5000"], &["--slice-us", "5000"]),
            // Whitespace inside a single chunk now separates arguments.
            (
                &["--verbose --slice-us 5000"],
                &["--verbose", "--slice-us", "5000"],
            ),
            // Mixed: comma split first (clap), then shell split per chunk.
            (
                &["--verbose", "--slice-us 5000"],
                &["--verbose", "--slice-us", "5000"],
            ),
            // Double quotes are interpreted: value with a space stays one token.
            (&["--name \"foo bar\""], &["--name", "foo bar"]),
            // Single quotes likewise.
            (&["--name 'foo bar'"], &["--name", "foo bar"]),
            // Backslash escapes a space.
            (&["--path /tmp/a\\ b"], &["--path", "/tmp/a b"]),
            // An explicit empty token is explicit: passed through as-is.
            (&["\"\""], &[""]),
            // Empty values remain visible when attached to another option.
            (&["--name \"\""], &["--name", ""]),
        ];
        for (input, expected) in cases {
            let input: Vec<String> = input.iter().map(ToString::to_string).collect();
            let expected: Vec<String> = expected.iter().map(ToString::to_string).collect();
            assert_eq!(
                expand_scheduler_args(&input),
                Ok(expected),
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn args_expansion_shared_vector_errors() {
        // An unclosed quote in a chunk is a parse error.
        let unclosed = vec!["--name \"foo".to_string()];
        assert!(matches!(
            expand_scheduler_args(&unclosed),
            Err(ArgsExpandError::Parse(_))
        ));

        // A quoted region spanning a comma cannot survive clap's earlier
        // comma split: each side arrives as an unbalanced chunk. Surfacing
        // a parse error here (instead of silently mangled tokens) is the
        // intended behavior.
        let quote_spanning_comma = vec!["\"foo".to_string(), "bar\"".to_string()];
        assert!(matches!(
            expand_scheduler_args(&quote_spanning_comma),
            Err(ArgsExpandError::Parse(_))
        ));

        // Whitespace-only input expands to nothing: rejected client-side
        // instead of sending an empty list to the daemon.
        let blank = vec!["   ".to_string()];
        assert_eq!(expand_scheduler_args(&blank), Err(ArgsExpandError::Empty));

        // clap turns `--args ""` into a single empty chunk; same outcome.
        let empty_chunk = vec![String::new()];
        assert_eq!(
            expand_scheduler_args(&empty_chunk),
            Err(ArgsExpandError::Empty)
        );
    }

    /// Pins the actual clap behavior the shared vector assumes: the raw
    /// input is comma-split by `value_delimiter(',')` before expansion,
    /// and the `--args=VALUE` form carries values starting with a dash.
    #[test]
    fn args_expansion_matches_clap_output() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from([
            "scxctl",
            "start",
            "--sched",
            "bpfland",
            "--args=-s 20000,-m powersave,-I 100,-t 100",
        ])
        .expect("mixed shell-style and comma-separated arguments should parse");

        let Commands::Start { args } = cli.command else {
            panic!("expected start command");
        };
        let raw_args = args.args.expect("--args should be present");

        assert_eq!(
            raw_args,
            ["-s 20000", "-m powersave", "-I 100", "-t 100"]
                .map(str::to_string)
                .to_vec()
        );
        assert_eq!(
            expand_scheduler_args(&raw_args),
            Ok(["-s", "20000", "-m", "powersave", "-I", "100", "-t", "100"]
                .map(str::to_string)
                .to_vec())
        );
    }

    /// What `format_scheduler_args` renders must parse back into the same
    /// tokens when passed directly to `shell_words::split`, including empty
    /// strings, embedded quotes, and commas.
    #[test]
    fn scheduler_args_format_round_trips() {
        let args = ["--name", "foo bar", "", "a'b", "foo,bar"]
            .map(str::to_string)
            .to_vec();
        let formatted = format_scheduler_args(&args);

        assert_eq!(shell_words::split(&formatted), Ok(args));
    }

    fn snapshot(scheduler: &str, mode: SchedMode, args: &[&str]) -> LoaderSnapshot {
        LoaderSnapshot {
            scheduler: scheduler.to_owned(),
            mode,
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            generation: None,
        }
    }

    #[test]
    fn custom_args_win_over_the_mode_qualifier() {
        assert_eq!(
            loader_observation(&snapshot("scx_lavd", SchedMode::Gaming, &["--foo"]), false),
            "the loader now reports scx_lavd with arguments \"--foo\""
        );
    }

    #[test]
    fn qualifier_requires_the_same_generation_on_both_sides() {
        assert!(same_instance_confirmed(Some(":1.42"), Some(":1.42")));
        assert!(!same_instance_confirmed(Some(":1.42"), Some(":1.97")));
        assert!(!same_instance_confirmed(None, Some(":1.42")));
        assert!(!same_instance_confirmed(Some(":1.42"), None));
        assert!(!same_instance_confirmed(None, None));
    }

    #[test]
    fn observation_for_no_scheduler_running() {
        assert_eq!(
            loader_observation(&snapshot("unknown", SchedMode::Auto, &[]), true),
            "the loader now reports no scheduler running"
        );
    }

    #[test]
    fn observation_for_a_mode_based_scheduler() {
        assert_eq!(
            loader_observation(&snapshot("scx_lavd", SchedMode::LowLatency, &[]), true),
            "the loader now reports scx_lavd in LowLatency mode"
        );
    }

    #[test]
    fn observation_for_an_unconfigured_mode_carries_the_defaults_qualifier() {
        assert_eq!(
            loader_observation(&snapshot("scx_flash", SchedMode::Gaming, &[]), false),
            "the loader now reports scx_flash in Gaming mode (no configured arguments; scheduler defaults in effect)"
        );
    }

    #[test]
    fn observation_for_a_scheduler_with_custom_arguments() {
        assert_eq!(
            loader_observation(
                &snapshot("scx_lavd", SchedMode::Auto, &["--performance"]),
                true
            ),
            "the loader now reports scx_lavd with arguments \"--performance\""
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
    fn sample_dump() -> ConfigDump {
        build_config_dump(
            "scx_lavd".to_owned(),
            SchedMode::Gaming,
            vec![
                (
                    "scx_lavd".to_owned(),
                    vec![
                        (SchedMode::Auto, vec![]),
                        (SchedMode::Gaming, vec!["--performance".to_owned()]),
                        (SchedMode::PowerSave, vec!["--powersave".to_owned()]),
                        (SchedMode::LowLatency, vec![]),
                        (SchedMode::Server, vec![]),
                    ],
                ),
                (
                    "scx_bpfland".to_owned(),
                    vec![
                        (SchedMode::Auto, vec![]),
                        (
                            SchedMode::LowLatency,
                            vec!["-m".to_owned(), "performance".to_owned()],
                        ),
                    ],
                ),
            ],
        )
    }

    #[test]
    fn config_dump_maps_modes_onto_config_fields() {
        let dump = sample_dump();
        let lavd = &dump.scheds["scx_lavd"];
        assert_eq!(lavd.auto_mode, Some(vec![]));
        assert_eq!(lavd.gaming_mode, Some(vec!["--performance".to_owned()]));
        assert_eq!(lavd.powersave_mode, Some(vec!["--powersave".to_owned()]));

        // A mode the daemon did not report stays unset rather than
        // being invented as "no arguments".
        let bpfland = &dump.scheds["scx_bpfland"];
        assert_eq!(bpfland.gaming_mode, None);
        assert_eq!(
            bpfland.lowlatency_mode,
            Some(vec!["-m".to_owned(), "performance".to_owned()])
        );
    }

    #[test]
    fn config_dump_treats_unknown_default_as_unset() {
        let dump = build_config_dump("unknown".to_owned(), SchedMode::Auto, vec![]);
        assert_eq!(dump.default_sched, None);
        let json = render_config(&dump, true).unwrap();
        assert!(json.contains("\"default_sched\": null"), "{json}");
        let toml = render_config(&dump, false).unwrap();
        assert!(!toml.contains("default_sched"), "{toml}");
    }

    #[test]
    fn config_dump_is_sorted_by_scheduler_name() {
        let dump = sample_dump();
        let names: Vec<&str> = dump.scheds.keys().map(String::as_str).collect();
        assert_eq!(names, ["scx_bpfland", "scx_lavd"]);
    }

    fn assert_parses_as_loader_config(config: &scx_loader::config::Config, dump: &ConfigDump) {
        assert_eq!(config.default_sched, Some(SupportedSched::Lavd));
        assert_eq!(config.default_mode, Some(SchedMode::Gaming));
        let scheds: BTreeMap<String, Sched> = config.scheds.clone().into_iter().collect();
        assert_eq!(scheds, dump.scheds);
    }

    /// The TOML output is a valid loader config file: it parses back
    /// with the loader's own types and keeps every value.
    #[test]
    fn config_toml_round_trips_through_loader_config() {
        let dump = sample_dump();
        let toml = render_config(&dump, false).unwrap();
        let config: scx_loader::config::Config = toml::from_str(&toml).unwrap();
        assert_parses_as_loader_config(&config, &dump);
    }

    /// The JSON output uses the same field names and value spellings as
    /// the config file, so it deserializes into the loader's types too.
    #[test]
    fn config_json_round_trips_through_loader_config() {
        let dump = sample_dump();
        let json = render_config(&dump, true).unwrap();
        let config: scx_loader::config::Config = serde_json::from_str(&json).unwrap();
        assert_parses_as_loader_config(&config, &dump);
    }

    #[test]
    fn config_read_requires_one_daemon_instance() {
        use Generation::{Present, Unreadable, Unsupported};
        let present = |g: &str| Present(g.to_owned());
        let failed = || Unreadable("boom".to_owned());

        assert_eq!(generation_mismatch(&present("a"), &present("a")), None);
        // Pre-generation daemon: unconfirmable, accepted.
        assert_eq!(generation_mismatch(&Unsupported, &Unsupported), None);

        // Replaced mid-read, including across the legacy boundary.
        for (before, after) in [
            (present("a"), present("b")),
            (Unsupported, present("b")),
            (present("a"), Unsupported),
        ] {
            assert!(
                generation_mismatch(&before, &after).is_some(),
                "{before:?} -> {after:?}"
            );
        }

        // A failed read never passes, whatever the other side says - in
        // particular two failures must not pass as a legacy daemon.
        for (before, after) in [
            (failed(), failed()),
            (failed(), Unsupported),
            (Unsupported, failed()),
            (present("a"), failed()),
            (failed(), present("a")),
        ] {
            assert_eq!(
                generation_mismatch(&before, &after),
                Some("boom".to_owned()),
                "{before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn generation_read_tells_legacy_daemon_from_failure() {
        use zbus::fdo::Error;
        use zbus::zvariant::{OwnedValue, Value};

        let string_value = || OwnedValue::try_from(Value::from("gen-1")).unwrap();
        let wrong_type = || OwnedValue::from(7u32);

        assert_eq!(
            Generation::from_get_all(Some(string_value())),
            Generation::Present("gen-1".to_owned())
        );
        assert_eq!(Generation::from_get_all(None), Generation::Unsupported);
        assert!(matches!(
            Generation::from_get_all(Some(wrong_type())),
            Generation::Unreadable(_)
        ));

        assert_eq!(
            Generation::from_get(Ok(string_value())),
            Generation::Present("gen-1".to_owned())
        );
        assert_eq!(
            Generation::from_get(Err(Error::UnknownProperty("DaemonGeneration".to_owned()))),
            Generation::Unsupported
        );
        for err in [
            Error::Failed("bus went away".to_owned()),
            Error::NoReply("timeout".to_owned()),
            Error::InvalidArgs("DaemonGeneration".to_owned()),
        ] {
            assert!(matches!(
                Generation::from_get(Err(err)),
                Generation::Unreadable(_)
            ));
        }
        assert!(matches!(
            Generation::from_get(Ok(wrong_type())),
            Generation::Unreadable(_)
        ));
    }

    /// A newer daemon can report schedulers this build has no
    /// `SupportedSched` variant for. The dump must carry their names
    /// verbatim; the round-trip through the local `Config` cannot apply
    /// here (its `default_sched` is `Option<SupportedSched>`), so this is
    /// checked against the generic TOML/JSON value trees instead.
    #[test]
    fn config_dump_keeps_schedulers_unknown_to_this_build() {
        let dump = build_config_dump(
            "scx_from_the_future".to_owned(),
            SchedMode::Auto,
            vec![(
                "scx_from_the_future".to_owned(),
                vec![(SchedMode::Gaming, vec!["--fast".to_owned()])],
            )],
        );

        let json: serde_json::Value =
            serde_json::from_str(&render_config(&dump, true).unwrap()).unwrap();
        assert_eq!(json["default_sched"], "scx_from_the_future");
        assert_eq!(
            json["scheds"]["scx_from_the_future"]["gaming_mode"],
            serde_json::json!(["--fast"])
        );

        let toml: toml::Table = render_config(&dump, false).unwrap().parse().unwrap();
        assert_eq!(toml["default_sched"].as_str(), Some("scx_from_the_future"));
        assert_eq!(
            toml["scheds"]["scx_from_the_future"]["gaming_mode"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn config_command_parses_json_switch() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from(["scxctl", "config", "--json"]).unwrap();
        assert!(matches!(cli.command, Commands::Config { args } if args.json));

        let cli = Cli::try_parse_from(["scxctl", "config"]).unwrap();
        assert!(matches!(cli.command, Commands::Config { args } if !args.json));
    }
}
