// SPDX-License-Identifier: GPL-2.0

//! Application state and event loop.

use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::DefaultTerminal;
use scx_loader::SchedMode;

use crate::backend::loader::LoaderBackend;
use crate::backend::service::ServiceBackend;
use crate::backend::{Capabilities, SchedulerBackend, Status};
use crate::kernel::{self, KernelState};
use crate::logs::{self, LogLine};
use crate::ui;

/// All modes, in the cycling order used by the mode selector.
pub const MODES: [SchedMode; 5] = [
    SchedMode::Auto,
    SchedMode::Gaming,
    SchedMode::PowerSave,
    SchedMode::LowLatency,
    SchedMode::Server,
];

/// Lower-case mode name, matching the status panel and scxctl's CLI.
fn mode_label(mode: SchedMode) -> &'static str {
    <&str>::from(mode)
}

/// How often the input poll wakes up to redraw / refresh.
const TICK: Duration = Duration::from_millis(250);
/// How often the status is refreshed in the background. The scheduler can
/// change under us (scxctl, another scxtui, a desktop applet), so the view
/// must not assume it is the only writer. Kept moderate because with property
/// caching disabled every refresh is a real round-trip to the daemon. The
/// proper end state is the daemon emitting `PropertiesChanged` so clients
/// subscribe instead of polling; until that lands upstream this stays a poll.
const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// Minimum spacing between two scheduler-affecting actions. Linux terminals
/// deliver key autorepeat as plain `Press` events (no kitty protocol), so
/// without this, holding `r` would fire one restart per repeat.
const ACTION_DEBOUNCE: Duration = Duration::from_millis(500);

/// Which scheduler-management backend drives the app.
#[derive(Clone, Copy, PartialEq)]
pub enum BackendKind {
    Loader,
    Service,
}

impl BackendKind {
    fn other(self) -> Self {
        match self {
            BackendKind::Loader => BackendKind::Service,
            BackendKind::Service => BackendKind::Loader,
        }
    }
}

/// Builds a backend of the given kind, verifying it is usable (connects to
/// D-Bus / finds the systemd unit).
pub fn make_backend(kind: BackendKind) -> Result<Box<dyn SchedulerBackend>> {
    Ok(match kind {
        BackendKind::Loader => Box::new(LoaderBackend::connect()?),
        BackendKind::Service => Box::new(ServiceBackend::connect()?),
    })
}

/// Deferred user actions: queued by key handlers, executed by the event
/// loop right after the frame announcing them has been drawn. Backend
/// calls block (a hung daemon holds the D-Bus timeout, ~25 s), so the UI
/// must show feedback *before* making them.
#[derive(Clone, Copy)]
enum PendingAction {
    StartOrSwitch,
    Stop,
    Restart,
    RestoreDefault,
    ToggleBackend,
    Monitor,
}

/// Which screen is currently shown.
#[derive(Clone, Copy, PartialEq)]
pub enum View {
    Schedulers,
    Logs,
}

pub struct Message {
    pub text: String,
    pub is_error: bool,
}

pub struct App {
    backend: Box<dyn SchedulerBackend>,
    backend_kind: BackendKind,
    pub schedulers: Vec<String>,
    pub selected: usize,
    pub mode_idx: usize,
    pub status: Option<Status>,
    /// The kernel's own view of `sched_ext`, refreshed alongside `status`.
    /// `None` = kernel without `sched_ext` support.
    pub kernel: Option<KernelState>,
    /// Configured modes for the currently selected scheduler.
    pub configured_modes: Vec<SchedMode>,
    pub message: Option<Message>,
    /// Timestamp of the last scheduler-affecting action, for debouncing.
    last_action: Option<Instant>,
    pub view: View,
    /// Index into [`logs::UNITS`].
    pub log_unit: usize,
    /// `false` = current boot, `true` = previous boot (`journalctl -b -1`).
    pub log_previous_boot: bool,
    /// Flattened journal lines, oldest first.
    pub log_lines: Vec<LogLine>,
    /// Scroll offset counted from the bottom; 0 sticks to the newest line.
    pub log_scroll: usize,
    /// Last known height of the log viewport, written back by the UI so
    /// `PgUp`/`PgDn` can page by exactly one screen.
    pub log_page: usize,
    /// Queued by key handlers, executed by the event loop after the next
    /// draw; see [`PendingAction`].
    pending_action: Option<PendingAction>,
    should_quit: bool,
}

