use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    DemodConfig, Frame, FrameError, LONG_FRAME_TOTAL_SAMPLES, Protocol, RadioConfig,
    decode_frames_from_iq,
};

/// Current decoded frame-record schema version.
pub const FRAME_RECORD_SCHEMA_VERSION: u32 = 1;
/// Current raw I/Q capture-record schema version.
pub const IQ_CAPTURE_RECORD_SCHEMA_VERSION: u32 = 1;

/// Versioned NDJSON record for a demodulated Mode S frame.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FrameRecord {
    pub schema_version: u32,
    #[serde(default)]
    pub protocol: Protocol,
    pub now_ms: u64,
    pub sample_index: u64,
    pub raw: String,
    pub downlink_format: u8,
    pub bit_len: usize,
    pub crc_valid: bool,
}

impl FrameRecord {
    #[must_use]
    pub fn new(now_ms: u64, sample_index: u64, frame: &Frame) -> Self {
        Self::from_modes_frame(Protocol::Adsb1090, now_ms, sample_index, frame)
    }

    /// Builds a decoded frame record for the configured protocol.
    ///
    /// # Errors
    ///
    /// Returns an error when `protocol` does not use the current Mode S frame
    /// format.
    pub fn new_for_protocol(
        protocol: Protocol,
        now_ms: u64,
        sample_index: u64,
        frame: &Frame,
    ) -> Result<Self, FrameRecordError> {
        if protocol.uses_current_modes_decoder() {
            Ok(Self::from_modes_frame(
                protocol,
                now_ms,
                sample_index,
                frame,
            ))
        } else {
            Err(FrameRecordError::UnsupportedProtocol(protocol))
        }
    }

    fn from_modes_frame(protocol: Protocol, now_ms: u64, sample_index: u64, frame: &Frame) -> Self {
        Self {
            schema_version: FRAME_RECORD_SCHEMA_VERSION,
            protocol,
            now_ms,
            sample_index,
            raw: frame.to_hex(),
            downlink_format: frame.downlink_format().bits(),
            bit_len: frame.bit_len(),
            crc_valid: frame.is_crc_valid(),
        }
    }

    /// Parses the record's raw hex frame.
    ///
    /// # Errors
    ///
    /// Returns an error when the record schema version is unsupported or the
    /// raw frame cannot be parsed as a 56-bit or 112-bit Mode S frame.
    pub fn parse_frame(&self) -> Result<Frame, FrameRecordError> {
        self.validate_schema_version()?;
        if !self.protocol.uses_current_modes_decoder() {
            return Err(FrameRecordError::UnsupportedProtocol(self.protocol));
        }
        Frame::from_hex(&self.raw).map_err(FrameRecordError::InvalidFrame)
    }

    /// Checks whether this record belongs to the expected protocol.
    ///
    /// # Errors
    ///
    /// Returns an error when the record protocol does not match `protocol`.
    pub fn validate_protocol(&self, protocol: Protocol) -> Result<(), FrameRecordError> {
        if self.protocol == protocol {
            Ok(())
        } else {
            Err(FrameRecordError::ProtocolMismatch {
                expected: protocol,
                actual: self.protocol,
            })
        }
    }

    /// Checks whether the record schema is supported by this crate.
    ///
    /// # Errors
    ///
    /// Returns an error when `schema_version` is not
    /// [`FRAME_RECORD_SCHEMA_VERSION`].
    pub const fn validate_schema_version(&self) -> Result<(), FrameRecordError> {
        if self.schema_version == FRAME_RECORD_SCHEMA_VERSION {
            Ok(())
        } else {
            Err(FrameRecordError::UnsupportedSchemaVersion {
                expected: FRAME_RECORD_SCHEMA_VERSION,
                actual: self.schema_version,
            })
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FrameRecordError {
    UnsupportedSchemaVersion {
        expected: u32,
        actual: u32,
    },
    UnsupportedProtocol(Protocol),
    ProtocolMismatch {
        expected: Protocol,
        actual: Protocol,
    },
    InvalidFrame(FrameError),
}

impl fmt::Display for FrameRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { expected, actual } => write!(
                formatter,
                "unsupported frame record schema_version {actual}; expected {expected}"
            ),
            Self::UnsupportedProtocol(protocol) => {
                write!(formatter, "unsupported frame record protocol {protocol}")
            }
            Self::ProtocolMismatch { expected, actual } => write!(
                formatter,
                "frame record protocol mismatch: expected {expected}, got {actual}"
            ),
            Self::InvalidFrame(error) => {
                write!(formatter, "invalid frame record raw frame: {error}")
            }
        }
    }
}

/// Versioned NDJSON record for a raw unsigned 8-bit interleaved I/Q chunk.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct IqCaptureRecord {
    pub schema_version: u32,
    pub radio: RadioConfig,
    pub now_ms: u64,
    pub sample_index: u64,
    pub iq_bytes: Vec<u8>,
}

