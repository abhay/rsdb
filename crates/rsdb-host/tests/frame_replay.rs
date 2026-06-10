use rsdb::{
    AircraftSnapshot, AircraftStore, CodeCount, DecodeStatus, FRAME_RECORD_SCHEMA_VERSION,
    FeedMessage, Frame, FrameRecord, FrameRecordError, FrameReplayConfig, FrameSignalMetrics,
    NameCount, Protocol, ReceiverIdentity, ReceiverSite, audit_frame_records, replay_frame_records,
};

const SFO_LIVE_FRAMES: &str = include_str!("fixtures/sfo-live-frames.txt");
const START_MS: u64 = 1_780_891_560_000;
const FRAME_SPACING_MS: u64 = 250;
const SAMPLE_SPACING: u64 = 600_000;

#[test]
fn frame_record_round_trips_raw_frame_metadata() {
    let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
    let record = FrameRecord::new(START_MS, 123, &frame);

    assert_eq!(record.schema_version, FRAME_RECORD_SCHEMA_VERSION);
    assert_eq!(record.protocol, Protocol::Adsb1090);
    assert_eq!(record.now_ms, START_MS);
    assert_eq!(record.sample_index, 123);
    assert_eq!(record.frame_sequence, None);
    assert_eq!(record.stream_start_ms, None);
    assert_eq!(record.rx_elapsed_ns, Some(61_500));
    assert_eq!(record.rx_timestamp_uncertainty_ns_estimate, Some(500));
    assert_eq!(
        record.center_frequency_hz,
        Protocol::Adsb1090.default_center_frequency_hz()
    );
    assert_eq!(
        record.sample_rate_hz,
        Protocol::Adsb1090.default_sample_rate_hz()
    );
    assert_eq!(record.signal, None);
    assert_eq!(record.icao.as_deref(), Some("A062EF"));
    assert_eq!(record.adsb_type_code, Some(19));
    assert_eq!(record.raw, "8DA062EF9910B19A38040ACE2B14");
    assert_eq!(record.downlink_format, 17);
    assert_eq!(record.bit_len, 112);
    assert!(record.crc_valid);

    let encoded = serde_json::to_string(&record).unwrap();
    let decoded = serde_json::from_str::<FrameRecord>(&encoded).unwrap();

    assert_eq!(decoded, record);
    assert_eq!(decoded.parse_frame().unwrap(), frame);
}

#[test]
fn rejects_unknown_frame_record_schema_version() {
    let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
    let mut record = FrameRecord::new(START_MS, 0, &frame);

    record.schema_version = FRAME_RECORD_SCHEMA_VERSION + 1;

    assert_eq!(
        record.parse_frame().unwrap_err(),
        FrameRecordError::UnsupportedSchemaVersion {
            expected: FRAME_RECORD_SCHEMA_VERSION,
            actual: FRAME_RECORD_SCHEMA_VERSION + 1,
        }
    );
}

#[test]
fn rejects_non_modes_frame_record_protocol() {
    let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
    let mut record = FrameRecord::new(START_MS, 0, &frame);

    record.protocol = Protocol::Uat978;

    assert_eq!(
        record.parse_frame().unwrap_err(),
        FrameRecordError::UnsupportedProtocol(Protocol::Uat978)
    );
}

#[test]
fn rejects_frame_record_metadata_mismatch() {
    let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
    let mut record = FrameRecord::new(START_MS, 0, &frame);
    record.downlink_format = 18;

    assert!(matches!(
        record.parse_frame().unwrap_err(),
        FrameRecordError::MetadataMismatch {
            field: "downlink_format",
            ..
        }
    ));
}

#[test]
fn rejects_frame_record_invalid_signal_metrics() {
    let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
    let mut record = FrameRecord::new(START_MS, 0, &frame);
    record.signal = Some(FrameSignalMetrics {
        signal_power: 4_096,
        noise_power: 16,
        signal_dbfs_estimate: Some(-9.0),
        snr_db_estimate: Some(24.0),
        chunk_noise_power: Some(16),
        beast_signal_level: 90,
        preamble_high_avg: 4_096,
        preamble_low_avg: 16,
        preamble_delta: 1,
        bit_margin_min: 4_080,
        bit_margin_mean: 4_080,
    });

    assert!(matches!(
        record.parse_frame().unwrap_err(),
        FrameRecordError::MetadataMismatch {
            field: "signal.preamble_delta",
            ..
        }
    ));
}

