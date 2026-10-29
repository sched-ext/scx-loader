// SPDX-License-Identifier: GPL-2.0
//
// Copyright (c) 2024 Vladislav Nepogodin <vnepogodin@cachyos.org>

// This software may be used and distributed according to the terms of the
// GNU General Public License version 2.

pub mod config;
pub mod dbus;

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Serialize;
use zvariant::OwnedValue;
use zvariant::Type;
use zvariant::Value;

/// A scheduler `scx_loader` can run.
///
/// It goes over D-Bus and into the config as its name, e.g. `"scx_lavd"`.
/// Any other name of the form `scx_[a-z0-9_]+` parses as `Custom`. Such a
/// scheduler can only be started if the config defines it with a `path`;
/// see `config::is_known_sched`.
#[derive(Clone, PartialEq, Type)]
#[zvariant(signature = "s")]
pub enum SupportedSched {
    Bpfland,
    Rusty,
    Lavd,
    Flash,
    P2DQ,
    Tickless,
    Rustland,
    Cosmos,
    Beerland,
    Cake,
    Pandemonium,
    Flow,
    Forge,
    MLFQ,
    EEVDF,
    /// A scheduler that isn't built in, defined by the administrator in the
    /// config.
    Custom(String),
}

impl SupportedSched {
    /// The scheduler's name, which is also the binary name of a built-in
    /// scheduler.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            SupportedSched::Beerland => "scx_beerland",
            SupportedSched::Bpfland => "scx_bpfland",
            SupportedSched::Cake => "scx_cake",
            SupportedSched::Cosmos => "scx_cosmos",
            SupportedSched::EEVDF => "scx_eevdf",
            SupportedSched::Flash => "scx_flash",
            SupportedSched::Flow => "scx_flow",
            SupportedSched::Forge => "scx_forge",
            SupportedSched::Lavd => "scx_lavd",
            SupportedSched::MLFQ => "scx_mlfq",
            SupportedSched::Pandemonium => "scx_pandemonium",
            SupportedSched::P2DQ => "scx_p2dq",
            SupportedSched::Tickless => "scx_tickless",
            SupportedSched::Rustland => "scx_rustland",
            SupportedSched::Rusty => "scx_rusty",
            SupportedSched::Custom(name) => name,
        }
    }
}

/// Prints the variant name of a built-in scheduler, as `derive(Debug)` would,
/// and the name of a custom one, such as `scx_foo`. Messages like "running
/// Lavd" stay as they were.
impl fmt::Debug for SupportedSched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SupportedSched::Bpfland => "Bpfland",
            SupportedSched::Rusty => "Rusty",
            SupportedSched::Lavd => "Lavd",
            SupportedSched::Flash => "Flash",
            SupportedSched::P2DQ => "P2DQ",
            SupportedSched::Tickless => "Tickless",
            SupportedSched::Rustland => "Rustland",
            SupportedSched::Cosmos => "Cosmos",
            SupportedSched::Beerland => "Beerland",
            SupportedSched::Cake => "Cake",
            SupportedSched::Pandemonium => "Pandemonium",
            SupportedSched::Flow => "Flow",
            SupportedSched::Forge => "Forge",
            SupportedSched::MLFQ => "MLFQ",
            SupportedSched::EEVDF => "EEVDF",
            SupportedSched::Custom(name) => name,
        })
    }
}

