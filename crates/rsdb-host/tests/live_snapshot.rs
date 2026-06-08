use rsdb::{AircraftSnapshot, AircraftStore, Frame};

const SFO_LIVE_FRAMES: &str = include_str!("fixtures/sfo-live-frames.txt");

#[test]
fn replays_sfo_live_frame_fixture() {
    let mut store = AircraftStore::default();

    for (index, raw) in fixture_frames().enumerate() {
        let frame = Frame::from_hex(raw).expect("fixture frame parses");
        let _ = store.update_frame(&frame, 1_780_891_560_000 + index as u64 * 250);
    }

    let snapshots = store.snapshots();
    assert_eq!(snapshots.len(), 5);

    let dal = aircraft(&snapshots, "A062EF");
    assert_eq!(dal.altitude_baro_ft, Some(10_900));
    assert_eq!(dal.ground_speed_kt, Some(261.0));
    assert_eq!(dal.vertical_rate_fpm, Some(-1_024));
    assert_eq!(dal.target_state_subtype, Some(1));

    let swa = aircraft(&snapshots, "A26FC9");
    assert_eq!(swa.callsign.as_deref(), Some("SWA1585"));
    assert_eq!(swa.category, Some(3));
    assert_eq!(swa.target_state_subtype, Some(1));

    let cks = aircraft(&snapshots, "AA5BC5");
    assert_eq!(cks.callsign.as_deref(), Some("CKS527"));
    assert_eq!(cks.altitude_baro_ft, Some(37_000));
    assert_eq!(cks.ground_speed_kt, Some(539.0));

    let adc = aircraft(&snapshots, "ADC5FF");
    assert_eq!(adc.operational_status_subtype, Some(0));
    assert_eq!(adc.adsb_version, Some(2));
    assert_eq!(adc.nac_p, Some(10));

    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.raw_messages.len() <= 8)
    );
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

fn aircraft<'a>(snapshots: &'a [AircraftSnapshot], icao: &str) -> &'a AircraftSnapshot {
    snapshots
        .iter()
        .find(|snapshot| snapshot.icao == icao)
        .expect("aircraft exists in fixture")
}
