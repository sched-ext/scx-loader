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
use crate::backend::{Capabilities, ModeArgs, SchedulerBackend, Status};
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

/// Filters a [`ModeArgs`] answer down to the modes that count as
/// configured: `Auto` always, anything else when its resolved argument
/// list is non-empty.
fn derive_configured(modes: ModeArgs) -> Vec<SchedMode> {
    modes
        .into_iter()
        .filter(|(mode, args)| *mode == SchedMode::Auto || !args.is_empty())
        .map(|(mode, _)| mode)
        .collect()
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
/// How long informational messages stay in the message bar. Without a
/// TTL, a stale "refreshed" from minutes ago reads as fresh feedback.
/// Errors are exempt: they are actionable and stay until replaced.
const MESSAGE_TTL: Duration = Duration::from_secs(8);

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
    /// When the message was posted; drives [`MESSAGE_TTL`] expiry.
    shown_at: Instant,
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
        let app = Self {
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
        // No initial status/modes fetch here: `run` draws the first frame
        // with the scheduler list alone and fetches right after, so the UI
        // appears immediately instead of waiting on D-Bus round-trips.
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
        let mut primed = false;
        while !self.should_quit {
            terminal.draw(|frame| ui::draw(frame, self))?;

            // First frame is a skeleton: scheduler list only, status panel
            // in its "unknown" placeholder. The initial fetch happens right
            // after it is on screen and the immediate redraw fills it in —
            // on a healthy system that is a single-digit-ms flicker, and on
            // a slow daemon the user at least sees a live UI instead of a
            // blank terminal.
            if !primed {
                primed = true;
                self.refresh_status();
                self.sync_selection_to_running();
                self.refresh_modes();
                last_refresh = Instant::now();
                continue;
            }

            // Informational messages expire so stale feedback never reads
            // as fresh; errors stay until replaced (see MESSAGE_TTL).
            if self
                .message
                .as_ref()
                .is_some_and(|m| !m.is_error && m.shown_at.elapsed() >= MESSAGE_TTL)
            {
                self.message = None;
            }

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
                // The success notice must not clobber a list-refresh error,
                // same rule as refresh_status not touching the message bar.
                let list_ok = self.refresh_schedulers();
                self.refresh_status();
                self.refresh_modes();
                if list_ok {
                    self.info("refreshed");
                }
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
                self.sync_selection_to_running();
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
            Ok(fetch) => {
                let boot = if self.log_previous_boot { "-1" } else { "0" };
                let tail_note = if fetch.truncated {
                    " — older entries omitted, see journalctl for the full log"
                } else {
                    ""
                };
                self.info(&format!(
                    "loaded {} lines from {unit} (boot {boot}){tail_note}",
                    fetch.lines.len()
                ));
                self.log_lines = fetch.lines;
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

    /// Aligns the scheduler selection and the mode selector with what is
    /// actually running, so the first `Enter`/`r` after startup acts on
    /// what the user sees instead of on `schedulers[0]` + `Auto`. Only
    /// called on startup and after a backend switch — never on the
    /// periodic refresh, which must not fight the user's own selection.
    ///
    /// All-or-nothing: a running scheduler outside the advertised list
    /// (hand-launched next to the loader) leaves *both* selectors alone —
    /// syncing only the mode would pair the foreign scheduler's mode with
    /// an unrelated selection. The mode half is additionally skipped for
    /// custom args, since no selector position represents that state.
    fn sync_selection_to_running(&mut self) {
        let Some(status) = &self.status else {
            return;
        };
        let Some(current) = &status.current else {
            return;
        };
        let Some(idx) = self.schedulers.iter().position(|s| s == current) else {
            return;
        };
        self.selected = idx;
        if status.args.is_empty() {
            if let Some(idx) = MODES.iter().position(|m| *m == status.mode) {
                self.mode_idx = idx;
            }
        }
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

    /// Re-enumerates the scheduler list. The list is otherwise fetched
    /// once at startup, so without this a daemon restarted with a changed
    /// configuration would keep serving a stale list until scxtui itself
    /// restarts. The selection survives by name where possible; on a
    /// failed query the old list stays — a stale list beats an empty one.
    fn refresh_schedulers(&mut self) -> bool {
        match self.backend.supported_schedulers() {
            Ok(schedulers) => {
                let previous = self.selected_scheduler().map(str::to_owned);
                self.selected = previous
                    .and_then(|name| schedulers.iter().position(|s| *s == name))
                    .unwrap_or(0);
                self.schedulers = schedulers;
                true
            }
            Err(err) => {
                self.error(&format!("scheduler list refresh failed: {err:#}"));
                false
            }
        }
    }

    /// Configured modes are derived from the full per-mode argument query
    /// (`SchedulerModeArgs`): `Auto` always counts, any other mode counts
    /// when its resolved argument list is non-empty — the same rule the
    /// daemon applies in `SchedulerModes`. Deriving locally lets a single
    /// query serve both this indicator and an argument preview without a
    /// second source of truth.
    fn refresh_modes(&mut self) {
        self.configured_modes = match self.selected_scheduler() {
            Some(sched) if self.backend.capabilities().modes => {
                // Fail open: a successful answer always contains at least
                // `Auto`, so an empty list still means "unknown" to
                // `selected_mode_configured`, not "nothing configured".
                self.backend
                    .mode_args(sched)
                    .map(derive_configured)
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
    }

    fn info(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_owned(),
            is_error: false,
            shown_at: Instant::now(),
        });
    }

    fn error(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_owned(),
            is_error: true,
            shown_at: Instant::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal in-memory backend: enough for exercising selection logic
    /// without a D-Bus connection.
    struct StubBackend {
        schedulers: Vec<String>,
    }

    impl SchedulerBackend for StubBackend {
        fn label(&self) -> &'static str {
            "stub"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                live_switch: true,
                modes: true,
                restore_default: true,
            }
        }
        fn status(&self) -> Result<Status> {
            unreachable!("tests set App::status directly")
        }
        fn supported_schedulers(&self) -> Result<Vec<String>> {
            Ok(self.schedulers.clone())
        }
        fn mode_args(&self, _sched: &str) -> Result<ModeArgs> {
            Ok(Vec::new())
        }
        fn start(&self, _sched: &str, _mode: SchedMode) -> Result<()> {
            Ok(())
        }
        fn switch(&self, _sched: &str, _mode: SchedMode) -> Result<()> {
            Ok(())
        }
        fn stop(&self) -> Result<()> {
            Ok(())
        }
        fn restart(&self) -> Result<()> {
            Ok(())
        }
        fn restore_default(&self) -> Result<()> {
            Ok(())
        }
    }

    fn app_with_status(status: Status) -> App {
        let backend = StubBackend {
            schedulers: vec!["scx_bpfland".into(), "scx_lavd".into(), "scx_cake".into()],
        };
        let mut app = App::new(BackendKind::Loader, Box::new(backend)).unwrap();
        app.status = Some(status);
        app
    }

    fn running(current: &str, mode: SchedMode, args: &[&str]) -> Status {
        Status {
            current: Some(current.to_owned()),
            mode,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            default_sched: None,
            default_mode: SchedMode::Auto,
        }
    }

    #[test]
    fn sync_aligns_selection_and_mode_with_running_scheduler() {
        let mut app = app_with_status(running("scx_lavd", SchedMode::Gaming, &[]));
        app.sync_selection_to_running();
        assert_eq!(app.selected_scheduler(), Some("scx_lavd"));
        assert_eq!(app.selected_mode(), SchedMode::Gaming);
    }

    #[test]
    fn sync_with_custom_args_keeps_mode_selector_untouched() {
        let mut app = app_with_status(running(
            "scx_cake",
            SchedMode::Gaming,
            &["--slice-us", "500"],
        ));
        app.sync_selection_to_running();
        // Selection follows the running scheduler, but no selector position
        // represents "custom args", so the mode stays where it was.
        assert_eq!(app.selected_scheduler(), Some("scx_cake"));
        assert_eq!(app.selected_mode(), SchedMode::Auto);
    }

    #[test]
    fn sync_with_unknown_scheduler_is_a_full_no_op() {
        // A hand-launched scheduler outside the advertised list must not
        // touch either selector: syncing only the mode would pair the
        // foreign scheduler's mode with an unrelated selection.
        let mut app = app_with_status(running("scx_homebrew", SchedMode::Server, &[]));
        app.selected = 1;
        app.mode_idx = 1;
        app.sync_selection_to_running();
        assert_eq!(app.selected_scheduler(), Some("scx_lavd"));
        assert_eq!(app.selected_mode(), SchedMode::Gaming);
    }

    #[test]
    fn sync_with_nothing_running_is_a_no_op() {
        let mut app = app_with_status(Status {
            current: None,
            mode: SchedMode::Auto,
            args: Vec::new(),
            default_sched: None,
            default_mode: SchedMode::Auto,
        });
        app.mode_idx = 2;
        app.sync_selection_to_running();
        assert_eq!(app.selected, 0);
        assert_eq!(app.mode_idx, 2);
    }
}
