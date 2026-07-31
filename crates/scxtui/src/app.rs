// SPDX-License-Identifier: GPL-2.0

//! Application state and event loop.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use scx_loader::SchedMode;

use crate::args::{expand_input, ArgsExpandError};
use crate::backend::loader::LoaderBackend;
use crate::backend::service::ServiceBackend;
use crate::backend::{Capabilities, ModeArgs, RuntimeStatus, SchedulerBackend, Status};
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
/// Poll cadence until the push channel proves itself.
const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// Relaxed cadence once push is confirmed.
const SAFETY_REFRESH: Duration = Duration::from_secs(30);
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
#[derive(Clone)]
enum PendingAction {
    StartOrSwitch,
    /// Expanded field content; see [`App::submit_args`].
    StartOrSwitchWithArgs(Vec<String>),
    Stop,
    Restart,
    RestoreDefault,
    ToggleBackend,
    Monitor,
}

/// State of the session-only custom-arguments field. The cursor is
/// counted in characters, not bytes, so editing multi-byte input stays
/// sound; the UI derives its cursor block from the same count.
#[derive(Default)]
pub struct ArgsInput {
    pub buffer: String,
    pub cursor: usize,
}

impl ArgsInput {
    /// Byte offset of the character cursor, for `String` edits.
    fn byte_cursor(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor)
            .map_or(self.buffer.len(), |(idx, _)| idx)
    }

    fn len_chars(&self) -> usize {
        self.buffer.chars().count()
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_cursor();
        self.buffer.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte_cursor();
            self.buffer.remove(at);
        }
    }

    fn delete(&mut self) {
        let at = self.byte_cursor();
        if at < self.buffer.len() {
            self.buffer.remove(at);
        }
    }
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

/// One refresh's fetch result and token check; trust needs both.
struct RefreshOutcome {
    instance: DaemonInstance,
    status_ok: bool,
}

/// Outcome of a daemon instance-token check, from the push path's point
/// of view (see [`App::check_daemon_instance`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum DaemonInstance {
    /// Owner resolved and matches the previously observed instance.
    Same,
    /// Owner resolved to a different instance than previously observed.
    Replaced,
    /// Owner could not be resolved — the daemon is down, or a transient
    /// resolution hiccup; indistinguishable from here.
    Unknown,
}

