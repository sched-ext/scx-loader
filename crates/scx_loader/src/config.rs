// SPDX-License-Identifier: GPL-2.0
//
// Copyright (c) 2024-2025 Vladislav Nepogodin <vnepogodin@cachyos.org>

// This software may be used and distributed according to the terms of the
// GNU General Public License version 2.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;

use crate::SchedMode;
use crate::SupportedSched;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_sched: Option<SupportedSched>,
    pub default_mode: Option<SchedMode>,
    /// Allow `[scheds.NAME]` entries with a `path`, which define schedulers
    /// that aren't built in. Off by default.
    pub allow_custom_scheds: bool,
    pub scheds: HashMap<String, Sched>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Sched {
    /// Absolute path of the binary. Only allowed for custom schedulers,
    /// i.e. names that aren't built in, and required for them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub auto_mode: Option<Vec<String>>,
    pub gaming_mode: Option<Vec<String>>,
    pub lowlatency_mode: Option<Vec<String>>,
    pub powersave_mode: Option<Vec<String>>,
    pub server_mode: Option<Vec<String>>,
}

/// Initialize config from first found config path, overwise fallback to default config
///
/// # Errors
///
/// This function will return an error if a config file is found but fails to be parsed.
pub fn init_config() -> Result<Config> {
    if let Ok(config_path) = get_config_path() {
        parse_config_file(&config_path)
    } else {
        Ok(get_default_config())
    }
}

/// Parses the config file at the given path.
///
/// # Errors
///
/// This function will return an error if:
/// - The file cannot be read (e.g., permissions, not found).
/// - The file content is empty.
/// - The file content is not valid TOML.
/// - A custom scheduler entry or `default_sched` is invalid (see
///   `validate_config`).
pub fn parse_config_file(filepath: &str) -> Result<Config> {
    let file_content = fs::read_to_string(filepath)?;
    parse_config_content(&file_content)
}

/// Searches for and returns the path to the configuration file.
///
/// # Errors
///
/// This function will return an error if no config file is found in any of the
/// predefined locations.
pub fn get_config_path() -> Result<String> {
    let vendordir = option_env!("VENDORDIR").unwrap_or("/usr/share");
    // Search in system directories
    let check_paths = [
        // locations for user config
        "/etc/scx_loader/config.toml".to_owned(),
        "/etc/scx_loader.toml".to_owned(),
        // locations for distributions to ship default configuration
        format!("{vendordir}/scx_loader/config.toml").to_owned(),
        format!("{vendordir}/scx_loader.toml").to_owned(),
    ];
    for check_path in check_paths {
        if !Path::new(&check_path).exists() {
            continue;
        }
        // we found config path
        return Ok(check_path);
    }

    anyhow::bail!("Failed to find config!");
}

fn parse_config_content(file_content: &str) -> Result<Config> {
    if file_content.is_empty() {
        anyhow::bail!("The config file is empty!")
    }
    let config: Config = toml::from_str(file_content)?;
    validate_config(&config)?;
    Ok(config)
}

/// Checks the custom scheduler entries and `default_sched`.
///
/// A `[scheds.NAME]` entry with a `path` defines a custom scheduler, which
/// needs `allow_custom_scheds = true`. `NAME` must not be a built-in
/// scheduler, must look like `scx_[a-z0-9_]+`, and `path` must be absolute.
/// Entries without a `path` are left alone, so a config that still lists a
/// scheduler `scx_loader` no longer ships keeps loading.
fn validate_config(config: &Config) -> Result<()> {
    for (name, sched) in &config.scheds {
        let Some(path) = &sched.path else {
            continue;
        };
        if !config.allow_custom_scheds {
            anyhow::bail!(
                "[scheds.{name}]: `path` defines a custom scheduler, which needs \
                 `allow_custom_scheds = true`"
            );
        }
        match name.parse::<SupportedSched>() {
            Ok(SupportedSched::Custom(_)) => {}
            Ok(_) => anyhow::bail!(
                "[scheds.{name}]: `path` is only allowed for custom schedulers, and {name} is \
                 built in"
            ),
            Err(_) => anyhow::bail!(
                "[scheds.{name}]: a custom scheduler name must be `scx_` followed by lowercase \
                 letters, digits or `_`"
            ),
        }
        if !Path::new(path).is_absolute() {
            anyhow::bail!("[scheds.{name}]: `path` must be absolute, got {path:?}");
        }
    }

    if let Some(default_sched) = &config.default_sched
        && !is_known_sched(config, default_sched)
    {
        let name = default_sched.as_str();
        anyhow::bail!("default_sched {name} is not built in and [scheds.{name}] has no `path`");
    }

    Ok(())
}

