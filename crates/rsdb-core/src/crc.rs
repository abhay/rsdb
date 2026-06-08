use alloc::vec::Vec;

use crate::frame::Frame;

const MODES_GENERATOR: u32 = 0x01ff_f409;
const MODES_GENERATOR_BITS: usize = 25;
const MODES_PARITY_BITS: usize = 24;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ModeSChecksum {
    pub remainder: u32,
}

impl ModeSChecksum {
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.remainder == 0
    }
}

#[must_use]
pub fn crc24_modes(frame: &Frame) -> ModeSChecksum {
    ModeSChecksum {
        remainder: crc24_modes_bytes(frame.bytes(), frame.bit_len()),
    }
}

#[must_use]
pub(crate) fn crc24_modes_bytes(bytes: &[u8], bit_len: usize) -> u32 {
    assert!(
        bit_len == 56 || bit_len == 112,
        "Mode S frames are 56 or 112 bits"
    );
    assert!(
        bytes.len() * 8 >= bit_len,
        "not enough bytes for bit length"
    );

    let mut bits = bytes_to_bits(bytes, bit_len);
    let data_bits = bit_len - MODES_PARITY_BITS;

    for bit_offset in 0..data_bits {
        if !bits[bit_offset] {
            continue;
        }

        for generator_offset in 0..MODES_GENERATOR_BITS {
            if generator_bit(generator_offset) {
                bits[bit_offset + generator_offset] = !bits[bit_offset + generator_offset];
            }
        }
    }

    bits[bit_len - MODES_PARITY_BITS..]
        .iter()
        .fold(0, |acc, bit| (acc << 1) | u32::from(*bit))
}

fn bytes_to_bits(bytes: &[u8], bit_len: usize) -> Vec<bool> {
    (0..bit_len)
        .map(|bit_index| {
            let byte = bytes[bit_index / 8];
            let shift = 7 - (bit_index % 8);
            ((byte >> shift) & 1) == 1
        })
        .collect()
}

fn generator_bit(offset: usize) -> bool {
    let shift = MODES_GENERATOR_BITS - 1 - offset;
    ((MODES_GENERATOR >> shift) & 1) == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn validates_known_extended_squitter_frame() {
        let frame = Frame::from_hex("8D4840D6202CC371C32CE0576098").unwrap();

        assert_eq!(crc24_modes(&frame), ModeSChecksum { remainder: 0 });
        assert!(crc24_modes(&frame).is_valid());
    }

    #[test]
    fn rejects_frame_with_changed_payload_bit() {
        let frame = Frame::from_hex("8D4840D6202CC371C32CE0576099").unwrap();

        assert_ne!(crc24_modes(&frame).remainder, 0);
    }
}
