mod cli;

use clap::Parser;
use cli::{Cli, Commands};
use colored::Colorize;
use scx_loader::{dbus::LoaderClientProxyBlocking, SchedMode, SupportedSched};
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

fn read_loader_snapshot(scx_loader: &LoaderClientProxyBlocking) -> Option<LoaderSnapshot> {
    let properties = zbus::blocking::fdo::PropertiesProxy::builder(scx_loader.inner().connection())
        .destination("org.scx.Loader")
        .ok()?
        .path("/org/scx/Loader")
        .ok()?
        .build()
        .ok()?;
    let mut props = properties
        .get_all(InterfaceName::try_from("org.scx.Loader").ok()?)
        .ok()?;
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
}
