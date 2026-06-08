use alloc::{borrow::ToOwned, string::String};

use crate::{DownlinkFormat, Frame, IcaoAddress};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedSquitter {
    pub icao: IcaoAddress,
    pub capability: u8,
    pub type_code: u8,
    pub message: AdsbMessage,
}

impl ExtendedSquitter {
    #[must_use]
    pub fn parse(frame: &Frame) -> Option<Self> {
        if frame.frame_length().byte_len() != crate::FrameLength::LONG_BYTES {
            return None;
        }

        match frame.downlink_format() {
            DownlinkFormat::ExtendedSquitter | DownlinkFormat::ExtendedSquitterNonTransponder => {}
            _ => return None,
        }

        let me = extended_squitter_payload(frame);
        let type_code = me[0] >> 3;
        let message = AdsbMessage::parse(type_code, me);

        Some(Self {
            icao: frame.explicit_icao_address()?,
            capability: frame.first_field(),
            type_code,
            message,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdsbMessage {
    AircraftIdentification(AircraftIdentification),
    AirbornePosition(AirbornePosition),
    AirborneVelocity(AirborneVelocity),
    AircraftStatus(AircraftStatus),
    TargetStateAndStatus(TargetStateAndStatus),
    AircraftOperationalStatus(AircraftOperationalStatus),
    Unknown { raw_me: [u8; 7] },
}

impl AdsbMessage {
    fn parse(type_code: u8, me: [u8; 7]) -> Self {
        if let Some(identification) = AircraftIdentification::parse(type_code, me) {
            return Self::AircraftIdentification(identification);
        }

        if let Some(position) = AirbornePosition::parse(type_code, me) {
            return Self::AirbornePosition(position);
        }

        if let Some(velocity) = AirborneVelocity::parse(type_code, me) {
            return Self::AirborneVelocity(velocity);
        }

        match type_code {
            28 => Self::AircraftStatus(AircraftStatus::parse(me)),
            29 => Self::TargetStateAndStatus(TargetStateAndStatus::parse(me)),
            31 => Self::AircraftOperationalStatus(AircraftOperationalStatus::parse(me)),
            _ => Self::Unknown { raw_me: me },
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AircraftIdentification {
    pub category: u8,
    pub callsign: String,
}

impl AircraftIdentification {
    fn parse(type_code: u8, me: [u8; 7]) -> Option<Self> {
        if !(1..=4).contains(&type_code) {
            return None;
        }

        Some(Self {
            category: me[0] & 0b0000_0111,
            callsign: decode_callsign(me)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CprFormat {
    Even,
    Odd,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct AirbornePosition {
    pub surveillance_status: u8,
    pub nic_supplement_b: bool,
    pub altitude_baro_ft: Option<i32>,
    pub altitude_geometric_ft: Option<i32>,
    pub time_flag: bool,
    pub cpr_format: CprFormat,
    pub cpr_lat: u32,
    pub cpr_lon: u32,
}

impl AirbornePosition {
    fn parse(type_code: u8, me: [u8; 7]) -> Option<Self> {
        if !(9..=18).contains(&type_code) && !(20..=22).contains(&type_code) {
            return None;
        }

        let altitude_ft = decode_ac12_altitude(get_bits_u16(me, 8, 12));
        let is_baro_altitude = (9..=18).contains(&type_code);

        Some(Self {
            surveillance_status: get_bits_u8(me, 5, 2),
            nic_supplement_b: get_bit(me, 7),
            altitude_baro_ft: is_baro_altitude.then_some(altitude_ft).flatten(),
            altitude_geometric_ft: (!is_baro_altitude).then_some(altitude_ft).flatten(),
            time_flag: get_bit(me, 20),
            cpr_format: if get_bit(me, 21) {
                CprFormat::Odd
            } else {
                CprFormat::Even
            },
            cpr_lat: get_bits_u32(me, 22, 17),
            cpr_lon: get_bits_u32(me, 39, 17),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedType {
    GroundSpeed,
    Airspeed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerticalRateSource {
    Barometric,
    Geometric,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AirborneVelocity {
    pub subtype: u8,
    pub speed_type: SpeedType,
    pub ground_speed_kt: Option<f64>,
    pub airspeed_kt: Option<f64>,
    pub track_deg: Option<f64>,
    pub heading_deg: Option<f64>,
    pub vertical_rate_source: VerticalRateSource,
    pub vertical_rate_fpm: Option<i32>,
}

impl AirborneVelocity {
    fn parse(type_code: u8, me: [u8; 7]) -> Option<Self> {
        if type_code != 19 {
            return None;
        }

        let subtype = get_bits_u8(me, 5, 3);
        let vertical_rate_source = if get_bit(me, 35) {
            VerticalRateSource::Geometric
        } else {
            VerticalRateSource::Barometric
        };
        let vertical_rate_fpm = decode_vertical_rate(get_bit(me, 36), get_bits_u16(me, 37, 9));

        match subtype {
            1 | 2 => {
                let scale = if subtype == 2 { 4 } else { 1 };
                let ew_velocity =
                    decode_signed_velocity(get_bit(me, 13), get_bits_u16(me, 14, 10), scale);
                let ns_velocity =
                    decode_signed_velocity(get_bit(me, 24), get_bits_u16(me, 25, 10), scale);
                let (ground_speed_kt, track_deg) = match (ew_velocity, ns_velocity) {
                    (Some(east_west_kt), Some(north_south_kt)) => {
                        let speed = f64::from(rounded_hypot(east_west_kt, north_south_kt));
                        let track = normalize_degrees(atan2_degrees(
                            f64::from(east_west_kt),
                            f64::from(north_south_kt),
                        ));
                        (Some(speed), Some(track))
                    }
                    _ => (None, None),
                };

                Some(Self {
                    subtype,
                    speed_type: SpeedType::GroundSpeed,
                    ground_speed_kt,
                    airspeed_kt: None,
                    track_deg,
                    heading_deg: None,
                    vertical_rate_source,
                    vertical_rate_fpm,
                })
            }
            3 | 4 => {
                let heading_deg =
                    get_bit(me, 13).then(|| f64::from(get_bits_u16(me, 14, 10)) * 360.0 / 1024.0);
                let scale = if subtype == 4 { 4.0 } else { 1.0 };
                let airspeed = decode_unsigned_velocity(get_bits_u16(me, 25, 10), scale);

                Some(Self {
                    subtype,
                    speed_type: SpeedType::Airspeed,
                    ground_speed_kt: None,
                    airspeed_kt: airspeed,
                    track_deg: None,
                    heading_deg,
                    vertical_rate_source,
                    vertical_rate_fpm,
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmergencyState {
    NoEmergency,
    GeneralEmergency,
    LifeguardMedical,
    MinimumFuel,
    NoCommunications,
    UnlawfulInterference,
    DownedAircraft,
    Reserved,
}

impl EmergencyState {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::NoEmergency,
            1 => Self::GeneralEmergency,
            2 => Self::LifeguardMedical,
            3 => Self::MinimumFuel,
            4 => Self::NoCommunications,
            5 => Self::UnlawfulInterference,
            6 => Self::DownedAircraft,
            _ => Self::Reserved,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct AircraftStatus {
    pub subtype: u8,
    pub emergency_state: Option<EmergencyState>,
    pub emergency_state_code: Option<u8>,
    pub mode_a_identity_code: Option<u16>,
    pub raw_me: [u8; 7],
}

impl AircraftStatus {
    fn parse(me: [u8; 7]) -> Self {
        let subtype = get_bits_u8(me, 5, 3);
        let emergency_state_code = (subtype == 1).then(|| get_bits_u8(me, 8, 3));

        Self {
            subtype,
            emergency_state: emergency_state_code.map(EmergencyState::from_code),
            emergency_state_code,
            mode_a_identity_code: (subtype == 1).then(|| get_bits_u16(me, 11, 13)),
            raw_me: me,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct TargetStateAndStatus {
    pub subtype: u8,
    pub raw_me: [u8; 7],
}

impl TargetStateAndStatus {
    fn parse(me: [u8; 7]) -> Self {
        Self {
            subtype: get_bits_u8(me, 5, 2),
            raw_me: me,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct AircraftOperationalStatus {
    pub subtype: u8,
    pub capability_class_code: u16,
    pub operational_mode_code: u16,
    pub adsb_version: u8,
    pub nic_supplement_a: bool,
    pub nac_p: u8,
    pub geometric_vertical_accuracy: u8,
    pub source_integrity_level: u8,
    pub baro_altitude_integrity: bool,
    pub horizontal_reference_direction: bool,
    pub sil_supplement: bool,
    pub raw_me: [u8; 7],
}

impl AircraftOperationalStatus {
    fn parse(me: [u8; 7]) -> Self {
        Self {
            subtype: get_bits_u8(me, 5, 3),
            capability_class_code: get_bits_u16(me, 8, 16),
            operational_mode_code: get_bits_u16(me, 24, 16),
            adsb_version: get_bits_u8(me, 40, 3),
            nic_supplement_a: get_bit(me, 43),
            nac_p: get_bits_u8(me, 44, 4),
            geometric_vertical_accuracy: get_bits_u8(me, 48, 2),
            source_integrity_level: get_bits_u8(me, 50, 2),
            baro_altitude_integrity: get_bit(me, 52),
            horizontal_reference_direction: get_bit(me, 53),
            sil_supplement: get_bit(me, 54),
            raw_me: me,
        }
    }
}

fn extended_squitter_payload(frame: &Frame) -> [u8; 7] {
    let mut payload = [0; 7];
    payload.copy_from_slice(&frame.bytes()[4..11]);
    payload
}

fn decode_callsign(me: [u8; 7]) -> Option<String> {
    let mut callsign = String::with_capacity(8);
    let mut accumulator = u64::from_be_bytes([0, 0, me[1], me[2], me[3], me[4], me[5], me[6]]);

    for _ in 0..8 {
        let symbol = ((accumulator >> 42) & 0b11_1111) as u8;
        callsign.push(callsign_char(symbol)?);
        accumulator <<= 6;
    }

    Some(callsign.trim_end().to_owned())
}

fn callsign_char(symbol: u8) -> Option<char> {
    match symbol {
        1..=26 => Some(char::from(b'A' + symbol - 1)),
        32 => Some(' '),
        48..=57 => Some(char::from(b'0' + symbol - 48)),
        _ => None,
    }
}

fn decode_ac12_altitude(ac12: u16) -> Option<i32> {
    let q_bit_set = (ac12 & 0x0010) != 0;

    if !q_bit_set {
        return None;
    }

    let n = ((ac12 & 0x0fe0) >> 1) | (ac12 & 0x000f);
    Some(i32::from(n) * 25 - 1_000)
}

fn decode_signed_velocity(sign_bit: bool, encoded: u16, scale: i32) -> Option<i32> {
    if encoded == 0 {
        return None;
    }

    let velocity = i32::from(encoded - 1) * scale;
    Some(if sign_bit { -velocity } else { velocity })
}

fn decode_unsigned_velocity(encoded: u16, scale: f64) -> Option<f64> {
    if encoded == 0 {
        return None;
    }

    Some(f64::from(encoded - 1) * scale)
}

fn decode_vertical_rate(sign_bit: bool, encoded: u16) -> Option<i32> {
    if encoded == 0 {
        return None;
    }

    let rate = i32::from(encoded - 1) * 64;
    Some(if sign_bit { -rate } else { rate })
}

fn rounded_hypot(a: i32, b: i32) -> u32 {
    let a = u64::from(a.unsigned_abs());
    let b = u64::from(b.unsigned_abs());
    let sum = a * a + b * b;
    let floor = integer_sqrt(sum);
    let next = floor + 1;

    let rounded = if sum - floor * floor >= next * next - sum {
        next
    } else {
        floor
    };
    u32::try_from(rounded).expect("ADS-B velocity magnitude fits in u32")
}

fn integer_sqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }

    let mut low = 1;
    let mut high = value / 2 + 1;
    let mut answer = 1;

    while low <= high {
        let mid = low + (high - low) / 2;
        let square = mid * mid;
        if square == value {
            return mid;
        }
        if square < value {
            answer = mid;
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }

    answer
}

fn atan2_degrees(y: f64, x: f64) -> f64 {
    const PI: f64 = core::f64::consts::PI;
    const FRAC_PI_4: f64 = PI / 4.0;
    const THREE_FRAC_PI_4: f64 = 3.0 * PI / 4.0;
    const RAD_TO_DEG: f64 = 180.0 / PI;

    if x == 0.0 && y == 0.0 {
        return 0.0;
    }

    let abs_y = abs_f64(y) + f64::EPSILON;
    let angle = if x < 0.0 {
        let ratio = (x + abs_y) / (abs_y - x);
        THREE_FRAC_PI_4 + (0.1963 * ratio * ratio - 0.9817) * ratio
    } else {
        let ratio = (x - abs_y) / (x + abs_y);
        FRAC_PI_4 + (0.1963 * ratio * ratio - 0.9817) * ratio
    };

    if y < 0.0 {
        -angle * RAD_TO_DEG
    } else {
        angle * RAD_TO_DEG
    }
}

fn abs_f64(value: f64) -> f64 {
    if value < 0.0 { -value } else { value }
}

fn normalize_degrees(degrees: f64) -> f64 {
    let normalized = degrees % 360.0;
    if normalized < 0.0 {
        normalized + 360.0
    } else {
        normalized
    }
}

fn get_bit(bytes: [u8; 7], bit_index: usize) -> bool {
    get_bits(bytes, bit_index, 1) == 1
}

fn get_bits_u8(bytes: [u8; 7], start: usize, len: usize) -> u8 {
    u8::try_from(get_bits(bytes, start, len)).expect("ADS-B bit field fits in u8")
}

fn get_bits_u16(bytes: [u8; 7], start: usize, len: usize) -> u16 {
    u16::try_from(get_bits(bytes, start, len)).expect("ADS-B bit field fits in u16")
}

fn get_bits_u32(bytes: [u8; 7], start: usize, len: usize) -> u32 {
    u32::try_from(get_bits(bytes, start, len)).expect("ADS-B bit field fits in u32")
}

fn get_bits(bytes: [u8; 7], start: usize, len: usize) -> u64 {
    assert!(start + len <= 56, "ADS-B ME bit range out of bounds");

    let mut value = 0_u64;
    for bit_index in start..start + len {
        let byte = bytes[bit_index / 8];
        let shift = 7 - bit_index % 8;
        value = (value << 1) | u64::from((byte >> shift) & 1);
    }
    value
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn decodes_aircraft_identification() {
        let frame = Frame::from_hex("8D4840D6202CC371C32CE0576098").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();

        assert_eq!(squitter.icao.to_string(), "4840D6");
        assert_eq!(squitter.capability, 5);
        assert_eq!(squitter.type_code, 4);
        assert_eq!(
            squitter.message,
            AdsbMessage::AircraftIdentification(AircraftIdentification {
                category: 0,
                callsign: "KLM1023".to_owned(),
            })
        );
    }

    #[test]
    fn keeps_unimplemented_message_types_as_raw_payload() {
        let frame = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();

        assert_eq!(squitter.icao.to_string(), "40621D");
        assert_eq!(squitter.type_code, 11);
        assert!(matches!(squitter.message, AdsbMessage::AirbornePosition(_)));
    }

    #[test]
    fn decodes_airborne_position_fields() {
        let frame = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();
        let AdsbMessage::AirbornePosition(position) = squitter.message else {
            panic!("expected airborne position");
        };

        assert_eq!(position.altitude_baro_ft, Some(38_000));
        assert_eq!(position.altitude_geometric_ft, None);
        assert_eq!(position.cpr_format, CprFormat::Even);
        assert_eq!(position.cpr_lat, 93_000);
        assert_eq!(position.cpr_lon, 51_372);
    }

    #[test]
    fn decodes_airborne_velocity_fields() {
        let frame = Frame::from_hex("8DA611DB9908AA993804097D4891").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();
        let AdsbMessage::AirborneVelocity(velocity) = squitter.message else {
            panic!("expected airborne velocity");
        };

        assert_eq!(velocity.subtype, 1);
        assert_eq!(velocity.speed_type, SpeedType::GroundSpeed);
        assert_eq!(velocity.ground_speed_kt, Some(262.0));
        assert_eq!(velocity.airspeed_kt, None);
        assert!(abs_f64(velocity.track_deg.unwrap() - 140.0) < 1.0);
    }

    #[test]
    fn decodes_airborne_velocity_airspeed_fields() {
        let frame = frame_with_me(
            [0xAB, 0xCD, 0xEF],
            velocity_airspeed_me(3, Some(90), 250, true, false, 512),
        );
        let squitter = ExtendedSquitter::parse(&frame).unwrap();
        let AdsbMessage::AirborneVelocity(velocity) = squitter.message else {
            panic!("expected airborne velocity");
        };

        assert_eq!(squitter.type_code, 19);
        assert_eq!(velocity.subtype, 3);
        assert_eq!(velocity.speed_type, SpeedType::Airspeed);
        assert_eq!(velocity.ground_speed_kt, None);
        assert_eq!(velocity.airspeed_kt, Some(250.0));
        assert_eq!(velocity.heading_deg, Some(90.0));
        assert_eq!(velocity.vertical_rate_source, VerticalRateSource::Geometric);
        assert_eq!(velocity.vertical_rate_fpm, Some(512));
    }

    #[test]
    fn rounds_ground_speed_without_std_math() {
        assert_eq!(rounded_hypot(185, 185), 262);
        assert_eq!(rounded_hypot(-3, 4), 5);
        assert_eq!(rounded_hypot(1, 1), 1);
    }

    #[test]
    fn normalizes_track_without_std_math() {
        assert!(abs_f64(atan2_degrees(1.0, 0.0) - 90.0) < 0.1);
        assert!(abs_f64(atan2_degrees(0.0, -1.0) - 180.0) < 0.1);
        assert!(abs_f64(normalize_degrees(-10.0) - 350.0) < 0.1);
    }

    #[test]
    fn decodes_target_state_subtype() {
        let frame = Frame::from_hex("8DA26FC9EA17884DD35C085FFDB6").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();
        let AdsbMessage::TargetStateAndStatus(status) = squitter.message else {
            panic!("expected target state and status");
        };

        assert_eq!(squitter.type_code, 29);
        assert_eq!(status.subtype, 1);
    }

    #[test]
    fn decodes_aircraft_status_emergency_fields() {
        let frame = Frame::from_hex("8DAABBCCE1800000000000000000").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();
        let AdsbMessage::AircraftStatus(status) = squitter.message else {
            panic!("expected aircraft status");
        };

        assert_eq!(squitter.type_code, 28);
        assert_eq!(status.subtype, 1);
        assert_eq!(
            status.emergency_state,
            Some(EmergencyState::NoCommunications)
        );
        assert_eq!(status.emergency_state_code, Some(4));
        assert_eq!(status.mode_a_identity_code, Some(0));
    }

    #[test]
    fn decodes_operational_status_fields() {
        let frame = Frame::from_hex("8DADC5FFF8230006004AB8532DF6").unwrap();
        let squitter = ExtendedSquitter::parse(&frame).unwrap();
        let AdsbMessage::AircraftOperationalStatus(status) = squitter.message else {
            panic!("expected aircraft operational status");
        };

        assert_eq!(squitter.type_code, 31);
        assert_eq!(status.subtype, 0);
        assert_eq!(status.capability_class_code, 0x2300);
        assert_eq!(status.operational_mode_code, 0x0600);
        assert_eq!(status.adsb_version, 2);
        assert_eq!(status.nac_p, 10);
    }

    fn velocity_airspeed_me(
        subtype: u64,
        heading_deg: Option<u16>,
        airspeed_kt: u64,
        vertical_rate_source_is_geometric: bool,
        vertical_rate_is_down: bool,
        vertical_rate_fpm: u64,
    ) -> [u8; 7] {
        let mut me = [0; 7];
        set_bits(&mut me, 0, 5, 19);
        set_bits(&mut me, 5, 3, subtype);

        if let Some(heading_deg) = heading_deg {
            set_bits(&mut me, 13, 1, 1);
            set_bits(&mut me, 14, 10, u64::from(heading_deg) * 1024 / 360);
        }

        set_bits(&mut me, 25, 10, airspeed_kt + 1);
        set_bits(&mut me, 35, 1, u64::from(vertical_rate_source_is_geometric));
        set_bits(&mut me, 36, 1, u64::from(vertical_rate_is_down));
        set_bits(&mut me, 37, 9, vertical_rate_fpm / 64 + 1);
        me
    }

    fn frame_with_me(icao: [u8; 3], me: [u8; 7]) -> Frame {
        let mut bytes = [0; 14];
        bytes[0] = 0x8d;
        bytes[1..4].copy_from_slice(&icao);
        bytes[4..11].copy_from_slice(&me);
        Frame::from_bytes(&bytes).unwrap()
    }

    fn set_bits(bytes: &mut [u8; 7], start: usize, len: usize, value: u64) {
        for offset in 0..len {
            let bit_index = start + offset;
            let shift = len - offset - 1;
            let bit = ((value >> shift) & 1) as u8;
            bytes[bit_index / 8] |= bit << (7 - bit_index % 8);
        }
    }
}
