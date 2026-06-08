use rsdb::FeedMessage;

const FEED_REPLAY: &str = include_str!("fixtures/feed-replay.ndjson");

#[test]
fn replays_ndjson_fixture_without_reordering() {
    let messages = parse_fixture();

    assert!(matches!(messages[0], FeedMessage::Snapshot { .. }));

    match &messages[1] {
        FeedMessage::Aircraft { aircraft, .. } => {
            assert_eq!(aircraft.icao, "A062EF");
            assert_eq!(aircraft.callsign.as_deref(), Some("DAL2809"));
        }
        other => panic!("expected aircraft message, got {other:?}"),
    }

    assert!(matches!(
        messages[2],
        FeedMessage::StaleAircraft { ref icao, .. } if icao == "A062EF"
    ));

    match &messages[3] {
        FeedMessage::Heartbeat { stats, .. } => {
            assert_eq!(stats.decoded_frames, 12);
            assert_eq!(stats.aircraft_updates, 10);
        }
        other => panic!("expected heartbeat message, got {other:?}"),
    }

    let replayed = messages
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture serializes")
        .join("\n");
    let reparsed = replayed
        .lines()
        .map(|line| serde_json::from_str::<FeedMessage>(line).expect("replayed line parses"))
        .collect::<Vec<_>>();

    assert_eq!(messages, reparsed);
}

fn parse_fixture() -> Vec<FeedMessage> {
    FEED_REPLAY
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture line parses"))
        .collect()
}
