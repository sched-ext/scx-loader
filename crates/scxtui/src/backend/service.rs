// SPDX-License-Identifier: GPL-2.0

//! `scx.service` backend: the pre-`scx_loader` way of running `sched_ext`
//! schedulers, driven by a config file (`SCX_SCHEDULER` / `SCX_FLAGS`) and
//! plain systemctl. It exists for systems without the loader daemon.
//!
//! The capability surface is honestly narrower than the D-Bus backend and
//! is declared as such: no live switch (a switch is a config edit plus a
//! service restart), no modes, no restore-default. The UI degrades based on
//! [`Capabilities`], which is exactly why that struct exists.
//!
//! Privileges: reading the config and querying systemd work as any user,
//! but editing the config and starting/stopping the unit generally require
//! root. Both paths fail with messages that say so instead of guessing.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use scx_loader::SchedMode;

use super::{Capabilities, SchedulerBackend, Status};

const UNIT: &str = "scx.service";
/// Debian/Arch convention first, Fedora/openSUSE second.
const CONFIG_CANDIDATES: [&str; 2] = ["/etc/default/scx", "/etc/sysconfig/scx"];

pub struct ServiceBackend {
    config_path: PathBuf,
}

impl ServiceBackend {
    /// Verifies the unit exists (so the fallback fails fast on systems
    /// without any scx service) and picks the config path — the first
    /// existing candidate, or the Debian/Arch default for creation.
    pub fn connect() -> Result<Self> {
        let out = Command::new("systemctl")
            .args(["cat", UNIT])
            .output()
            .context("failed to run systemctl — is this a systemd system?")?;
        if !out.status.success() {
            bail!("{UNIT} unit not found — is the scx service installed?");
        }
        let config_path = CONFIG_CANDIDATES
            .iter()
            .map(Path::new)
            .find(|p| p.exists())
            .map_or_else(|| PathBuf::from(CONFIG_CANDIDATES[0]), Path::to_path_buf);
        Ok(Self { config_path })
    }

    fn systemctl(verb: &str) -> Result<()> {
        let out = Command::new("systemctl")
            .args([verb, UNIT])
            .output()
            .context("failed to run systemctl")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!(
                "systemctl {verb} {UNIT} failed: {} (controlling the unit may require root)",
                stderr.trim()
            );
        }
        Ok(())
    }

    fn is_active() -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", UNIT])
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Returns (configured scheduler, configured flags). Missing or
    /// unreadable config degrades to "nothing configured" rather than
    /// erroring — the status view should still render.
    fn read_config(&self) -> (Option<String>, Vec<String>) {
        let Ok(content) = fs::read_to_string(&self.config_path) else {
            return (None, Vec::new());
        };
        let mut sched = None;
        let mut flags = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some(value) = line.strip_prefix("SCX_SCHEDULER=") {
                sched = Some(unquote(value).to_owned());
            } else if let Some(value) = line.strip_prefix("SCX_FLAGS=") {
                flags = unquote(value)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
            }
        }
        (sched.filter(|s| !s.is_empty()), flags)
    }

    /// Rewrites `SCX_SCHEDULER=` in place, preserving every other line
    /// (comments, `SCX_FLAGS`, unrelated variables), appending the key if it
    /// was absent.
    fn write_scheduler(&self, sched: &str) -> Result<()> {
        let content = fs::read_to_string(&self.config_path).unwrap_or_default();
        let text = render_config(&content, sched);
        // Write-then-rename so a crash or a full disk mid-write can never
        // leave the system config truncated; the temp file lives in the
        // same directory, which makes the rename atomic.
        let tmp = self.config_path.with_extension("scxtui-tmp");
        fs::write(&tmp, text).with_context(|| {
            format!(
                "cannot write {} — editing it requires root; run scxtui as root to control {UNIT}",
                tmp.display()
            )
        })?;
        // `fs::write` creates the temp file with the process umask; carry
        // the original config's permissions over so the rename cannot widen
        // access to a file the admin keeps tightened (e.g. 0600). A missing
        // config is fine (first-time creation keeps the defaults), but any
        // other metadata failure aborts: this block exists to protect
        // permissions, so it must fail closed rather than rename blindly.
        match fs::metadata(&self.config_path) {
            Ok(meta) => {
                fs::set_permissions(&tmp, meta.permissions()).with_context(|| {
                    format!(
                        "cannot preserve permissions from {} on {}",
                        self.config_path.display(),
                        tmp.display()
                    )
                })?;
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "cannot read permissions from {}",
                        self.config_path.display()
                    )
                });
            }
        }
        fs::rename(&tmp, &self.config_path)
            .with_context(|| format!("cannot move {} into place", tmp.display()))
    }
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(value)
}