impl IqCaptureRecord {
    #[must_use]
    pub const fn new(
        radio: RadioConfig,
        now_ms: u64,
        sample_index: u64,
        iq_bytes: Vec<u8>,
    ) -> Self {
        Self {
            schema_version: IQ_CAPTURE_RECORD_SCHEMA_VERSION,
            radio,
            now_ms,
            sample_index,
            iq_bytes,
        }
    }

    #[must_use]
    pub const fn protocol(&self) -> Protocol {
        self.radio.protocol
    }

    /// Checks whether the record schema is supported by this crate.
    ///
    /// # Errors
    ///
    /// Returns an error when `schema_version` is not
    /// [`IQ_CAPTURE_RECORD_SCHEMA_VERSION`].
    pub const fn validate_schema_version(&self) -> Result<(), IqCaptureRecordError> {
        if self.schema_version == IQ_CAPTURE_RECORD_SCHEMA_VERSION {
            Ok(())
        } else {
            Err(IqCaptureRecordError::UnsupportedSchemaVersion {
                expected: IQ_CAPTURE_RECORD_SCHEMA_VERSION,
                actual: self.schema_version,
            })
        }
    }
}

/// Decodes protocol-scoped I/Q capture records into decoded frame records.
///
/// # Errors
///
/// Returns an error when the capture records are not a contiguous run from one
/// supported radio configuration or contain malformed I/Q data.
pub fn replay_iq_capture_records(
    records: &[IqCaptureRecord],
) -> Result<Vec<FrameRecord>, IqCaptureRecordError> {
    let Some(first_record) = records.first() else {
        return Ok(Vec::new());
    };
    let radio = first_record.radio;

    if !radio.protocol.uses_current_modes_decoder() {
        return Err(IqCaptureRecordError::UnsupportedProtocol(radio.protocol));
    }

    let initial_sample_index = usize::try_from(first_record.sample_index)
        .map_err(|_| IqCaptureRecordError::SampleIndexOverflow(first_record.sample_index))?;
    let mut decoder = ModesFrameDecoder::with_stream_sample_offset(initial_sample_index);
    let mut expected_sample_index = first_record.sample_index;
    let mut frames = Vec::new();

    for record in records {
        record.validate_schema_version()?;
        if record.radio != radio {
            return Err(IqCaptureRecordError::RadioMismatch {
                expected: radio,
                actual: record.radio,
            });
        }
        if record.sample_index != expected_sample_index {
            return Err(IqCaptureRecordError::NonContiguousSampleIndex {
                expected: expected_sample_index,
                actual: record.sample_index,
            });
        }
        if !record.iq_bytes.len().is_multiple_of(2) {
            return Err(IqCaptureRecordError::OddIqByteLength(record.iq_bytes.len()));
        }

        for (sample_index, frame) in decoder.decode_chunk(&record.iq_bytes) {
            let sample_index = u64::try_from(sample_index)
                .map_err(|_| IqCaptureRecordError::SampleIndexOverflow(u64::MAX))?;
            frames.push(
                FrameRecord::new_for_protocol(radio.protocol, record.now_ms, sample_index, &frame)
                    .map_err(IqCaptureRecordError::FrameRecord)?,
            );
        }

        let chunk_samples = u64::try_from(record.iq_bytes.len() / 2)
            .map_err(|_| IqCaptureRecordError::SampleIndexOverflow(u64::MAX))?;
        expected_sample_index = expected_sample_index.saturating_add(chunk_samples);
    }

    Ok(frames)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum IqCaptureRecordError {
    UnsupportedSchemaVersion {
        expected: u32,
        actual: u32,
    },
    UnsupportedProtocol(Protocol),
    RadioMismatch {
        expected: RadioConfig,
        actual: RadioConfig,
    },
    NonContiguousSampleIndex {
        expected: u64,
        actual: u64,
    },
    OddIqByteLength(usize),
    SampleIndexOverflow(u64),
    FrameRecord(FrameRecordError),
}

impl fmt::Display for IqCaptureRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { expected, actual } => write!(
                formatter,
                "unsupported I/Q capture record schema_version {actual}; expected {expected}"
            ),
            Self::UnsupportedProtocol(protocol) => {
                write!(formatter, "unsupported I/Q capture protocol {protocol}")
            }
            Self::RadioMismatch { expected, actual } => write!(
                formatter,
                "I/Q capture radio mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::NonContiguousSampleIndex { expected, actual } => write!(
                formatter,
                "I/Q capture sample_index is not contiguous: expected {expected}, got {actual}"
            ),
            Self::OddIqByteLength(len) => {
                write!(formatter, "I/Q capture record has odd byte length {len}")
            }
            Self::SampleIndexOverflow(sample_index) => {
                write!(
                    formatter,
                    "I/Q capture sample index overflows usize: {sample_index}"
                )
            }
            Self::FrameRecord(error) => error.fmt(formatter),
        }
    }
}

/// Stateful decoder for Mode S frames split across unsigned I/Q chunks.
#[derive(Debug, Default)]
pub struct ModesFrameDecoder {
    tail: Vec<u8>,
    stream_sample_offset: usize,
}

