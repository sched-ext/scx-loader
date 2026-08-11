// SPDX-License-Identifier: GPL-2.0

use scx_loader::config::PowerProfilesConfig;
use scx_loader::SchedMode;

#[must_use]
fn resolve_mode(config: &PowerProfilesConfig, profile: &str) -> Option<SchedMode> {
    match profile {
        "power-saver" => config.power_saver,
        "balanced" => config.balanced,
        "performance" => config.performance,
        _ => None,
    }
}

#[must_use]
fn should_apply(
    current_scheduler: &str,
    current_mode: SchedMode,
    current_args: &[String],
    mapped_mode: SchedMode,
) -> bool {
    current_scheduler != "unknown" && (current_mode != mapped_mode || !current_args.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mappings() -> PowerProfilesConfig {
        PowerProfilesConfig {
            enabled: true,
            power_saver: Some(SchedMode::PowerSave),
            balanced: Some(SchedMode::Auto),
            performance: Some(SchedMode::Gaming),
        }
    }

    #[test]
    fn resolves_each_known_profile() {
        let config = mappings();

        assert_eq!(
            resolve_mode(&config, "power-saver"),
            Some(SchedMode::PowerSave)
        );
        assert_eq!(resolve_mode(&config, "balanced"), Some(SchedMode::Auto));
        assert_eq!(
            resolve_mode(&config, "performance"),
            Some(SchedMode::Gaming)
        );
    }

    #[test]
    fn omitted_mapping_resolves_to_no_action() {
        let config = PowerProfilesConfig {
            enabled: true,
            balanced: Some(SchedMode::Server),
            ..PowerProfilesConfig::default()
        };

        assert_eq!(resolve_mode(&config, "power-saver"), None);
    }

    #[test]
    fn unknown_profile_resolves_to_no_action() {
        assert_eq!(resolve_mode(&mappings(), "future-profile"), None);
    }

    #[test]
    fn skips_when_no_scheduler_is_running() {
        assert!(!should_apply(
            "unknown",
            SchedMode::Auto,
            &[],
            SchedMode::Gaming
        ));
    }

    #[test]
    fn skips_same_mode_without_custom_arguments() {
        assert!(!should_apply(
            "scx_lavd",
            SchedMode::Gaming,
            &[],
            SchedMode::Gaming
        ));
    }

    #[test]
    fn applies_same_reported_mode_when_custom_arguments_are_active() {
        assert!(should_apply(
            "scx_lavd",
            SchedMode::Auto,
            &["--performance".to_owned()],
            SchedMode::Auto
        ));
    }

    #[test]
    fn applies_a_different_mapped_mode() {
        assert!(should_apply(
            "scx_lavd",
            SchedMode::Auto,
            &[],
            SchedMode::PowerSave
        ));
    }
}
