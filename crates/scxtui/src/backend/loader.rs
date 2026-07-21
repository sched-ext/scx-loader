// SPDX-License-Identifier: GPL-2.0

//! `org.scx.Loader` D-Bus backend.
//!
//! Deliberately defines its own thin proxy instead of reusing
//! `scx_loader::dbus::LoaderClientProxyBlocking`. The generated client
//! validates every scheduler name against the `SupportedSched` enum on the
//! *client* side, while the daemon advertises its scheduler list as plain
//! strings from an independently maintained table. The two can drift (extra
//! schedulers compiled into a local daemon build, version skew between the
//! running daemon and the enum this binary was built against) — and when
//! they do, the TUI would happily list a scheduler that the client refuses
//! to start. The daemon's advertised list is the single authority here:
//! names are passed through verbatim, and if the daemon itself rejects one,
//! that error surfaces honestly in the message bar. `SupportedSched` has
//! zvariant signature "s", so `&str` is wire-identical.
//!
//! Blocking is a deliberate phase-1 choice: every call is a short local
//! D-Bus round-trip, which keeps the event loop a plain `crossterm` poll
//! instead of a full async runtime.

use anyhow::{Context, Result};
use scx_loader::SchedMode;
use zbus::blocking::Connection;
use zbus::proxy::CacheProperties;

use super::{Capabilities, SchedulerBackend, Status};

/// Sentinel used by scx_loader for "nothing running / not configured".
const UNKNOWN: &str = "unknown";

/// Minimal string-based client for `org.scx.Loader`. Method names map to
/// D-Bus member names via zbus's snake_case -> PascalCase convention.
#[zbus::proxy(
    interface = "org.scx.Loader",
    default_service = "org.scx.Loader",
    default_path = "/org/scx/Loader"
)]
trait Loader {
    fn start_scheduler(&self, scx_name: &str, sched_mode: SchedMode) -> zbus::Result<()>;

    fn switch_scheduler(&self, scx_name: &str, sched_mode: SchedMode) -> zbus::Result<()>;

    fn stop_scheduler(&self) -> zbus::Result<()>;

    fn restart_scheduler(&self) -> zbus::Result<()>;

    fn restore_default(&self) -> zbus::Result<()>;

    fn scheduler_modes(&self, scx_name: &str) -> zbus::Result<Vec<SchedMode>>;

    #[zbus(property)]
    fn current_scheduler(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn scheduler_mode(&self) -> zbus::Result<SchedMode>;

    #[zbus(property)]
    fn current_scheduler_args(&self) -> zbus::Result<Vec<String>>;

    #[zbus(property)]
    fn supported_schedulers(&self) -> zbus::Result<Vec<String>>;

    #[zbus(property)]
    fn default_scheduler(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn default_mode(&self) -> zbus::Result<SchedMode>;
}

pub struct LoaderBackend {
    // The generated proxy holds its own reference to the connection,
    // so we don't need to keep the `Connection` around separately.
    proxy: LoaderProxyBlocking<'static>,
}

impl LoaderBackend {
    /// Connects to the system bus and verifies that `org.scx.Loader`
    /// actually responds, so the TUI can fail fast with a clear message
    /// before the terminal is put into raw mode.
    pub fn connect() -> Result<Self> {
        let conn = Connection::system().context("failed to connect to the system D-Bus")?;
        // Property caching must stay off: zbus invalidates its cache only on
        // `PropertiesChanged`, and the scx_loader daemon never emits it. With
        // the default (lazy) caching, a long-lived client like this one would
        // freeze `CurrentScheduler` at its first-read value forever. One-shot
        // clients such as scxctl never notice, which is why they get away
        // with `::new()`. Should the daemon ever start emitting the signal,
        // this can be reverted and the status poll replaced with a
        // property-change subscription.
        let proxy = LoaderProxyBlocking::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .context("failed to create the org.scx.Loader proxy")?;
        proxy.supported_schedulers().context(
            "org.scx.Loader did not respond — is the scx_loader service installed and running?",
        )?;
        Ok(Self { proxy })
    }
}

fn none_if_unknown(value: String) -> Option<String> {
    if value == UNKNOWN {
        None
    } else {
        Some(value)
    }
}

impl SchedulerBackend for LoaderBackend {
    fn label(&self) -> &'static str {
        "scx_loader (D-Bus)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            live_switch: true,
            modes: true,
            restore_default: true,
        }
    }

    fn status(&self) -> Result<Status> {
        Ok(Status {
            current: none_if_unknown(self.proxy.current_scheduler()?),
            mode: self.proxy.scheduler_mode()?,
            args: self.proxy.current_scheduler_args()?,
            default_sched: none_if_unknown(self.proxy.default_scheduler()?),
            default_mode: self.proxy.default_mode()?,
        })
    }

    fn supported_schedulers(&self) -> Result<Vec<String>> {
        Ok(self.proxy.supported_schedulers()?)
    }

    fn configured_modes(&self, sched: &str) -> Result<Vec<SchedMode>> {
        Ok(self.proxy.scheduler_modes(sched)?)
    }

    fn start(&self, sched: &str, mode: SchedMode) -> Result<()> {
        Ok(self.proxy.start_scheduler(sched, mode)?)
    }

    fn switch(&self, sched: &str, mode: SchedMode) -> Result<()> {
        Ok(self.proxy.switch_scheduler(sched, mode)?)
    }

    fn stop(&self) -> Result<()> {
        Ok(self.proxy.stop_scheduler()?)
    }

    fn restart(&self) -> Result<()> {
        Ok(self.proxy.restart_scheduler()?)
    }

    fn restore_default(&self) -> Result<()> {
        Ok(self.proxy.restore_default()?)
    }
}
