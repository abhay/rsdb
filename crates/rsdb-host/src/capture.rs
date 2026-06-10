use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    DecodedFrame, DecodedFrameSignal, DemodConfig, ExtendedSquitter, Frame, FrameError,
    LONG_FRAME_TOTAL_SAMPLES, Protocol, RadioConfig, ReceiverIdentity, ReceiverSite,
    decode_frames_from_iq,
};

/// Current decoded frame-record schema version.
pub const FRAME_RECORD_SCHEMA_VERSION: u32 = 2;
/// Current signed frame-record batch schema version.
pub const FRAME_RECORD_BATCH_SCHEMA_VERSION: u32 = 1;
/// Current raw I/Q capture-record schema version.
pub const IQ_CAPTURE_RECORD_SCHEMA_VERSION: u32 = 1;
const RTL_U8_FULL_SCALE_MAGNITUDE_POWER: u32 = 2 * 128 * 128;

/// Versioned NDJSON record for a demodulated Mode S frame.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FrameRecord {
    pub schema_version: u32,
    #[serde(default)]
    pub protocol: Protocol,
    pub now_ms: u64,
    pub sample_index: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_start_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rx_elapsed_ns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rx_timestamp_uncertainty_ns_estimate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ReceiverIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_site: Option<ReceiverSite>,
    pub center_frequency_hz: u32,
    pub sample_rate_hz: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain_tenth_db: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bias_t: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tuner_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_sample_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_samples_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipped_sample_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dc_i_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dc_q_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<FrameSignalMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icao: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adsb_type_code: Option<u8>,
    pub raw: String,
    pub downlink_format: u8,
    pub bit_len: usize,
    pub crc_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FrameSignalMetrics {
    pub signal_power: u32,
    pub noise_power: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_dbfs_estimate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snr_db_estimate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_noise_power: Option<u32>,
    pub beast_signal_level: u8,
    pub preamble_high_avg: u32,
    pub preamble_low_avg: u32,
    pub preamble_delta: u32,
    pub bit_margin_min: u32,
    pub bit_margin_mean: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FrameRecordBatch {
    pub schema_version: u32,
    pub protocol: Protocol,
    pub receiver: ReceiverIdentity,
    pub records: Vec<FrameRecord>,
}

impl FrameRecordBatch {
    #[must_use]
    pub const fn new(
        protocol: Protocol,
        receiver: ReceiverIdentity,
        records: Vec<FrameRecord>,
    ) -> Self {
        Self {
            schema_version: FRAME_RECORD_BATCH_SCHEMA_VERSION,
            protocol,
            receiver,
            records,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub const fn is_supported_schema_version(&self) -> bool {
        self.schema_version == FRAME_RECORD_BATCH_SCHEMA_VERSION
    }

    /// Validates all records in this signed batch.
    ///
    /// # Errors
    ///
    /// Returns an error when the batch schema is unsupported, the receiver is
    /// missing, records are empty, any record is invalid, records belong to a
    /// different protocol, or a record claims a different receiver.
    pub fn validate(&self) -> Result<(), FrameRecordError> {
        if !self.is_supported_schema_version() {
            return Err(FrameRecordError::UnsupportedBatchSchemaVersion {
                expected: FRAME_RECORD_BATCH_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if self.receiver.id.trim().is_empty() {
            return Err(FrameRecordError::InvalidField {
                field: "receiver.id",
                reason: "must not be empty".to_owned(),
            });
        }
        if self.records.is_empty() {
            return Err(FrameRecordError::InvalidField {
                field: "records",
                reason: "must contain at least one frame record".to_owned(),
            });
        }

        let mut sequence_validator = FrameRecordSequenceValidator::default();
        for record in &self.records {
            sequence_validator.validate_next(record)?;
            record.validate_protocol(self.protocol)?;
            if let Some(record_receiver) = &record.receiver
                && record_receiver.id != self.receiver.id
            {
                return Err(FrameRecordError::MetadataMismatch {
                    field: "receiver.id",
                    expected: self.receiver.id.clone(),
                    actual: record_receiver.id.clone(),
                });
            }
            record.parse_frame()?;
        }

        Ok(())
    }

    #[must_use]
    pub fn receiver_site(&self) -> Option<ReceiverSite> {
        self.records
            .iter()
            .find_map(|record| record.receiver_site.clone())
    }
}

impl FrameRecord {
    #[must_use]
    pub fn new(now_ms: u64, sample_index: u64, frame: &Frame) -> Self {
        Self::from_modes_frame(
            RadioConfig::for_protocol(Protocol::Adsb1090),
            now_ms,
            sample_index,
            frame,
            None,
        )
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
        Self::new_for_radio(
            RadioConfig::for_protocol(protocol),
            now_ms,
            sample_index,
            frame,
        )
    }

    /// Builds a decoded frame record for the configured radio.
    ///
    /// # Errors
    ///
    /// Returns an error when `radio.protocol` does not use the current Mode S
    /// frame format.
    pub fn new_for_radio(
        radio: RadioConfig,
        now_ms: u64,
        sample_index: u64,
        frame: &Frame,
    ) -> Result<Self, FrameRecordError> {
        if radio.protocol.uses_current_modes_decoder() {
            Ok(Self::from_modes_frame(
                radio,
                now_ms,
                sample_index,
                frame,
                None,
            ))
        } else {
            Err(FrameRecordError::UnsupportedProtocol(radio.protocol))
        }
    }

    /// Builds a decoded frame record from a demodulator result.
    ///
    /// # Errors
    ///
    /// Returns an error when `radio.protocol` does not use the current Mode S
    /// frame format or when the decoded sample index does not fit in `u64`.
    pub fn from_decoded_frame(
        radio: RadioConfig,
        now_ms: u64,
        decoded: &DecodedFrame,
    ) -> Result<Self, FrameRecordError> {
        let sample_index = u64::try_from(decoded.sample_index)
            .map_err(|_| FrameRecordError::SampleIndexOverflow(decoded.sample_index))?;

        if !radio.protocol.uses_current_modes_decoder() {
            return Err(FrameRecordError::UnsupportedProtocol(radio.protocol));
        }

        Ok(Self::from_modes_frame(
            radio,
            now_ms,
            sample_index,
            &decoded.frame,
            Some(FrameSignalMetrics::from(decoded.signal)),
        ))
    }

    #[must_use]
    pub fn with_receiver(mut self, receiver: Option<ReceiverIdentity>) -> Self {
        self.receiver = receiver;
        self
    }

    #[must_use]
    pub fn with_receiver_site(mut self, receiver_site: Option<ReceiverSite>) -> Self {
        self.receiver_site = receiver_site;
        self
    }

    pub fn apply_iq_chunk_metrics(&mut self, metrics: IqChunkMetrics) {
        self.clipped_sample_ratio = Some(metrics.clipped_sample_ratio);
        self.dc_i_offset = Some(metrics.dc_i_offset);
        self.dc_q_offset = Some(metrics.dc_q_offset);

        if let Some(signal) = &mut self.signal {
            signal.chunk_noise_power = Some(metrics.noise_power);
            signal.snr_db_estimate = snr_db_estimate(signal.signal_power, metrics.noise_power)
                .or(signal.snr_db_estimate);
        }
    }

    pub fn set_dropped_samples_before(&mut self, dropped_samples_before: u64) {
        self.dropped_samples_before = Some(dropped_samples_before);
        self.rx_timestamp_uncertainty_ns_estimate =
            rx_timestamp_uncertainty_ns_estimate(self.sample_rate_hz, Some(dropped_samples_before));
    }

    fn from_modes_frame(
        radio: RadioConfig,
        now_ms: u64,
        sample_index: u64,
        frame: &Frame,
        signal: Option<FrameSignalMetrics>,
    ) -> Self {
        Self {
            schema_version: FRAME_RECORD_SCHEMA_VERSION,
            protocol: radio.protocol,
            now_ms,
            sample_index,
            frame_sequence: None,
            stream_start_ms: None,
            rx_elapsed_ns: rx_elapsed_ns(sample_index, radio.sample_rate_hz),
            rx_timestamp_uncertainty_ns_estimate: rx_timestamp_uncertainty_ns_estimate(
                radio.sample_rate_hz,
                None,
            ),
            receiver: None,
            receiver_site: None,
            center_frequency_hz: radio.center_frequency_hz,
            sample_rate_hz: radio.sample_rate_hz,
            gain_mode: None,
            gain_tenth_db: None,
            bias_t: None,
            device_index: None,
            tuner_name: None,
            stream_id: None,
            chunk_sequence: None,
            chunk_sample_index: None,
            dropped_samples_before: None,
            clipped_sample_ratio: None,
            dc_i_offset: None,
            dc_q_offset: None,
            signal,
            icao: frame.explicit_icao_address().map(|icao| icao.to_string()),
            adsb_type_code: ExtendedSquitter::parse(frame).map(|squitter| squitter.type_code),
            raw: frame.to_hex(),
            downlink_format: frame.downlink_format().bits(),
            bit_len: frame.bit_len(),
            crc_valid: frame.is_crc_valid(),
        }
    }

    /// Parses and validates the record's raw hex frame.
    ///
    /// # Errors
    ///
    /// Returns an error when the record schema version is unsupported, the raw
    /// frame cannot be parsed, or the stored metadata is inconsistent with the
    /// parsed frame.
    pub fn parse_frame(&self) -> Result<Frame, FrameRecordError> {
        self.validate_schema_version()?;
        if !self.protocol.uses_current_modes_decoder() {
            return Err(FrameRecordError::UnsupportedProtocol(self.protocol));
        }
        self.validate_radio_config()?;
        self.validate_numeric_fields()?;

        let frame = Frame::from_hex(&self.raw).map_err(FrameRecordError::InvalidFrame)?;
        self.validate_frame_metadata(&frame)?;

        Ok(frame)
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

    fn validate_radio_config(&self) -> Result<(), FrameRecordError> {
        if self.sample_rate_hz == 0 {
            return Err(FrameRecordError::InvalidField {
                field: "sample_rate_hz",
                reason: "must be greater than zero".to_owned(),
            });
        }

        if self.protocol == Protocol::Adsb1090
            && !(1_080_000_000..=1_100_000_000).contains(&self.center_frequency_hz)
        {
            return Err(FrameRecordError::InvalidField {
                field: "center_frequency_hz",
                reason: "must be near 1090 MHz for adsb1090 records".to_owned(),
            });
        }

        if let Some(gain_mode) = &self.gain_mode {
            match gain_mode.as_str() {
                "auto" if self.gain_tenth_db.is_some() => {
                    return Err(FrameRecordError::InvalidField {
                        field: "gain_tenth_db",
                        reason: "must be absent when gain_mode is auto".to_owned(),
                    });
                }
                "auto" | "manual" => {}
                _ => {
                    return Err(FrameRecordError::InvalidField {
                        field: "gain_mode",
                        reason: "must be auto or manual".to_owned(),
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_numeric_fields(&self) -> Result<(), FrameRecordError> {
        if let Some(site) = &self.receiver_site {
            validate_f64_range("receiver_site.lat", site.lat, -90.0, 90.0)?;
            validate_f64_range("receiver_site.lon", site.lon, -180.0, 180.0)?;
        }

        if let Some(receiver) = &self.receiver
            && receiver.id.trim().is_empty()
        {
            return Err(FrameRecordError::InvalidField {
                field: "receiver.id",
                reason: "must not be empty".to_owned(),
            });
        }

        if let Some(actual_rx_elapsed_ns) = self.rx_elapsed_ns {
            let expected =
                rx_elapsed_ns(self.sample_index, self.sample_rate_hz).ok_or_else(|| {
                    FrameRecordError::InvalidField {
                        field: "rx_elapsed_ns",
                        reason: "cannot be derived from sample_index and sample_rate_hz".to_owned(),
                    }
                })?;
            if actual_rx_elapsed_ns != expected {
                return Err(FrameRecordError::MetadataMismatch {
                    field: "rx_elapsed_ns",
                    expected: expected.to_string(),
                    actual: actual_rx_elapsed_ns.to_string(),
                });
            }
        }

        if let Some(uncertainty_ns) = self.rx_timestamp_uncertainty_ns_estimate {
            let expected_min = rx_timestamp_uncertainty_ns_estimate(
                self.sample_rate_hz,
                self.dropped_samples_before,
            )
            .ok_or_else(|| FrameRecordError::InvalidField {
                field: "rx_timestamp_uncertainty_ns_estimate",
                reason: "cannot be derived from sample_rate_hz".to_owned(),
            })?;
            if uncertainty_ns < expected_min {
                return Err(FrameRecordError::InvalidField {
                    field: "rx_timestamp_uncertainty_ns_estimate",
                    reason: format!("must be at least {expected_min} ns"),
                });
            }
        }

        validate_optional_f64_range("clipped_sample_ratio", self.clipped_sample_ratio, 0.0, 1.0)?;
        validate_optional_f64_range("dc_i_offset", self.dc_i_offset, -127.0, 128.0)?;
        validate_optional_f64_range("dc_q_offset", self.dc_q_offset, -127.0, 128.0)?;

        if let Some(signal) = &self.signal {
            signal.validate()?;
        }

        Ok(())
    }

    fn validate_frame_metadata(&self, frame: &Frame) -> Result<(), FrameRecordError> {
        let expected_df = frame.downlink_format().bits();
        if self.downlink_format != expected_df {
            return Err(FrameRecordError::MetadataMismatch {
                field: "downlink_format",
                expected: expected_df.to_string(),
                actual: self.downlink_format.to_string(),
            });
        }

        let expected_bit_len = frame.bit_len();
        if self.bit_len != expected_bit_len {
            return Err(FrameRecordError::MetadataMismatch {
                field: "bit_len",
                expected: expected_bit_len.to_string(),
                actual: self.bit_len.to_string(),
            });
        }

        let expected_crc_valid = frame.is_crc_valid();
        if self.crc_valid != expected_crc_valid {
            return Err(FrameRecordError::MetadataMismatch {
                field: "crc_valid",
                expected: expected_crc_valid.to_string(),
                actual: self.crc_valid.to_string(),
            });
        }

        let expected_icao = frame.explicit_icao_address().map(|icao| icao.to_string());
        if self.icao.as_ref() != expected_icao.as_ref() && self.icao.is_some() {
            return Err(FrameRecordError::MetadataMismatch {
                field: "icao",
                expected: expected_icao.unwrap_or_else(|| "none".to_owned()),
                actual: self.icao.clone().unwrap_or_default(),
            });
        }

        let expected_type_code = ExtendedSquitter::parse(frame).map(|squitter| squitter.type_code);
        if self.adsb_type_code != expected_type_code && self.adsb_type_code.is_some() {
            return Err(FrameRecordError::MetadataMismatch {
                field: "adsb_type_code",
                expected: expected_type_code
                    .map_or_else(|| "none".to_owned(), |type_code| type_code.to_string()),
                actual: self
                    .adsb_type_code
                    .map_or_else(String::new, |type_code| type_code.to_string()),
            });
        }

        Ok(())
    }
}

impl FrameSignalMetrics {
    fn validate(&self) -> Result<(), FrameRecordError> {
        validate_signal_power("signal.signal_power", self.signal_power)?;
        validate_signal_power("signal.noise_power", self.noise_power)?;
        validate_signal_power("signal.preamble_high_avg", self.preamble_high_avg)?;
        validate_signal_power("signal.preamble_low_avg", self.preamble_low_avg)?;
        validate_signal_power("signal.preamble_delta", self.preamble_delta)?;
        validate_signal_power("signal.bit_margin_min", self.bit_margin_min)?;
        validate_signal_power("signal.bit_margin_mean", self.bit_margin_mean)?;
        if let Some(chunk_noise_power) = self.chunk_noise_power {
            validate_signal_power("signal.chunk_noise_power", chunk_noise_power)?;
        }

        if self.preamble_high_avg < self.preamble_low_avg {
            return Err(FrameRecordError::InvalidField {
                field: "signal.preamble_high_avg",
                reason: "must be greater than or equal to signal.preamble_low_avg".to_owned(),
            });
        }

        let expected_delta = self.preamble_high_avg - self.preamble_low_avg;
        if self.preamble_delta != expected_delta {
            return Err(FrameRecordError::MetadataMismatch {
                field: "signal.preamble_delta",
                expected: expected_delta.to_string(),
                actual: self.preamble_delta.to_string(),
            });
        }

        if self.bit_margin_min > self.bit_margin_mean {
            return Err(FrameRecordError::InvalidField {
                field: "signal.bit_margin_min",
                reason: "must be less than or equal to signal.bit_margin_mean".to_owned(),
            });
        }

        validate_optional_f64_max(
            "signal.signal_dbfs_estimate",
            self.signal_dbfs_estimate,
            0.0,
        )?;
        validate_optional_f64_min("signal.snr_db_estimate", self.snr_db_estimate, 0.0)?;

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct FrameRecordSequenceValidator {
    stream_started: bool,
    stream_id: Option<String>,
    frame_sequence: Option<u64>,
    sample_index: Option<u64>,
    chunk_sequence: Option<u64>,
    chunk_sample_index: Option<u64>,
}

impl FrameRecordSequenceValidator {
    /// Validates a record's sequence fields against records already seen in the
    /// same stream.
    ///
    /// # Errors
    ///
    /// Returns an error when sequence, sample, or chunk indexes regress within
    /// one stream.
    pub fn validate_next(&mut self, record: &FrameRecord) -> Result<(), FrameRecordError> {
        if self.stream_started && self.stream_id != record.stream_id {
            self.reset_for_stream();
        }
        if !self.stream_started {
            self.stream_started = true;
            self.stream_id.clone_from(&record.stream_id);
        }

        validate_strictly_increases(
            "frame_sequence",
            &mut self.frame_sequence,
            record.frame_sequence,
        )?;
        validate_does_not_regress(
            "sample_index",
            &mut self.sample_index,
            Some(record.sample_index),
        )?;
        validate_does_not_regress(
            "chunk_sequence",
            &mut self.chunk_sequence,
            record.chunk_sequence,
        )?;
        validate_does_not_regress(
            "chunk_sample_index",
            &mut self.chunk_sample_index,
            record.chunk_sample_index,
        )?;

        Ok(())
    }

    fn reset_for_stream(&mut self) {
        self.stream_started = false;
        self.stream_id = None;
        self.frame_sequence = None;
        self.sample_index = None;
        self.chunk_sequence = None;
        self.chunk_sample_index = None;
    }
}

/// Validates sequence fields across a batch of frame records.
///
/// # Errors
///
/// Returns an error when sequence, sample, or chunk indexes regress within a
/// stream.
pub fn validate_frame_record_sequence(records: &[FrameRecord]) -> Result<(), FrameRecordError> {
    let mut validator = FrameRecordSequenceValidator::default();
    for record in records {
        validator.validate_next(record)?;
    }
    Ok(())
}

fn validate_strictly_increases(
    field: &'static str,
    previous: &mut Option<u64>,
    actual: Option<u64>,
) -> Result<(), FrameRecordError> {
    let Some(actual) = actual else {
        return Ok(());
    };

    if let Some(previous_value) = *previous
        && actual <= previous_value
    {
        return Err(FrameRecordError::SequenceRegression {
            field,
            previous: previous_value,
            actual,
        });
    }

    *previous = Some(actual);
    Ok(())
}

fn validate_does_not_regress(
    field: &'static str,
    previous: &mut Option<u64>,
    actual: Option<u64>,
) -> Result<(), FrameRecordError> {
    let Some(actual) = actual else {
        return Ok(());
    };

    if let Some(previous_value) = *previous
        && actual < previous_value
    {
        return Err(FrameRecordError::SequenceRegression {
            field,
            previous: previous_value,
            actual,
        });
    }

    *previous = Some(actual);
    Ok(())
}

impl From<DecodedFrameSignal> for FrameSignalMetrics {
    fn from(signal: DecodedFrameSignal) -> Self {
        Self {
            signal_power: signal.signal_power,
            noise_power: signal.noise_power,
            signal_dbfs_estimate: signal_dbfs_estimate(signal.signal_power),
            snr_db_estimate: snr_db_estimate(signal.signal_power, signal.noise_power),
            chunk_noise_power: None,
            beast_signal_level: beast_signal_level(signal.signal_power),
            preamble_high_avg: signal.preamble_high_avg,
            preamble_low_avg: signal.preamble_low_avg,
            preamble_delta: signal.preamble_delta,
            bit_margin_min: signal.bit_margin_min,
            bit_margin_mean: signal.bit_margin_mean,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IqChunkMetrics {
    pub clipped_sample_ratio: f64,
    pub dc_i_offset: f64,
    pub dc_q_offset: f64,
    pub noise_power: u32,
}

#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn iq_chunk_metrics(iq: &[u8]) -> Option<IqChunkMetrics> {
    let sample_count = iq.len() / 2;
    if sample_count == 0 || !iq.len().is_multiple_of(2) {
        return None;
    }

    let mut clipped_samples = 0_usize;
    let mut i_sum = 0_u64;
    let mut q_sum = 0_u64;
    let mut magnitudes = Vec::with_capacity(sample_count);

    for sample in iq.chunks_exact(2) {
        let i = sample[0];
        let q = sample[1];
        if i == 0 || i == u8::MAX || q == 0 || q == u8::MAX {
            clipped_samples += 1;
        }
        i_sum += u64::from(i);
        q_sum += u64::from(q);
        magnitudes.push(centered_power(i, q));
    }

    magnitudes.sort_unstable();
    let floor_count = sample_count.div_ceil(4).max(1);
    let floor_sum = magnitudes
        .iter()
        .take(floor_count)
        .map(|magnitude| u64::from(*magnitude))
        .sum::<u64>();
    let floor_count = u64::try_from(floor_count).ok()?;

    Some(IqChunkMetrics {
        clipped_sample_ratio: clipped_samples as f64 / sample_count as f64,
        dc_i_offset: (i_sum as f64 / sample_count as f64) - 127.0,
        dc_q_offset: (q_sum as f64 / sample_count as f64) - 127.0,
        noise_power: average_u32(floor_sum, floor_count),
    })
}

fn centered_power(i: u8, q: u8) -> u32 {
    let i = centered_magnitude(i);
    let q = centered_magnitude(q);
    i * i + q * q
}

fn centered_magnitude(sample: u8) -> u32 {
    let centered = i16::from(sample) - 127;
    u32::from(centered.unsigned_abs())
}

fn average_u32(sum: u64, count: u64) -> u32 {
    u32::try_from(sum / count).expect("average magnitude fits u32")
}

fn beast_signal_level(signal_power: u32) -> u8 {
    const RTL_U8_FULL_SCALE_MAGNITUDE_POWER: u64 = 2 * 128 * 128;
    const BEAST_SIGNAL_SCALE: u64 = 255;

    if signal_power == 0 {
        return 0;
    }

    let scaled_power = u64::from(signal_power)
        .saturating_mul(BEAST_SIGNAL_SCALE)
        .saturating_mul(BEAST_SIGNAL_SCALE)
        / RTL_U8_FULL_SCALE_MAGNITUDE_POWER;
    let level = integer_sqrt(scaled_power);
    u8::try_from(level.min(255)).expect("BEAST signal level is clamped")
}

fn integer_sqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }

    let mut estimate = value;
    let mut next = u64::midpoint(estimate, value / estimate);
    while next < estimate {
        estimate = next;
        next = u64::midpoint(estimate, value / estimate);
    }
    estimate
}

fn signal_dbfs_estimate(signal_power: u32) -> Option<f64> {
    const RTL_U8_FULL_SCALE_MAGNITUDE_POWER: f64 = 2.0 * 128.0 * 128.0;

    if signal_power == 0 {
        return None;
    }

    Some(10.0 * (f64::from(signal_power) / RTL_U8_FULL_SCALE_MAGNITUDE_POWER).log10())
}

fn snr_db_estimate(signal_power: u32, noise_power: u32) -> Option<f64> {
    if noise_power == 0 || signal_power <= noise_power {
        return None;
    }

    Some(10.0 * (f64::from(signal_power - noise_power) / f64::from(noise_power)).log10())
}

fn validate_signal_power(field: &'static str, value: u32) -> Result<(), FrameRecordError> {
    if value <= RTL_U8_FULL_SCALE_MAGNITUDE_POWER {
        Ok(())
    } else {
        Err(FrameRecordError::InvalidField {
            field,
            reason: format!("must be <= {RTL_U8_FULL_SCALE_MAGNITUDE_POWER}"),
        })
    }
}

fn validate_f64_range(
    field: &'static str,
    value: f64,
    min: f64,
    max: f64,
) -> Result<(), FrameRecordError> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(FrameRecordError::InvalidField {
            field,
            reason: format!("must be finite and between {min} and {max}"),
        })
    }
}

fn validate_optional_f64_range(
    field: &'static str,
    value: Option<f64>,
    min: f64,
    max: f64,
) -> Result<(), FrameRecordError> {
    if let Some(value) = value {
        validate_f64_range(field, value, min, max)?;
    }
    Ok(())
}

fn validate_optional_f64_min(
    field: &'static str,
    value: Option<f64>,
    min: f64,
) -> Result<(), FrameRecordError> {
    if let Some(value) = value
        && (!value.is_finite() || value < min)
    {
        return Err(FrameRecordError::InvalidField {
            field,
            reason: format!("must be finite and >= {min}"),
        });
    }
    Ok(())
}

fn validate_optional_f64_max(
    field: &'static str,
    value: Option<f64>,
    max: f64,
) -> Result<(), FrameRecordError> {
    if let Some(value) = value
        && (!value.is_finite() || value > max)
    {
        return Err(FrameRecordError::InvalidField {
            field,
            reason: format!("must be finite and <= {max}"),
        });
    }
    Ok(())
}

fn rx_elapsed_ns(sample_index: u64, sample_rate_hz: u32) -> Option<u64> {
    if sample_rate_hz == 0 {
        return None;
    }

    let elapsed =
        u128::from(sample_index).saturating_mul(1_000_000_000) / u128::from(sample_rate_hz);
    u64::try_from(elapsed).ok()
}

fn rx_timestamp_uncertainty_ns_estimate(
    sample_rate_hz: u32,
    dropped_samples_before: Option<u64>,
) -> Option<u64> {
    if sample_rate_hz == 0 {
        return None;
    }

    let sample_period_ns = u64::from(sample_rate_hz)
        .saturating_sub(1)
        .saturating_add(1_000_000_000)
        / u64::from(sample_rate_hz);
    let drop_uncertainty =
        rx_elapsed_ns(dropped_samples_before.unwrap_or(0), sample_rate_hz).unwrap_or(0);

    Some(sample_period_ns.saturating_add(drop_uncertainty))
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FrameRecordError {
    UnsupportedSchemaVersion {
        expected: u32,
        actual: u32,
    },
    UnsupportedBatchSchemaVersion {
        expected: u32,
        actual: u32,
    },
    UnsupportedProtocol(Protocol),
    ProtocolMismatch {
        expected: Protocol,
        actual: Protocol,
    },
    InvalidField {
        field: &'static str,
        reason: String,
    },
    MetadataMismatch {
        field: &'static str,
        expected: String,
        actual: String,
    },
    SequenceRegression {
        field: &'static str,
        previous: u64,
        actual: u64,
    },
    SampleIndexOverflow(usize),
    InvalidFrame(FrameError),
}

impl fmt::Display for FrameRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { expected, actual } => write!(
                formatter,
                "unsupported frame record schema_version {actual}; expected {expected}"
            ),
            Self::UnsupportedBatchSchemaVersion { expected, actual } => write!(
                formatter,
                "unsupported frame record batch schema_version {actual}; expected {expected}"
            ),
            Self::UnsupportedProtocol(protocol) => {
                write!(formatter, "unsupported frame record protocol {protocol}")
            }
            Self::ProtocolMismatch { expected, actual } => write!(
                formatter,
                "frame record protocol mismatch: expected {expected}, got {actual}"
            ),
            Self::InvalidField { field, reason } => {
                write!(formatter, "invalid frame record {field}: {reason}")
            }
            Self::MetadataMismatch {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "frame record {field} mismatch: expected {expected}, got {actual}"
            ),
            Self::SequenceRegression {
                field,
                previous,
                actual,
            } => write!(
                formatter,
                "frame record {field} regressed: previous {previous}, got {actual}"
            ),
            Self::SampleIndexOverflow(sample_index) => {
                write!(
                    formatter,
                    "frame sample index overflows u64: {sample_index}"
                )
            }
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
    let stream_start_ms = first_record.now_ms;
    let mut frame_sequence = 0_u64;

    for (chunk_sequence, record) in records.iter().enumerate() {
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

        let chunk_metrics = iq_chunk_metrics(&record.iq_bytes);
        for decoded in decoder.decode_chunk(&record.iq_bytes) {
            let mut frame_record = FrameRecord::from_decoded_frame(radio, record.now_ms, &decoded)
                .map_err(IqCaptureRecordError::FrameRecord)?;
            frame_record.frame_sequence = Some(frame_sequence);
            frame_record.stream_start_ms = Some(stream_start_ms);
            frame_record.chunk_sequence = Some(
                u64::try_from(chunk_sequence)
                    .map_err(|_| IqCaptureRecordError::SampleIndexOverflow(u64::MAX))?,
            );
            frame_record.chunk_sample_index = Some(record.sample_index);
            if let Some(metrics) = chunk_metrics {
                frame_record.apply_iq_chunk_metrics(metrics);
            }
            frames.push(frame_record);
            frame_sequence = frame_sequence.saturating_add(1);
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
    pub fn decode_chunk(&mut self, data: &[u8]) -> Vec<DecodedFrame> {
        let tail_samples = self.tail.len() / 2;
        let combined_base_sample = self.stream_sample_offset.saturating_sub(tail_samples);
        let mut combined = Vec::with_capacity(self.tail.len() + data.len());
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(data);

        let frames = decode_frames_from_iq(&combined, DemodConfig::default())
            .into_iter()
            .filter(|decoded| decoded.sample_index >= tail_samples)
            .map(|mut decoded| {
                decoded.sample_index += combined_base_sample;
                decoded
            })
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
        assert_eq!(frames[0].frame_sequence, Some(0));
        assert_eq!(frames[0].stream_start_ms, Some(100));
        assert_eq!(frames[0].rx_elapsed_ns, Some(27_500));
        assert_eq!(frames[0].rx_timestamp_uncertainty_ns_estimate, Some(500));
        assert_eq!(frames[0].center_frequency_hz, radio.center_frequency_hz);
        assert_eq!(frames[0].sample_rate_hz, radio.sample_rate_hz);
        assert_eq!(frames[0].chunk_sequence, Some(0));
        assert_eq!(frames[0].chunk_sample_index, Some(50));
        assert_eq!(frames[0].clipped_sample_ratio, Some(0.0));
        assert!(frames[0].dc_i_offset.expect("dc i offset exists") > 0.0);
        assert_eq!(frames[0].dc_q_offset, Some(0.0));
        assert_eq!(frames[0].raw, frame.to_hex());
        assert_eq!(frames[0].icao.as_deref(), Some("4840D6"));
        assert_eq!(frames[0].adsb_type_code, Some(4));
        let signal = frames[0].signal.as_ref().expect("signal metrics exist");
        assert_eq!(signal.signal_power, HIGH_MAGNITUDE);
        assert_eq!(signal.noise_power, LOW_MAGNITUDE);
        assert_eq!(signal.chunk_noise_power, Some(LOW_MAGNITUDE));
        assert_eq!(signal.beast_signal_level, 90);
        assert_eq!(signal.preamble_delta, HIGH_MAGNITUDE - LOW_MAGNITUDE);
        assert_eq!(signal.bit_margin_min, HIGH_MAGNITUDE - LOW_MAGNITUDE);
        assert!(signal.signal_dbfs_estimate.is_some());
        assert!(signal.snr_db_estimate.is_some());
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
