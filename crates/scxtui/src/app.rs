// SPDX-License-Identifier: GPL-2.0

//! Application state and event loop.

use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::DefaultTerminal;
use scx_loader::SchedMode;

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

/// How often the input poll wakes up to redraw / refresh.
const TICK: Duration = Duration::from_millis(250);
/// How often the status is refreshed in the background. The scheduler can
/// change under us (scxctl, another scxtui, a desktop applet), so the view
/// must not assume it is the only writer. Kept moderate because with property
/// caching disabled every refresh hits the daemon, and its CurrentScheduler
/// getter currently logs each read to the journal; can go back down once the
/// daemon-side log demotion / PropertiesChanged work lands.
const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// Minimum spacing between two scheduler-affecting actions. Linux terminals
/// deliver key autorepeat as plain `Press` events (no kitty protocol), so
/// without this, holding `r` would fire one restart per repeat.
const ACTION_DEBOUNCE: Duration = Duration::from_millis(500);

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
    pub schedulers: Vec<String>,
    pub selected: usize,
    pub mode_idx: usize,
    pub status: Option<Status>,
    /// The kernel's own view of sched_ext, refreshed alongside `status`.
    /// `None` = kernel without sched_ext support.
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
    /// PgUp/PgDn can page by exactly one screen.
    pub log_page: usize,
    /// Set by the `t` key; the event loop launches scxtop on the next
    /// iteration, where it has access to the terminal.
    pending_monitor: bool,
    should_quit: bool,
}

impl App {
    pub fn new(backend: Box<dyn SchedulerBackend>) -> Result<Self> {
        let schedulers = backend.supported_schedulers()?;
        let mut app = Self {
            backend,
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
            pending_monitor: false,
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

            if event::poll(TICK)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key);
                    }
                }
            }

            if self.pending_monitor {
                self.pending_monitor = false;
                self.run_monitor(&mut terminal)?;
                last_refresh = Instant::now();
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
    /// dependency chain (and its root/CAP_BPF requirement) out of this
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
            KeyCode::Char('t') => self.pending_monitor = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Tab | KeyCode::Char('m') => self.cycle_mode(1),
            KeyCode::BackTab | KeyCode::Char('M') => self.cycle_mode(-1),
            KeyCode::Enter => {
                if self.action_allowed() {
                    self.start_or_switch();
                }
            }
            KeyCode::Char('s') => {
                if self.action_allowed() {
                    self.act("stopped", |b| b.stop());
                }
            }
            KeyCode::Char('r') => {
                if self.action_allowed() {
                    self.act("restarted", |b| b.restart());
                }
            }
            KeyCode::Char('d') => {
                if self.backend.capabilities().restore_default && self.action_allowed() {
                    self.act("restored default scheduler", |b| b.restore_default());
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
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Char('l') => self.view = View::Schedulers,
            KeyCode::Up | KeyCode::Char('k') => self.log_scroll_by(1),
            KeyCode::Down | KeyCode::Char('j') => self.log_scroll_by(-1),
            KeyCode::PageUp => self.log_scroll_by(self.log_page as isize),
            KeyCode::PageDown => self.log_scroll_by(-(self.log_page as isize)),
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

    fn log_scroll_by(&mut self, delta: isize) {
        // Upper bound is clamped against the viewport in the UI, which knows
        // the current height; saturate at zero here.
        self.log_scroll = self.log_scroll.saturating_add_signed(delta);
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

    fn cycle_mode(&mut self, dir: isize) {
        let len = MODES.len() as isize;
        self.mode_idx = ((self.mode_idx as isize + dir).rem_euclid(len)) as usize;
    }

    /// Debounce gate for scheduler-affecting actions (Enter/s/r/d). Returns
    /// `true` and arms the timer when enough time has passed since the last
    /// action; swallows the event otherwise. See [`ACTION_DEBOUNCE`].
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

    /// `Enter`: start when nothing is running, switch otherwise — the TUI
    /// can make that call itself instead of erroring like a CLI has to.
    fn start_or_switch(&mut self) {
        let Some(sched) = self.selected_scheduler().map(str::to_owned) else {
            return;
        };
        let mode = self.selected_mode();
        let running = self
            .status
            .as_ref()
            .is_some_and(|status| status.current.is_some());

        let (verb, result) = if running {
            ("switched to", self.backend.switch(&sched, mode))
        } else {
            ("started", self.backend.start(&sched, mode))
        };

        match result {
            Ok(()) => {
                let text = if self.selected_mode_configured() {
                    format!("{verb} {sched} in {mode:?} mode")
                } else {
                    format!("{verb} {sched} with its own defaults ({mode:?} not configured)")
                };
                self.info(&text);
            }
            Err(err) => self.error(&format!("{verb} {sched} failed: {err:#}")),
        }
        self.refresh_status();
    }

    fn act(&mut self, ok_text: &str, op: impl FnOnce(&dyn SchedulerBackend) -> Result<()>) {
        match op(self.backend.as_ref()) {
            Ok(()) => self.info(ok_text),
            Err(err) => self.error(&format!("{err:#}")),
        }
        self.refresh_status();
    }

    fn refresh_status(&mut self) {
        match self.backend.status() {
            Ok(status) => self.status = Some(status),
            Err(err) => self.error(&format!("status query failed: {err:#}")),
        }
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