/// Pure core of [`ServiceBackend::write_scheduler`]: rewrites
/// `SCX_SCHEDULER=` in place, preserving every other line, appending the
/// key if it was absent. Always ends with a trailing newline.
fn render_config(content: &str, sched: &str) -> String {
    let mut replaced = false;
    let mut lines: Vec<String> = content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("SCX_SCHEDULER=") {
                replaced = true;
                format!("SCX_SCHEDULER={sched}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    if !replaced {
        lines.push(format!("SCX_SCHEDULER={sched}"));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The service has no advertised scheduler list, so the closest honest
/// source of truth is what is actually installed: every `scx_*` executable
/// in PATH, minus the loader daemon itself. Sorted and deduplicated.
fn scan_path_for_schedulers() -> Vec<String> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    scan_dirs(dirs)
}

fn scan_dirs(dirs: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
    use std::collections::BTreeSet;

    let mut found = BTreeSet::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // These names end up verbatim in the service config file, so
            // beyond the prefix filter, restrict them to a conservative
            // character set — a stray `scx_$(...)` in PATH must never
            // travel into `SCX_SCHEDULER=`.
            let clean = name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if clean && name.starts_with("scx_") && name != "scx_loader" && entry.path().is_file() {
                found.insert(name);
            }
        }
    }
    found.into_iter().collect()
}

impl SchedulerBackend for ServiceBackend {
    fn label(&self) -> &'static str {
        "scx.service (systemd)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            live_switch: false,
            modes: false,
            restore_default: false,
        }
    }

    fn status(&self) -> Result<Status> {
        let (sched, flags) = self.read_config();
        let active = Self::is_active();
        Ok(Status {
            current: if active {
                // Active unit with no readable SCX_SCHEDULER: still show
                // *something* runs; the kernel panel will name the ops.
                Some(sched.clone().unwrap_or_else(|| "scx_(unset)".to_owned()))
            } else {
                None
            },
            mode: SchedMode::Auto,
            args: flags,
            default_sched: sched,
            default_mode: SchedMode::Auto,
        })
    }

    fn supported_schedulers(&self) -> Result<Vec<String>> {
        let scheds = scan_path_for_schedulers();
        if scheds.is_empty() {
            bail!("no scx_* scheduler binaries found in PATH");
        }
        Ok(scheds)
    }

    fn configured_modes(&self, _sched: &str) -> Result<Vec<SchedMode>> {
        Ok(Vec::new())
    }

    fn start(&self, sched: &str, _mode: SchedMode) -> Result<()> {
        self.write_scheduler(sched)?;
        Self::systemctl("start")
    }

    fn switch(&self, sched: &str, _mode: SchedMode) -> Result<()> {
        // "Switch" here is config edit + restart; declared via
        // `live_switch: false` so the UI words it accordingly.
        self.write_scheduler(sched)?;
        Self::systemctl("restart")
    }

    fn stop(&self) -> Result<()> {
        Self::systemctl("stop")
    }

    fn restart(&self) -> Result<()> {
        Self::systemctl("restart")
    }

    fn restore_default(&self) -> Result<()> {
        bail!("the {UNIT} backend has no restore-default operation")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_handles_quote_styles() {
        assert_eq!(unquote("\"scx_lavd\""), "scx_lavd");
        assert_eq!(unquote("'scx_lavd'"), "scx_lavd");
        assert_eq!(unquote("scx_lavd"), "scx_lavd");
        assert_eq!(unquote("  scx_lavd  "), "scx_lavd");
        // A lone opening quote is not a quoted value.
        assert_eq!(unquote("\"scx_lavd"), "\"scx_lavd");
    }

    #[test]
    fn render_config_replaces_in_place() {
        let before = "# managed by hand\nSCX_SCHEDULER=scx_rusty\nSCX_FLAGS=\"-v\"\n";
        let after = render_config(before, "scx_lavd");
        assert_eq!(
            after,
            "# managed by hand\nSCX_SCHEDULER=scx_lavd\nSCX_FLAGS=\"-v\"\n"
        );
    }

    #[test]
    fn render_config_appends_when_absent() {
        let after = render_config("SCX_FLAGS=\"-v\"\n", "scx_lavd");
        assert_eq!(after, "SCX_FLAGS=\"-v\"\nSCX_SCHEDULER=scx_lavd\n");
    }

    #[test]
    fn render_config_from_empty_file() {
        assert_eq!(render_config("", "scx_lavd"), "SCX_SCHEDULER=scx_lavd\n");
    }

    #[test]
    fn scan_dirs_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("scxtui-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for name in ["scx_zeta", "scx_alpha", "scx_loader", "not_a_sched"] {
            fs::File::create(dir.join(name)).unwrap();
        }
        let found = scan_dirs([dir.clone()]);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(found, ["scx_alpha", "scx_zeta"]);
    }
}
