use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    #[serde(alias = "adsb", alias = "adsb_1090", alias = "mode_s", alias = "modes")]
    Adsb1090,
    #[serde(alias = "uat", alias = "uat_978")]
    Uat978,
    Acars,
    Vdl2,
    Ais,
}

impl Protocol {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Adsb1090 => "adsb1090",
            Self::Uat978 => "uat978",
            Self::Acars => "acars",
            Self::Vdl2 => "vdl2",
            Self::Ais => "ais",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Adsb1090 => "ADS-B / Mode S 1090ES",
            Self::Uat978 => "UAT 978",
            Self::Acars => "ACARS",
            Self::Vdl2 => "VDL2",
            Self::Ais => "AIS",
        }
    }

    #[must_use]
    pub const fn default_center_frequency_hz(self) -> u32 {
        match self {
            Self::Adsb1090 => 1_090_000_000,
            Self::Uat978 => 978_000_000,
            Self::Acars => 131_550_000,
            Self::Vdl2 => 136_975_000,
            Self::Ais => 162_000_000,
        }
    }

    #[must_use]
    pub const fn default_sample_rate_hz(self) -> u32 {
        match self {
            Self::Adsb1090 => 2_000_000,
            Self::Uat978 => 2_400_000,
            Self::Acars | Self::Vdl2 | Self::Ais => 1_024_000,
        }
    }

    #[must_use]
    pub const fn uses_current_modes_decoder(self) -> bool {
        matches!(self, Self::Adsb1090)
    }

    #[must_use]
    pub const fn produces_aircraft_state(self) -> bool {
        matches!(self, Self::Adsb1090 | Self::Uat978)
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.key())
    }
}

impl FromStr for Protocol {
    type Err = ProtocolParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "adsb" | "adsb1090" | "adsb_1090" | "mode_s" | "modes" => Ok(Self::Adsb1090),
            "uat" | "uat978" | "uat_978" => Ok(Self::Uat978),
            "acars" => Ok(Self::Acars),
            "vdl2" | "vdl_2" => Ok(Self::Vdl2),
            "ais" => Ok(Self::Ais),
            other => Err(ProtocolParseError {
                value: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProtocolParseError {
    value: String,
}

impl fmt::Display for ProtocolParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported protocol {}; expected one of adsb1090, uat978, acars, vdl2, ais",
            self.value
        )
    }
}

impl std::error::Error for ProtocolParseError {}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
pub struct RadioConfig {
    pub protocol: Protocol,
    pub center_frequency_hz: u32,
    pub sample_rate_hz: u32,
}

impl RadioConfig {
    #[must_use]
    pub const fn for_protocol(protocol: Protocol) -> Self {
        Self {
            protocol,
            center_frequency_hz: protocol.default_center_frequency_hz(),
            sample_rate_hz: protocol.default_sample_rate_hz(),
        }
    }

    #[must_use]
    pub const fn with_center_frequency_hz(mut self, center_frequency_hz: u32) -> Self {
        self.center_frequency_hz = center_frequency_hz;
        self
    }

    #[must_use]
    pub const fn with_sample_rate_hz(mut self, sample_rate_hz: u32) -> Self {
        self.sample_rate_hz = sample_rate_hz;
        self
    }
}

impl Default for RadioConfig {
    fn default() -> Self {
        Self::for_protocol(Protocol::default())
    }
}