/// Returns the configured path of the custom scheduler `name`, or `None` if
/// custom schedulers are off or the config doesn't define `name`.
fn custom_sched_path<'a>(config: &'a Config, name: &str) -> Option<&'a String> {
    if !config.allow_custom_scheds {
        return None;
    }
    config.scheds.get(name)?.path.as_ref()
}

/// Returns `true` if `scx_sched` can be started: it's built in, or the config
/// defines it as a custom scheduler.
#[must_use]
pub fn is_known_sched(config: &Config, scx_sched: &SupportedSched) -> bool {
    match scx_sched {
        SupportedSched::Custom(name) => custom_sched_path(config, name).is_some(),
        _ => true,
    }
}

/// Returns the program to run for `scx_sched`: the binary name of a built-in
/// scheduler, looked up in `PATH`, or the configured path of a custom one.
/// Returns `None` for a custom scheduler the config doesn't define.
#[must_use]
pub fn get_sched_program(config: &Config, scx_sched: &SupportedSched) -> Option<String> {
    match scx_sched {
        SupportedSched::Custom(name) => custom_sched_path(config, name).cloned(),
        builtin => Some(builtin.as_str().to_owned()),
    }
}

/// Returns the names of the custom schedulers defined in the config, sorted.
/// Empty unless `allow_custom_scheds` is set.
#[must_use]
pub fn get_custom_sched_names(config: &Config) -> Vec<String> {
    if !config.allow_custom_scheds {
        return Vec::new();
    }
    let mut names: Vec<String> = config
        .scheds
        .iter()
        .filter(|(_, sched)| sched.path.is_some())
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

pub fn get_default_config() -> Config {
    let supported_scheds = [
        SupportedSched::Bpfland,
        SupportedSched::Rusty,
        SupportedSched::Lavd,
        SupportedSched::Flash,
        SupportedSched::P2DQ,
        SupportedSched::Tickless,
        SupportedSched::Rustland,
        SupportedSched::Cosmos,
        SupportedSched::Beerland,
        SupportedSched::Cake,
        SupportedSched::Pandemonium,
        SupportedSched::Flow,
        SupportedSched::Forge,
        SupportedSched::MLFQ,
        SupportedSched::EEVDF,
    ];
    let scheds_map = HashMap::from(supported_scheds.map(init_default_config_entry));
    Config {
        default_sched: None,
        default_mode: Some(SchedMode::Auto),
        allow_custom_scheds: false,
        scheds: scheds_map,
    }
}

/// Get the scx flags for the given sched mode
pub fn get_scx_flags_for_mode(
    config: &Config,
    scx_sched: &SupportedSched,
    sched_mode: SchedMode,
) -> Vec<String> {
    if let Some(sched_config) = config.scheds.get(scx_sched.as_str()) {
        let scx_flags = extract_scx_flags_from_config(sched_config, sched_mode);

        // try to exact flags from config, otherwise fallback to hardcoded default
        scx_flags.unwrap_or({
            get_default_scx_flags_for_mode(scx_sched, sched_mode)
                .into_iter()
                .map(String::from)
                .collect()
        })
    } else {
        get_default_scx_flags_for_mode(scx_sched, sched_mode)
            .into_iter()
            .map(String::from)
            .collect()
    }
}

/// Extract the scx flags from config
fn extract_scx_flags_from_config(
    sched_config: &Sched,
    sched_mode: SchedMode,
) -> Option<Vec<String>> {
    match &sched_mode {
        SchedMode::Gaming => sched_config.gaming_mode.clone(),
        SchedMode::LowLatency => sched_config.lowlatency_mode.clone(),
        SchedMode::PowerSave => sched_config.powersave_mode.clone(),
        SchedMode::Server => sched_config.server_mode.clone(),
        SchedMode::Auto => sched_config.auto_mode.clone(),
    }
}

/// Get Sched object for configuration object
fn get_default_sched_for_config(scx_sched: &SupportedSched) -> Sched {
    Sched {
        path: None,
        auto_mode: Some(
            get_default_scx_flags_for_mode(scx_sched, SchedMode::Auto)
                .into_iter()
                .map(String::from)
                .collect(),
        ),
        gaming_mode: Some(
            get_default_scx_flags_for_mode(scx_sched, SchedMode::Gaming)
                .into_iter()
                .map(String::from)
                .collect(),
        ),
        lowlatency_mode: Some(
            get_default_scx_flags_for_mode(scx_sched, SchedMode::LowLatency)
                .into_iter()
                .map(String::from)
                .collect(),
        ),
        powersave_mode: Some(
            get_default_scx_flags_for_mode(scx_sched, SchedMode::PowerSave)
                .into_iter()
                .map(String::from)
                .collect(),
        ),
        server_mode: Some(
            get_default_scx_flags_for_mode(scx_sched, SchedMode::Server)
                .into_iter()
                .map(String::from)
                .collect(),
        ),
    }
}

/// Get the default scx flags for the given sched mode
fn get_default_scx_flags_for_mode(
    scx_sched: &SupportedSched,
    sched_mode: SchedMode,
) -> Vec<&'static str> {
    match &scx_sched {
        SupportedSched::Bpfland => match sched_mode {
            SchedMode::LowLatency => {
                vec!["-m", "performance", "-w"]
            }
            SchedMode::PowerSave => {
                vec!["-s", "20000", "-m", "powersave", "-I", "100", "-t", "100"]
            }
            SchedMode::Server => vec!["-s", "20000", "-S"],
            SchedMode::Gaming => vec!["-m", "all"],
            SchedMode::Auto => vec!["-m", "auto"],
        },
        SupportedSched::Lavd => match sched_mode {
            SchedMode::Gaming | SchedMode::LowLatency => {
                vec!["--performance", "--pinned-slice-us", "500"]
            }
            SchedMode::PowerSave => vec!["--powersave", "--pinned-slice-us", "500"],
            SchedMode::Server => vec![
                "--performance",
                "--slice-min-us",
                "3000",
                "--slice-max-us",
                "10000",
                "--pinned-slice-us",
                "3000",
            ],
            SchedMode::Auto => vec!["--autopilot", "--pinned-slice-us", "500"],
        },
        SupportedSched::P2DQ => match sched_mode {
            SchedMode::Gaming => vec!["--task-slice", "true", "-f", "--sched-mode", "performance"],
            SchedMode::LowLatency => vec!["-y", "-f", "--task-slice", "true"],
            SchedMode::PowerSave => vec!["--sched-mode", "efficiency"],
            SchedMode::Server => vec!["--keep-running"],
            SchedMode::Auto => vec!["--sched-mode", "default"],
        },
        SupportedSched::Tickless => match sched_mode {
            SchedMode::Gaming => vec!["-f", "5000", "-s", "5000"],
            SchedMode::LowLatency => vec!["-f", "5000", "-s", "1000"],
            SchedMode::PowerSave => vec!["-f", "50"],
            SchedMode::Server => vec!["-f", "100"],
            SchedMode::Auto => vec![],
        },
        SupportedSched::Cosmos => match sched_mode {
            SchedMode::Gaming => vec!["-s", "700"],
            SchedMode::LowLatency => vec!["-s", "700", "-m", "performance", "-w"],
            SchedMode::PowerSave => vec!["-m", "powersave"],
            SchedMode::Server | SchedMode::Auto => vec![],
        },
        // The below Schedulers haven't defined any modes. Custom schedulers
        // only have the modes their config entry sets.
        SupportedSched::Rusty
        | SupportedSched::Rustland
        | SupportedSched::Beerland
        | SupportedSched::Pandemonium
        | SupportedSched::Flash
        | SupportedSched::Flow
        | SupportedSched::Forge
        | SupportedSched::Cake
        | SupportedSched::MLFQ
        | SupportedSched::EEVDF
        | SupportedSched::Custom(_) => vec![],
    }
}