#[allow(clippy::struct_excessive_bools)]
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
    /// Lazily filled per-scheduler answers to the backend's mode-argument
    /// query. An absent key means "not fetched yet or the query failed" —
    /// never "nothing configured" — so consumers fail open on it. Valid
    /// for the lifetime of one daemon instance; see
    /// [`Self::check_daemon_instance`].
    mode_args: HashMap<String, ModeArgs>,
    /// Last observed backend instance token; a change invalidates
    /// `mode_args`.
    daemon_instance: Option<String>,
    /// Only a *difference* against it is acted on; never synced to polled
    /// truth.
    pushed: Option<RuntimeStatus>,
    /// First push movement relaxes the poll; the first read is baseline.
    push_confirmed: bool,
    /// Custom-arguments field; `Some` while the user is typing. While
    /// open it owns every key press.
    pub args_input: Option<ArgsInput>,
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
            mode_args: HashMap::new(),
            daemon_instance: None,
            pushed: None,
            push_confirmed: false,
            args_input: None,
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

    /// Resolved arguments for the selected scheduler and mode, when
    /// known. `None` covers "not fetched / query failed" as well as a mode
    /// the daemon did not report; the preview only renders a known,
    /// non-empty list, so both collapse into "nothing to show".
    pub fn selected_mode_args(&self) -> Option<&[String]> {
        let modes = self.mode_args.get(self.selected_scheduler()?)?;
        let mode = self.selected_mode();
        modes
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, args)| args.as_slice())
    }

    /// Whether the selected mode has configured arguments for the selected
    /// scheduler. Mirrors scxctl's client-side warning: `Auto` always
    /// counts, and an unknown answer (never fetched, or the query failed —
    /// either way no cache entry) fails open so we never scare the user
    /// over a transient D-Bus hiccup. A *successful* answer is exact: the
    /// daemon reports every mode, so a mode missing its arguments there
    /// genuinely has none configured.
    pub fn selected_mode_configured(&self) -> bool {
        let mode = self.selected_mode();
        if mode == SchedMode::Auto {
            return true;
        }
        let Some(modes) = self
            .selected_scheduler()
            .and_then(|sched| self.mode_args.get(sched))
        else {
            return true;
        };
        modes.iter().any(|(m, args)| *m == mode && !args.is_empty())
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

            // Sysfs can change with no daemon involved; rides every tick.
            self.kernel = kernel::read();

            // Pushed changes do not reset the safety poll.
            self.apply_pushed_status();
            if last_refresh.elapsed() >= self.refresh_interval() {
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
            PendingAction::StartOrSwitchWithArgs(args) => self.start_or_switch_with_args(&args),
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
        if self.view == View::Schedulers && self.args_input.is_some() {
            self.on_key_args(key);
            return;
        }
        match self.view {
            View::Schedulers => self.on_key_schedulers(key),
            View::Logs => self.on_key_logs(key),
        }
    }

    /// Key handling while the custom-arguments field is open. The field
    /// owns every key press: global shortcuts must not fire while the
    /// user is typing text that may well contain their letters — which
    /// also means `Esc` closes the field here instead of quitting.
    fn on_key_args(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.args_input = None,
            KeyCode::Enter => self.submit_args(),
            code => {
                let Some(input) = self.args_input.as_mut() else {
                    return;
                };
                match code {
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.insert(c);
                    }
                    KeyCode::Backspace => input.backspace(),
                    KeyCode::Delete => input.delete(),
                    KeyCode::Left => input.cursor = input.cursor.saturating_sub(1),
                    KeyCode::Right => input.cursor = (input.cursor + 1).min(input.len_chars()),
                    KeyCode::Home => input.cursor = 0,
                    KeyCode::End => input.cursor = input.len_chars(),
                    _ => {}
                }
            }
        }
    }

    /// `Enter` in the field: expand exactly like scxctl's `--args`, then
    /// queue the start/switch. Expansion errors keep the field open with
    /// the message bar explaining what to fix; only a successfully queued
    /// action closes it.
    fn submit_args(&mut self) {
        let Some(buffer) = self.args_input.as_ref().map(|input| input.buffer.clone()) else {
            return;
        };
        match expand_input(&buffer) {
            Ok(args) => {
                if self.action_allowed() {
                    self.args_input = None;
                    self.queue(PendingAction::StartOrSwitchWithArgs(args));
                }
            }
            Err(ArgsExpandError::Parse(msg)) => {
                self.error(&format!(
                    "invalid arguments: {msg} — quotes must be balanced and cannot span a comma"
                ));
            }
            Err(ArgsExpandError::Empty) => {
                self.error(
                    "arguments expanded to nothing — to run with scheduler defaults, \
press Esc and use Enter instead",
                );
            }
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
            KeyCode::Char('a') if self.backend.capabilities().custom_args => {
                if self.selected_scheduler().is_some() {
                    self.args_input = Some(ArgsInput::default());
                }
            }
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
                // Manual refresh means "give me the current truth", so the
                // lazy cache must not satisfy it with old answers.
                self.mode_args.clear();
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
                // observed token start over — and the push baseline with them.
                self.mode_args.clear();
                self.daemon_instance = None;
                self.pushed = None;
                self.push_confirmed = false;
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

    /// The with-args sibling of [`Self::start_or_switch`]: the same
    /// start-vs-switch decision against a re-read daemon state, but the
    /// scheduler receives the expanded field content instead of a mode.
    /// The arguments are session-only — the loader keeps them for this
    /// run and nothing is written to its config — and both the success
    /// notice and a failure render them in the same shell-words form the
    /// status panel and scxctl use.
    fn start_or_switch_with_args(&mut self, args: &[String]) {
        let Some(sched) = self.selected_scheduler().map(str::to_owned) else {
            return;
        };
        // Same staleness concern as in `start_or_switch`.
        self.refresh_status();
        let running = self
            .status
            .as_ref()
            .and_then(|status| status.current.clone());

        let result = if running.is_some() {
            self.backend.switch_with_args(&sched, args)
        } else {
            self.backend.start_with_args(&sched, args)
        };

        let rendered = shell_words::join(args);
        match result {
            Ok(()) => {
                let verb = if running.is_none() {
                    "started"
                } else if running.as_deref() == Some(sched.as_str()) {
                    "restarted"
                } else {
                    "switched to"
                };
                self.info(&format!(
                    "{verb} {sched} with args: {rendered} (session-only)"
                ));
            }
            Err(err) => {
                let verb = if running.is_some() {
                    "switch to"
                } else {
                    "start"
                };
                self.error(&format!(
                    "{verb} {sched} with args '{rendered}' failed: {err:#}"
                ));
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

    fn refresh_status(&mut self) -> RefreshOutcome {
        self.status = self.backend.status().ok();
        let status_ok = self.status.is_some();
        self.kernel = kernel::read();
        RefreshOutcome {
            instance: self.check_daemon_instance(),
            status_ok,
        }
    }

    /// Conservative until push is confirmed.
    fn refresh_interval(&self) -> Duration {
        if self.push_confirmed {
            SAFETY_REFRESH
        } else {
            REFRESH_EVERY
        }
    }

    /// Doorbell, never payload: a movement triggers one authoritative
    /// refresh and only that answer reaches the display; the poll relaxes
    /// only on a same-owner movement.
    fn apply_pushed_status(&mut self) {
        let now = match self.backend.cached_status() {
            Ok(Some(now)) => now,
            Ok(None) => return,
            Err(_) => {
                // Opportunistic; the real guarantee is the token reset.
                if self.pushed.is_some() {
                    self.pushed = None;
                    self.push_confirmed = false;
                }
                return;
            }
        };
        if self.pushed.as_ref() == Some(&now) {
            return;
        }
        if self.pushed.is_none() {
            // The first read primes the doorbell, it does not ring it.
            self.pushed = Some(now);
            return;
        }
        let outcome = self.refresh_status();
        // New baseline even on failure, or it rings every tick.
        self.pushed = Some(now);
        // Per ring: successful fetch AND same owner; failure revokes.
        self.push_confirmed = outcome.status_ok && outcome.instance == DaemonInstance::Same;
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

    /// Lazily fills the per-scheduler argument cache. The daemon reads its
    /// configuration once at startup, so a successful answer stays valid
    /// until the daemon itself is replaced — which `check_daemon_instance`
    /// watches for. A failed query is deliberately *not* cached: an absent
    /// entry reads as "unknown" (fail open in `selected_mode_configured`)
    /// and the next call here simply retries.
    fn refresh_modes(&mut self) {
        if !self.backend.capabilities().modes {
            return;
        }
        let Some(sched) = self.selected_scheduler() else {
            return;
        };
        if self.mode_args.contains_key(sched) {
            return;
        }
        let sched = sched.to_owned();
        if let Ok(modes) = self.backend.mode_args(&sched) {
            self.mode_args.insert(sched, modes);
        }
    }

    fn check_daemon_instance(&mut self) -> DaemonInstance {
        let Some(token) = self.backend.instance_token() else {
            // Caches stay; the relaxed cadence does not ride out an outage.
            self.pushed = None;
            self.push_confirmed = false;
            return DaemonInstance::Unknown;
        };
        let replaced = self
            .daemon_instance
            .as_deref()
            .is_some_and(|old| old != token);
        if replaced {
            self.mode_args.clear();
            self.refresh_modes();
            // Confirmation is per instance: a replaced daemon re-earns it.
            self.pushed = None;
            self.push_confirmed = false;
        }
        self.daemon_instance = Some(token);
        if replaced {
            DaemonInstance::Replaced
        } else {
            DaemonInstance::Same
        }
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
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::anyhow;

    use super::*;

    /// Minimal in-memory backend: enough for exercising selection and
    /// caching logic without a D-Bus connection.
    struct StubBackend {
        schedulers: Vec<String>,
        /// Per-scheduler answer for `mode_args`; a missing key errors,
        /// modeling a failed D-Bus query.
        modes: HashMap<String, ModeArgs>,
        /// How many times `mode_args` reached the backend, shared with the
        /// test so caching is observable from the outside.
        mode_queries: Rc<RefCell<usize>>,
        /// Current instance token, shared so a test can "restart" the
        /// daemon underneath the app.
        token: Rc<RefCell<Option<String>>>,
        /// `None` = no push channel.
        cached: Rc<RefCell<Option<Status>>>,
        /// When set, `cached_status` errors — models the daemon side of
        /// the push channel going away.
        push_broken: Rc<RefCell<bool>>,
        generation: Rc<RefCell<String>>,
        /// How many times `status` reached the backend, shared with the
        /// test so the dispute tripwire is observable from the outside.
        status_queries: Rc<RefCell<usize>>,
        /// The authoritative answer `status` returns, shared so a test
        /// can model what the daemon actually reports — as opposed to
        /// what the (possibly stale) push snapshot claims.
        truth: Rc<RefCell<Status>>,
        /// When set, `status` errors after counting the attempt — models
        /// the authoritative fetch failing (a dead daemon, a D-Bus
        /// hiccup) independently of the push channel.
        status_broken: Rc<RefCell<bool>>,
    }

    impl StubBackend {
        fn new() -> Self {
            Self {
                schedulers: vec!["scx_bpfland".into(), "scx_lavd".into(), "scx_cake".into()],
                modes: HashMap::new(),
                mode_queries: Rc::new(RefCell::new(0)),
                token: Rc::new(RefCell::new(None)),
                cached: Rc::new(RefCell::new(None)),
                push_broken: Rc::new(RefCell::new(false)),
                generation: Rc::new(RefCell::new(String::from(":1.1"))),
                status_queries: Rc::new(RefCell::new(0)),
                truth: Rc::new(RefCell::new(Status {
                    current: None,
                    mode: SchedMode::Auto,
                    args: Vec::new(),
                    default_sched: None,
                    default_mode: SchedMode::Auto,
                })),
                status_broken: Rc::new(RefCell::new(false)),
            }
        }
    }

    impl SchedulerBackend for StubBackend {
        fn label(&self) -> &'static str {
            "stub"
        }
        fn instance_token(&self) -> Option<String> {
            self.token.borrow().clone()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                live_switch: true,
                modes: true,
                custom_args: true,
                restore_default: true,
            }
        }
        fn status(&self) -> Result<Status> {
            *self.status_queries.borrow_mut() += 1;
            if *self.status_broken.borrow() {
                return Err(anyhow!("authoritative fetch broken"));
            }
            // The configured authoritative answer; defaults to a benign
            // "nothing running", so tests may drive refresh paths
            // without setting it up.
            Ok(self.truth.borrow().clone())
        }
        fn supported_schedulers(&self) -> Result<Vec<String>> {
            Ok(self.schedulers.clone())
        }
        fn cached_status(&self) -> Result<Option<RuntimeStatus>> {
            if *self.push_broken.borrow() {
                return Err(anyhow!("push channel broken"));
            }
            Ok(self.cached.borrow().as_ref().map(|status| RuntimeStatus {
                current: status.current.clone(),
                mode: status.mode,
                args: status.args.clone(),
                generation: self.generation.borrow().clone(),
            }))
        }
        fn mode_args(&self, sched: &str) -> Result<ModeArgs> {
            *self.mode_queries.borrow_mut() += 1;
            self.modes
                .get(sched)
                .cloned()
                .ok_or_else(|| anyhow!("no mode answer configured for {sched}"))
        }
        fn start(&self, _sched: &str, _mode: SchedMode) -> Result<()> {
            Ok(())
        }
        fn switch(&self, _sched: &str, _mode: SchedMode) -> Result<()> {
            Ok(())
        }
        fn start_with_args(&self, _sched: &str, _args: &[String]) -> Result<()> {
            Ok(())
        }
        fn switch_with_args(&self, _sched: &str, _args: &[String]) -> Result<()> {
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

    /// Answer for the default selection (`scx_bpfland`): gaming has
    /// arguments, powersave is present but empty, the rest untouched.
    fn bpfland_modes() -> ModeArgs {
        vec![
            (SchedMode::Auto, Vec::new()),
            (
                SchedMode::Gaming,
                vec!["-k".into(), "-s".into(), "5000".into()],
            ),
            (SchedMode::PowerSave, Vec::new()),
        ]
    }

    fn app_with_backend(backend: StubBackend) -> App {
        App::new(BackendKind::Loader, Box::new(backend)).unwrap()
    }

    fn app_with_status(status: Status) -> App {
        let mut app = app_with_backend(StubBackend::new());
        app.status = Some(status);
        app
    }

    fn select_mode(app: &mut App, mode: SchedMode) {
        app.mode_idx = MODES.iter().position(|m| *m == mode).unwrap();
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
    fn configured_follows_the_cached_argument_lists() {
        let mut backend = StubBackend::new();
        backend.modes.insert("scx_bpfland".into(), bpfland_modes());
        let mut app = app_with_backend(backend);
        app.refresh_modes();

        select_mode(&mut app, SchedMode::Auto);
        assert!(app.selected_mode_configured(), "Auto always counts");
        select_mode(&mut app, SchedMode::Gaming);
        assert!(app.selected_mode_configured(), "non-empty arguments");
        select_mode(&mut app, SchedMode::PowerSave);
        assert!(
            !app.selected_mode_configured(),
            "present in the answer with an empty argument list"
        );
        select_mode(&mut app, SchedMode::Server);
        assert!(
            !app.selected_mode_configured(),
            "absent from a successful answer means not configured"
        );
    }

    #[test]
    fn unknown_answer_fails_open() {
        // No cache entry — the query failed and nothing was stored. Every
        // mode must count as configured so a transient D-Bus hiccup never
        // produces a scary warning.
        let mut app = app_with_backend(StubBackend::new());
        select_mode(&mut app, SchedMode::Server);
        assert!(app.selected_mode_configured());
    }

    #[test]
    fn selected_mode_args_follows_the_selection() {
        let mut backend = StubBackend::new();
        backend.modes.insert("scx_bpfland".into(), bpfland_modes());
        let mut app = app_with_backend(backend);
        app.refresh_modes();

        select_mode(&mut app, SchedMode::Gaming);
        let expected = ["-k", "-s", "5000"].map(str::to_string);
        assert_eq!(app.selected_mode_args(), Some(&expected[..]));

        select_mode(&mut app, SchedMode::Server);
        assert_eq!(
            app.selected_mode_args(),
            None,
            "a mode absent from the answer has nothing to preview"
        );

        // scx_lavd has no cache entry at all: same outcome, for the
        // "unknown" reason.
        app.selected = 1;
        assert_eq!(app.selected_mode_args(), None);
    }

    #[test]
    fn cache_serves_repeat_queries() {
        let mut backend = StubBackend::new();
        backend.modes.insert("scx_bpfland".into(), bpfland_modes());
        let queries = Rc::clone(&backend.mode_queries);
        let mut app = app_with_backend(backend);

        app.refresh_modes();
        app.refresh_modes();
        assert_eq!(*queries.borrow(), 1, "second call must hit the cache");
    }

    #[test]
    fn failed_query_is_not_cached() {
        // The stub has no answer for any scheduler, so every query fails.
        let backend = StubBackend::new();
        let queries = Rc::clone(&backend.mode_queries);
        let mut app = app_with_backend(backend);

        app.refresh_modes();
        app.refresh_modes();
        assert_eq!(
            *queries.borrow(),
            2,
            "a failure must stay uncached so the next call retries"
        );
    }

    #[test]
    fn daemon_restart_clears_the_cache() {
        let mut backend = StubBackend::new();
        backend.modes.insert("scx_bpfland".into(), bpfland_modes());
        let queries = Rc::clone(&backend.mode_queries);
        let token = Rc::clone(&backend.token);
        let mut app = app_with_backend(backend);

        *token.borrow_mut() = Some(":1.7".into());
        app.check_daemon_instance();
        app.refresh_modes();
        assert_eq!(*queries.borrow(), 1);

        // Same daemon: the periodic check must not disturb the cache.
        app.check_daemon_instance();
        app.refresh_modes();
        assert_eq!(*queries.borrow(), 1);

        // "Restart" the daemon: new unique name, cache dropped and the
        // selected scheduler refetched immediately.
        *token.borrow_mut() = Some(":1.9".into());
        app.check_daemon_instance();
        assert_eq!(*queries.borrow(), 2);
    }

    #[test]
    fn manual_refresh_bypasses_the_cache() {
        let mut backend = StubBackend::new();
        backend.modes.insert("scx_bpfland".into(), bpfland_modes());
        let queries = Rc::clone(&backend.mode_queries);
        let mut app = app_with_backend(backend);

        app.refresh_modes();
        assert_eq!(*queries.borrow(), 1);
        app.on_key(KeyEvent::from(KeyCode::Char('R')));
        assert_eq!(
            *queries.borrow(),
            2,
            "R must refetch instead of serving the cache"
        );
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn args_field_opens_on_a_and_owns_global_keys() {
        let mut app = app_with_backend(StubBackend::new());
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.args_input.is_some());

        // 'q' quits globally, but inside the field it is just a letter.
        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert_eq!(app.args_input.as_ref().unwrap().buffer, "q");

        // Esc closes the field instead of quitting the app.
        app.on_key(key(KeyCode::Esc));
        assert!(app.args_input.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn args_field_edits_at_the_character_cursor() {
        let mut app = app_with_backend(StubBackend::new());
        app.on_key(key(KeyCode::Char('a')));
        type_str(&mut app, "ad");
        app.on_key(key(KeyCode::Left));
        type_str(&mut app, "bc");
        app.on_key(key(KeyCode::End));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.args_input.as_ref().unwrap().buffer, "abc");

        // Multi-byte characters: the cursor counts characters, not bytes.
        app.on_key(key(KeyCode::Home));
        type_str(&mut app, "żó");
        app.on_key(key(KeyCode::Home));
        app.on_key(key(KeyCode::Delete));
        assert_eq!(app.args_input.as_ref().unwrap().buffer, "óabc");
    }

    #[test]
    fn args_parse_error_keeps_the_field_open() {
        let mut app = app_with_backend(StubBackend::new());
        app.on_key(key(KeyCode::Char('a')));
        type_str(&mut app, "--name \"foo");
        app.on_key(key(KeyCode::Enter));

        assert!(app.args_input.is_some(), "the user must be able to fix it");
        assert!(app.message.as_ref().unwrap().is_error);
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn args_empty_input_is_rejected() {
        let mut app = app_with_backend(StubBackend::new());
        app.on_key(key(KeyCode::Char('a')));
        type_str(&mut app, "   ");
        app.on_key(key(KeyCode::Enter));

        assert!(app.args_input.is_some());
        assert!(app.message.as_ref().unwrap().is_error);
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn args_valid_input_queues_the_expanded_action() {
        let mut app = app_with_backend(StubBackend::new());
        app.on_key(key(KeyCode::Char('a')));
        type_str(&mut app, "-s 20000,-m powersave");
        app.on_key(key(KeyCode::Enter));

        assert!(app.args_input.is_none(), "a queued action closes the field");
        let Some(PendingAction::StartOrSwitchWithArgs(args)) = &app.pending_action else {
            panic!("expected a queued with-args action");
        };
        assert_eq!(
            args,
            &["-s", "20000", "-m", "powersave"]
                .map(str::to_string)
                .to_vec()
        );
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

    fn idle_status() -> Status {
        Status {
            current: None,
            mode: SchedMode::Auto,
            args: Vec::new(),
            default_sched: None,
            default_mode: SchedMode::Auto,
        }
    }

    fn running_status(sched: &str) -> Status {
        Status {
            current: Some(sched.into()),
            ..idle_status()
        }
    }

    fn running_with_args(sched: &str, args: &[&str]) -> Status {
        Status {
            args: args.iter().map(ToString::to_string).collect(),
            ..running_status(sched)
        }
    }

    #[test]
    fn first_push_read_is_baseline_only() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let mut app = app_with_backend(backend);
        *cached.borrow_mut() = Some(running_status("scx_bpfland"));

        app.apply_pushed_status();

        // The snapshot became the baseline but was not treated as news:
        // the status panel is untouched, the poll stays conservative.
        assert!(app.status.is_none());
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
    }

    #[test]
    fn push_movement_fetches_truth_and_relaxes_the_poll() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let queries = Rc::clone(&backend.status_queries);
        let truth = Rc::clone(&backend.truth);
        let mut app = app_with_backend(backend);
        *token.borrow_mut() = Some("owner-1".into());
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        assert_eq!(*queries.borrow(), 0);

        *truth.borrow_mut() = running_status("scx_cake");
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();

        // A moved snapshot rings the bell: one authoritative fetch, its
        // answer on the panel, and the safety poll takes over from the
        // 5s poll.
        assert_eq!(*queries.borrow(), 1);
        assert_eq!(
            app.status.as_ref().and_then(|s| s.current.as_deref()),
            Some("scx_cake")
        );
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);
    }

    #[test]
    fn push_payload_is_the_authoritative_answer_not_the_cache() {
        // The cache decides *whether* to fetch, never *what* to display:
        // when the two disagree — a torn read of the three getters, or
        // any staleness — the fetched answer wins, within the same tick.
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let truth = Rc::clone(&backend.truth);
        let mut app = app_with_backend(backend);
        *token.borrow_mut() = Some("owner-1".into());
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();

        *truth.borrow_mut() = running_status("scx_lavd");
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();

        assert_eq!(
            app.status.as_ref().and_then(|s| s.current.as_deref()),
            Some("scx_lavd")
        );
    }

    #[test]
    fn frozen_push_snapshot_never_overrides_polled_truth() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let mut app = app_with_backend(backend);
        // A daemon that never emits: the snapshot freezes at its first
        // value, here claiming scx_bpfland forever.
        *cached.borrow_mut() = Some(running_status("scx_bpfland"));
        app.apply_pushed_status();

        // The safety poll later learns the actual truth.
        app.status = Some(running_status("scx_lavd"));

        // Any number of further reads of the frozen snapshot must neither
        // clobber that truth nor relax the poll.
        app.apply_pushed_status();
        app.apply_pushed_status();
        assert_eq!(
            app.status.as_ref().and_then(|s| s.current.as_deref()),
            Some("scx_lavd")
        );
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
    }

    #[test]
    fn backend_without_push_channel_keeps_polling() {
        let mut app = app_with_backend(StubBackend::new());

        app.apply_pushed_status();

        assert!(app.status.is_none());
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
    }

    #[test]
    fn broken_push_channel_falls_back_to_the_conservative_poll() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let broken = Rc::clone(&backend.push_broken);
        let token = Rc::clone(&backend.token);
        let mut app = app_with_backend(backend);

        // Confirmed, relaxed channel...
        *token.borrow_mut() = Some("owner-1".into());
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);

        // ...whose daemon side goes away: back to the conservative poll.
        *broken.borrow_mut() = true;
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);

        // A repaired channel starts from scratch: the first read is a
        // baseline again, not a change to act on.
        *broken.borrow_mut() = false;
        *cached.borrow_mut() = Some(idle_status());
        let polled = app.status.clone();
        app.apply_pushed_status();
        assert_eq!(app.status, polled);
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
    }

    #[test]
    fn replaced_daemon_must_re_earn_the_relaxed_poll() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let mut app = app_with_backend(backend);

        // Confirmed channel against the first daemon instance.
        *token.borrow_mut() = Some("owner-1".into());
        app.refresh_status();
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);

        // The daemon gets replaced. The stub keeps serving the old
        // snapshot values on purpose — modeling a cache that never
        // errors after owner loss — because the reset must not depend
        // on the error path firing.
        *token.borrow_mut() = Some("owner-2".into());
        app.refresh_status();
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);

        // The frozen snapshot is a baseline for the new instance, not a
        // change to act on.
        let polled = app.status.clone();
        app.apply_pushed_status();
        assert_eq!(app.status, polled);
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
    }

    #[test]
    fn daemon_outage_drops_the_relaxed_cadence_but_keeps_config_caches() {
        let mut backend = StubBackend::new();
        backend
            .modes
            .insert("scx_bpfland".to_owned(), bpfland_modes());
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let mut app = app_with_backend(backend);

        // Healthy, confirmed instance with a populated mode cache.
        *token.borrow_mut() = Some("owner-1".into());
        app.refresh_modes();
        assert!(app.mode_args.contains_key("scx_bpfland"));
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);

        // Owner gone. The confirmation must not survive the outage, but
        // known-good config answers must — a mere hiccup looks exactly
        // the same from here, and their staleness across a *replacement*
        // is what the token-change reset handles once a new owner
        // appears.
        *token.borrow_mut() = None;
        app.refresh_status();
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
        assert!(app.mode_args.contains_key("scx_bpfland"));
    }

    #[test]
    fn dying_gasp_rings_the_bell_but_extends_no_trust() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let broken = Rc::clone(&backend.status_broken);
        let mut app = app_with_backend(backend);

        // Confirmed channel against a live owner.
        *token.borrow_mut() = Some("owner-1".into());
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_status("scx_lavd"));
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);

        // Clean shutdown: show unknown, extend no trust.
        *cached.borrow_mut() = Some(idle_status());
        *token.borrow_mut() = None;
        *broken.borrow_mut() = true;
        app.apply_pushed_status();
        assert_eq!(app.status, None);
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
    }

    #[test]
    fn failed_fetch_never_confirms_the_push() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let broken = Rc::clone(&backend.status_broken);
        let queries = Rc::clone(&backend.status_queries);
        let truth = Rc::clone(&backend.truth);
        let mut app = app_with_backend(backend);

        // Confirmed channel against owner-1.
        *token.borrow_mut() = Some("owner-1".into());
        *truth.borrow_mut() = running_status("scx_cake");
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);

        // Fetch fails, same owner: show unknown, revoke trust.
        *broken.borrow_mut() = true;
        *cached.borrow_mut() = Some(running_status("scx_lavd"));
        app.apply_pushed_status();
        let attempts = *queries.borrow();

        assert_eq!(app.status, None);
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);

        // The baseline advanced despite the failure: the same snapshot
        // must not ring the bell four times a second — recovery rides
        // the (now conservative) safety poll instead.
        app.apply_pushed_status();
        assert_eq!(*queries.borrow(), attempts);
    }

    #[test]
    fn stale_owner_keys_never_return_on_later_partial_changes() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let queries = Rc::clone(&backend.status_queries);
        let truth = Rc::clone(&backend.truth);
        let mut app = app_with_backend(backend);

        // Act 1: confirmed channel against owner-1, whose actual state
        // carries arguments — the key that will go stale.
        *token.borrow_mut() = Some("owner-1".into());
        *truth.borrow_mut() = running_with_args("scx_cake", &["--foo"]);
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_with_args("scx_cake", &["--foo"]));
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);
        let polls_so_far = *queries.borrow();

        // Act 2: owner-2 replaces owner-1 and its full-state
        // announcement is missed. Its diff re-emits only the scheduler,
        // so the cache mixes owner-2's scheduler with owner-1's
        // leftover arguments — a state no daemon ever held.
        *token.borrow_mut() = Some("owner-2".into());
        *truth.borrow_mut() = running_status("scx_lavd");
        *cached.borrow_mut() = Some(Status {
            current: Some("scx_lavd".into()),
            ..running_with_args("scx_lavd", &["--foo"])
        });
        app.apply_pushed_status();

        // One authoritative fetch answered instead of the chimera, and
        // the replacement movement did not confirm.
        assert_eq!(*queries.borrow(), polls_so_far + 1);
        assert_eq!(app.status, Some(running_status("scx_lavd")));
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);

        // Partial change leaves a stale cache key; show owner-2's truth.
        *truth.borrow_mut() = Status {
            mode: SchedMode::Gaming,
            ..running_status("scx_lavd")
        };
        *cached.borrow_mut() = Some(Status {
            mode: SchedMode::Gaming,
            ..running_with_args("scx_lavd", &["--foo"])
        });
        app.apply_pushed_status();

        assert_eq!(*queries.borrow(), polls_so_far + 2);
        let status = app.status.as_ref().expect("fetched");
        assert_eq!(status.mode, SchedMode::Gaming);
        assert!(status.args.is_empty(), "stale owner-1 arguments returned");
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);
    }

    #[test]
    fn poll_first_replacement_never_applies_the_stale_cache() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let queries = Rc::clone(&backend.status_queries);
        let truth = Rc::clone(&backend.truth);
        let mut app = app_with_backend(backend);

        // Confirmed channel against owner-1, running with arguments.
        *token.borrow_mut() = Some("owner-1".into());
        *truth.borrow_mut() = running_with_args("scx_cake", &["--foo"]);
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(running_with_args("scx_cake", &["--foo"]));
        app.apply_pushed_status();

        // The safety poll notices owner-2 first: the token check wipes
        // the baseline and the confirmation along the way.
        *token.borrow_mut() = Some("owner-2".into());
        *truth.borrow_mut() = running_status("scx_lavd");
        app.refresh_status();
        assert_eq!(app.status, Some(running_status("scx_lavd")));
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);
        let polls_so_far = *queries.borrow();

        // The next look at the still-stale cache is a baseline priming,
        // not data: nothing may reach the display from it.
        app.apply_pushed_status();
        assert_eq!(*queries.borrow(), polls_so_far);
        assert_eq!(app.status, Some(running_status("scx_lavd")));

        // Owner-2's later partial change moves the cache but leaves the
        // stale arguments key in place; the display gets the fetched
        // truth, never the leftover.
        *truth.borrow_mut() = Status {
            mode: SchedMode::PowerSave,
            ..running_status("scx_lavd")
        };
        *cached.borrow_mut() = Some(Status {
            mode: SchedMode::PowerSave,
            ..running_with_args("scx_lavd", &["--foo"])
        });
        app.apply_pushed_status();

        assert_eq!(*queries.borrow(), polls_so_far + 1);
        let status = app.status.as_ref().expect("fetched");
        assert_eq!(status.mode, SchedMode::PowerSave);
        assert!(status.args.is_empty(), "stale owner-1 arguments returned");
    }

    #[test]
    fn identical_values_under_a_new_generation_still_ring() {
        let backend = StubBackend::new();
        let cached = Rc::clone(&backend.cached);
        let token = Rc::clone(&backend.token);
        let queries = Rc::clone(&backend.status_queries);
        let generation = Rc::clone(&backend.generation);
        let mut app = app_with_backend(backend);

        // Confirmed channel against owner-1, which then goes idle — the
        // exact runtime values a silent replacement will re-announce.
        *token.borrow_mut() = Some("owner-1".into());
        *cached.borrow_mut() = Some(running_status("scx_cake"));
        app.apply_pushed_status();
        *cached.borrow_mut() = Some(idle_status());
        app.apply_pushed_status();
        assert_eq!(app.refresh_interval(), SAFETY_REFRESH);
        let polls_so_far = *queries.borrow();

        // Silent replacement, identical values: only the generation moves.
        *token.borrow_mut() = Some("owner-2".into());
        *generation.borrow_mut() = String::from(":1.2");
        app.apply_pushed_status();

        assert_eq!(*queries.borrow(), polls_so_far + 1);
        assert_eq!(app.refresh_interval(), REFRESH_EVERY);

        // The new generation is now the baseline: an unchanged snapshot
        // stays inert.
        app.apply_pushed_status();
        assert_eq!(*queries.borrow(), polls_so_far + 1);
    }
}
