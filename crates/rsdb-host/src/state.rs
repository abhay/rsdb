use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    AdsbMessage, AirbornePosition, AirborneVelocity, AircraftOperationalStatus, AircraftStatus,
    CprFormat, EmergencyState, ExtendedSquitter, Frame, FrameRecord, FrameRecordError, IcaoAddress,
    SpeedType, TargetStateAndStatus, VerticalRateSource,
};

const CPR_SCALE: f64 = 131_072.0;
const CPR_PAIR_MAX_AGE_MS: u64 = 10_000;
const POSITION_STALE_AFTER_MS: u64 = 30_000;
const POSITION_JUMP_WINDOW_MS: u64 = 10_000;
const MAX_POSITION_JUMP_KM: f64 = 100.0;
const EARTH_RADIUS_KM: f64 = 6_371.0;
const RAW_MESSAGE_HISTORY_LEN: usize = 8;

/// Maintains the latest decoded state for observed aircraft.
#[derive(Debug, Default)]
pub struct AircraftStore {
    aircraft: BTreeMap<IcaoAddress, AircraftState>,
}

impl AircraftStore {
    /// Applies a decoded Mode S frame to the store.
    ///
    /// Returns the updated aircraft snapshot when the frame contains a
    /// supported extended squitter message.
    #[must_use]
    pub fn update_frame(&mut self, frame: &Frame, now_ms: u64) -> Option<AircraftSnapshot> {
        let squitter = ExtendedSquitter::parse(frame)?;
        self.update_squitter(&squitter, frame, now_ms)
    }

    /// Replays a captured decoded-frame record into the store.
    ///
    /// The record timestamp becomes the state update time.
    ///
    /// # Errors
    ///
    /// Returns an error when the record schema version is unsupported or its
    /// raw frame cannot be parsed.
    pub fn update_frame_record(
        &mut self,
        record: &FrameRecord,
    ) -> Result<Option<AircraftSnapshot>, FrameRecordError> {
        let frame = record.parse_frame()?;
        Ok(self.update_frame(&frame, record.now_ms))
    }

    /// Applies a parsed extended squitter to the store.
    #[must_use]
    pub fn update_squitter(
        &mut self,
        squitter: &ExtendedSquitter,
        frame: &Frame,
        now_ms: u64,
    ) -> Option<AircraftSnapshot> {
        let state = self
            .aircraft
            .entry(squitter.icao)
            .or_insert_with(|| AircraftState::new(squitter.icao.to_string(), now_ms));

        state.snapshot.last_seen_ms = now_ms;
        state.snapshot.message_count += 1;
        state.snapshot.last_type_code = Some(squitter.type_code);
        let raw_frame = frame.to_hex();
        state.snapshot.last_raw.clone_from(&raw_frame);
        state.snapshot.raw_messages.push(raw_frame);
        if state.snapshot.raw_messages.len() > RAW_MESSAGE_HISTORY_LEN {
            state.snapshot.raw_messages.remove(0);
        }

        match &squitter.message {
            AdsbMessage::AircraftIdentification(identification) => {
                state.snapshot.callsign = Some(identification.callsign.clone());
                state.snapshot.callsign_last_seen_ms = Some(now_ms);
                state.snapshot.category = Some(identification.category);
                state.snapshot.last_decode_status = DecodeStatus::Updated;
            }
            AdsbMessage::AirbornePosition(position) => {
                state.snapshot.last_decode_status = state.update_position(position, now_ms);
            }
            AdsbMessage::AirborneVelocity(velocity) => {
                state.update_velocity(velocity, now_ms);
                state.snapshot.last_decode_status = DecodeStatus::Updated;
            }
            AdsbMessage::AircraftStatus(status) => {
                state.update_aircraft_status(*status, now_ms);
                state.snapshot.last_decode_status = DecodeStatus::Updated;
            }
            AdsbMessage::TargetStateAndStatus(status) => {
                state.update_target_state(*status, now_ms);
                state.snapshot.last_decode_status = DecodeStatus::Updated;
            }
            AdsbMessage::AircraftOperationalStatus(status) => {
                state.update_operational_status(status, now_ms);
                state.snapshot.last_decode_status = DecodeStatus::Updated;
            }
            AdsbMessage::Unknown { .. } => {
                state.snapshot.last_decode_status = DecodeStatus::Unsupported;
            }
        }
        state.refresh_position_status(now_ms);

        Some(state.snapshot.clone())
    }