/// All scheduler modes, in a stable order used for enumeration.
const ALL_MODES: [SchedMode; 5] = [
    SchedMode::Auto,
    SchedMode::Gaming,
    SchedMode::PowerSave,
    SchedMode::LowLatency,
    SchedMode::Server,
];

/// Returns `true` if selecting `sched_mode` for `scx_sched` would resolve to an
/// empty argument list, meaning the mode wouldn't actually change how the
/// scheduler behaves (it would just run with its own built-in defaults).
///
/// `Auto` is never considered to be lacking args: it represents the
/// scheduler's own defaults, so an empty argument list is expected and fine.
#[must_use]
pub fn mode_lacks_args(config: &Config, scx_sched: &SupportedSched, sched_mode: SchedMode) -> bool {
    if sched_mode == SchedMode::Auto {
        return false;
    }

    get_scx_flags_for_mode(config, scx_sched, sched_mode).is_empty()
}

/// Returns the modes that are meaningfully configured for `scx_sched`, i.e.
/// the modes that resolve to a non-empty argument list. `Auto` is always
/// included, since it is valid even without explicit arguments.
///
/// This lets clients (e.g. `scxctl`) discover ahead of time which modes will
/// actually change scheduler behavior for a given scheduler, instead of
/// `scx_loader` hard-rejecting "unconfigured" modes.
#[must_use]
pub fn get_configured_modes(config: &Config, scx_sched: &SupportedSched) -> Vec<SchedMode> {
    ALL_MODES
        .into_iter()
        .filter(|&mode| !mode_lacks_args(config, scx_sched, mode))
        .collect()
}