/// Returns `true` if `name` is usable as a custom scheduler name:
/// `scx_` followed by lowercase ASCII letters, digits and `_`.
fn is_valid_custom_name(name: &str) -> bool {
    name.strip_prefix("scx_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

impl Serialize for SupportedSched {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SupportedSched {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for SupportedSched {
    type Err = anyhow::Error;

    fn from_str(scx_name: &str) -> anyhow::Result<SupportedSched> {
        match scx_name {
            "scx_beerland" => Ok(SupportedSched::Beerland),
            "scx_bpfland" => Ok(SupportedSched::Bpfland),
            "scx_cake" => Ok(SupportedSched::Cake),
            "scx_cosmos" => Ok(SupportedSched::Cosmos),
            "scx_eevdf" => Ok(SupportedSched::EEVDF),
            "scx_flash" => Ok(SupportedSched::Flash),
            "scx_flow" => Ok(SupportedSched::Flow),
            "scx_forge" => Ok(SupportedSched::Forge),
            "scx_lavd" => Ok(SupportedSched::Lavd),
            "scx_mlfq" => Ok(SupportedSched::MLFQ),
            "scx_pandemonium" => Ok(SupportedSched::Pandemonium),
            "scx_p2dq" => Ok(SupportedSched::P2DQ),
            "scx_tickless" => Ok(SupportedSched::Tickless),
            "scx_rustland" => Ok(SupportedSched::Rustland),
            "scx_rusty" => Ok(SupportedSched::Rusty),
            name if is_valid_custom_name(name) => Ok(SupportedSched::Custom(name.to_owned())),
            _ => Err(anyhow::anyhow!("{scx_name} is not supported")),
        }
    }
}

impl TryFrom<&str> for SupportedSched {
    type Error = <SupportedSched as FromStr>::Err;
    fn try_from(s: &str) -> Result<SupportedSched, Self::Error> {
        <SupportedSched as FromStr>::from_str(s)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Type, Value, OwnedValue, PartialEq)]
pub enum SchedMode {
    /// Default values for the scheduler
    Auto = 0,
    /// Applies flags for better gaming experience
    Gaming = 1,
    /// Applies flags for lower power usage
    PowerSave = 2,
    /// Starts scheduler in low latency mode
    LowLatency = 3,
    /// Starts scheduler in server-oriented mode
    Server = 4,
}

impl FromStr for SchedMode {
    type Err = anyhow::Error;

    fn from_str(mode_name: &str) -> anyhow::Result<SchedMode> {
        match mode_name {
            "auto" => Ok(SchedMode::Auto),
            "gaming" => Ok(SchedMode::Gaming),
            "powersave" => Ok(SchedMode::PowerSave),
            "lowlatency" => Ok(SchedMode::LowLatency),
            "server" => Ok(SchedMode::Server),
            _ => Err(anyhow::anyhow!("{mode_name} is not supported")),
        }
    }
}

impl TryFrom<&str> for SchedMode {
    type Error = <SchedMode as FromStr>::Err;
    fn try_from(s: &str) -> Result<SchedMode, Self::Error> {
        <SchedMode as FromStr>::from_str(s)
    }
}

impl From<SchedMode> for &str {
    fn from(mode_name: SchedMode) -> Self {
        match mode_name {
            SchedMode::Auto => "auto",
            SchedMode::Gaming => "gaming",
            SchedMode::PowerSave => "powersave",
            SchedMode::LowLatency => "lowlatency",
            SchedMode::Server => "server",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zvariant::serialized::Context;
    use zvariant::{LE, to_bytes};

    #[test]
    fn parses_builtin_and_custom_names() {
        assert_eq!(
            "scx_lavd".parse::<SupportedSched>().unwrap(),
            SupportedSched::Lavd
        );
        assert_eq!(
            "scx_my_sched2".parse::<SupportedSched>().unwrap(),
            SupportedSched::Custom("scx_my_sched2".to_owned())
        );
    }

    #[test]
    fn rejects_invalid_custom_names() {
        for name in [
            "",
            "lavd",
            "scx_",
            "scx_Foo",
            "scx_a-b",
            "scx_a.b",
            "scx_../x",
            "/usr/bin/scx_x",
        ] {
            assert!(
                name.parse::<SupportedSched>().is_err(),
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn dbus_wire_format_is_the_name() {
        assert_eq!(SupportedSched::SIGNATURE, "s");

        let ctxt = Context::new_dbus(LE, 0);
        for sched in [
            SupportedSched::Lavd,
            SupportedSched::Custom("scx_mine".to_owned()),
        ] {
            let encoded = to_bytes(ctxt, &sched).unwrap();
            assert_eq!(encoded.deserialize::<String>().unwrap().0, sched.as_str());
            assert_eq!(encoded.deserialize::<SupportedSched>().unwrap().0, sched);
        }
    }

    #[test]
    fn debug_keeps_builtin_variant_names() {
        assert_eq!(format!("{:?}", SupportedSched::Lavd), "Lavd");
        assert_eq!(
            format!("{:?}", SupportedSched::Custom("scx_mine".to_owned())),
            "scx_mine"
        );
    }
}