impl ModesFrameDecoder {
    #[must_use]
    pub const fn with_stream_sample_offset(stream_sample_offset: usize) -> Self {
        Self {
            tail: Vec::new(),
            stream_sample_offset,
        }
    }

    #[must_use]
    pub fn decode_chunk(&mut self, data: &[u8]) -> Vec<(usize, Frame)> {
        let tail_samples = self.tail.len() / 2;
        let combined_base_sample = self.stream_sample_offset.saturating_sub(tail_samples);
        let mut combined = Vec::with_capacity(self.tail.len() + data.len());
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(data);

        let frames = decode_frames_from_iq(&combined, DemodConfig::default())
            .into_iter()
            .filter(|decoded| decoded.sample_index >= tail_samples)
            .map(|decoded| (combined_base_sample + decoded.sample_index, decoded.frame))
            .collect();

        self.stream_sample_offset += data.len() / 2;
        let tail_iq_bytes = (LONG_FRAME_TOTAL_SAMPLES - 1) * 2;
        let tail_start = combined.len().saturating_sub(tail_iq_bytes);
        self.tail.clear();
        self.tail.extend_from_slice(&combined[tail_start..]);

        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW_MAGNITUDE: u32 = 16;
    const HIGH_MAGNITUDE: u32 = 4_096;
    const PREAMBLE_HIGH_SAMPLES: [usize; 4] = [0, 2, 7, 9];
    const PREAMBLE_SAMPLES: usize = 16;
    const LONG_FRAME_BITS: usize = 112;
    const SAMPLES_PER_BIT: usize = 2;

    #[test]
    fn replay_iq_capture_records_decodes_modes_frames() {
        let radio = RadioConfig::for_protocol(Protocol::Adsb1090);
        let frame = Frame::from_hex("8D4840D6202CC371C32CE0576098").unwrap();
        let iq_bytes = synthetic_iq_for_frame(&frame, 5);
        let records = vec![IqCaptureRecord::new(radio, 100, 50, iq_bytes)];
        let frames = replay_iq_capture_records(&records).unwrap();

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].protocol, Protocol::Adsb1090);
        assert_eq!(frames[0].now_ms, 100);
        assert_eq!(frames[0].sample_index, 55);
        assert_eq!(frames[0].raw, frame.to_hex());
    }

    #[test]
    fn replay_iq_capture_records_rejects_mixed_radio_configs() {
        let first = IqCaptureRecord::new(
            RadioConfig::for_protocol(Protocol::Adsb1090),
            100,
            0,
            vec![],
        );
        let second =
            IqCaptureRecord::new(RadioConfig::for_protocol(Protocol::Uat978), 200, 0, vec![]);

        assert!(matches!(
            replay_iq_capture_records(&[first, second]),
            Err(IqCaptureRecordError::RadioMismatch { .. })
        ));
    }

    #[test]
    fn replay_iq_capture_records_rejects_non_contiguous_chunks() {
        let radio = RadioConfig::for_protocol(Protocol::Adsb1090);
        let first = IqCaptureRecord::new(radio, 100, 0, vec![127, 127]);
        let second = IqCaptureRecord::new(radio, 200, 2, vec![127, 127]);

        assert_eq!(
            replay_iq_capture_records(&[first, second]).unwrap_err(),
            IqCaptureRecordError::NonContiguousSampleIndex {
                expected: 1,
                actual: 2,
            }
        );
    }

    fn synthetic_iq_for_frame(frame: &Frame, offset: usize) -> Vec<u8> {
        synthetic_magnitudes_for_frame(frame, offset)
            .into_iter()
            .flat_map(iq_pair_for_magnitude)
            .collect()
    }

    fn synthetic_magnitudes_for_frame(frame: &Frame, offset: usize) -> Vec<u32> {
        let mut samples = vec![LOW_MAGNITUDE; offset + LONG_FRAME_TOTAL_SAMPLES + 8];

        for sample in PREAMBLE_HIGH_SAMPLES {
            samples[offset + sample] = HIGH_MAGNITUDE;
        }

        let bit_start = offset + PREAMBLE_SAMPLES;
        for bit_index in 0..LONG_FRAME_BITS {
            let byte = frame.bytes()[bit_index / 8];
            let shift = 7 - bit_index % 8;
            let bit = (byte >> shift) & 1;
            let sample_index = bit_start + bit_index * SAMPLES_PER_BIT;

            if bit == 1 {
                samples[sample_index] = HIGH_MAGNITUDE;
                samples[sample_index + 1] = LOW_MAGNITUDE;
            } else {
                samples[sample_index] = LOW_MAGNITUDE;
                samples[sample_index + 1] = HIGH_MAGNITUDE;
            }
        }

        samples
    }

    fn iq_pair_for_magnitude(magnitude: u32) -> [u8; 2] {
        match magnitude {
            LOW_MAGNITUDE => [131, 127],
            HIGH_MAGNITUDE => [191, 127],
            _ => [127, 127],
        }
    }
}