/// Returns the resolved arguments for every mode of `scx_sched`, in a stable
/// order. Modes with no configured arguments come back with an empty `Vec`,
/// meaning the scheduler would just run with its own built-in defaults for
/// that mode.
///
/// Unlike `get_configured_modes`, this includes every mode (not just the
/// ones that resolve to non-empty args) along with its actual arguments, so
/// callers (e.g. `scxctl modes --show`) can display the exact configuration
/// instead of just which modes are configured.
#[must_use]
pub fn get_all_mode_args(
    config: &Config,
    scx_sched: &SupportedSched,
) -> Vec<(SchedMode, Vec<String>)> {
    ALL_MODES
        .into_iter()
        .map(|mode| (mode, get_scx_flags_for_mode(config, scx_sched, mode)))
        .collect()
}

/// Initializes entry for config sched map
#[allow(clippy::needless_pass_by_value)]
fn init_default_config_entry(scx_sched: SupportedSched) -> (String, Sched) {
    let default_modes = get_default_sched_for_config(&scx_sched);
    (scx_sched.as_str().to_owned(), default_modes)
}

#[cfg(test)]
mod tests {
    use crate::config::*;

    /// TOML fixture mirroring the hardcoded defaults from `get_default_config`.
    /// Lives in its own file (rather than an inline string) so it's easy to
    /// read/edit as real TOML and doesn't bloat the test function.
    const DEFAULT_CONFIG_TOML: &str = include_str!("../default_config.toml");

    #[test]
    fn test_default_config() {
        let parsed_config =
            parse_config_content(DEFAULT_CONFIG_TOML).expect("Failed to parse config");
        let expected_config = get_default_config();

        assert_eq!(parsed_config, expected_config);
    }

    #[test]
    fn test_simple_fallback_config_flags() {
        let config_str = r#"
default_mode = "Auto"
"#;

        let parsed_config = parse_config_content(config_str).expect("Failed to parse config");

        let bpfland_flags =
            get_scx_flags_for_mode(&parsed_config, &SupportedSched::Bpfland, SchedMode::Gaming);
        let expected_flags =
            get_default_scx_flags_for_mode(&SupportedSched::Bpfland, SchedMode::Gaming);
        assert_eq!(
            bpfland_flags
                .iter()
                .map(std::string::String::as_str)
                .collect::<Vec<&str>>(),
            expected_flags
        );
    }

