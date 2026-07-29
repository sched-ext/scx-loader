// SPDX-License-Identifier: GPL-2.0

//! Backend abstraction.
//!
//! Two implementations exist: [`loader::LoaderBackend`] talks to
//! `org.scx.Loader` over D-Bus and is the preferred choice, while
//! [`service::ServiceBackend`] drives `scx.service` through its config file
//! and `systemctl` on systems without the loader daemon. The trait keeps
//! rendering and input handling independent of either: backends with a
//! reduced feature set declare it via [`Capabilities`] and the UI degrades
//! gracefully instead of offering operations that cannot work.

pub mod loader;
pub mod service;

use anyhow::Result;
use scx_loader::SchedMode;

/// Resolved arguments for every mode of one scheduler, in the daemon's
/// stable order. Modes with no configured arguments carry an empty `Vec`.
pub type ModeArgs = Vec<(SchedMode, Vec<String>)>;

/// What a given backend can actually do. The UI greys out or hides
/// anything the active backend does not support.
// Independent yes/no capability flags are the point of this struct;
// `struct_excessive_bools` suggests a state machine, which this is not.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// Can switch schedulers at runtime without a service restart.
    pub live_switch: bool,
    /// Exposes per-scheduler mode configuration (`SchedulerModeArgs`).
    pub modes: bool,
    /// Can start or switch a scheduler with free-form arguments instead
    /// of a mode (`StartSchedulerWithArgs` / `SwitchSchedulerWithArgs`).
    pub custom_args: bool,
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
/// `scx_` prefix): the trait stays agnostic of `SupportedSched`, since the
/// backends enumerate schedulers from different sources (the daemon's
/// advertised list vs. binaries installed in `PATH`).
pub trait SchedulerBackend {
    /// Short human-readable backend name for the status bar.
    fn label(&self) -> &'static str;

    /// Opaque token identifying the live backend instance behind this
    /// connection — for the loader, the unique bus name of the current
    /// `org.scx.Loader` owner. The daemon reads its configuration once at
    /// startup, so client-side caches of configuration answers stay valid
    /// exactly as long as the token does: a changed token is a replaced
    /// daemon. `None` means the backend has no such notion, or the owner
    /// is momentarily unknown; callers should keep their caches on `None`
    /// rather than dropping known-good data over a transient hiccup.
    fn instance_token(&self) -> Option<String> {
        None
    }

    fn capabilities(&self) -> Capabilities;

    fn status(&self) -> Result<Status>;

    fn supported_schedulers(&self) -> Result<Vec<String>>;

    /// Resolved arguments for every mode of `sched` (see [`ModeArgs`]).
    /// This is a superset of the old "which modes are configured" query:
    /// a mode counts as configured exactly when it is `Auto` or its
    /// argument list here is non-empty — the same rule the daemon applies
    /// in `SchedulerModes` — so callers derive that locally instead of
    /// holding a second source of truth that could drift from the
    /// arguments they display.
    fn mode_args(&self, sched: &str) -> Result<ModeArgs>;

    fn start(&self, sched: &str, mode: SchedMode) -> Result<()>;

    fn switch(&self, sched: &str, mode: SchedMode) -> Result<()>;

    /// Starts `sched` with free-form arguments instead of a mode. The
    /// arguments live only in the daemon's memory for this run — nothing
    /// is written to the loader config, hence "session-only" in the UI.
    fn start_with_args(&self, sched: &str, args: &[String]) -> Result<()>;

    /// Same as [`Self::start_with_args`], but stops a running scheduler
    /// first, like [`Self::switch`].
    fn switch_with_args(&self, sched: &str, args: &[String]) -> Result<()>;

    fn stop(&self) -> Result<()>;

    fn restart(&self) -> Result<()>;

    fn restore_default(&self) -> Result<()>;
}