impl App {
    pub fn new(kind: BackendKind, backend: Box<dyn SchedulerBackend>) -> Result<Self> {
        let schedulers = backend.supported_schedulers()?;
        let mut app = Self {
            backend,
            backend_kind: kind,
            schedulers,
            selected: 0,
            mode_idx: 0,
            status: None,
            kernel: None,
            configured_modes: Vec::new(),
            message: None,
            last_action: None,
            view: View::Schedulers,
            log_unit: 0,
            log_previous_boot: false,
            log_lines: Vec::new(),
            log_scroll: 0,
            log_page: 20,
            pending_action: None,
            should_quit: false,
        };
        app.refresh_status();
        app.refresh_modes();
        Ok(app)
    }

    pub fn backend_label(&self) -> &'static str {
        self.backend.label()
    }

    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    pub fn selected_scheduler(&self) -> Option<&str> {
        self.schedulers.get(self.selected).map(String::as_str)
    }

    pub fn selected_mode(&self) -> SchedMode {
        MODES[self.mode_idx]
    }

    /// Whether the selected mode has configured arguments for the selected
    /// scheduler. Mirrors scxctl's client-side warning: `Auto` always counts,
    /// and an earlier query failure fails open (empty list is treated as
    /// "unknown", not as "nothing configured") so we never scare the user
    /// over a transient D-Bus hiccup.
    pub fn selected_mode_configured(&self) -> bool {
        self.selected_mode() == SchedMode::Auto
            || self.configured_modes.is_empty()
            || self.configured_modes.contains(&self.selected_mode())
    }

    pub fn run(&mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let mut last_refresh = Instant::now();
        while !self.should_quit {
            terminal.draw(|frame| ui::draw(frame, self))?;

            // Execute a queued action only after the frame announcing it
            // ("working…") has been drawn, then redraw immediately so the
            // result replaces the notice.
            if let Some(action) = self.pending_action.take() {
                self.run_action(action, &mut terminal)?;
                last_refresh = Instant::now();
                continue;
            }

            if event::poll(TICK)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key);
                    }
                }
            }

            if last_refresh.elapsed() >= REFRESH_EVERY {
                self.refresh_status();
                last_refresh = Instant::now();
            }
        }
        Ok(())
    }

    /// Hands the terminal over to `scxtop` and takes it back afterwards —
    /// the lazygit-spawns-an-editor pattern. Restoring the terminal to
    /// cooked mode first lets scxtop own the alternate screen and raw mode
    /// itself; a fresh `ratatui::init()` afterwards re-enters ours, and the
    /// explicit clear forces a full repaint of a screen scxtop scribbled
    /// over. Keeping scxtop out-of-process also keeps its heavyweight BPF
    /// dependency chain (and its root/`CAP_BPF` requirement) out of this
    /// binary: scxtui itself stays an unprivileged D-Bus client.
    fn run_monitor(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        ratatui::restore();
        let result = Command::new("scxtop").status();
        *terminal = ratatui::init();
        terminal.clear()?;

        match result {
            Ok(status) if status.success() => self.info("scxtop exited"),
            Ok(status) => self.error(&format!(
                "scxtop exited with {status} — it needs root/CAP_BPF; \
try running scxtui as root or granting scxtop capabilities"
            )),
            Err(err) if err.kind() == ErrorKind::NotFound => self.error(
                "scxtop not found in PATH — install it (cargo install scxtop, \
or your distro's scx tools package)",
            ),
            Err(err) => self.error(&format!("failed to launch scxtop: {err}")),
        }
        Ok(())
    }

    /// Queues a scheduler-affecting action behind a "working…" notice, so
    /// the notice renders before the blocking backend call starts.
    fn queue(&mut self, action: PendingAction) {
        self.info("working…");
        self.pending_action = Some(action);
    }

    fn run_action(&mut self, action: PendingAction, terminal: &mut DefaultTerminal) -> Result<()> {
        match action {
            PendingAction::StartOrSwitch => self.start_or_switch(),
            PendingAction::Stop => self.act("stopped", |b| b.stop()),
            PendingAction::Restart => self.restart_scheduler(),
            PendingAction::RestoreDefault => {
                self.act("restored default scheduler", |b| b.restore_default());
            }
            PendingAction::ToggleBackend => self.toggle_backend(),
            PendingAction::Monitor => self.run_monitor(terminal)?,
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        match self.view {
            View::Schedulers => self.on_key_schedulers(key),
            View::Logs => self.on_key_logs(key),
        }
    }

    fn on_key_schedulers(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('l') => self.open_logs(),
            KeyCode::Char('t') => {
                if self.action_allowed() {
                    self.pending_action = Some(PendingAction::Monitor);
                }
            }
            KeyCode::Char('B') => {
                if self.action_allowed() {
                    self.queue(PendingAction::ToggleBackend);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Tab | KeyCode::Char('m') if self.backend.capabilities().modes => {
                self.cycle_mode(true);
            }
            KeyCode::BackTab | KeyCode::Char('M') if self.backend.capabilities().modes => {
                self.cycle_mode(false);
            }
            KeyCode::Enter => {
                if self.action_allowed() {
                    self.queue(PendingAction::StartOrSwitch);
                }
            }
            KeyCode::Char('s') => {
                if self.action_allowed() {
                    self.queue(PendingAction::Stop);
                }
            }
            KeyCode::Char('r') => {
                if self.action_allowed() {
                    self.queue(PendingAction::Restart);
                }
            }
            KeyCode::Char('d') => {
                if self.backend.capabilities().restore_default && self.action_allowed() {
                    self.queue(PendingAction::RestoreDefault);
                }
            }
            KeyCode::Char('R') => {
                self.refresh_status();
                self.refresh_modes();
                self.info("refreshed");
            }
            _ => {}
        }
    }

    fn on_key_logs(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q' | 'l') | KeyCode::Esc => {
                self.view = View::Schedulers;
            }
            KeyCode::Up | KeyCode::Char('k') => self.log_scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.log_scroll_down(1),
            KeyCode::PageUp => self.log_scroll_up(self.log_page),
            KeyCode::PageDown => self.log_scroll_down(self.log_page),
            KeyCode::Char('g') => self.log_scroll = usize::MAX, // clamped by the UI
            KeyCode::Char('G') => self.log_scroll = 0,
            KeyCode::Char('b') => {
                self.log_previous_boot = !self.log_previous_boot;
                self.reload_logs();
            }
            KeyCode::Char('u') => {
                self.log_unit = (self.log_unit + 1) % logs::UNITS.len();
                self.reload_logs();
            }
            KeyCode::Char('R') => self.reload_logs(),
            _ => {}
        }
    }

    /// Swaps the running backend for the other kind. The switch is
    /// transactional: the target backend must connect *and* enumerate its
    /// schedulers before any state is replaced, so a failure at either step
    /// leaves the current backend fully intact.
    fn toggle_backend(&mut self) {
        let target = self.backend_kind.other();
        let prepared = make_backend(target).and_then(|backend| {
            let schedulers = backend.supported_schedulers()?;
            Ok((backend, schedulers))
        });
        match prepared {
            Ok((backend, schedulers)) => {
                self.backend = backend;
                self.backend_kind = target;
                self.schedulers = schedulers;
                self.selected = 0;
                self.mode_idx = 0;
                self.refresh_status();
                self.refresh_modes();
                self.info(&format!("switched to {} backend", self.backend.label()));
            }
            Err(err) => {
                let label = self.backend.label();
                self.error(&format!(
                    "cannot switch backend: {err:#} (staying on {label})"
                ));
            }
        }
    }

    /// Public entry for one-off notices (e.g. the startup fallback note).
    pub fn notify(&mut self, text: &str) {
        self.info(text);
    }

    fn open_logs(&mut self) {
        self.view = View::Logs;
        self.reload_logs();
    }

    fn reload_logs(&mut self) {
        let unit = logs::UNITS[self.log_unit];
        match logs::fetch(unit, self.log_previous_boot) {
            Ok(lines) => {
                let boot = if self.log_previous_boot { "-1" } else { "0" };
                self.info(&format!(
                    "loaded {} lines from {unit} (boot {boot})",
                    lines.len()
                ));
                self.log_lines = lines;
                self.log_scroll = 0;
            }
            Err(err) => {
                self.log_lines = Vec::new();
                self.error(&format!("{err:#}"));
            }
        }
    }

    /// The upper bound is clamped against the viewport in the UI, which
    /// knows the current height; only the zero bound is handled here.
    fn log_scroll_up(&mut self, lines: usize) {
        self.log_scroll = self.log_scroll.saturating_add(lines);
    }

    fn log_scroll_down(&mut self, lines: usize) {
        self.log_scroll = self.log_scroll.saturating_sub(lines);
    }

    fn select_next(&mut self) {
        if !self.schedulers.is_empty() {
            self.selected = (self.selected + 1) % self.schedulers.len();
            self.refresh_modes();
        }
    }

    fn select_prev(&mut self) {
        if !self.schedulers.is_empty() {
            self.selected = (self.selected + self.schedulers.len() - 1) % self.schedulers.len();
            self.refresh_modes();
        }
    }

    fn cycle_mode(&mut self, forward: bool) {
        let len = MODES.len();
        self.mode_idx = if forward {
            (self.mode_idx + 1) % len
        } else {
            (self.mode_idx + len - 1) % len
        };
    }

    /// Debounce gate for scheduler-affecting actions (Enter/s/r/d/B).
    /// Returns `true` and arms the timer when enough time has passed since
    /// the last action; swallows the event otherwise. See
    /// [`ACTION_DEBOUNCE`].
    fn action_allowed(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_action
            .is_some_and(|last| now.duration_since(last) < ACTION_DEBOUNCE)
        {
            return false;
        }
        self.last_action = Some(now);
        true
    }

    /// `Enter`: start when nothing is running, switch otherwise — including
    /// "switching" the running scheduler to a different mode, which is how
    /// the loader models a mode change. The TUI can make that call itself
    /// instead of erroring like a CLI has to.
    fn start_or_switch(&mut self) {
        let Some(sched) = self.selected_scheduler().map(str::to_owned) else {
            return;
        };
        let mode = self.selected_mode();
        // Re-read the daemon state right before deciding start vs switch:
        // the cached status can be up to REFRESH_EVERY old, and another
        // client may have started a scheduler in that window.
        self.refresh_status();
        let running = self
            .status
            .as_ref()
            .and_then(|status| status.current.clone());

        let same_scheduler = running.as_deref() == Some(sched.as_str());
        let result = if running.is_some() {
            self.backend.switch(&sched, mode)
        } else {
            self.backend.start(&sched, mode)
        };

        match result {
            Ok(()) => {
                let mode_str = mode_label(mode);
                let base = if running.is_none() {
                    format!("started {sched} in {mode_str} mode")
                } else if same_scheduler {
                    format!("switched {sched} to {mode_str} mode")
                } else {
                    format!("switched to {sched} in {mode_str} mode")
                };
                let text = if self.selected_mode_configured() {
                    base
                } else {
                    format!("{base} — no arguments configured, scheduler defaults in use")
                };
                self.info(&text);
            }
            Err(err) => {
                let verb = if running.is_some() {
                    "switch to"
                } else {
                    "start"
                };
                self.error(&format!("{verb} {sched} failed: {err:#}"));
            }
        }
        self.refresh_status();
    }

    /// `r`: restart. Plain `RestartScheduler` deliberately reuses the
    /// original configuration, so by itself it can never apply a mode
    /// change — and the very first piece of community feedback was someone
    /// restarting after flipping the mode selector and reasonably expecting
    /// the new mode to stick. When the selection points at the running
    /// scheduler, no custom arguments are in play and the selector differs
    /// from the active mode, `r` therefore means "restart into the selected
    /// mode" (a same-scheduler switch in loader terms). In every other case
    /// it stays a faithful restart of the original configuration.
    fn restart_scheduler(&mut self) {
        // Same staleness concern as in `start_or_switch`.
        self.refresh_status();
        let running = self
            .status
            .as_ref()
            .and_then(|status| status.current.clone());
        let custom_args = self
            .status
            .as_ref()
            .is_some_and(|status| !status.args.is_empty());
        let active_mode = self.status.as_ref().map(|status| status.mode);
        let selected = self.selected_scheduler().map(str::to_owned);
        let mode = self.selected_mode();

        let mode_change = self.backend.capabilities().modes
            && !custom_args
            && running.is_some()
            && running == selected
            && active_mode != Some(mode);

        if let (true, Some(sched)) = (mode_change, running) {
            match self.backend.switch(&sched, mode) {
                Ok(()) => {
                    self.info(&format!("restarted {sched} in {} mode", mode_label(mode)));
                }
                Err(err) => self.error(&format!("restart {sched} failed: {err:#}")),
            }
            self.refresh_status();
        } else {
            self.act("restarted", |b| b.restart());
        }
    }

    fn act(&mut self, ok_text: &str, op: impl FnOnce(&dyn SchedulerBackend) -> Result<()>) {
        match op(self.backend.as_ref()) {
            Ok(()) => self.info(ok_text),
            Err(err) => self.error(&format!("{err:#}")),
        }
        self.refresh_status();
    }

    /// Deliberately never touches the message bar: the status panel has
    /// its own channel for a failed query (`State: unknown` in red via
    /// `status = None`), and writing an error here would clobber a fresh
    /// action result — the action can succeed while the follow-up poll
    /// fails, and the user should still see the success.
    fn refresh_status(&mut self) {
        self.status = self.backend.status().ok();
        self.kernel = kernel::read();
    }

    fn refresh_modes(&mut self) {
        self.configured_modes = match self.selected_scheduler() {
            Some(sched) if self.backend.capabilities().modes => {
                // Fail open: an empty list means "unknown" to
                // `selected_mode_configured`, not "nothing configured".
                self.backend.configured_modes(sched).unwrap_or_default()
            }
            _ => Vec::new(),
        };
    }

    fn info(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_owned(),
            is_error: false,
        });
    }

    fn error(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_owned(),
            is_error: true,
        });
    }
}
