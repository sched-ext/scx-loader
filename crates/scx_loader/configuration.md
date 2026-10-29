# scx_loader Configuration File

The `scx_loader` can be configured using a TOML file. This file allows you to customize the default scheduler mode, specify custom flags for each supported scheduler and mode, and set a default scheduler to start on boot.

## Configuration File Location

`scx_loader` looks for the configuration file in the following paths (in order):

1. `/etc/scx_loader/config.toml`
2. `/etc/scx_loader.toml`
3. `$VENDORDIR/scx_loader/config.toml` (`$VENDORDIR` is `/usr/share` by default, though your distribution may customize this)
4. `$VENDORDIR/scx_loader.toml` (`$VENDORDIR` is `/usr/share` by default, though your distribution may customize this)

If no configuration file is found at any of these paths, `scx_loader` will use the built-in default configuration.

## Configuration Structure

The configuration file has the following structure:

```toml
default_sched = "scx_cosmos"
default_mode = "Auto"

[scheds.scx_bpfland]
auto_mode = ["-m", "auto"]
gaming_mode = ["-m", "all"]
lowlatency_mode = ["-m", "performance", "-w"]
powersave_mode = ["-s", "20000", "-m", "powersave", "-I", "100", "-t", "100"]
server_mode = ["-s", "20000", "-S"]

[scheds.scx_rusty]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_lavd]
auto_mode = ["--autopilot", "--pinned-slice-us", "500"]
gaming_mode = ["--performance", "--pinned-slice-us", "500"]
lowlatency_mode = ["--performance", "--pinned-slice-us", "500"]
powersave_mode = ["--powersave", "--pinned-slice-us", "500"]
server_mode = ["--performance", "--slice-min-us", "3000", "--slice-max-us", "10000", "--pinned-slice-us", "3000"]

[scheds.scx_flash]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_p2dq]
auto_mode = ["--sched-mode", "default"]
gaming_mode = ["--task-slice", "true", "-f", "--sched-mode", "performance"]
lowlatency_mode = ["-y", "-f", "--task-slice", "true"]
powersave_mode = ["--sched-mode", "efficiency"]
server_mode = ["--keep-running"]

[scheds.scx_tickless]
auto_mode = []
gaming_mode = ["-f", "5000", "-s", "5000"]
lowlatency_mode = ["-f", "5000", "-s", "1000"]
powersave_mode = ["-f", "50"]
server_mode = ["-f", "100"]

[scheds.scx_rustland]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_cosmos]
auto_mode = []
gaming_mode = ["-s", "700"]
lowlatency_mode = ["-s", "700", "-m", "performance", "-w"]
powersave_mode = ["-m", "powersave"]
server_mode = []

[scheds.scx_beerland]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_cake]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_pandemonium]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_flow]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_forge]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_mlfq]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []

[scheds.scx_eevdf]
auto_mode = []
gaming_mode = []
lowlatency_mode = []
powersave_mode = []
server_mode = []
```

**`default_sched`:**

* This field specifies the scheduler that will be started automatically when `scx_loader` starts (e.g., on boot).
* It should be set to the name of a supported scheduler (e.g., `"scx_bpfland"`, `"scx_rusty"`, `"scx_lavd"`, `"scx_flash"`, `"scx_p2dq"`, `"scx_rustland"`).
* If this field is not present or is set to an empty string, no scheduler will be started automatically.

**`default_mode`:**

* This field specifies the default scheduler mode that will be used when starting a scheduler without explicitly specifying a mode.
* Possible values are: `"Auto"`, `"Gaming"`, `"LowLatency"`, `"PowerSave"`, `"Server"`.
* If this field is not present, it defaults to `"Auto"`.

**`allow_custom_scheds`:**

* Set to `true` to allow schedulers that aren't built into `scx_loader`. See [Custom Schedulers](#custom-schedulers).
* Defaults to `false`.

**`[scheds.scx_name]`:**

* This section defines the custom flags for a specific scheduler. Replace `scx_name` with the actual name of the scheduler (e.g., `scx_bpfland`, `scx_rusty`, `scx_lavd`, `scx_flash`, `scx_p2dq`, `scx_rustland`).

**`auto_mode`, `gaming_mode`, `lowlatency_mode`, `powersave_mode`, `server_mode`:**

* These fields specify the flags to be used for each scheduler mode.
* Each field is an array of strings, where each string represents a flag.
* If a field is not present or is an empty array, the default flags for that mode will be used.

## Example Configuration

The example configuration above shows how to set custom flags for different schedulers and modes, and how to configure `scx_cosmos` to start automatically on boot.

* For `scx_bpfland`:
    * Low Latency mode: `-m performance -w`
    * Power Save mode: `-s 20000 -m powersave -I 100 -t 100`
    * Server mode: `-s 20000 -S`
* For `scx_rusty`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_lavd`:
    * Auto mode: `--autopilot --pinned-slice-us 500`
    * Gaming mode: `--performance --pinned-slice-us 500`
    * Low Latency mode: `--performance --pinned-slice-us 500`
    * Power Save mode: `--powersave --pinned-slice-us 500`
    * Server mode: `--performance --slice-min-us 3000 --slice-max-us 10000 --pinned-slice-us 3000`
* For `scx_flash`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_tickless`:
    * Gaming mode: `-f 5000 -s 5000`
    * Low Latency mode: `-f 5000 -s 1000`
    * Power Save mode: `-f 50`
    * Server mode: `-f 100`
* For `scx_p2dq`:
    * Gaming mode: `--task-slice true -f --sched-mode performance`
    * Low Latency mode: `-y -f --task-slice true`
    * Power Save mode: `--sched-mode efficiency`
    * Server mode: `--keep-running`
* For `scx_rustland`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_cosmos`:
    * Gaming mode: `-s 700`
    * Low Latency mode: `-s 700 -m performance -w`
    * Power Save mode: `-m powersave`
* For `scx_beerland`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_cake`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_pandemonium`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_flow`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_forge`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_mlfq`:
    * No custom flags are defined, so the default flags for each mode will be used.
* For `scx_eevdf`:
    * No custom flags are defined, so the default flags for each mode will be used.

### Fallback Behavior

If a specific flag is not defined in the configuration file, `scx_loader` will fall back to the default flags defined in the code.

### Missing Required Fields

If the `default_mode` field is missing, it will default to `"Auto"`. If a `[scheds.scx_name]` section is missing, or if specific mode flags are missing within that section, the default flags for the corresponding scheduler and mode will be used. If `default_sched` is missing or empty, no scheduler will be started automatically.

## Custom Schedulers

`scx_loader` only runs the schedulers it knows about by default. To run another one, for example a scheduler you are developing, set `allow_custom_scheds = true` and give it a `[scheds.scx_name]` section with a `path`:

```toml
allow_custom_scheds = true

[scheds.scx_mysched]
path = "/usr/local/bin/scx_mysched"
auto_mode = []
gaming_mode = ["--fast"]
```

* The name must start with `scx_`, followed by lowercase letters, digits or `_`, and must not be the name of a built-in scheduler.
* `path` must be absolute. It's only read from this file, so D-Bus clients can pick a custom scheduler by name but can't make `scx_loader` run a different binary.
* The mode fields work as for built-in schedulers, except that there are no default flags to fall back to. A mode without flags runs the scheduler with its own defaults.
* Custom schedulers show up in `SupportedSchedulers` and can be used everywhere a built-in one can, including `default_sched`.
* `scx_loader` runs the binary as root, so it should be owned by root and not writable by other users.

A `path` without `allow_custom_scheds = true` is an error, so the setting can't be ignored by mistake.
