<p align="center">
  <img
    src="assets/scxtui-logo.png"
    alt="scxtui logo"
    width="320"
  >
</p>

# scxtui

**A terminal user interface for managing Linux [`sched_ext`](https://github.com/sched-ext/scx) schedulers through [`scx_loader`](https://github.com/sched-ext/scx-loader) or `scx.service`.**

[![Crates.io](https://img.shields.io/crates/v/scxtui.svg)](https://crates.io/crates/scxtui)
[![License](https://img.shields.io/crates/l/scxtui.svg)](../../LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux-blue.svg)](#requirements)

</div>

`scxtui` provides an interactive view of the available `sched_ext` schedulers.
It can start and switch schedulers, select operating modes, inspect the
current state, restore the configured default, browse journal logs, and
launch `scxtop` without leaving the interface. Alongside the state reported
by the management backend, it reads the kernel's own view of `sched_ext`
from sysfs and warns whenever the two disagree — a scheduler started behind
the backend's back, or one that crashed without the backend noticing.

Two management backends are supported: the `scx_loader` D-Bus daemon
(preferred) and the plain `scx.service` systemd unit for systems without the
loader. With the loader backend the application is a lightweight,
unprivileged D-Bus client; scheduler lifecycle management remains in
`scx_loader`, while monitoring is delegated to `scxtop`.

## Features

- Lists schedulers advertised by the running `scx_loader` daemon, or — with
  the `scx.service` backend — the `scx_*` binaries installed in `PATH`.
- Starts a scheduler or switches the currently running scheduler in place,
  including switching the running scheduler to a different mode.
- Supports all standard modes: `auto`, `gaming`, `powersave`, `lowlatency`,
  and `server`.
- Shows the active scheduler, mode or custom arguments, and configured
  default.
- Displays the kernel's own `sched_ext` state from sysfs and warns when it
  disagrees with the backend: schedulers attached outside the backend's
  control, crashed or watchdog-ejected schedulers, and ops-name mismatches.
- Previews the arguments a selected mode is configured with, rendered in
  the same shell-words form as `scxctl`, cached per scheduler and refreshed
  when the daemon restarts.
- Warns when a selected mode has no configured arguments and scheduler
  defaults will be used.
- Stops, restarts, or restores the configured default scheduler. A restart
  with a different mode selected for the running scheduler applies that
  mode.
- Auto-detects the management backend at startup (preferring `scx_loader`),
  with explicit selection via `--backend` and runtime switching on a key.
- Refreshes status periodically, including changes made by `scxctl`, desktop
  applets, or another `scxtui` instance.
- Browses logs from `scx_loader.service` and `scx.service` for the current
  or previous boot.
- Highlights journal messages according to their syslog priority and
  preserves multi-line entries.
- Launches `scxtop` as an external monitor and restores the TUI after it
  exits.

## Requirements

- Linux with `sched_ext` support.
- One of the management backends:
  - a recent `scx_loader` installation available through the system D-Bus as
    `org.scx.Loader` (preferred), or
  - the `scx.service` systemd unit with its `/etc/default/scx` (or
    `/etc/sysconfig/scx`) configuration file. Controlling this backend
    (editing the config, starting and stopping the unit) generally requires
    root; reading its state does not.
- `systemd` and `journalctl` for the integrated log viewer.
- Permission to read the relevant system journal entries.
- Optional: `scxtop` in `PATH` for integrated monitoring. On Arch-based
  distributions it ships with the scx scheduler packages. `scxtop` may
  require root privileges or suitable BPF capabilities; `scxtui` itself does
  not when using the loader backend.

Mode-configuration warnings require a `scx_loader` daemon exposing the
`SchedulerModes` D-Bus methods; with an older daemon they are silently
skipped and everything else keeps working.

## Installation

### Build from source

```bash
git clone https://github.com/sched-ext/scx-loader.git
cd scx-loader
cargo build --release -p scxtui
```

Run the resulting binary:

```bash
./target/release/scxtui
```

To install only `scxtui` into Cargo's binary directory:

```bash
cargo install --path crates/scxtui --locked
```

Make sure `~/.cargo/bin` is present in your `PATH`.

### Distribution packages

Distributions may package `scxtui` together with `scx_loader`, `scxctl`, and
the scheduler binaries. Use the package supplied by your distribution when
available so the client and daemon versions stay in sync.

## Usage

Start the interface with:

```bash
scxtui
```

The management backend is auto-detected: `scx_loader` is preferred, with a
fallback to `scx.service` when the loader is unavailable. A specific backend
can be forced with `--backend loader` or `--backend service`, and the active
backend can be switched at runtime with `B`.

`scxtui` connects to the backend before switching the terminal into raw
mode. If no backend is usable, it exits with a normal error message instead
of leaving the terminal in a broken state.

### Scheduler view

| Key | Action |
|---|---|
| `↑` / `↓`, `k` / `j` | Select the previous or next scheduler |
| `Tab`, `m` | Select the next mode |
| `Shift+Tab`, `M` | Select the previous mode |
| `Enter` | Start the selected scheduler, or switch to it when one is already running |
| `s` | Stop the running scheduler |
| `r` | Restart the running scheduler; applies the selected mode when it differs |
| `d` | Restore the scheduler and mode configured as default |
| `l` | Open the journal log viewer |
| `t` | Launch `scxtop` |
| `B` | Switch between the `scx_loader` and `scx.service` backends |
| `R` | Refresh scheduler state and configured modes |
| `q`, `Esc` | Quit |

Scheduler-changing actions are debounced to prevent terminal key repeat from
triggering several starts, stops, or restarts in quick succession. Keys for
operations the active backend does not support are disabled and hidden from
the key bar.

### Log view

| Key | Action |
|---|---|
| `↑` / `↓`, `k` / `j` | Scroll one line |
| `Page Up` / `Page Down` | Scroll one page |
| `g` / `G` | Jump to the oldest or newest entry |
| `b` | Toggle between the current and previous boot |
| `u` | Switch between `scx_loader.service` and `scx.service` |
| `R` | Reload the journal |
| `q`, `Esc`, `l` | Return to the scheduler view |

The log viewer calls `journalctl --output=json` and parses the result
locally. This avoids a build-time dependency on `libsystemd` while retaining
journal priority information for message highlighting.

## How it works

The `scx_loader` backend communicates with `org.scx.Loader` over the system
D-Bus. The daemon's advertised scheduler list is treated as authoritative,
so locally added schedulers and version differences are not rejected
prematurely by a client-side enum. D-Bus property caching is disabled
because scheduler state can change outside this process; `scxtui`
periodically queries the daemon so the displayed state remains accurate when
another client performs an operation.

The `scx.service` backend edits `SCX_SCHEDULER` in the service's
configuration file (atomically, via a temporary file and rename) and drives
the unit with `systemctl`. Its narrower capabilities — no live switching, no
modes, no default restore — are declared through the backend trait, and the
interface adapts accordingly.

Independently of either backend, `scxtui` reads
`/sys/kernel/sched_ext/state` and `/sys/kernel/sched_ext/root/ops` to show
what the kernel itself reports, and flags disagreements between the kernel
and the backend.

The UI is built with [`ratatui`](https://ratatui.rs/) and uses the backend
trait to keep rendering and input handling separate from scheduler
management.

## Current limitations

- Custom scheduler arguments cannot yet be entered from the TUI.
- The log viewer loads the selected boot into memory rather than following
  new entries continuously.
- `scxtop` integration depends on a separately installed executable.
- Controlling the `scx.service` backend requires running `scxtui` as root.

## Troubleshooting

### `org.scx.Loader did not respond`

Verify that `scx_loader` and its D-Bus service files are installed correctly:

```bash
systemctl status scx_loader.service
busctl --system introspect org.scx.Loader /org/scx/Loader
```

The daemon may be started on demand through D-Bus, so an inactive systemd
unit is not necessarily an error by itself.

### Logs are empty or inaccessible

Check that the selected unit has entries for the selected boot and that your
user can read the system journal:

```bash
journalctl --unit scx_loader.service --boot 0
```

### `scxtop` fails to start

Confirm that it is installed and visible in `PATH`:

```bash
command -v scxtop
```

Depending on the system configuration, run `scxtop` with root privileges or
grant it the required BPF capabilities.

## Contributing

Bug reports, design discussions, and pull requests are welcome in the
[`sched-ext/scx-loader`](https://github.com/sched-ext/scx-loader)
repository. Please keep commits focused, run the standard formatting and
lint checks, and include a clear description of user-visible behavior
changes.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

## License

`scxtui` is distributed under the terms of the
[GNU General Public License v2.0 only](../../LICENSE), matching the rest of
the `scx_loader` project.