    /// Returns all aircraft snapshots sorted by ICAO address.
    #[must_use]
    pub fn snapshots(&self) -> Vec<AircraftSnapshot> {
        self.aircraft
            .values()
            .map(|state| state.snapshot.clone())
            .collect()
    }

    /// Removes aircraft not seen within `stale_after_ms`.
    ///
    /// A zero timeout disables eviction.
    pub fn evict_stale(&mut self, now_ms: u64, stale_after_ms: u64) -> Vec<AircraftSnapshot> {
        if stale_after_ms == 0 {
            return Vec::new();
        }

        let stale = self
            .aircraft
            .iter()
            .filter_map(|(icao, state)| {
                (now_ms.saturating_sub(state.snapshot.last_seen_ms) > stale_after_ms)
                    .then_some(*icao)
            })
            .collect::<Vec<_>>();
        let mut removed = Vec::with_capacity(stale.len());

        for icao in stale {
            if let Some(state) = self.aircraft.remove(&icao) {
                removed.push(state.snapshot);
            }
        }

        removed
    }

    /// Returns the number of aircraft currently tracked.
    #[must_use]
    pub fn aircraft_count(&self) -> usize {
        self.aircraft.len()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ReceiverSite {
    pub name: Option<String>,
    pub lat: f64,
    pub lon: f64,
}

impl ReceiverSite {
    #[must_use]
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self {
            name: None,
            lat,
            lon,
        }
    }

    #[must_use]
    pub fn named(name: String, lat: f64, lon: f64) -> Self {
        Self {
            name: Some(name),
            lat,
            lon,
        }
    }
}

#[derive(Debug)]
struct AircraftState {
    snapshot: AircraftSnapshot,
    cpr_even: Option<CprSample>,
    cpr_odd: Option<CprSample>,
}

impl AircraftState {
    fn new(icao: String, now_ms: u64) -> Self {
        Self {
            snapshot: AircraftSnapshot {
                icao,
                callsign: None,
                callsign_last_seen_ms: None,
                category: None,
                altitude_baro_ft: None,
                altitude_geometric_ft: None,
                altitude_last_seen_ms: None,
                lat: None,
                lon: None,
                distance_km: None,
                bearing_deg: None,
                seen_seconds_ago: None,
                position_status: PositionStatus::Unavailable,
                position_last_seen_ms: None,
                surveillance_status: None,
                nic_supplement_b: None,
                time_flag: None,
                cpr_format: None,
                ground_speed_kt: None,
                airspeed_kt: None,
                track_deg: None,
                heading_deg: None,
                speed_type: None,
                velocity_last_seen_ms: None,
                vertical_rate_source: None,
                vertical_rate_fpm: None,
                aircraft_status_subtype: None,
                aircraft_status_last_seen_ms: None,
                emergency_state: None,
                emergency_state_code: None,
                mode_a_identity_code: None,
                target_state_subtype: None,
                target_state_last_seen_ms: None,
                operational_status_subtype: None,
                operational_status_last_seen_ms: None,
                capability_class_code: None,
                operational_mode_code: None,
                adsb_version: None,
                nic_supplement_a: None,
                nac_p: None,
                geometric_vertical_accuracy: None,
                source_integrity_level: None,
                baro_altitude_integrity: None,
                horizontal_reference_direction: None,
                sil_supplement: None,
                last_seen_ms: now_ms,
                message_count: 0,
                last_type_code: None,
                last_decode_status: DecodeStatus::Unknown,
                last_raw: String::new(),
                raw_messages: Vec::new(),
            },
            cpr_even: None,
            cpr_odd: None,
        }
    }

