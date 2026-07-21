// SPDX-License-Identifier: GPL-2.0

//! Journal access for the log view.
//!
//! Spawns `journalctl` as a one-shot subprocess with `--output=json` rather
//! than linking sd-journal bindings: no libsystemd build dependency, and the
//! JSON output carries `PRIORITY` reliably, which drives the coloring.
//! Multi-line messages (scheduler `Opts { ... }` dumps and the like) are
//! flattened into display lines here, with continuations marked so the UI
//! can indent or fold them.

use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::TimeZone;

/// Units the log view can inspect. `scx_loader.service` is the phase-1
/// default; `scx.service` is here ahead of the phase-2 backend so the log
/// view doesn't need touching then.
pub const UNITS: [&str; 2] = ["scx_loader.service", "scx.service"];

/// One display line of the log view.
pub struct LogLine {
    /// Local wall-clock time, `HH:MM:SS`. Empty for continuation lines.
    pub time: String,
    /// syslog priority (0-7); 6 (info) when the entry carries none.
    pub priority: u8,
    pub text: String,
    /// Second and further lines of a multi-line journal entry.
    pub continuation: bool,
}

/// Fetches the journal for `unit`, current boot or the previous one, and
/// flattens it into display lines (oldest first).
pub fn fetch(unit: &str, previous_boot: bool) -> Result<Vec<LogLine>> {
    let boot = if previous_boot { "-1" } else { "0" };
    let output = Command::new("journalctl")
        .args([
            "--unit",
            unit,
            "--boot",
            boot,
            "--output=json",
            "--no-pager",
            "--quiet",
        ])
        .output()
        .context("failed to run journalctl — is this a systemd system?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // journalctl exits non-zero e.g. when boot -1 is absent (volatile
        // journal) — surface its own wording, it is usually clear enough.
        bail!(
            "journalctl failed{}{}",
            if stderr.is_empty() { "" } else { ": " },
            stderr
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = Vec::new();
    for raw in stdout.lines() {
        if raw.is_empty() {
            continue;
        }
        // Tolerate individual malformed entries instead of failing the view.
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };

        let priority = entry
            .get("PRIORITY")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse().ok())
            .unwrap_or(6);
        let time = entry
            .get("__REALTIME_TIMESTAMP")
            .and_then(|t| t.as_str())
            .and_then(|t| t.parse::<i64>().ok())
            .map(format_local_time)
            .unwrap_or_default();
        // MESSAGE is a JSON string for UTF-8 payloads and a byte array
        // otherwise (journald convention); handle both.
        let message = match entry.get("MESSAGE") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(bytes)) => {
                let raw: Vec<u8> = bytes
                    .iter()
                    .filter_map(|b| b.as_u64().map(|b| b as u8))
                    .collect();
                String::from_utf8_lossy(&raw).into_owned()
            }
            _ => continue,
        };

        for (i, text) in message.lines().enumerate() {
            lines.push(LogLine {
                time: if i == 0 { time.clone() } else { String::new() },
                priority,
                text: text.to_owned(),
                continuation: i > 0,
            });
        }
    }
    Ok(lines)
}

fn format_local_time(usec: i64) -> String {
    chrono::Local
        .timestamp_opt(usec / 1_000_000, 0)
        .single()
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}
