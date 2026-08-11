// SPDX-License-Identifier: GPL-2.0

use std::time::Duration;

use anyhow::{Context, Result};
use scx_loader::config::PowerProfilesConfig;
use scx_loader::dbus::LoaderClientProxy;
use scx_loader::SchedMode;
use scx_loader::SupportedSched;
use zbus::export::ordered_stream::OrderedStreamExt;
use zbus::fdo::PropertiesProxy;
use zbus::proxy::CacheProperties;
use zbus::Connection;

const PPD_SERVICE: &str = "org.freedesktop.UPower.PowerProfiles";
const PPD_INTERFACE: &str = "org.freedesktop.UPower.PowerProfiles";
const PPD_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";

#[zbus::proxy(
    interface = "org.freedesktop.UPower.PowerProfiles",
    default_service = "org.freedesktop.UPower.PowerProfiles",
    default_path = "/org/freedesktop/UPower/PowerProfiles"
)]
trait PowerProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
}

async fn apply_profile(
    loader: &LoaderClientProxy<'_>,
    config: &PowerProfilesConfig,
    profile: &str,
) -> Result<()> {
    let Some(mapped_mode) = resolve_mode(config, profile) else {
        match profile {
            "power-saver" | "balanced" | "performance" => {
                log::debug!("No scx-loader mode is mapped for PPD profile {profile:?}");
            }
            _ => log::warn!("Ignoring unknown PPD profile {profile:?}"),
        }
        return Ok(());
    };

    let current_scheduler = loader
        .current_scheduler()
        .await
        .context("Failed to query the current scheduler")?;
    let current_mode = loader
        .scheduler_mode()
        .await
        .context("Failed to query the current scheduler mode")?;
    let current_args = loader
        .current_scheduler_args()
        .await
        .context("Failed to query the current scheduler arguments")?;

    if !should_apply(&current_scheduler, current_mode, &current_args, mapped_mode) {
        return Ok(());
    }

    let scheduler = SupportedSched::try_from(current_scheduler.as_str())
        .with_context(|| format!("Unsupported current scheduler {current_scheduler:?}"))?;
    loader
        .switch_scheduler(scheduler, mapped_mode)
        .await
        .with_context(|| {
            format!("Failed to apply PPD profile {profile:?} as mode {mapped_mode:?}")
        })?;

    Ok(())
}

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

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[must_use]
const fn next_retry_delay(delay: Duration) -> Duration {
    let doubled = delay.saturating_mul(2);
    if doubled.as_secs() > MAX_RETRY_DELAY.as_secs()
        || (doubled.as_secs() == MAX_RETRY_DELAY.as_secs()
            && doubled.subsec_nanos() > MAX_RETRY_DELAY.subsec_nanos())
    {
        MAX_RETRY_DELAY
    } else {
        doubled
    }
}

async fn monitor_session(connection: &Connection, config: &PowerProfilesConfig) -> Result<()> {
    let power_profiles = PowerProfilesProxy::builder(connection)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .context("Failed to create the PPD proxy")?;
    let loader = LoaderClientProxy::builder(connection)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .context("Failed to create the scx-loader proxy")?;
    let properties = PropertiesProxy::builder(connection)
        .destination(PPD_SERVICE)
        .context("Invalid PPD service name")?
        .path(PPD_PATH)
        .context("Invalid PPD object path")?
        .build()
        .await
        .context("Failed to create the PPD properties proxy")?;

    let mut profile_changes = properties
        .receive_properties_changed()
        .await
        .context("Failed to subscribe to PPD property changes")?;
    let mut owner_changes = power_profiles
        .inner()
        .receive_owner_changed()
        .await
        .context("Failed to subscribe to PPD owner changes")?;

    let active_profile = power_profiles
        .active_profile()
        .await
        .context("Failed to read PPD's active profile")?;
    if let Err(error) = apply_profile(&loader, config, &active_profile).await {
        log::warn!("Failed to process PPD profile {active_profile:?}: {error:#}");
    }

    loop {
        tokio::select! {
            changed = profile_changes.next() => {
                let Some(changed) = changed else {
                    return Ok(());
                };
                let args = match changed.args() {
                    Ok(args) => args,
                    Err(error) => {
                        log::warn!("Failed to decode PPD property changes: {error}");
                        continue;
                    }
                };
                let active_profile_changed = args.interface_name().as_str() == PPD_INTERFACE
                    && (args.changed_properties().contains_key("ActiveProfile")
                        || args
                            .invalidated_properties()
                            .contains(&"ActiveProfile"));
                if !active_profile_changed {
                    continue;
                }

                match power_profiles.active_profile().await {
                    Ok(profile) => {
                        if let Err(error) = apply_profile(&loader, config, &profile).await {
                            log::warn!("Failed to process PPD profile {profile:?}: {error:#}");
                        }
                    }
                    Err(error) => log::warn!("Failed to read changed PPD profile: {error}"),
                }
            }
            _ = owner_changes.next() => return Ok(()),
        }
    }
}

pub(crate) async fn monitor(connection: Connection, config: PowerProfilesConfig) {
    let mut retry_delay = INITIAL_RETRY_DELAY;

    loop {
        match monitor_session(&connection, &config).await {
            Ok(()) => {
                retry_delay = INITIAL_RETRY_DELAY;
                log::warn!("PPD connection changed; reconnecting");
            }
            Err(error) => {
                log::warn!("PPD monitoring unavailable: {error:#}; retrying in {retry_delay:?}");
            }
        }

        tokio::time::sleep(retry_delay).await;
        retry_delay = next_retry_delay(retry_delay);
    }
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

    #[test]
    fn retry_delay_doubles() {
        assert_eq!(
            next_retry_delay(Duration::from_secs(1)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn retry_delay_caps_at_thirty_seconds() {
        assert_eq!(
            next_retry_delay(Duration::from_secs(16)),
            Duration::from_secs(30)
        );
        assert_eq!(
            next_retry_delay(Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn retry_delay_caps_doubled_subsecond_remainder() {
        assert_eq!(
            next_retry_delay(Duration::from_secs(15) + Duration::from_nanos(1)),
            Duration::from_secs(30)
        );
    }
}
