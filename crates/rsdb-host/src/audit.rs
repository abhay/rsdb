use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    AdsbMessage, ExtendedSquitter, Frame, FrameRecord, FrameRecordError,
    validate_frame_record_sequence,
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct FrameAuditReport {
    pub total_frames: u64,
    pub crc_valid_frames: u64,
    pub crc_invalid_frames: u64,
    pub extended_squitter_frames: u64,
    pub supported_adsb_frames: u64,
    pub unsupported_adsb_frames: u64,
    pub downlink_formats: Vec<CodeCount>,
    pub adsb_type_codes: Vec<CodeCount>,
    pub adsb_message_families: Vec<NameCount>,
    pub adsb_field_observations: Vec<NameCount>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct CodeCount {
    pub code: u8,
    pub count: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct NameCount {
    pub name: String,
    pub count: u64,
}

/// Builds a decoder coverage report from parsed Mode S frames.
#[must_use]
pub fn audit_frames<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> FrameAuditReport {
    let mut audit = Accumulator::default();

    for frame in frames {
        audit.observe_frame(frame, frame.is_crc_valid());
    }

    audit.finish()
}

/// Builds a decoder coverage report from decoded frame records.
///
/// # Errors
///
/// Returns an error when any record has an unsupported schema version or an
/// invalid raw frame.
pub fn audit_frame_records(records: &[FrameRecord]) -> Result<FrameAuditReport, FrameRecordError> {
    let mut audit = Accumulator::default();
    validate_frame_record_sequence(records)?;

    for record in records {
        let frame = record.parse_frame()?;
        audit.observe_frame(&frame, record.crc_valid);
    }

    Ok(audit.finish())
}

#[derive(Default)]
struct Accumulator {
    total_frames: u64,
    crc_valid_frames: u64,
    crc_invalid_frames: u64,
    extended_squitter_frames: u64,
    supported_adsb_frames: u64,
    unsupported_adsb_frames: u64,
    downlink_formats: BTreeMap<u8, u64>,
    adsb_type_codes: BTreeMap<u8, u64>,
    adsb_message_families: BTreeMap<&'static str, u64>,
    adsb_field_observations: BTreeMap<&'static str, u64>,
}

impl Accumulator {
    fn observe_frame(&mut self, frame: &Frame, crc_valid: bool) {
        self.total_frames += 1;
        if crc_valid {
            self.crc_valid_frames += 1;
        } else {
            self.crc_invalid_frames += 1;
        }
        increment_code(&mut self.downlink_formats, frame.downlink_format().bits());

        let Some(squitter) = ExtendedSquitter::parse(frame) else {
            return;
        };

        self.extended_squitter_frames += 1;
        increment_code(&mut self.adsb_type_codes, squitter.type_code);
        self.observe_adsb_message(&squitter.message);
    }

    fn observe_adsb_message(&mut self, message: &AdsbMessage) {
        match message {
            AdsbMessage::AircraftIdentification(identification) => {
                self.supported_adsb_frames += 1;
                self.observe_family("aircraft_identification");
                self.observe_field("aircraft_identification.callsign");
                self.observe_field("aircraft_identification.category");
                if identification.callsign.is_empty() {
                    self.observe_field("aircraft_identification.empty_callsign");
                }
            }
            AdsbMessage::AirbornePosition(position) => {
                self.supported_adsb_frames += 1;
                self.observe_family("airborne_position");
                self.observe_field("airborne_position.surveillance_status");
                self.observe_field("airborne_position.nic_supplement_b");
                self.observe_field("airborne_position.time_flag");
                self.observe_field(match position.cpr_format {
                    crate::CprFormat::Even => "airborne_position.cpr_even",
                    crate::CprFormat::Odd => "airborne_position.cpr_odd",
                });
                self.observe_field("airborne_position.cpr_lat");
                self.observe_field("airborne_position.cpr_lon");
                if position.altitude_baro_ft.is_some() {
                    self.observe_field("airborne_position.altitude_baro_ft");
                }
                if position.altitude_geometric_ft.is_some() {
                    self.observe_field("airborne_position.altitude_geometric_ft");
                }
            }
            AdsbMessage::AirborneVelocity(velocity) => {
                self.supported_adsb_frames += 1;
                self.observe_family("airborne_velocity");
                self.observe_field("airborne_velocity.subtype");
                self.observe_field("airborne_velocity.speed_type");
                self.observe_field("airborne_velocity.vertical_rate_source");
                if velocity.ground_speed_kt.is_some() {
                    self.observe_field("airborne_velocity.ground_speed_kt");
                }
                if velocity.airspeed_kt.is_some() {
                    self.observe_field("airborne_velocity.airspeed_kt");
                }
                if velocity.track_deg.is_some() {
                    self.observe_field("airborne_velocity.track_deg");
                }
                if velocity.heading_deg.is_some() {
                    self.observe_field("airborne_velocity.heading_deg");
                }
                if velocity.vertical_rate_fpm.is_some() {
                    self.observe_field("airborne_velocity.vertical_rate_fpm");
                }
            }
            AdsbMessage::AircraftStatus(status) => {
                self.supported_adsb_frames += 1;
                self.observe_family("aircraft_status");
                self.observe_field("aircraft_status.subtype");
                if status.emergency_state.is_some() {
                    self.observe_field("aircraft_status.emergency_state");
                }
                if status.emergency_state_code.is_some() {
                    self.observe_field("aircraft_status.emergency_state_code");
                }
                if status.mode_a_identity_code.is_some() {
                    self.observe_field("aircraft_status.mode_a_identity_code");
                }
            }
            AdsbMessage::TargetStateAndStatus(_status) => {
                self.supported_adsb_frames += 1;
                self.observe_family("target_state_and_status");
                self.observe_field("target_state_and_status.subtype");
            }
            AdsbMessage::AircraftOperationalStatus(_status) => {
                self.supported_adsb_frames += 1;
                self.observe_family("aircraft_operational_status");
                self.observe_field("aircraft_operational_status.subtype");
                self.observe_field("aircraft_operational_status.capability_class_code");
                self.observe_field("aircraft_operational_status.operational_mode_code");
                self.observe_field("aircraft_operational_status.adsb_version");
                self.observe_field("aircraft_operational_status.nic_supplement_a");
                self.observe_field("aircraft_operational_status.nac_p");
                self.observe_field("aircraft_operational_status.geometric_vertical_accuracy");
                self.observe_field("aircraft_operational_status.source_integrity_level");
                self.observe_field("aircraft_operational_status.baro_altitude_integrity");
                self.observe_field("aircraft_operational_status.horizontal_reference_direction");
                self.observe_field("aircraft_operational_status.sil_supplement");
            }
            AdsbMessage::Unknown { .. } => {
                self.unsupported_adsb_frames += 1;
                self.observe_family("unknown");
                self.observe_field("unknown.raw_me");
            }
        }
    }

    fn observe_family(&mut self, name: &'static str) {
        increment_name(&mut self.adsb_message_families, name);
    }

    fn observe_field(&mut self, name: &'static str) {
        increment_name(&mut self.adsb_field_observations, name);
    }

    fn finish(self) -> FrameAuditReport {
        FrameAuditReport {
            total_frames: self.total_frames,
            crc_valid_frames: self.crc_valid_frames,
            crc_invalid_frames: self.crc_invalid_frames,
            extended_squitter_frames: self.extended_squitter_frames,
            supported_adsb_frames: self.supported_adsb_frames,
            unsupported_adsb_frames: self.unsupported_adsb_frames,
            downlink_formats: code_counts(self.downlink_formats),
            adsb_type_codes: code_counts(self.adsb_type_codes),
            adsb_message_families: name_counts(self.adsb_message_families),
            adsb_field_observations: name_counts(self.adsb_field_observations),
        }
    }
}

fn increment_code(counts: &mut BTreeMap<u8, u64>, code: u8) {
    *counts.entry(code).or_default() += 1;
}

fn increment_name(counts: &mut BTreeMap<&'static str, u64>, name: &'static str) {
    *counts.entry(name).or_default() += 1;
}

fn code_counts(counts: BTreeMap<u8, u64>) -> Vec<CodeCount> {
    counts
        .into_iter()
        .map(|(code, count)| CodeCount { code, count })
        .collect()
}

fn name_counts(counts: BTreeMap<&str, u64>) -> Vec<NameCount> {
    counts
        .into_iter()
        .map(|(name, count)| NameCount {
            name: name.to_owned(),
            count,
        })
        .collect()
}
