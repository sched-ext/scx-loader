// SPDX-License-Identifier: GPL-2.0

//! Application state and event loop.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::DefaultTerminal;
use scx_loader::SchedMode;

use crate::backend::{Capabilities, SchedulerBackend, Status};
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
    /// Configured modes for the currently selected scheduler.
    pub configured_modes: Vec<SchedMode>,
    pub message: Option<Message>,
    /// Timestamp of the last scheduler-affecting action, for debouncing.
    last_action: Option<Instant>,
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
            configured_modes: Vec::new(),
            message: None,
            last_action: None,
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

            if last_refresh.elapsed() >= REFRESH_EVERY {
                self.refresh_status();
                last_refresh = Instant::now();
            }
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
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
