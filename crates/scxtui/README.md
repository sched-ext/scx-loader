# scxtui

A terminal user interface for managing [sched_ext](https://github.com/sched-ext/scx)
schedulers via [scx_loader](https://github.com/sched-ext/scx-loader).

> **Status: placeholder / work in progress.**
> This release only reserves the crate name. There is no functionality yet.

## Planned scope

1. Full `scx_loader` D-Bus support: start / switch / modes / get / list /
   restore / restart (feature parity with `scxctl`).
2. Optional fallback to `scx.service` for systems without `scx_loader`.
3. Log inspection via the systemd journal (`scx_loader.service` /
   `scx.service`, current and previous boot).
4. Integrated monitoring by launching [`scxtop`](https://crates.io/crates/scxtop).

## License

GPL-2.0-only, matching the rest of the sched_ext ecosystem.
