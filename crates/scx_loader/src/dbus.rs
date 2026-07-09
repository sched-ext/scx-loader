// SPDX-License-Identifier: GPL-2.0
//
// Copyright (c) 2024 Vladislav Nepogodin <vnepogodin@cachyos.org>

// This software may be used and distributed according to the terms of the
// GNU General Public License version 2.

use crate::SchedMode;
use crate::SupportedSched;

#[zbus::proxy(
    interface = "org.scx.Loader",
    default_service = "org.scx.Loader",
    default_path = "/org/scx/Loader"
)]
pub trait LoaderClient {
    /// Starts the specified scheduler with the given mode.
    fn start_scheduler(&self, scx_name: SupportedSched, sched_mode: SchedMode) -> zbus::Result<()>;

    /// Starts the specified scheduler with the provided arguments.
    fn start_scheduler_with_args(
        &self,
        scx_name: SupportedSched,
        scx_args: &[String],
    ) -> zbus::Result<()>;

    /// Stops the currently running scheduler.
    fn stop_scheduler(&self) -> zbus::Result<()>;

    /// Method for switching to the specified scheduler with the given mode.
    /// This method will stop the currently running scheduler (if any) and
    /// then start the new scheduler.
    fn switch_scheduler(&self, scx_name: SupportedSched, sched_mode: SchedMode)
        -> zbus::Result<()>;

    /// Switches to the specified scheduler with the provided arguments. This
    /// method will stop the currently running scheduler (if any) and then
    /// start the new scheduler with the given arguments.
    fn switch_scheduler_with_args(
        &self,
        scx_name: SupportedSched,
        scx_args: &[String],
    ) -> zbus::Result<()>;

    /// Restarts the currently running scheduler with its original configuration.
    /// This method will stop the currently running scheduler and then restart
    /// it with the same scheduler and arguments/mode that were used originally.
    fn restart_scheduler(&self) -> zbus::Result<()>;

    /// Restores the default scheduler with the default mode as configured
    /// in the config file. This method will stop the currently running
    /// scheduler (if any) and then start the default scheduler.
    /// Returns an error if no default scheduler is configured.
    fn restore_default(&self) -> zbus::Result<()>;

    /// The name of the currently running scheduler. If no scheduler is active,
    /// this property will be set to "unknown".
    #[zbus(property)]
    fn current_scheduler(&self) -> zbus::Result<String>;

    /// The currently active scheduler mode.  Scheduler modes allow you to
    /// apply pre-defined configurations to a scheduler that are
    /// optimized for different use cases. If no scheduler is active,
    /// this property will be set to 0 (Auto).
    #[zbus(property)]
    fn scheduler_mode(&self) -> zbus::Result<SchedMode>;

    /// The arguments used for the currently running scheduler. If no scheduler
    /// is active or the scheduler was started with a predefined mode (not custom
    /// arguments), this property will return an empty array.
    #[zbus(property)]
    fn current_scheduler_args(&self) -> zbus::Result<Vec<String>>;

    /// A list of the schedulers currently supported by the Scheduler Loader.
    /// The names of the supported schedulers will be listed as strings in
    /// this array.
    #[zbus(property)]
    fn supported_schedulers(&self) -> zbus::Result<Vec<String>>;

    /// The name of the default scheduler configured in the config file.
    /// If no default scheduler is configured, this property will be set
    /// to "unknown".
    #[zbus(property)]
    fn default_scheduler(&self) -> zbus::Result<String>;

    /// The default scheduler mode configured in the config file.
    /// Defaults to 0 (Auto) if not explicitly configured.
    #[zbus(property)]
    fn default_mode(&self) -> zbus::Result<SchedMode>;

    /// Returns the scheduler modes that are meaningfully configured for
    /// `scx_name`, i.e. the modes that resolve to a non-empty argument list.
    /// `Auto` is always included, since it represents the scheduler's own
    /// defaults and is valid even without explicit arguments.
    fn scheduler_modes(&self, scx_name: SupportedSched) -> zbus::Result<Vec<SchedMode>>;
}
