// SPDX-License-Identifier: GPL-2.0

//! Backend abstraction.
//!
//! Phase 1 ships a single implementation ([`loader::LoaderBackend`]) talking
//! to `org.scx.Loader` over D-Bus. The trait exists so that a future
//! `scx.service` backend (config file + systemctl) can slot in without
//! touching the UI: backends with a reduced feature set declare it via
//! [`Capabilities`] and the UI degrades gracefully.

pub mod loader;
pub mod service;

use anyhow::Result;
use scx_loader::SchedMode;

/// What a given backend can actually do. The UI greys out or hides
/// anything the active backend does not support.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// Can switch schedulers at runtime without a service restart.
    pub live_switch: bool,
    /// Exposes per-scheduler mode configuration (`SchedulerModes`).
    pub modes: bool,
    /// Supports restoring a configured default scheduler.
    pub restore_default: bool,
}

/// Snapshot of the scheduler state as reported by the backend.
#[derive(Debug, Clone)]
pub struct Status {
    /// Currently running scheduler (full name, e.g. `scx_bpfland`),
    /// or `None` when nothing is running.
    pub current: Option<String>,
    /// Mode of the running scheduler (only meaningful when `args` is empty).
    pub mode: SchedMode,
    /// Custom arguments the scheduler was started with, if any.
    pub args: Vec<String>,
    /// Default scheduler from the config file, if configured.
    pub default_sched: Option<String>,
    /// Default mode from the config file.
    pub default_mode: SchedMode,
}

/// Common interface every scheduler-management backend implements.
///
/// Scheduler names cross this boundary as plain strings (full names with the
/// `scx_` prefix): the trait must stay agnostic of `SupportedSched`, since a
/// `scx.service` backend would enumerate schedulers differently.
pub trait SchedulerBackend {
    /// Short human-readable backend name for the status bar.
    fn label(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    fn status(&self) -> Result<Status>;

    fn supported_schedulers(&self) -> Result<Vec<String>>;

    /// Modes that resolve to a non-empty argument list for `sched`.
    /// `Auto` is always considered configured.
    fn configured_modes(&self, sched: &str) -> Result<Vec<SchedMode>>;

    fn start(&self, sched: &str, mode: SchedMode) -> Result<()>;

    fn switch(&self, sched: &str, mode: SchedMode) -> Result<()>;

    fn stop(&self) -> Result<()>;

    fn restart(&self) -> Result<()>;

    fn restore_default(&self) -> Result<()>;
}
