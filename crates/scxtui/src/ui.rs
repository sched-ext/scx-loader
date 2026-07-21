// SPDX-License-Identifier: GPL-2.0

//! Rendering. Almost a pure function of the [`App`] state — the one
//! exception is the log view writing its viewport height and clamped scroll
//! offset back into the app, so paging and bounds always match the actual
//! terminal size.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, View};
use crate::logs;

const SCHED_PREFIX: &str = "scx_";

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::new(
        Direction::Vertical,
        [
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ],
    )
    .areas(frame.area());

    draw_header(frame, app, header);

    match app.view {
        View::Schedulers => {
            let [left, right] = Layout::new(
                Direction::Horizontal,
                [Constraint::Percentage(40), Constraint::Percentage(60)],
            )
            .areas(body);
            draw_scheduler_list(frame, app, left);
            draw_status_panel(frame, app, right);
        }
        View::Logs => draw_logs(frame, app, body),
    }

    draw_footer(frame, app, footer);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            " scxtui ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" backend: "),
        Span::styled(app.backend_label(), Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_scheduler_list(frame: &mut Frame, app: &App, area: Rect) {
    let current = app
        .status
        .as_ref()
        .and_then(|status| status.current.as_deref());

    let items: Vec<ListItem> = app
        .schedulers
        .iter()
        .map(|sched| {
            let is_running = Some(sched.as_str()) == current;
            let marker = if is_running { "● " } else { "  " };
            let style = if is_running {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Green)),
                Span::styled(strip_prefix(sched), style),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Schedulers "))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_status_panel(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    match &app.status {
        Some(status) => match &status.current {
            Some(sched) if !status.args.is_empty() => {
                lines.push(kv("State", "running", Color::Green));
                lines.push(kv("Scheduler", strip_prefix(sched), Color::White));
                lines.push(kv("Arguments", &status.args.join(" "), Color::White));
            }
            Some(sched) => {
                lines.push(kv("State", "running", Color::Green));
                lines.push(kv("Scheduler", strip_prefix(sched), Color::White));
                lines.push(kv("Mode", mode_name(status.mode), Color::White));
            }
            None => lines.push(kv("State", "no scheduler running", Color::Yellow)),
        },
        None => lines.push(kv("State", "unknown", Color::Red)),
    }

    if let Some(status) = &app.status {
        let default = match &status.default_sched {
            Some(sched) => format!(
                "{} ({})",
                strip_prefix(sched),
                mode_name(status.default_mode)
            ),
            None => "not configured".to_owned(),
        };
        lines.push(kv("Default", &default, Color::White));
    }

    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("Mode: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("◀ {} ▶", mode_name(app.selected_mode())),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  (Tab to cycle)"),
    ]));
    if !app.selected_mode_configured() {
        lines.push(Line::from(Span::styled(
            "  no configured arguments for this mode — scheduler defaults will be used",
            Style::default().fg(Color::Yellow),
        )));
    }

    let panel =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Status "));
    frame.render_widget(panel, area);
}

fn draw_logs(frame: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height.saturating_sub(2) as usize;
    app.log_page = height.max(1);

    let total = app.log_lines.len();
    let max_scroll = total.saturating_sub(height);
    // Clamp and write back, so `g` (jump to top, scroll = MAX) and paging
    // past the ends settle on real bounds.
    if app.log_scroll > max_scroll {
        app.log_scroll = max_scroll;
    }
    let end = total - app.log_scroll;
    let start = end.saturating_sub(height);

    let lines: Vec<Line> = app.log_lines[start..end]
        .iter()
        .map(|line| {
            if line.continuation {
                Line::from(Span::styled(
                    format!("           ↳ {}", line.text),
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{:8} ", line.time),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(line.text.clone(), priority_style(line.priority)),
                ])
            }
        })
        .collect();

    let boot = if app.log_previous_boot {
        "-1 (previous)"
    } else {
        "0 (current)"
    };
    let title = format!(
        " Logs — {} · boot {} · {} lines ",
        logs::UNITS[app.log_unit],
        boot,
        total
    );

    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(Style::default().add_modifier(Modifier::BOLD)),
    );
    frame.render_widget(panel, area);
}

fn priority_style(priority: u8) -> Style {
    match priority {
        0..=3 => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        4 => Style::default().fg(Color::Yellow),
        7 => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let [keys_area, message_area] = Layout::new(
        Direction::Vertical,
        [Constraint::Length(1), Constraint::Length(1)],
    )
    .areas(area);

    let key_help = match app.view {
        View::Schedulers => {
            let caps = app.capabilities();
            let mut help = String::from(" ↑/↓ select · Tab mode · Enter start");
            if caps.live_switch {
                help.push_str("/switch");
            }
            help.push_str(" · s stop · r restart");
            if caps.restore_default {
                help.push_str(" · d restore");
            }
            help.push_str(" · l logs · R refresh · q quit");
            help
        }
        View::Logs => String::from(
            " ↑/↓ scroll · PgUp/PgDn page · g/G top/bottom · b boot · u unit · R reload · Esc back · q quit",
        ),
    };

    let keys = Line::from(Span::styled(key_help, Style::default().fg(Color::DarkGray)));
    frame.render_widget(Paragraph::new(keys), keys_area);

    if let Some(message) = &app.message {
        let color = if message.is_error {
            Color::Red
        } else {
            Color::Green
        };
        let line = Line::from(Span::styled(
            format!(" {}", message.text),
            Style::default().fg(color),
        ));
        frame.render_widget(Paragraph::new(line), message_area);
    }
}

fn kv<'a>(key: &'a str, value: &str, value_color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{key}: "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_owned(), Style::default().fg(value_color)),
    ])
}

fn strip_prefix(sched: &str) -> &str {
    sched.strip_prefix(SCHED_PREFIX).unwrap_or(sched)
}

fn mode_name(mode: scx_loader::SchedMode) -> &'static str {
    <&str>::from(mode)
}