#[test]
fn replay_rejects_frame_sequence_regression() {
    let mut records = fixture_frame_records();
    records[0].frame_sequence = Some(2);
    records[1].frame_sequence = Some(1);

    let error = replay_frame_records(
        &records,
        &FrameReplayConfig::new(Protocol::Adsb1090, START_MS),
    )
    .unwrap_err();

    assert_eq!(
        error,
        FrameRecordError::SequenceRegression {
            field: "frame_sequence",
            previous: 2,
            actual: 1,
        }
    );
}

#[test]
fn aircraft_store_replays_frame_records() {
    let mut store = AircraftStore::default();
    let record = fixture_frame_records().remove(0);
    let snapshot = store
        .update_frame_record(&record)
        .unwrap()
        .expect("fixture frame updates aircraft state");

    assert_eq!(snapshot.icao, "A062EF");
    assert_eq!(snapshot.last_raw, record.raw);
    assert_eq!(store.aircraft_count(), 1);
}

#[test]
fn replays_sfo_frame_records_through_feed_output() {
    let records = fixture_frame_records();
    let messages = replay_frame_records(
        &records,
        &FrameReplayConfig::new(Protocol::Adsb1090, START_MS),
    )
    .unwrap();

    assert!(matches!(
        messages.first(),
        Some(FeedMessage::Snapshot {
            schema_version: 1,
            protocol: Protocol::Adsb1090,
            now_ms: START_MS,
            aircraft,
            ..
        }) if aircraft.is_empty()
    ));
    assert_eq!(messages.len(), records.len() + 1);

    let dal = latest_aircraft(&messages, "A062EF");
    assert_eq!(dal.altitude_baro_ft, Some(10_900));
    assert_eq!(dal.ground_speed_kt, Some(261.0));
    assert_eq!(dal.vertical_rate_fpm, Some(-1_024));
    assert_eq!(dal.target_state_subtype, Some(1));
    assert_eq!(dal.last_decode_status, DecodeStatus::Partial);
    assert!(dal.altitude_last_seen_ms.is_some());
    assert!(dal.velocity_last_seen_ms.is_some());
    assert!(dal.target_state_last_seen_ms.is_some());

    let swa = latest_aircraft(&messages, "A26FC9");
    assert_eq!(swa.callsign.as_deref(), Some("SWA1585"));
    assert_eq!(swa.category, Some(3));
    assert!(swa.callsign_last_seen_ms.is_some());

    let cks = latest_aircraft(&messages, "AA5BC5");
    assert_eq!(cks.callsign.as_deref(), Some("CKS527"));
    assert_eq!(cks.altitude_baro_ft, Some(37_000));
    assert_eq!(cks.ground_speed_kt, Some(539.0));

    let adc = latest_aircraft(&messages, "ADC5FF");
    assert_eq!(adc.adsb_version, Some(2));
    assert_eq!(adc.nac_p, Some(10));
    assert!(adc.operational_status_last_seen_ms.is_some());
}

#[test]
fn replay_rejects_frame_records_for_the_wrong_protocol() {
    let records = fixture_frame_records();
    let error = replay_frame_records(
        &records,
        &FrameReplayConfig::new(Protocol::Uat978, START_MS),
    )
    .unwrap_err();

    assert_eq!(
        error,
        FrameRecordError::ProtocolMismatch {
            expected: Protocol::Uat978,
            actual: Protocol::Adsb1090,
        }
    );
}

