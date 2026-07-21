<div align="center">

# scxtui

**A terminal user interface for managing Linux [`sched_ext`](https://github.com/sched-ext/scx) schedulers through [`scx_loader`](https://github.com/sched-ext/scx-loader).**

[![Crates.io](https://img.shields.io/crates/v/scxtui.svg)](https://crates.io/crates/scxtui)
[![License](https://img.shields.io/crates/l/scxtui.svg)](../../LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux-blue.svg)](#requirements)

</div>

<p align="center">
  <img src="assets/scxtui-schedulers.png" alt="scxtui scheduler view" width="900">
</p>

`scxtui` provides an interactive view of the schedulers exposed by the running
`scx_loader` daemon. It can start and switch schedulers, select operating modes,
inspect the current state, restore the configured default, browse journal logs,
and launch `scxtop` without leaving the interface.

The application is a lightweight, unprivileged D-Bus client. Scheduler lifecycle
management remains in `scx_loader`, while monitoring is delegated to `scxtop`.

## Features

- Lists schedulers advertised by the running `scx_loader` daemon.
- Starts a scheduler or switches the currently running scheduler in place.
- Supports all standard modes: `Auto`, `Gaming`, `PowerSave`, `LowLatency`, and
  `Server`.
- Shows the active scheduler, mode or custom arguments, and configured default.
- Warns when a selected mode has no configured arguments and scheduler defaults
  will be used.
- Stops, restarts, or restores the configured default scheduler.
- Refreshes status periodically, including changes made by `scxctl`, desktop
  applets, or another `scxtui` instance.
- Browses logs from `scx_loader.service` and `scx.service` for the current or
  previous boot.
- Highlights journal messages according to their syslog priority and preserves
  multi-line entries.
- Launches `scxtop` as an external monitor and restores the TUI after it exits.

## Requirements

- Linux with `sched_ext` support.
- A recent `scx_loader` installation available through the system D-Bus as
  `org.scx.Loader`.
- `systemd` and `journalctl` for the integrated log viewer.
- Permission to read the relevant system journal entries.
- Optional: `scxtop` in `PATH` for integrated monitoring. `scxtop` may require
  root privileges or suitable BPF capabilities; `scxtui` itself does not.

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

Distributions may package `scxtui` together with `scx_loader`, `scxctl`, and the
scheduler binaries. Use the package supplied by your distribution when
available so the client and daemon versions stay in sync.

## Usage

Start the interface with:

```bash
scxtui
```

`scxtui` connects to the system bus before switching the terminal into raw mode.
If the loader is unavailable, it exits with a normal error message instead of
leaving the terminal in a broken state.

### Scheduler view

| Key | Action |
|---|---|
| `↑` / `↓`, `k` / `j` | Select the previous or next scheduler |
| `Tab`, `m` | Select the next mode |
| `Shift+Tab`, `M` | Select the previous mode |
| `Enter` | Start the selected scheduler, or switch to it when one is already running |
| `s` | Stop the running scheduler |
| `r` | Restart the running scheduler |
| `d` | Restore the scheduler and mode configured as default |
| `l` | Open the journal log viewer |
| `t` | Launch `scxtop` |
| `R` | Refresh scheduler state and configured modes |
| `B` | Switch between the `scx_loader` and `scx.service` backends |
| `q`, `Esc` | Quit |

Scheduler-changing actions are debounced to prevent terminal key repeat from
triggering several starts, stops, or restarts in quick succession.

### Log view

<p align="center">
  <img src="assets/scxtui-logs.png" alt="scxtui journal log view" width="900">
</p>

| Key | Action |
|---|---|
| `↑` / `↓`, `k` / `j` | Scroll one line |
| `Page Up` / `Page Down` | Scroll one page |
| `g` / `G` | Jump to the oldest or newest entry |
| `b` | Toggle between the current and previous boot |
| `u` | Switch between `scx_loader.service` and `scx.service` |
| `R` | Reload the journal |
| `Esc`, `l` | Return to the scheduler view |
| `q` | Quit |

The log viewer calls `journalctl --output=json` and parses the result locally.
This avoids a build-time dependency on `libsystemd` while retaining journal
priority information for message highlighting.

## How it works

The current backend communicates with `org.scx.Loader` over the system D-Bus.
The daemon's advertised scheduler list is treated as authoritative, so locally
added schedulers and version differences are not rejected prematurely by a
client-side enum.

D-Bus property caching is disabled because scheduler state can change outside
this process. `scxtui` periodically queries the daemon so the displayed state
remains accurate when another client performs an operation.

The UI is built with [`ratatui`](https://ratatui.rs/) and uses a backend trait to
keep rendering and input handling separate from scheduler management. This also
leaves room for a future fallback backend based on `scx.service` on systems that
do not use `scx_loader`.

## Current limitations

- Scheduler management currently requires the `scx_loader` D-Bus backend.
- Custom scheduler arguments cannot yet be entered from the TUI.
- The log viewer loads the selected boot into memory rather than following new
  entries continuously.
- `scxtop` integration depends on a separately installed executable.

## Troubleshooting

### `org.scx.Loader did not respond`

Verify that `scx_loader` and its D-Bus service files are installed correctly:

```bash
systemctl status scx_loader.service
busctl --system introspect org.scx.Loader /org/scx/Loader
```

The daemon may be started on demand through D-Bus, so an inactive systemd unit
is not necessarily an error by itself.

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
[`sched-ext/scx-loader`](https://github.com/sched-ext/scx-loader) repository.
Please keep commits focused, run the standard formatting and lint checks, and
include a clear description of user-visible behavior changes.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

## License

`scxtui` is distributed under the terms of the
[GNU General Public License v2.0 only](../../LICENSE), matching the rest of the
`scx_loader` project.
