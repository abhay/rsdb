use alloc::{string::String, vec::Vec};
use core::fmt::{self, Write as _};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Frame {
    bytes: [u8; FrameLength::LONG_BYTES],
    length: FrameLength,
}

impl Frame {
    /// Builds a Mode S frame from raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::InvalidByteLength`] unless `bytes` contains exactly
    /// 7 or 14 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FrameError> {
        let length = FrameLength::from_byte_len(bytes.len())?;
        let mut frame_bytes = [0; FrameLength::LONG_BYTES];
        frame_bytes[..bytes.len()].copy_from_slice(bytes);

        Ok(Self {
            bytes: frame_bytes,
            length,
        })
    }

    /// Builds a Mode S frame from an ASCII hex frame.
    ///
    /// Accepts bare hex and common AVR-style `*...;` delimiters.
    ///
    /// # Errors
    ///
    /// Returns an error when the input has invalid hex, an odd number of hex
    /// digits, or any byte length other than 7 or 14.
    pub fn from_hex(hex: &str) -> Result<Self, FrameError> {
        let normalized = normalize_hex_frame(hex);

        if !normalized.len().is_multiple_of(2) {
            return Err(FrameError::OddHexLength);
        }

        let mut bytes = Vec::with_capacity(normalized.len() / 2);
        for chunk in normalized.as_bytes().chunks_exact(2) {
            let high = hex_value(chunk[0]).ok_or(FrameError::InvalidHexByte { byte: chunk[0] })?;
            let low = hex_value(chunk[1]).ok_or(FrameError::InvalidHexByte { byte: chunk[1] })?;
            bytes.push((high << 4) | low);
        }

        Self::from_bytes(&bytes)
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self.length {
            FrameLength::Short => &self.bytes[..FrameLength::SHORT_BYTES],
            FrameLength::Long => &self.bytes[..FrameLength::LONG_BYTES],
        }
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut output = String::with_capacity(self.bytes().len() * 2);

        for byte in self.bytes() {
            let _ = write!(&mut output, "{byte:02X}");
        }

        output
    }

    #[must_use]
    pub const fn frame_length(&self) -> FrameLength {
        self.length
    }

    #[must_use]
    pub const fn bit_len(&self) -> usize {
        self.length.bit_len()
    }

    #[must_use]
    pub fn downlink_format(&self) -> DownlinkFormat {
        DownlinkFormat::from_bits(self.bytes[0] >> 3)
    }

    #[must_use]
    pub const fn first_field(&self) -> u8 {
        self.bytes[0] & 0b0000_0111
    }

    #[must_use]
    pub fn explicit_icao_address(&self) -> Option<IcaoAddress> {
        match self.downlink_format() {
            DownlinkFormat::AllCallReply
            | DownlinkFormat::ExtendedSquitter
            | DownlinkFormat::ExtendedSquitterNonTransponder => Some(IcaoAddress::from_bytes([
                self.bytes[1],
                self.bytes[2],
                self.bytes[3],
            ])),
            _ => None,
        }
    }

    #[must_use]
    pub fn parity(&self) -> u32 {
        let parity = &self.bytes()[self.bytes().len() - 3..];
        u32::from(parity[0]) << 16 | u32::from(parity[1]) << 8 | u32::from(parity[2])
    }

    #[must_use]
    pub fn is_crc_valid(&self) -> bool {
        crate::crc::crc24_modes(self).is_valid()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FrameLength {
    Short,
    Long,
}

impl FrameLength {
    pub const SHORT_BYTES: usize = 7;
    pub const LONG_BYTES: usize = 14;

    fn from_byte_len(len: usize) -> Result<Self, FrameError> {
        match len {
            Self::SHORT_BYTES => Ok(Self::Short),
            Self::LONG_BYTES => Ok(Self::Long),
            _ => Err(FrameError::InvalidByteLength(len)),
        }
    }

    #[must_use]
    pub const fn bit_len(self) -> usize {
        match self {
            Self::Short => 56,
            Self::Long => 112,
        }
    }

    #[must_use]
    pub const fn byte_len(self) -> usize {
        match self {
            Self::Short => Self::SHORT_BYTES,
            Self::Long => Self::LONG_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DownlinkFormat {
    ShortAirAirSurveillance,
    SurveillanceAltitudeReply,
    SurveillanceIdentityReply,
    AllCallReply,
    ExtendedSquitter,
    ExtendedSquitterNonTransponder,
    CommBAltitudeReply,
    CommBIdentityReply,
    Unknown(u8),
}

impl DownlinkFormat {
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        match bits {
            0 => Self::ShortAirAirSurveillance,
            4 => Self::SurveillanceAltitudeReply,
            5 => Self::SurveillanceIdentityReply,
            11 => Self::AllCallReply,
            17 => Self::ExtendedSquitter,
            18 => Self::ExtendedSquitterNonTransponder,
            20 => Self::CommBAltitudeReply,
            21 => Self::CommBIdentityReply,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::ShortAirAirSurveillance => 0,
            Self::SurveillanceAltitudeReply => 4,
            Self::SurveillanceIdentityReply => 5,
            Self::AllCallReply => 11,
            Self::ExtendedSquitter => 17,
            Self::ExtendedSquitterNonTransponder => 18,
            Self::CommBAltitudeReply => 20,
            Self::CommBIdentityReply => 21,
            Self::Unknown(bits) => bits,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IcaoAddress(u32);

impl IcaoAddress {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 3]) -> Self {
        Self(u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]))
    }

    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for IcaoAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:06X}", self.0)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FrameError {
    InvalidByteLength(usize),
    InvalidHexByte { byte: u8 },
    OddHexLength,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidByteLength(len) => write!(
                formatter,
                "Mode S frames must be 7 or 14 bytes, got {len} bytes"
            ),
            Self::InvalidHexByte { byte } => {
                write!(formatter, "invalid hex byte 0x{byte:02X}")
            }
            Self::OddHexLength => write!(formatter, "hex frame has an odd number of digits"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FrameError {}

fn normalize_hex_frame(hex: &str) -> String {
    let trimmed = hex.trim();
    let without_delimiters = trimmed
        .strip_prefix('*')
        .and_then(|value| value.strip_suffix(';'))
        .unwrap_or(trimmed);

    without_delimiters
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .map(char::from)
        .collect()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn parses_common_ascii_hex_formats() {
        let frame = Frame::from_hex(" *8d4840d6202cc371c32ce0576098; ").unwrap();

        assert_eq!(frame.frame_length(), FrameLength::Long);
        assert_eq!(frame.downlink_format(), DownlinkFormat::ExtendedSquitter);
        assert_eq!(frame.explicit_icao_address().unwrap().to_string(), "4840D6");
        assert_eq!(frame.parity(), 0x57_60_98);
        assert_eq!(frame.to_hex(), "8D4840D6202CC371C32CE0576098");
    }

    #[test]
    fn rejects_unsupported_frame_lengths() {
        let error = Frame::from_hex("8D4840").unwrap_err();

        assert_eq!(error, FrameError::InvalidByteLength(3));
    }
}
