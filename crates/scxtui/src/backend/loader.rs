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

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use scx_loader::SchedMode;
use zbus::blocking::fdo::{DBusProxy, PropertiesProxy};
use zbus::blocking::Connection;
use zbus::names::{BusName, InterfaceName};
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedValue;

use super::{Capabilities, ModeArgs, SchedulerBackend, Status};

/// Sentinel used by `scx_loader` for "nothing running / not configured".
const UNKNOWN: &str = "unknown";

/// Well-known bus name of the loader daemon, shared by the liveness gate
/// and the `GetAll` interface argument.
const SERVICE: &str = "org.scx.Loader";

/// Minimal string-based client for `org.scx.Loader`. Method names map to
/// D-Bus member names via zbus's `snake_case` -> `PascalCase` convention.
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

    fn scheduler_mode_args(&self, scx_name: &str) -> zbus::Result<ModeArgs>;

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
    /// `org.freedesktop.DBus.Properties` proxy for the same object, used to
    /// fetch the whole status in a single `GetAll` round-trip instead of
    /// five per-property `Get`s (see [`SchedulerBackend::status`]).
    props: PropertiesProxy<'static>,
    /// Scheduler list obtained by the `connect()` probe, consumed by the
    /// *first* `supported_schedulers()` call so startup does not repeat a
    /// round-trip whose answer it already holds. Later calls always go to
    /// the daemon, so a refresh sees the current list.
    initial_schedulers: RefCell<Option<Vec<String>>>,
}

impl LoaderBackend {
    /// Connects to the system bus and verifies that `org.scx.Loader`
    /// actually responds, so the TUI can fail fast with a clear message
    /// before the terminal is put into raw mode.
    pub fn connect() -> Result<Self> {
        let conn = Connection::system().context("failed to connect to the system D-Bus")?;
        // Liveness gate before the real probe: `NameHasOwner` answers
        // instantly from the bus daemon itself and never triggers D-Bus
        // activation. Without it, probing a name that is not running would
        // either wait out an activation attempt (activatable but broken
        // unit) or a method-call timeout before auto-detection can fall
        // back to `scx.service`. If the name is not owned but *is*
        // activatable, the probe below is allowed to proceed and start the
        // daemon — that is the activation working as intended.
        let dbus = DBusProxy::new(&conn).context("failed to create the D-Bus proxy")?;
        let name = BusName::from_static_str(SERVICE).expect("valid bus name literal");
        let owned = dbus
            .name_has_owner(name)
            .map_err(|err| anyhow!("{err}"))
            .context("failed to query the bus for org.scx.Loader")?;
        if !owned {
            let activatable = dbus
                .list_activatable_names()
                .map_err(|err| anyhow!("{err}"))
                .context("failed to list activatable bus names")?
                .iter()
                .any(|n| n.as_str() == SERVICE);
            if !activatable {
                bail!(
                    "org.scx.Loader is neither running nor D-Bus-activatable — \
is the scx_loader service installed?"
                );
            }
        }
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
        // zbus errors already render their full cause in `Display`, so the
        // source chain is flattened here — otherwise anyhow's `{:#}` output
        // repeats the underlying D-Bus message twice.
        let schedulers = proxy
            .supported_schedulers()
            .map_err(|err| anyhow!("{err}"))
            .context(
                "org.scx.Loader did not respond — is the scx_loader service installed and running?",
            )?;
        let props = PropertiesProxy::builder(&conn)
            .destination(SERVICE)
            .context("invalid destination for the Properties proxy")?
            .path("/org/scx/Loader")
            .context("invalid path for the Properties proxy")?
            .build()
            .context("failed to create the Properties proxy")?;
        Ok(Self {
            proxy,
            props,
            initial_schedulers: RefCell::new(Some(schedulers)),
        })
    }
}

/// Removes `name` from a `GetAll` result and converts it to `T`. A missing
/// or mistyped property is a daemon-side contract violation, reported like
/// any other failed status query.
fn take_prop<T>(props: &mut HashMap<String, OwnedValue>, name: &str) -> Result<T>
where
    T: TryFrom<OwnedValue>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    let value = props
        .remove(name)
        .with_context(|| format!("daemon did not report the {name} property"))?;
    T::try_from(value).with_context(|| format!("unexpected type for the {name} property"))
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

    /// One `GetAll` round-trip instead of five `Get`s. With property
    /// caching off (see `connect`), every per-property read is a real
    /// D-Bus call, so the batched form cuts both startup and the periodic
    /// background refresh from five round-trips to one.
    fn status(&self) -> Result<Status> {
        let iface = InterfaceName::from_static_str(SERVICE).expect("valid interface literal");
        let mut props = self
            .props
            .get_all(iface)
            .map_err(|err| anyhow!("{err}"))
            .context("GetAll on org.scx.Loader failed")?;
        Ok(Status {
            current: none_if_unknown(take_prop::<String>(&mut props, "CurrentScheduler")?),
            mode: take_prop(&mut props, "SchedulerMode")?,
            args: take_prop(&mut props, "CurrentSchedulerArgs")?,
            default_sched: none_if_unknown(take_prop::<String>(&mut props, "DefaultScheduler")?),
            default_mode: take_prop(&mut props, "DefaultMode")?,
        })
    }

    fn supported_schedulers(&self) -> Result<Vec<String>> {
        // First call consumes the connect-time probe result; see the field
        // docs. Every later call is a fresh query.
        if let Some(cached) = self.initial_schedulers.borrow_mut().take() {
            return Ok(cached);
        }
        Ok(self.proxy.supported_schedulers()?)
    }

    fn mode_args(&self, sched: &str) -> Result<ModeArgs> {
        Ok(self.proxy.scheduler_mode_args(sched)?)
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