    #[test]
    fn test_sched_fallback_config_flags() {
        let config_str = r#"
default_mode = "Auto"

[scheds.scx_lavd]
auto_mode = ["--help"]
"#;

        let parsed_config = parse_config_content(config_str).expect("Failed to parse config");

        let lavd_flags =
            get_scx_flags_for_mode(&parsed_config, &SupportedSched::Lavd, SchedMode::Gaming);
        let expected_flags =
            get_default_scx_flags_for_mode(&SupportedSched::Lavd, SchedMode::Gaming);
        assert_eq!(
            lavd_flags
                .iter()
                .map(std::string::String::as_str)
                .collect::<Vec<&str>>(),
            expected_flags
        );

        let lavd_flags =
            get_scx_flags_for_mode(&parsed_config, &SupportedSched::Lavd, SchedMode::Auto);
        assert_eq!(
            lavd_flags
                .iter()
                .map(std::string::String::as_str)
                .collect::<Vec<&str>>(),
            vec!["--help"]
        );
    }

    #[test]
    fn test_empty_config() {
        let config_str = "";
        let result = parse_config_content(config_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_mode_never_lacks_args() {
        let config_str = r"
[scheds.scx_lavd]
auto_mode = []
";
        let parsed_config = parse_config_content(config_str).expect("Failed to parse config");

        assert!(!mode_lacks_args(
            &parsed_config,
            &SupportedSched::Lavd,
            SchedMode::Auto
        ));
    }

    #[test]
    fn test_mode_lacks_args_when_explicitly_empty() {
        let config_str = r"
[scheds.scx_lavd]
gaming_mode = []
";
        let parsed_config = parse_config_content(config_str).expect("Failed to parse config");

        assert!(mode_lacks_args(
            &parsed_config,
            &SupportedSched::Lavd,
            SchedMode::Gaming
        ));
    }

    #[test]
    fn test_mode_lacks_args_falls_back_to_defaults() {
        // scx_rusty doesn't define any hardcoded mode flags, so every non-auto
        // mode resolves to an empty argument list unless the user configures one.
        let parsed_config = get_default_config();

        assert!(mode_lacks_args(
            &parsed_config,
            &SupportedSched::Rusty,
            SchedMode::Gaming
        ));
        // scx_lavd does define hardcoded flags for gaming mode.
        assert!(!mode_lacks_args(
            &parsed_config,
            &SupportedSched::Lavd,
            SchedMode::Gaming
        ));
    }

    #[test]
    fn test_get_configured_modes() {
        let config_str = r#"
[scheds.scx_cake]
auto_mode = ["--profile", "default"]
gaming_mode = []
lowlatency_mode = ["--profile", "esports"]
powersave_mode = ["--profile", "battery"]
server_mode = []
"#;
        let parsed_config = parse_config_content(config_str).expect("Failed to parse config");

        let configured_modes = get_configured_modes(&parsed_config, &SupportedSched::Cake);

        // Auto is always included, LowLatency/PowerSave were explicitly given
        // non-empty args, but Gaming/Server were explicitly emptied out.
        assert_eq!(
            configured_modes,
            vec![SchedMode::Auto, SchedMode::PowerSave, SchedMode::LowLatency]
        );
    }

    #[test]
    fn test_get_all_mode_args() {
        let config_str = r#"
[scheds.scx_cake]
auto_mode = ["--profile", "default"]
gaming_mode = []
powersave_mode = ["--profile", "battery"]
lowlatency_mode = ["--profile", "esports"]
server_mode = ["--profile", "gaming"]
"#;
        let parsed_config = parse_config_content(config_str).expect("Failed to parse config");

        let mode_args = get_all_mode_args(&parsed_config, &SupportedSched::Cake);

        // Every mode is present, in a stable order, with its actual
        // resolved arguments (explicit, hardcoded-default, or empty).
        assert_eq!(
            mode_args,
            vec![
                (
                    SchedMode::Auto,
                    vec!["--profile".to_owned(), "default".to_owned()]
                ),
                (SchedMode::Gaming, vec![]),
                (
                    SchedMode::PowerSave,
                    vec!["--profile".to_owned(), "battery".to_owned()]
                ),
                (
                    SchedMode::LowLatency,
                    vec!["--profile".to_owned(), "esports".to_owned()]
                ),
                (
                    SchedMode::Server,
                    vec!["--profile".to_owned(), "gaming".to_owned()]
                ),
            ]
        );
    }

    fn custom(name: &str) -> SupportedSched {
        SupportedSched::Custom(name.to_owned())
    }

    #[test]
    fn test_custom_sched() {
        let config_str = r#"
allow_custom_scheds = true
default_sched = "scx_mine"

[scheds.scx_mine]
path = "/usr/local/bin/scx_mine"
gaming_mode = ["--fast"]
"#;
        let config = parse_config_content(config_str).expect("Failed to parse config");
        let mine = custom("scx_mine");

        assert_eq!(config.default_sched, Some(mine.clone()));
        assert!(is_known_sched(&config, &mine));
        assert_eq!(
            get_sched_program(&config, &mine).as_deref(),
            Some("/usr/local/bin/scx_mine")
        );
        assert_eq!(get_custom_sched_names(&config), vec!["scx_mine".to_owned()]);
        assert_eq!(
            get_scx_flags_for_mode(&config, &mine, SchedMode::Gaming),
            vec!["--fast".to_owned()]
        );
        // No hardcoded defaults for custom schedulers.
        assert_eq!(
            get_scx_flags_for_mode(&config, &mine, SchedMode::PowerSave),
            Vec::<String>::new()
        );
        // Built-in schedulers still run by name.
        assert_eq!(
            get_sched_program(&config, &SupportedSched::Lavd).as_deref(),
            Some("scx_lavd")
        );
    }

    #[test]
    fn test_custom_sched_needs_opt_in() {
        let config_str = r#"
[scheds.scx_mine]
path = "/usr/local/bin/scx_mine"
"#;
        let err = parse_config_content(config_str).unwrap_err();
        assert!(err.to_string().contains("allow_custom_scheds"), "{err}");
    }

    #[test]
    fn test_custom_sched_rejects_bad_entries() {
        for entry in [
            // A built-in scheduler can't get a path.
            "[scheds.scx_lavd]\npath = \"/opt/scx_lavd\"",
            // The path must be absolute, so it's never looked up in PATH.
            "[scheds.scx_mine]\npath = \"scx_mine\"",
            "[scheds.scx_mine]\npath = \"./scx_mine\"",
            // The name must look like a scheduler name.
            "[scheds.mine]\npath = \"/opt/mine\"",
            "[scheds.scx_Mine]\npath = \"/opt/scx_mine\"",
        ] {
            let config_str = format!("allow_custom_scheds = true\n{entry}\n");
            let err = parse_config_content(&config_str).unwrap_err();
            // A validation error, not a TOML syntax error in the fixture.
            assert!(err.to_string().starts_with("[scheds."), "{entry}: {err}");
        }
    }

    #[test]
    fn test_default_sched_must_be_defined() {
        let config_str = r#"
allow_custom_scheds = true
default_sched = "scx_mine"
"#;
        assert!(parse_config_content(config_str).is_err());
    }

    #[test]
    fn test_unknown_sched_without_path_is_ignored() {
        // An entry for a scheduler scx_loader doesn't know still loads, as
        // before, but doesn't make that scheduler available.
        let config_str = r#"
allow_custom_scheds = true

[scheds.scx_gone]
auto_mode = ["-v"]
"#;
        let config = parse_config_content(config_str).expect("Failed to parse config");
        assert!(!is_known_sched(&config, &custom("scx_gone")));
        assert!(get_sched_program(&config, &custom("scx_gone")).is_none());
        assert_eq!(get_custom_sched_names(&config), Vec::<String>::new());
    }

    #[test]
    fn test_custom_sched_off_by_default() {
        let config = get_default_config();
        assert!(!is_known_sched(&config, &custom("scx_mine")));
        assert_eq!(get_custom_sched_names(&config), Vec::<String>::new());
    }
}
