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
use zbus::Type;

/// A built-in scheduler `scx_loader` can run.
///
/// It goes over D-Bus and into the config as its name, e.g. `"scx_lavd"`.
/// See [`SchedName`] for schedulers defined in the config.
#[derive(Clone, PartialEq, Type)]
#[zbus(signature = "s")]
#[non_exhaustive]
pub enum SupportedSched {
    Bpfland,
    Rusty,
    Lavd,
    Flash,
    P2DQ,
    Tickless,
    Cosmos,
    Beerland,
    Cake,
    Pandemonium,
    Flow,
    Forge,
    MLFQ,
    EEVDF,
    Rburst,
    Reevdf,
    Rlocal,
    Rvtime,
}

impl SupportedSched {
    /// The scheduler's name, which is also its binary name.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
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
            SupportedSched::Rburst => "scx_rburst",
            SupportedSched::Reevdf => "scx_reevdf",
            SupportedSched::Rlocal => "scx_rlocal",
            SupportedSched::Rvtime => "scx_rvtime",
            SupportedSched::Rusty => "scx_rusty",
        }
    }
}

/// Prints the variant name, as `derive(Debug)` would, so messages like
/// "running Lavd" stay as they were.
impl fmt::Debug for SupportedSched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SupportedSched::Bpfland => "Bpfland",
            SupportedSched::Rusty => "Rusty",
            SupportedSched::Lavd => "Lavd",
            SupportedSched::Flash => "Flash",
            SupportedSched::P2DQ => "P2DQ",
            SupportedSched::Tickless => "Tickless",
            SupportedSched::Cosmos => "Cosmos",
            SupportedSched::Beerland => "Beerland",
            SupportedSched::Cake => "Cake",
            SupportedSched::Pandemonium => "Pandemonium",
            SupportedSched::Flow => "Flow",
            SupportedSched::Forge => "Forge",
            SupportedSched::MLFQ => "MLFQ",
            SupportedSched::EEVDF => "EEVDF",
            SupportedSched::Rburst => "Rburst",
            SupportedSched::Reevdf => "Reevdf",
            SupportedSched::Rlocal => "Rlocal",
            SupportedSched::Rvtime => "Rvtime",
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
            "scx_rburst" => Ok(SupportedSched::Rburst),
            "scx_reevdf" => Ok(SupportedSched::Reevdf),
            "scx_rlocal" => Ok(SupportedSched::Rlocal),
            "scx_rvtime" => Ok(SupportedSched::Rvtime),
            "scx_rusty" => Ok(SupportedSched::Rusty),
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

impl From<SupportedSched> for &str {
    fn from(scx_name: SupportedSched) -> Self {
        scx_name.as_str()
    }
}

/// The name of a scheduler that isn't built in, defined in the config:
/// `scx_` followed by lowercase ASCII letters, digits and `_`,
/// and not the name of a built-in scheduler.
#[derive(Clone, PartialEq, Eq, Hash, Type)]
#[zbus(signature = "s")]
pub struct CustomSchedName(String);

impl CustomSchedName {
    /// The scheduler's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CustomSchedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Prints the bare name.
impl fmt::Debug for CustomSchedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for CustomSchedName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CustomSchedName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for CustomSchedName {
    type Err = anyhow::Error;

    fn from_str(scx_name: &str) -> anyhow::Result<CustomSchedName> {
        if SupportedSched::from_str(scx_name).is_ok() {
            anyhow::bail!("{scx_name} is a built-in scheduler");
        }
        if !is_valid_custom_name(scx_name) {
            anyhow::bail!("{scx_name} is not a valid custom scheduler name");
        }
        Ok(CustomSchedName(scx_name.to_owned()))
    }
}

impl TryFrom<&str> for CustomSchedName {
    type Error = <CustomSchedName as FromStr>::Err;
    fn try_from(s: &str) -> Result<CustomSchedName, Self::Error> {
        <CustomSchedName as FromStr>::from_str(s)
    }
}

/// The name of a scheduler `scx_loader` can run, built in or custom.
///
/// It goes over D-Bus and into the config as its name.
/// Any name that isn't builtin, but has the form `scx_[a-z0-9_]+` parses as
/// `Custom`. Such a scheduler can only be started if the config defines it
/// with a `path`.
#[derive(Clone, PartialEq, Type)]
#[zbus(signature = "s")]
#[non_exhaustive]
pub enum SchedName {
    Builtin(SupportedSched),
    /// A scheduler that isn't built in, defined in the config.
    Custom(CustomSchedName),
}

impl SchedName {
    /// The scheduler's name, which is also the binary name of a built-in
    /// scheduler.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            SchedName::Builtin(builtin) => builtin.as_str(),
            SchedName::Custom(custom) => custom.as_str(),
        }
    }
}

/// Prints the variant name of a built-in scheduler, and the
/// name of a custom one. Messages like "running Lavd" stay
/// as they were.
impl fmt::Debug for SchedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchedName::Builtin(builtin) => builtin.fmt(f),
            SchedName::Custom(custom) => custom.fmt(f),
        }
    }
}

impl From<SupportedSched> for SchedName {
    fn from(builtin: SupportedSched) -> Self {
        SchedName::Builtin(builtin)
    }
}

impl From<CustomSchedName> for SchedName {
    fn from(custom: CustomSchedName) -> Self {
        SchedName::Custom(custom)
    }
}

impl Serialize for SchedName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SchedName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for SchedName {
    type Err = anyhow::Error;

