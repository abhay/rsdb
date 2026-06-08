#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod adsb;
pub mod crc;
pub mod demod;
pub mod frame;

pub use adsb::{
    AdsbMessage, AirbornePosition, AirborneVelocity, AircraftIdentification,
    AircraftOperationalStatus, AircraftStatus, CprFormat, EmergencyState, ExtendedSquitter,
    SpeedType, TargetStateAndStatus, VerticalRateSource,
};
pub use crc::{ModeSChecksum, crc24_modes};
pub use demod::{
    DecodedFrame, DemodConfig, LONG_FRAME_TOTAL_SAMPLES, MODES_SAMPLE_RATE_HZ,
    decode_frames_from_iq, decode_frames_from_magnitudes, unsigned_iq_to_magnitudes,
};
pub use frame::{DownlinkFormat, Frame, FrameError, FrameLength, IcaoAddress};
