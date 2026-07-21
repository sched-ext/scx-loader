// SPDX-License-Identifier: GPL-2.0

//! The kernel's own view of sched_ext, read straight from sysfs.
//!
//! scx_loader only knows about schedulers *it* started; the kernel knows
//! what is actually attached. Reading `/sys/kernel/sched_ext` lets the UI
//! show ground truth and flag disagreements — a scheduler launched by hand
//! next to the loader, a scheduler that got kicked out by the watchdog
//! while the loader still believes it is running, and so on. Everything
//! here is plain world-readable sysfs, so no privileges are needed.

use std::fs;
use std::path::Path;

const SYSFS_DIR: &str = "/sys/kernel/sched_ext";

pub struct KernelState {
    /// Contents of `state`: "enabled", "disabled", or a transitional value
    /// ("enabling"/"disabling") if we catch a switch mid-flight.
    pub state: String,
    /// BPF ops name from `root/ops` when a scheduler is attached — the name
    /// the scheduler registered with, typically without the `scx_` prefix
    /// (e.g. "lavd" for scx_lavd).
    pub ops: Option<String>,
}

impl KernelState {
    pub fn enabled(&self) -> bool {
        self.state == "enabled"
    }

    /// Whether `loader_name` (full name, e.g. `scx_lavd`) plausibly matches
    /// the ops the kernel reports. Ops names are chosen by each scheduler
    /// and some embed extra detail — bpfland registers e.g.
    /// `bpfland_1.1.2_x86_64_unknown_linux_gnu` — so beyond an exact match
    /// (with or without the `scx_` prefix) an ops name that *starts with*
    /// the scheduler name followed by `_` also counts. Used for a soft
    /// warning only, so a rare false negative is acceptable.
    pub fn matches(&self, loader_name: &str) -> bool {
        let stripped = loader_name.strip_prefix("scx_").unwrap_or(loader_name);
        self.ops.as_deref().is_some_and(|ops| {
            ops == loader_name
                || ops == stripped
                || ops
                    .strip_prefix(stripped)
                    .is_some_and(|rest| rest.starts_with('_'))
        })
    }
}

/// Reads the kernel's sched_ext state. `None` means the sysfs directory is
/// absent, i.e. the running kernel was built without sched_ext.
pub fn read() -> Option<KernelState> {
    let dir = Path::new(SYSFS_DIR);
    if !dir.is_dir() {
        return None;
    }
    let state = fs::read_to_string(dir.join("state"))
        .ok()?
        .trim()
        .to_owned();
    let ops = fs::read_to_string(dir.join("root/ops"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    Some(KernelState { state, ops })
}