#[test]
fn sfo_live_fixture_covers_supported_adsb_message_families() {
    let records = fixture_frame_records();
    let report = audit_frame_records(&records).unwrap();

    assert_eq!(report.total_frames, 38);
    assert_eq!(report.crc_valid_frames, 38);
    assert_eq!(report.crc_invalid_frames, 0);
    assert_eq!(report.extended_squitter_frames, 38);
    assert_eq!(report.supported_adsb_frames, 38);
    assert_eq!(report.unsupported_adsb_frames, 0);
    assert_eq!(report.downlink_formats, vec![code_count(17, 38)]);
    assert_eq!(
        report.adsb_type_codes,
        vec![
            code_count(4, 2),
            code_count(11, 13),
            code_count(19, 15),
            code_count(28, 1),
            code_count(29, 6),
            code_count(31, 1),
        ]
    );
    assert_eq!(
        report.adsb_message_families,
        vec![
            name_count("airborne_position", 13),
            name_count("airborne_velocity", 15),
            name_count("aircraft_identification", 2),
            name_count("aircraft_operational_status", 1),
            name_count("aircraft_status", 1),
            name_count("target_state_and_status", 6),
        ]
    );
    assert!(observed_field(
        &report,
        "airborne_position.altitude_baro_ft"
    ));
    assert!(observed_field(&report, "airborne_velocity.ground_speed_kt"));
    assert!(observed_field(
        &report,
        "airborne_velocity.vertical_rate_fpm"
    ));
    assert!(observed_field(&report, "aircraft_identification.callsign"));
    assert!(observed_field(&report, "aircraft_status.emergency_state"));
    assert!(observed_field(
        &report,
        "aircraft_operational_status.adsb_version"
    ));
    assert!(observed_field(&report, "target_state_and_status.subtype"));
}

#[test]
fn replay_derives_receiver_range_fields_deterministically() {
    let records = fixture_frame_records();
    let mut config = FrameReplayConfig::new(Protocol::Adsb1090, START_MS);
    config.receiver_site = Some(ReceiverSite::named(
        "SFO".to_owned(),
        37.618_805_6,
        -122.375_416_7,
    ));

    let messages = replay_frame_records(&records, &config).unwrap();
    let cks = latest_aircraft(&messages, "AA5BC5");

    assert_eq!(cks.distance_km, Some(49.7));
    assert_eq!(cks.bearing_deg, Some(6.7));
    assert_eq!(cks.seen_seconds_ago, Some(0));
}

#[test]
fn replay_tags_feed_messages_with_receiver_identity() {
    let records = fixture_frame_records();
    let mut config = FrameReplayConfig::new(Protocol::Adsb1090, START_MS);
    config.receiver_identity = Some(ReceiverIdentity::named(
        "sfo-test".to_owned(),
        "SFO Test".to_owned(),
    ));

    let messages = replay_frame_records(&records, &config).unwrap();

    assert_eq!(
        messages.first().and_then(FeedMessage::receiver),
        config.receiver_identity.as_ref()
    );
    assert!(messages.iter().all(|message| message.receiver().is_some()));
}

fn fixture_frame_records() -> Vec<FrameRecord> {
    fixture_frames()
        .enumerate()
        .map(|(index, raw)| {
            let frame = Frame::from_hex(raw).expect("fixture frame parses");
            let index = u64::try_from(index).expect("fixture index fits in u64");
            FrameRecord::new(
                START_MS + index * FRAME_SPACING_MS,
                index * SAMPLE_SPACING,
                &frame,
            )
        })
        .collect()
}

fn fixture_frames() -> impl Iterator<Item = &'static str> {
    SFO_LIVE_FRAMES.lines().filter_map(|line| {
        let line = line
            .split_once('#')
            .map_or(line, |(before, _)| before)
            .trim();
        (!line.is_empty()).then_some(line)
    })
}

fn latest_aircraft<'a>(messages: &'a [FeedMessage], icao: &str) -> &'a AircraftSnapshot {
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            FeedMessage::Aircraft { aircraft, .. } if aircraft.icao == icao => Some(aircraft),
            _ => None,
        })
        .expect("aircraft exists in replayed feed")
}

fn code_count(code: u8, count: u64) -> CodeCount {
    CodeCount { code, count }
}

fn name_count(name: &str, count: u64) -> NameCount {
    NameCount {
        name: name.to_owned(),
        count,
    }
}

fn observed_field(report: &rsdb::FrameAuditReport, name: &str) -> bool {
    report
        .adsb_field_observations
        .iter()
        .any(|field| field.name == name && field.count > 0)
}