    fn update_position(&mut self, position: &AirbornePosition, now_ms: u64) -> DecodeStatus {
        let mut altitude_updated = false;
        if let Some(altitude_baro_ft) = position.altitude_baro_ft {
            self.snapshot.altitude_baro_ft = Some(altitude_baro_ft);
            altitude_updated = true;
        }
        if let Some(altitude_geometric_ft) = position.altitude_geometric_ft {
            self.snapshot.altitude_geometric_ft = Some(altitude_geometric_ft);
            altitude_updated = true;
        }
        if altitude_updated {
            self.snapshot.altitude_last_seen_ms = Some(now_ms);
        }
        self.snapshot.surveillance_status = Some(position.surveillance_status);
        self.snapshot.nic_supplement_b = Some(position.nic_supplement_b);
        self.snapshot.time_flag = Some(position.time_flag);
        self.snapshot.cpr_format = Some(position.cpr_format);

        let sample = CprSample {
            lat: position.cpr_lat,
            lon: position.cpr_lon,
            received_ms: now_ms,
        };

        match position.cpr_format {
            CprFormat::Even => self.cpr_even = Some(sample),
            CprFormat::Odd => self.cpr_odd = Some(sample),
        }

        let (Some(even), Some(odd)) = (self.cpr_even, self.cpr_odd) else {
            return DecodeStatus::Partial;
        };

        if even.received_ms.abs_diff(odd.received_ms) > CPR_PAIR_MAX_AGE_MS {
            return DecodeStatus::Partial;
        }

        if let Some(position) = decode_global_cpr(even, odd) {
            let position = DecodedPosition {
                lat: round_coordinate(position.lat),
                lon: round_coordinate(position.lon),
            };

            if self.is_implausible_position_jump(position, now_ms) {
                self.snapshot.position_status = PositionStatus::RejectedJump;
                return DecodeStatus::Rejected;
            }

            self.snapshot.lat = Some(position.lat);
            self.snapshot.lon = Some(position.lon);
            self.snapshot.position_status = PositionStatus::Fresh;
            self.snapshot.position_last_seen_ms = Some(now_ms);
            return DecodeStatus::Updated;
        }

        DecodeStatus::Partial
    }

    fn update_velocity(&mut self, velocity: &AirborneVelocity, now_ms: u64) {
        self.snapshot.speed_type = Some(velocity.speed_type);
        self.snapshot.ground_speed_kt = velocity.ground_speed_kt;
        self.snapshot.airspeed_kt = velocity.airspeed_kt;
        self.snapshot.track_deg = velocity.track_deg.map(round_heading);
        self.snapshot.heading_deg = velocity.heading_deg.map(round_heading);
        self.snapshot.velocity_last_seen_ms = Some(now_ms);
        self.snapshot.vertical_rate_source = Some(velocity.vertical_rate_source);
        self.snapshot.vertical_rate_fpm = velocity.vertical_rate_fpm;
    }

    fn update_aircraft_status(&mut self, status: AircraftStatus, now_ms: u64) {
        self.snapshot.aircraft_status_subtype = Some(status.subtype);
        self.snapshot.aircraft_status_last_seen_ms = Some(now_ms);
        self.snapshot.emergency_state = status.emergency_state;
        self.snapshot.emergency_state_code = status.emergency_state_code;
        self.snapshot.mode_a_identity_code = status.mode_a_identity_code;
    }

    fn update_target_state(&mut self, status: TargetStateAndStatus, now_ms: u64) {
        self.snapshot.target_state_subtype = Some(status.subtype);
        self.snapshot.target_state_last_seen_ms = Some(now_ms);
    }

    fn update_operational_status(&mut self, status: &AircraftOperationalStatus, now_ms: u64) {
        self.snapshot.operational_status_subtype = Some(status.subtype);
        self.snapshot.operational_status_last_seen_ms = Some(now_ms);
        self.snapshot.capability_class_code = Some(status.capability_class_code);
        self.snapshot.operational_mode_code = Some(status.operational_mode_code);
        self.snapshot.adsb_version = Some(status.adsb_version);
        self.snapshot.nic_supplement_a = Some(status.nic_supplement_a);
        self.snapshot.nac_p = Some(status.nac_p);
        self.snapshot.geometric_vertical_accuracy = Some(status.geometric_vertical_accuracy);
        self.snapshot.source_integrity_level = Some(status.source_integrity_level);
        self.snapshot.baro_altitude_integrity = Some(status.baro_altitude_integrity);
        self.snapshot.horizontal_reference_direction = Some(status.horizontal_reference_direction);
        self.snapshot.sil_supplement = Some(status.sil_supplement);
    }

