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

use anyhow::{Context, Result, anyhow, bail};
use scx_loader::SchedMode;
use zbus::blocking::Connection;
use zbus::blocking::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::{BusName, InterfaceName};
use zbus::zvariant::OwnedValue;

use super::{Capabilities, ModeArgs, RuntimeStatus, SchedulerBackend, Status};

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

    fn start_scheduler_with_args(&self, scx_name: &str, scx_args: &[String]) -> zbus::Result<()>;

    fn switch_scheduler_with_args(&self, scx_name: &str, scx_args: &[String]) -> zbus::Result<()>;

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

    // Never-signalling so zbus does not cache it: a manual refresh stays a real query.
    #[zbus(property(emits_changed_signal = "false"))]
    fn supported_schedulers(&self) -> zbus::Result<Vec<String>>;

    /// Doorbell key only ([`super::RuntimeStatus::generation`]); absent on older daemons.
    #[zbus(property)]
    fn daemon_generation(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn default_scheduler(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn default_mode(&self) -> zbus::Result<SchedMode>;
}

pub struct LoaderBackend {
    // The generated proxy holds its own reference to the connection,
    // so we don't need to keep the `Connection` around separately.
    proxy: LoaderProxyBlocking<'static>,
    /// `org.freedesktop.DBus` proxy, kept for `GetNameOwner`: the unique
    /// name of the current `org.scx.Loader` owner is the instance token
    /// (see [`SchedulerBackend::instance_token`]).
    dbus: DBusProxy<'static>,
    /// `org.freedesktop.DBus.Properties` proxy for the same object, used to
    /// fetch the whole status in a single `GetAll` round-trip instead of
    /// five per-property `Get`s (see [`SchedulerBackend::status`]).
    props: PropertiesProxy<'static>,
    /// Probed once at `connect()`; without it the push path stays disabled.
    has_generation: bool,
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
        // `NameHasOwner` never triggers activation — fail fast.
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
        // Signals keep the cache current; `status()` stays on the cache-bypassing `GetAll`.
        let proxy = LoaderProxyBlocking::builder(&conn)
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
        // Pre-push daemon: disabled for the session; also primes the cache.
        let has_generation = proxy.daemon_generation().is_ok();
        Ok(Self {
            proxy,
            props,
            dbus,
            has_generation,
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
    if value == UNKNOWN { None } else { Some(value) }
}

impl SchedulerBackend for LoaderBackend {
    fn label(&self) -> &'static str {
        "scx_loader (D-Bus)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            live_switch: true,
            modes: true,
            custom_args: true,
            restore_default: true,
        }
    }

    /// One cheap round-trip to the bus daemon itself (not to `scx_loader`),
    /// so it can ride along the periodic status refresh. A momentarily
    /// unowned name maps to `None` — the caller keeps its caches and a
    /// later successful read reports the (new) owner.
    fn instance_token(&self) -> Option<String> {
        let name = BusName::from_static_str(SERVICE).expect("valid bus name literal");
        self.dbus
            .get_name_owner(name)
            .ok()
            .map(|owner| owner.to_string())
    }

    /// `GetAll` bypassing the proxy cache: the authoritative read must see the daemon.
    /// Read until two consecutive answers agree: the daemon serves properties
    /// individually, so one `GetAll` racing a transition can mix two states.
    /// A capped disagreement fails open with the last answer - the next poll
    /// corrects it, and a possibly-torn snapshot beats none.
    fn status(&self) -> Result<Status> {
        let iface = InterfaceName::from_static_str(SERVICE).expect("valid interface literal");
        let mut props = self
            .props
            .get_all(iface.clone())
            .map_err(|err| anyhow!("{err}"))
            .context("GetAll on org.scx.Loader failed")?;
        for _ in 0..2 {
            let again = self
                .props
                .get_all(iface.clone())
                .map_err(|err| anyhow!("{err}"))
                .context("GetAll on org.scx.Loader failed")?;
            if again == props {
                break;
            }
            props = again;
        }
        Ok(Status {
            current: none_if_unknown(take_prop::<String>(&mut props, "CurrentScheduler")?),
            mode: take_prop(&mut props, "SchedulerMode")?,
            args: take_prop(&mut props, "CurrentSchedulerArgs")?,
            default_sched: none_if_unknown(take_prop::<String>(&mut props, "DefaultScheduler")?),
            default_mode: take_prop(&mut props, "DefaultMode")?,
        })
    }

    /// Movement detector; not atomic — a torn snapshot rings once more.
    fn cached_status(&self) -> Result<Option<RuntimeStatus>> {
        if !self.has_generation {
            return Ok(None);
        }
        Ok(Some(RuntimeStatus {
            current: none_if_unknown(self.proxy.current_scheduler()?),
            mode: self.proxy.scheduler_mode()?,
            args: self.proxy.current_scheduler_args()?,
            generation: self.proxy.daemon_generation()?,
        }))
    }

    fn supported_schedulers(&self) -> Result<Vec<String>> {
        // First call consumes the connect-time probe; later calls are fresh queries.
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

    fn start_with_args(&self, sched: &str, args: &[String]) -> Result<()> {
        Ok(self.proxy.start_scheduler_with_args(sched, args)?)
    }

    fn switch_with_args(&self, sched: &str, args: &[String]) -> Result<()> {
        Ok(self.proxy.switch_scheduler_with_args(sched, args)?)
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
