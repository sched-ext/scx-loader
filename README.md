# scx_loader & scxctl & scxtui

[![Crates.io](https://img.shields.io/crates/v/scx_loader.svg)](https://crates.io/crates/scx_loader)
[![Crates.io](https://img.shields.io/crates/v/scxctl.svg)](https://crates.io/crates/scxctl)
[![Crates.io](https://img.shields.io/crates/v/scxtui.svg)](https://crates.io/crates/scxtui)

`scx_loader` is a system daemon and DBus-based loader for [sched_ext](https://github.com/sched-ext/scx) schedulers.
`scxctl` is the command-line client for interacting with the loader, allowing users to switch schedulers, modes, and arguments dynamically.
`scxtui` is an interactive terminal user interface for managing schedulers, inspecting the current loader state, and viewing recent scheduler logs.

These tools were originally part of the main `sched-ext/scx` repository and are now developed independently.

---

## ✨ Features

- Systemd service: `scx_loader.service`
- DBus interface for scheduler management (`org.scx.Loader`)
- CLI client `scxctl` for scriptable and manual control
- Interactive terminal client `scxtui` for scheduler management and status inspection
- Start, switch, stop, restart, and restore schedulers from the terminal UI
- View the active scheduler, mode, arguments, and recent journal entries
- Multiple runtime modes (Auto, Gaming, LowLatency, PowerSave, Server)
- Per-scheduler arguments via TOML configuration
- Installable directly from crates.io or buildable from source

---

## 🚀 Installation

### 1. Install from crates.io

```bash
cargo install scx_loader
cargo install scxctl
cargo install scxtui
```

The binaries will be installed in `~/.cargo/bin`.
Ensure this directory is in your `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

### 2. Build from source

Clone and compile the repository:

```bash
git clone https://github.com/sched-ext/scx-loader.git
cd scx-loader

# Build optimized binaries
cargo build --release
```

Then install them system-wide:

```bash
sudo install -Dm755 target/release/scx_loader /usr/bin/
sudo install -Dm755 target/release/scxctl /usr/bin/
sudo install -Dm755 target/release/scxtui /usr/bin/
```
or

```bash
sudo find target/release \
    -maxdepth 1 -type f -executable ! -name '*.so' \
    -exec install -Dm755 -t /usr/bin/ {} +
```

Make sure you have also installed the necessary configuration files.

```bash
sudo install -Dm644 services/scx_loader.service \
    -t /usr/lib/systemd/system/
sudo install -Dm644 services/org.scx.Loader.service \
    -t /usr/share/dbus-1/system-services/
sudo install -Dm644 configs/org.scx.Loader.conf \
    -t /usr/share/dbus-1/system.d/
sudo install -Dm644 configs/org.scx.Loader.xml \
    -t /usr/share/dbus-1/interfaces/
sudo install -Dm644 configs/org.scx.Loader.policy \
    -t /usr/share/polkit-1/actions/
sudo install -Dm644 configs/scx_loader.toml \
    /usr/share/scx_loader/config.toml
```

### 3. Install with xtask

Alternatively, you can use `xtask` to install the required files:

```bash
cargo xtask install
```

### Environment Variables for xtask

The `xtask install` command respects several environment variables for customizing installation paths, which is useful for packaging by distributions:

- `VENDOR_PREFIX`: Overrides the default `/usr` prefix for installation paths. (e.g., `/usr/local`)
- `VENDOR_DATADIR`: Overrides the default `/usr/share` for data files.
- `VENDOR_SYSCONFDIR`: Overrides the default `/etc` for configuration files.
- `VENDOR_LIBDIR`: Overrides the default `$VENDOR_PREFIX/lib` for library files.

Additionally, the `--destdir` flag can be used with `xtask install` to create package:

```bash
cargo xtask install --destdir $pkgdir
```
---

## ⚙️ Configuration

`scx_loader` reads a TOML configuration file that defines the default scheduler and mode, along with per-scheduler arguments.

Example configuration:

```toml
# This field specifies the scheduler that will be started automatically when scx_loader starts (e.g., on boot).
default_sched = "scx_cosmos"

# This field specifies the mode which will be used when scx_loader starts (e.g., on boot).
default_mode = "Auto"

# This "structure" allows configuring flags for each scheduler mode of particular scx scheduler
#[scheds.'scheduler']
#auto_mode = []
#gaming_mode = []
#lowlatency_mode = []
#powersave_mode = []
#server_mode = []
```

### Configuration lookup order

`scx_loader` searches configuration files in the following order:

1. `/etc/scx_loader/config.toml`
2. `/etc/scx_loader.toml`
3. `$VENDORDIR/scx_loader/config.toml`
4. `$VENDORDIR/scx_loader.toml`

> `$VENDORDIR` defaults to `/usr/share`, but distributions may override it.

This environment variable applies only at build time, so developers or distributions must set it before building.

---

## 🧩 Systemd Integration

The package provides a systemd service unit for automatic startup:

```bash
sudo systemctl enable --now scx_loader.service
```

You can check its status using:

```bash
systemctl status scx_loader.service
```
---

## 💡 Using scxctl

See: [scxctl](crates/scxctl/README.md)

---

## 🖥️ Using scxtui

Launch the terminal interface with:

```bash
scxtui
```

`scxtui` provides an interactive view of the schedulers exposed by the active backend. It can be used to:

- inspect the currently running scheduler, mode, and arguments;
- select and start or switch schedulers and modes;
- stop, restart, or restore the configured default scheduler;
- refresh the scheduler list and runtime state;
- inspect recent `scx_loader` journal entries.

The interface discovers loader capabilities at runtime and degrades gracefully when optional D-Bus methods are unavailable.

See: [scxtui](crates/scxtui/README.md)

---

## 🧠 Troubleshooting

📖 Full usage guide available in the [CachyOS wiki](https://wiki.cachyos.org/configuration/sched-ext/).

---

## 🤝 Contributing

Pull requests and discussions are welcome!
Follow Rust coding conventions and include descriptive commit messages.
Bug reports and proposals can be submitted in the [scx-tools issue tracker](https://github.com/sched-ext/scx-loader/issues).

---

