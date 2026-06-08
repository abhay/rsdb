use alloc::vec::Vec;

use crate::Frame;

/// Sample rate used by the first-pass Mode S demodulator.
pub const MODES_SAMPLE_RATE_HZ: u32 = 2_000_000;

const PREAMBLE_SAMPLES: usize = 16;
const SAMPLES_PER_BIT: usize = 2;
const LONG_FRAME_BITS: usize = 112;
const PREAMBLE_HIGH_SAMPLES: [usize; 4] = [0, 2, 7, 9];
const PREAMBLE_LOW_SAMPLES: [usize; 12] = [1, 3, 4, 5, 6, 8, 10, 11, 12, 13, 14, 15];

/// Number of 2.0 MS/s samples occupied by an ADS-B preamble and long frame.
pub const LONG_FRAME_TOTAL_SAMPLES: usize = PREAMBLE_SAMPLES + LONG_FRAME_BITS * SAMPLES_PER_BIT;

/// Tunables for the 2.0 MS/s Mode S demodulator.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DemodConfig {
    /// Minimum average magnitude gap between preamble pulse and quiet samples.
    pub min_preamble_delta: u32,
    /// Require a zero Mode S CRC remainder before returning a frame.
    pub require_valid_crc: bool,
}

impl Default for DemodConfig {
    fn default() -> Self {
        Self {
            min_preamble_delta: 64,
            require_valid_crc: true,
        }
    }
}

/// A Mode S frame decoded from a sample stream.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DecodedFrame {
    /// Starting sample index of the detected Mode S preamble.
    pub sample_index: usize,
    /// Decoded Mode S frame.
    pub frame: Frame,
}

/// Converts unsigned interleaved RTL-SDR I/Q bytes into magnitude-squared samples.
#[must_use]
pub fn unsigned_iq_to_magnitudes(iq: &[u8]) -> Vec<u32> {
    iq.chunks_exact(2)
        .map(|sample| {
            let i = centered_magnitude(sample[0]);
            let q = centered_magnitude(sample[1]);
            i * i + q * q
        })
        .collect()
}

/// Decodes Mode S long frames from unsigned interleaved RTL-SDR I/Q bytes.
#[must_use]
pub fn decode_frames_from_iq(iq: &[u8], config: DemodConfig) -> Vec<DecodedFrame> {
    let magnitudes = unsigned_iq_to_magnitudes(iq);
    decode_frames_from_magnitudes(&magnitudes, config)
}

/// Decodes Mode S long frames from 2.0 MS/s magnitude-squared samples.
#[must_use]
pub fn decode_frames_from_magnitudes(magnitudes: &[u32], config: DemodConfig) -> Vec<DecodedFrame> {
    if magnitudes.len() < LONG_FRAME_TOTAL_SAMPLES {
        return Vec::new();
    }

    let mut frames = Vec::new();
    let mut sample_index = 0;
    let last_start = magnitudes.len() - LONG_FRAME_TOTAL_SAMPLES;

    while sample_index <= last_start {
        if !has_modes_preamble(magnitudes, sample_index, config.min_preamble_delta) {
            sample_index += 1;
            continue;
        }

        if let Some(frame) = decode_long_frame_at(magnitudes, sample_index, config) {
            frames.push(DecodedFrame {
                sample_index,
                frame,
            });
            sample_index += LONG_FRAME_TOTAL_SAMPLES;
        } else {
            sample_index += 1;
        }
    }

    frames
}

fn centered_magnitude(sample: u8) -> u32 {
    let centered = i16::from(sample) - 127;
    let magnitude = centered.unsigned_abs();
    u32::from(magnitude)
}

fn has_modes_preamble(magnitudes: &[u32], offset: usize, min_delta: u32) -> bool {
    let high_sum = PREAMBLE_HIGH_SAMPLES
        .iter()
        .map(|sample| magnitudes[offset + sample])
        .sum::<u32>();
    let low_sum = PREAMBLE_LOW_SAMPLES
        .iter()
        .map(|sample| magnitudes[offset + sample])
        .sum::<u32>();

    let high_avg = high_sum / u32::try_from(PREAMBLE_HIGH_SAMPLES.len()).expect("nonzero length");
    let low_avg = low_sum / u32::try_from(PREAMBLE_LOW_SAMPLES.len()).expect("nonzero length");

    if high_avg <= low_avg.saturating_add(min_delta) {
        return false;
    }

    let pulse_floor = low_avg.saturating_add((high_avg - low_avg) / 2);
    let quiet_ceiling = low_avg.saturating_add((high_avg - low_avg) / 3);

    PREAMBLE_HIGH_SAMPLES
        .iter()
        .all(|sample| magnitudes[offset + sample] >= pulse_floor)
        && PREAMBLE_LOW_SAMPLES
            .iter()
            .all(|sample| magnitudes[offset + sample] <= quiet_ceiling)
}

fn decode_long_frame_at(
    magnitudes: &[u32],
    preamble_index: usize,
    config: DemodConfig,
) -> Option<Frame> {
    let mut bytes = [0_u8; FrameLengthBytes::LONG];
    let bit_start = preamble_index + PREAMBLE_SAMPLES;

    for bit_index in 0..LONG_FRAME_BITS {
        let sample_index = bit_start + bit_index * SAMPLES_PER_BIT;
        let early = magnitudes[sample_index];
        let late = magnitudes[sample_index + 1];
        let bit = u8::from(early > late);
        let byte_index = bit_index / 8;
        let bit_shift = 7 - bit_index % 8;
        bytes[byte_index] |= bit << bit_shift;
    }

    let frame = Frame::from_bytes(&bytes).ok()?;

    if config.require_valid_crc && !frame.is_crc_valid() {
        return None;
    }

    Some(frame)
}

struct FrameLengthBytes;

impl FrameLengthBytes {
    const LONG: usize = 14;
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const LOW_MAGNITUDE: u32 = 16;
    const HIGH_MAGNITUDE: u32 = 4_096;

    #[test]
    fn converts_unsigned_iq_to_magnitudes() {
        let magnitudes = unsigned_iq_to_magnitudes(&[127, 127, 130, 127, 127, 131]);

        assert_eq!(magnitudes, vec![0, 9, 16]);
    }

    #[test]
    fn decodes_synthetic_long_frame_from_magnitudes() {
        let expected = Frame::from_hex("8D4840D6202CC371C32CE0576098").unwrap();
        let samples = synthetic_magnitudes_for_frame(&expected, 5);
        let frames = decode_frames_from_magnitudes(&samples, DemodConfig::default());

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].sample_index, 5);
        assert_eq!(frames[0].frame, expected);
    }

    #[test]
    fn rejects_synthetic_frame_with_invalid_crc() {
        let expected = Frame::from_hex("8D4840D6202CC371C32CE0576099").unwrap();
        let samples = synthetic_magnitudes_for_frame(&expected, 0);
        let frames = decode_frames_from_magnitudes(&samples, DemodConfig::default());

        assert!(frames.is_empty());
    }

    #[test]
    fn can_return_invalid_crc_frames_when_configured() {
        let expected = Frame::from_hex("8D4840D6202CC371C32CE0576099").unwrap();
        let samples = synthetic_magnitudes_for_frame(&expected, 0);
        let frames = decode_frames_from_magnitudes(
            &samples,
            DemodConfig {
                require_valid_crc: false,
                ..DemodConfig::default()
            },
        );

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame, expected);
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
}