    fn refresh_position_status(&mut self, now_ms: u64) {
        if self.snapshot.position_status == PositionStatus::RejectedJump {
            return;
        }

        let Some(position_last_seen_ms) = self.snapshot.position_last_seen_ms else {
            self.snapshot.position_status = PositionStatus::Unavailable;
            return;
        };

        self.snapshot.position_status =
            if now_ms.saturating_sub(position_last_seen_ms) > POSITION_STALE_AFTER_MS {
                PositionStatus::Stale
            } else {
                PositionStatus::Fresh
            };
    }

    fn is_implausible_position_jump(&self, position: DecodedPosition, now_ms: u64) -> bool {
        let (Some(lat), Some(lon), Some(position_last_seen_ms)) = (
            self.snapshot.lat,
            self.snapshot.lon,
            self.snapshot.position_last_seen_ms,
        ) else {
            return false;
        };

        if now_ms.saturating_sub(position_last_seen_ms) > POSITION_JUMP_WINDOW_MS {
            return false;
        }

        haversine_distance_km(lat, lon, position.lat, position.lon) > MAX_POSITION_JUMP_KM
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionStatus {
    Unavailable,
    Fresh,
    Stale,
    RejectedJump,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeStatus {
    #[default]
    Unknown,
    Updated,
    Partial,
    Rejected,
    Unsupported,
}

/// Serializable aircraft state used by JSON and WebSocket outputs.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AircraftSnapshot {
    pub icao: String,
    pub callsign: Option<String>,
    #[serde(default)]
    pub callsign_last_seen_ms: Option<u64>,
    pub category: Option<u8>,
    pub altitude_baro_ft: Option<i32>,
    pub altitude_geometric_ft: Option<i32>,
    #[serde(default)]
    pub altitude_last_seen_ms: Option<u64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub distance_km: Option<f64>,
    pub bearing_deg: Option<f64>,
    pub seen_seconds_ago: Option<u64>,
    pub position_status: PositionStatus,
    pub position_last_seen_ms: Option<u64>,
    pub surveillance_status: Option<u8>,
    pub nic_supplement_b: Option<bool>,
    pub time_flag: Option<bool>,
    pub cpr_format: Option<CprFormat>,
    pub ground_speed_kt: Option<f64>,
    #[serde(default)]
    pub airspeed_kt: Option<f64>,
    pub track_deg: Option<f64>,
    pub heading_deg: Option<f64>,
    pub speed_type: Option<SpeedType>,
    #[serde(default)]
    pub velocity_last_seen_ms: Option<u64>,
    pub vertical_rate_source: Option<VerticalRateSource>,
    pub vertical_rate_fpm: Option<i32>,
    pub aircraft_status_subtype: Option<u8>,
    #[serde(default)]
    pub aircraft_status_last_seen_ms: Option<u64>,
    pub emergency_state: Option<EmergencyState>,
    pub emergency_state_code: Option<u8>,
    pub mode_a_identity_code: Option<u16>,
    pub target_state_subtype: Option<u8>,
    #[serde(default)]
    pub target_state_last_seen_ms: Option<u64>,
    pub operational_status_subtype: Option<u8>,
    #[serde(default)]
    pub operational_status_last_seen_ms: Option<u64>,
    pub capability_class_code: Option<u16>,
    pub operational_mode_code: Option<u16>,
    pub adsb_version: Option<u8>,
    pub nic_supplement_a: Option<bool>,
    pub nac_p: Option<u8>,
    pub geometric_vertical_accuracy: Option<u8>,
    pub source_integrity_level: Option<u8>,
    pub baro_altitude_integrity: Option<bool>,
    pub horizontal_reference_direction: Option<bool>,
    pub sil_supplement: Option<bool>,
    pub last_seen_ms: u64,
    pub message_count: u64,
    pub last_type_code: Option<u8>,
    #[serde(default)]
    pub last_decode_status: DecodeStatus,
    pub last_raw: String,
    pub raw_messages: Vec<String>,
}

impl AircraftSnapshot {
    #[must_use]
    pub fn enriched_for_receiver(
        mut self,
        receiver_site: Option<&ReceiverSite>,
        now_ms: u64,
    ) -> Self {
        self.enrich_for_receiver(receiver_site, now_ms);
        self
    }

    pub fn enrich_for_receiver(&mut self, receiver_site: Option<&ReceiverSite>, now_ms: u64) {
        self.seen_seconds_ago = Some(now_ms.saturating_sub(self.last_seen_ms) / 1_000);

        let (Some(receiver_site), Some(lat), Some(lon)) = (receiver_site, self.lat, self.lon)
        else {
            self.distance_km = None;
            self.bearing_deg = None;
            return;
        };

        self.distance_km = Some(round_distance(haversine_distance_km(
            receiver_site.lat,
            receiver_site.lon,
            lat,
            lon,
        )));
        self.bearing_deg = Some(round_heading(initial_bearing_deg(
            receiver_site.lat,
            receiver_site.lon,
            lat,
            lon,
        )));
    }
}

#[derive(Debug, Clone, Copy)]
struct CprSample {
    lat: u32,
    lon: u32,
    received_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct DecodedPosition {
    lat: f64,
    lon: f64,
}

#[allow(clippy::cast_possible_truncation)]
fn decode_global_cpr(even: CprSample, odd: CprSample) -> Option<DecodedPosition> {
    let even_lat = f64::from(even.lat) / CPR_SCALE;
    let even_lon = f64::from(even.lon) / CPR_SCALE;
    let odd_lat = f64::from(odd.lat) / CPR_SCALE;
    let odd_lon = f64::from(odd.lon) / CPR_SCALE;
    let dlat_even = 360.0 / 60.0;
    let dlat_odd = 360.0 / 59.0;
    let j = (59.0 * even_lat - 60.0 * odd_lat + 0.5).floor() as i32;
    let mut rlat_even = dlat_even * (f64::from(modulo(j, 60)) + even_lat);
    let mut rlat_odd = dlat_odd * (f64::from(modulo(j, 59)) + odd_lat);

    if rlat_even >= 270.0 {
        rlat_even -= 360.0;
    }
    if rlat_odd >= 270.0 {
        rlat_odd -= 360.0;
    }

    if cpr_nl(rlat_even) != cpr_nl(rlat_odd) {
        return None;
    }

    let use_even = even.received_ms >= odd.received_ms;
    let nl = cpr_nl(if use_even { rlat_even } else { rlat_odd });
    let m = (even_lon * f64::from(nl - 1) - odd_lon * f64::from(nl) + 0.5).floor() as i32;

    let (lat, mut lon) = if use_even {
        let ni = nl.max(1);
        (
            rlat_even,
            360.0 / f64::from(ni) * (f64::from(modulo(m, ni)) + even_lon),
        )
    } else {
        let ni = (nl - 1).max(1);
        (
            rlat_odd,
            360.0 / f64::from(ni) * (f64::from(modulo(m, ni)) + odd_lon),
        )
    };

    if lon > 180.0 {
        lon -= 360.0;
    }

    Some(DecodedPosition { lat, lon })
}

fn modulo(value: i32, modulus: i32) -> i32 {
    value.rem_euclid(modulus)
}

#[allow(clippy::too_many_lines)]
fn cpr_nl(lat: f64) -> i32 {
    let lat = lat.abs();

    if lat < 10.470_471_30 {
        59
    } else if lat < 14.828_174_37 {
        58
    } else if lat < 18.186_263_57 {
        57
    } else if lat < 21.029_394_93 {
        56
    } else if lat < 23.545_044_87 {
        55
    } else if lat < 25.829_247_07 {
        54
    } else if lat < 27.938_987_10 {
        53
    } else if lat < 29.911_356_86 {
        52
    } else if lat < 31.772_097_08 {
        51
    } else if lat < 33.539_934_36 {
        50
    } else if lat < 35.228_995_98 {
        49
    } else if lat < 36.850_251_08 {
        48
    } else if lat < 38.412_418_92 {
        47
    } else if lat < 39.922_566_84 {
        46
    } else if lat < 41.386_518_32 {
        45
    } else if lat < 42.809_140_12 {
        44
    } else if lat < 44.194_549_51 {
        43
    } else if lat < 45.546_267_23 {
        42
    } else if lat < 46.867_332_52 {
        41
    } else if lat < 48.160_391_28 {
        40
    } else if lat < 49.427_764_39 {
        39
    } else if lat < 50.671_501_66 {
        38
    } else if lat < 51.893_424_69 {
        37
    } else if lat < 53.095_161_53 {
        36
    } else if lat < 54.278_174_72 {
        35
    } else if lat < 55.443_784_44 {
        34
    } else if lat < 56.593_187_56 {
        33
    } else if lat < 57.727_473_54 {
        32
    } else if lat < 58.847_637_76 {
        31
    } else if lat < 59.954_592_77 {
        30
    } else if lat < 61.049_177_74 {
        29
    } else if lat < 62.132_166_59 {
        28
    } else if lat < 63.204_274_79 {
        27
    } else if lat < 64.266_165_23 {
        26
    } else if lat < 65.318_453_10 {
        25
    } else if lat < 66.361_710_08 {
        24
    } else if lat < 67.396_467_74 {
        23
    } else if lat < 68.423_220_22 {
        22
    } else if lat < 69.442_426_31 {
        21
    } else if lat < 70.454_510_75 {
        20
    } else if lat < 71.459_864_73 {
        19
    } else if lat < 72.458_845_45 {
        18
    } else if lat < 73.451_774_42 {
        17
    } else if lat < 74.438_934_16 {
        16
    } else if lat < 75.420_562_57 {
        15
    } else if lat < 76.396_843_91 {
        14
    } else if lat < 77.367_894_61 {
        13
    } else if lat < 78.333_740_83 {
        12
    } else if lat < 79.294_282_25 {
        11
    } else if lat < 80.249_232_13 {
        10
    } else if lat < 81.198_013_49 {
        9
    } else if lat < 82.139_569_81 {
        8
    } else if lat < 83.071_994_45 {
        7
    } else if lat < 83.991_735_63 {
        6
    } else if lat < 84.891_661_91 {
        5
    } else if lat < 85.755_416_21 {
        4
    } else if lat < 86.535_369_98 {
        3
    } else if lat < 87.0 {
        2
    } else {
        1
    }
}

fn round_coordinate(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn round_heading(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn round_distance(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn initial_bearing_deg(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let delta_lon = (to_lon - from_lon).to_radians();
    let y = delta_lon.sin() * to_lat.cos();
    let x = from_lat.cos() * to_lat.sin() - from_lat.sin() * to_lat.cos() * delta_lon.cos();

    y.atan2(x).to_degrees().rem_euclid(360.0)
}

fn haversine_distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let delta_lat = to_lat - from_lat;
    let delta_lon = (to_lon - from_lon).to_radians();
    let a = (delta_lat / 2.0).sin().powi(2)
        + from_lat.cos() * to_lat.cos() * (delta_lon / 2.0).sin().powi(2);

    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_even_and_odd_cpr_positions() {
        let mut store = AircraftStore::default();
        let even = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let odd = Frame::from_hex("8D40621D58C386435CC412692AD6").unwrap();

        let partial = store.update_frame(&odd, 1_000).unwrap();
        let snapshot = store.update_frame(&even, 2_000).unwrap();

        assert_eq!(partial.last_decode_status, DecodeStatus::Partial);
        assert_eq!(partial.altitude_last_seen_ms, Some(1_000));
        assert_eq!(partial.position_last_seen_ms, None);
        assert_eq!(snapshot.icao, "40621D");
        assert_eq!(snapshot.altitude_baro_ft, Some(38_000));
        assert_eq!(snapshot.altitude_last_seen_ms, Some(2_000));
        assert_eq!(snapshot.lat, Some(52.257_202));
        assert_eq!(snapshot.lon, Some(3.919_373));
        assert_eq!(snapshot.position_status, PositionStatus::Fresh);
        assert_eq!(snapshot.position_last_seen_ms, Some(2_000));
        assert_eq!(snapshot.cpr_format, Some(CprFormat::Even));
        assert_eq!(snapshot.last_decode_status, DecodeStatus::Updated);
        assert_eq!(
            snapshot.raw_messages,
            vec![
                "8D40621D58C386435CC412692AD6".to_owned(),
                "8D40621D58C382D690C8AC2863A7".to_owned(),
            ]
        );
    }

    #[test]
    fn updates_velocity_state() {
        let mut store = AircraftStore::default();
        let frame = Frame::from_hex("8DA611DB9908AA993804097D4891").unwrap();
        let snapshot = store.update_frame(&frame, 5_000).unwrap();

        assert_eq!(snapshot.icao, "A611DB");
        assert_eq!(snapshot.ground_speed_kt, Some(262.0));
        assert!((snapshot.track_deg.unwrap() - 140.0).abs() < 1.0);
        assert_eq!(snapshot.speed_type, Some(SpeedType::GroundSpeed));
        assert_eq!(snapshot.velocity_last_seen_ms, Some(5_000));
        assert_eq!(
            snapshot.vertical_rate_source,
            Some(VerticalRateSource::Geometric)
        );
        assert_eq!(snapshot.message_count, 1);
        assert_eq!(snapshot.last_decode_status, DecodeStatus::Updated);
    }

    #[test]
    fn updates_airspeed_velocity_state_without_ground_speed() {
        let mut store = AircraftStore::default();
        let frame =
            Frame::from_bytes(&[0x8d, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let squitter = ExtendedSquitter {
            icao: IcaoAddress::from_bytes([0xab, 0xcd, 0xef]),
            capability: 5,
            type_code: 19,
            message: AdsbMessage::AirborneVelocity(AirborneVelocity {
                subtype: 3,
                speed_type: SpeedType::Airspeed,
                ground_speed_kt: None,
                airspeed_kt: Some(250.0),
                track_deg: None,
                heading_deg: Some(90.0),
                vertical_rate_source: VerticalRateSource::Barometric,
                vertical_rate_fpm: Some(512),
            }),
        };
        let snapshot = store.update_squitter(&squitter, &frame, 5_000).unwrap();

        assert_eq!(snapshot.icao, "ABCDEF");
        assert_eq!(snapshot.ground_speed_kt, None);
        assert_eq!(snapshot.airspeed_kt, Some(250.0));
        assert_eq!(snapshot.track_deg, None);
        assert_eq!(snapshot.heading_deg, Some(90.0));
        assert_eq!(snapshot.speed_type, Some(SpeedType::Airspeed));
        assert_eq!(snapshot.velocity_last_seen_ms, Some(5_000));
        assert_eq!(snapshot.last_decode_status, DecodeStatus::Updated);
    }

    #[test]
    fn updates_aircraft_status_state() {
        let mut store = AircraftStore::default();
        let frame = Frame::from_hex("8DAABBCCE1800000000000000000").unwrap();
        let snapshot = store.update_frame(&frame, 5_000).unwrap();

        assert_eq!(snapshot.icao, "AABBCC");
        assert_eq!(snapshot.aircraft_status_subtype, Some(1));
        assert_eq!(snapshot.aircraft_status_last_seen_ms, Some(5_000));
        assert_eq!(
            snapshot.emergency_state,
            Some(EmergencyState::NoCommunications)
        );
        assert_eq!(snapshot.emergency_state_code, Some(4));
        assert_eq!(snapshot.mode_a_identity_code, Some(0));
        assert_eq!(snapshot.last_decode_status, DecodeStatus::Updated);
    }

    #[test]
    fn marks_unknown_adsb_payloads_unsupported() {
        let mut store = AircraftStore::default();
        let frame = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let squitter = ExtendedSquitter {
            icao: IcaoAddress::from_bytes([0x40, 0x62, 0x1D]),
            capability: 5,
            type_code: 23,
            message: AdsbMessage::Unknown { raw_me: [0xB8; 7] },
        };

        let snapshot = store.update_squitter(&squitter, &frame, 7_000).unwrap();

        assert_eq!(snapshot.last_type_code, Some(23));
        assert_eq!(snapshot.last_decode_status, DecodeStatus::Unsupported);
        assert_eq!(snapshot.last_seen_ms, 7_000);
        assert_eq!(snapshot.message_count, 1);
    }

    #[test]
    fn marks_accepted_positions_stale_after_freshness_window() {
        let mut store = AircraftStore::default();
        let even = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let odd = Frame::from_hex("8D40621D58C386435CC412692AD6").unwrap();
        let velocity_frame = Frame::from_hex("8DA611DB9908AA993804097D4891").unwrap();
        let mut velocity = ExtendedSquitter::parse(&velocity_frame).unwrap();
        velocity.icao = IcaoAddress::from_bytes([0x40, 0x62, 0x1D]);

        store.update_frame(&odd, 1_000).unwrap();
        let fresh = store.update_frame(&even, 2_000).unwrap();
        let stale = store
            .update_squitter(&velocity, &velocity_frame, 32_001)
            .unwrap();

        assert_eq!(fresh.position_status, PositionStatus::Fresh);
        assert_eq!(stale.position_status, PositionStatus::Stale);
        assert_eq!(stale.lat, Some(52.257_202));
        assert_eq!(stale.lon, Some(3.919_373));
    }

    #[test]
    fn rejects_implausible_cpr_position_jumps() {
        let mut store = AircraftStore::default();
        let raw_frame = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let icao = IcaoAddress::from_bytes([0x40, 0x62, 0x1D]);
        let odd = Frame::from_hex("8D40621D58C386435CC412692AD6").unwrap();

        store.update_frame(&odd, 1_000).unwrap();
        let accepted = store.update_frame(&raw_frame, 2_000).unwrap();

        apply_position(
            &mut store,
            &raw_frame,
            icao,
            zero_cpr_position(CprFormat::Even),
            3_000,
        );
        let rejected = apply_position(
            &mut store,
            &raw_frame,
            icao,
            zero_cpr_position(CprFormat::Odd),
            4_000,
        );

        assert_eq!(accepted.position_status, PositionStatus::Fresh);
        assert_eq!(rejected.position_status, PositionStatus::RejectedJump);
        assert_eq!(rejected.lat, accepted.lat);
        assert_eq!(rejected.lon, accepted.lon);
        assert_eq!(rejected.last_decode_status, DecodeStatus::Rejected);
    }

    #[test]
    fn evicts_stale_aircraft() {
        let mut store = AircraftStore::default();
        let frame = Frame::from_hex("8DA611DB9908AA993804097D4891").unwrap();
        store.update_frame(&frame, 5_000).unwrap();

        assert!(store.evict_stale(64_999, 60_000).is_empty());
        let removed = store.evict_stale(65_001, 60_000);

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].icao, "A611DB");
        assert_eq!(store.aircraft_count(), 0);
    }

    #[test]
    fn zero_stale_timeout_disables_eviction() {
        let mut store = AircraftStore::default();
        let frame = Frame::from_hex("8DA611DB9908AA993804097D4891").unwrap();
        store.update_frame(&frame, 5_000).unwrap();

        assert!(store.evict_stale(u64::MAX, 0).is_empty());
        assert_eq!(store.aircraft_count(), 1);
    }

    fn apply_position(
        store: &mut AircraftStore,
        frame: &Frame,
        icao: IcaoAddress,
        position: AirbornePosition,
        now_ms: u64,
    ) -> AircraftSnapshot {
        let squitter = ExtendedSquitter {
            icao,
            capability: 5,
            type_code: 11,
            message: AdsbMessage::AirbornePosition(position),
        };

        store.update_squitter(&squitter, frame, now_ms).unwrap()
    }

    const fn zero_cpr_position(cpr_format: CprFormat) -> AirbornePosition {
        AirbornePosition {
            surveillance_status: 0,
            nic_supplement_b: false,
            altitude_baro_ft: Some(38_000),
            altitude_geometric_ft: None,
            time_flag: false,
            cpr_format,
            cpr_lat: 0,
            cpr_lon: 0,
        }
    }
}