    fn from_str(scx_name: &str) -> anyhow::Result<SchedName> {
        match SupportedSched::from_str(scx_name) {
            Ok(builtin) => Ok(SchedName::Builtin(builtin)),
            Err(_) if is_valid_custom_name(scx_name) => {
                Ok(SchedName::Custom(CustomSchedName(scx_name.to_owned())))
            }
            Err(err) => Err(err),
        }
    }
}

impl TryFrom<&str> for SchedName {
    type Error = <SchedName as FromStr>::Err;
    fn try_from(s: &str) -> Result<SchedName, Self::Error> {
        <SchedName as FromStr>::from_str(s)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Type, PartialEq)]
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
    use zbus::wire::serialized::Context;
    use zbus::wire::{LE, to_bytes};

    fn encode<T: Serialize + Type>(value: &T) -> (String, String) {
        let encoded = to_bytes(Context::new(LE, 0), value).unwrap();
        (T::SIGNATURE.to_string(), hex(encoded.bytes()))
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        bytes.iter().fold(String::new(), |mut out, b| {
            write!(out, "{b:02x}").unwrap();
            out
        })
    }

    #[test]
    fn parses_builtin_and_custom_names() {
        assert_eq!(
            "scx_lavd".parse::<SupportedSched>().unwrap(),
            SupportedSched::Lavd
        );
        assert_eq!(
            "scx_lavd".parse::<SchedName>().unwrap(),
            SchedName::Builtin(SupportedSched::Lavd)
        );
        let custom = "scx_my_sched2".parse::<CustomSchedName>().unwrap();
        assert_eq!(custom.as_str(), "scx_my_sched2");
        assert_eq!(
            "scx_my_sched2".parse::<SchedName>().unwrap(),
            SchedName::Custom(custom)
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
                name.parse::<SchedName>().is_err(),
                "{name:?} should be rejected"
            );
            assert!(
                name.parse::<CustomSchedName>().is_err(),
                "{name:?} should be rejected"
            );
        }
        assert!("scx_mine".parse::<SupportedSched>().is_err());
        assert!("scx_lavd".parse::<CustomSchedName>().is_err());
    }

    #[test]
    fn dbus_wire_format_is_the_name() {
        assert_eq!(SupportedSched::SIGNATURE, "s");
        assert_eq!(CustomSchedName::SIGNATURE, "s");
        assert_eq!(SchedName::SIGNATURE, "s");

        let ctxt = Context::new(LE, 0);
        let encoded = to_bytes(ctxt, &SupportedSched::Lavd).unwrap();
        assert_eq!(encoded.deserialize::<String>().unwrap().0, "scx_lavd");
        assert_eq!(
            encoded.deserialize::<SupportedSched>().unwrap().0,
            SupportedSched::Lavd
        );

        for sched in [
            SchedName::Builtin(SupportedSched::Lavd),
            SchedName::Custom("scx_mine".parse().unwrap()),
        ] {
            let encoded = to_bytes(ctxt, &sched).unwrap();
            assert_eq!(encoded.deserialize::<String>().unwrap().0, sched.as_str());
            assert_eq!(encoded.deserialize::<SchedName>().unwrap().0, sched);
        }
    }

    #[test]
    fn dbus_wire_format_is_pinned() {
        // hardcoded to catch signature val during zbus upgrades
        let lavd = ("s".to_owned(), "080000007363785f6c61766400".to_owned());
        let mine = ("s".to_owned(), "080000007363785f6d696e6500".to_owned());
        assert_eq!(encode(&SupportedSched::Lavd), lavd);
        assert_eq!(encode(&SchedName::Builtin(SupportedSched::Lavd)), lavd);
        assert_eq!(encode(&"scx_mine".parse::<SchedName>().unwrap()), mine);
        assert_eq!(
            encode(&"scx_mine".parse::<CustomSchedName>().unwrap()),
            mine
        );

        let modes = [
            SchedMode::Auto,
            SchedMode::Gaming,
            SchedMode::PowerSave,
            SchedMode::LowLatency,
            SchedMode::Server,
        ];
        for (index, mode) in (0u32..).zip(modes) {
            let expected = ("u".to_owned(), hex(&index.to_le_bytes()));
            assert_eq!(encode(&mode), expected, "{mode:?}");
        }
        assert_eq!(
            encode(&modes.to_vec()),
            (
                "au".to_owned(),
                "140000000000000001000000020000000300000004000000".to_owned()
            )
        );

        let mode_args: Vec<(SchedMode, Vec<String>)> = vec![
            (SchedMode::Auto, vec![]),
            (SchedMode::Gaming, vec!["-m".into(), "performance".into()]),
            (SchedMode::Server, vec!["--x".into()]),
        ];
        assert_eq!(
            encode(&mode_args),
            (
                "a(uas)".to_owned(),
                concat!(
                    "38000000000000000000000000000000",
                    "0100000018000000020000002d6d0000",
                    "0b000000706572666f726d616e636500",
                    "0400000008000000030000002d2d7800",
                )
                .to_owned()
            )
        );
    }

    #[test]
    fn into_str_matches_as_str() {
        let name: &str = SupportedSched::Lavd.into();
        assert_eq!(name, SupportedSched::Lavd.as_str());
    }

    #[test]
    fn debug_keeps_builtin_variant_names() {
        assert_eq!(format!("{:?}", SupportedSched::Lavd), "Lavd");
        assert_eq!(
            format!("{:?}", SchedName::Builtin(SupportedSched::Lavd)),
            "Lavd"
        );
        assert_eq!(
            format!("{:?}", SchedName::Custom("scx_mine".parse().unwrap())),
            "scx_mine"
        );
    }
}
