use rsdb::{AircraftSnapshot, FeedMessage};
use serde::Serialize;

const EARTH_RADIUS_KM: f64 = 6371.0;
const AIRPORT_MATCH_KM: f64 = 70.0;
const LOW_ALTITUDE_FT: i32 = 18_000;
const CLIMB_FPM: i32 = 256;
const DESCENT_FPM: i32 = -256;

#[derive(Debug, Serialize)]
pub(crate) struct RouteLookupResponse {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteLookup>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteLookup {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign_icao: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign_iata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<RouteAirport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<RouteAirport>,
    pub source: &'static str,
    pub source_label: &'static str,
    pub confidence: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteAirport {
    pub icao_code: &'static str,
    pub iata_code: &'static str,
    pub name: &'static str,
    pub municipality: &'static str,
    pub country_name: &'static str,
    pub country_iso_name: &'static str,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation: i32,
}

#[derive(Debug, Clone, Copy)]
struct Airport {
    icao: &'static str,
    iata: &'static str,
    name: &'static str,
    municipality: &'static str,
    country: &'static str,
    country_iso: &'static str,
    lat: f64,
    lon: f64,
    elevation_ft: i32,
}

#[derive(Debug, Clone)]
struct Observation {
    time_ms: u64,
    lat: f64,
    lon: f64,
    altitude_ft: Option<i32>,
    vertical_rate_fpm: Option<i32>,
}

#[derive(Debug, Clone)]
struct AirportCandidate {
    airport: RouteAirport,
    vertical_rate_fpm: Option<i32>,
}

pub(crate) fn route_lookup(
    messages: &[FeedMessage],
    icao: Option<&str>,
    callsign: Option<&str>,
) -> RouteLookupResponse {
    let normalized_icao = icao.map(normalize);
    let normalized_callsign = callsign.map(normalize);
    let mut observations = messages
        .iter()
        .filter_map(|message| aircraft_from_message(message))
        .filter(|aircraft| {
            aircraft_matches(
                aircraft,
                normalized_icao.as_deref(),
                normalized_callsign.as_deref(),
            )
        })
        .filter_map(observation_from_aircraft)
        .collect::<Vec<_>>();

    observations.sort_by_key(|observation| observation.time_ms);

    RouteLookupResponse {
        status: "ready",
        route: infer_route(&observations, normalized_callsign),
    }
}

fn aircraft_from_message(message: &FeedMessage) -> Option<&AircraftSnapshot> {
    match message {
        FeedMessage::Aircraft { aircraft, .. } => Some(aircraft),
        _ => None,
    }
}

fn aircraft_matches(
    aircraft: &AircraftSnapshot,
    icao: Option<&str>,
    callsign: Option<&str>,
) -> bool {
    let aircraft_icao = normalize(&aircraft.icao);
    let aircraft_callsign = aircraft.callsign.as_deref().map(normalize);
    icao.is_some_and(|value| value == aircraft_icao)
        || callsign.is_some_and(|value| aircraft_callsign.as_deref() == Some(value))
}

fn observation_from_aircraft(aircraft: &AircraftSnapshot) -> Option<Observation> {
    let lat = finite(aircraft.lat?)?;
    let lon = finite(aircraft.lon?)?;
    Some(Observation {
        time_ms: aircraft.position_last_seen_ms.unwrap_or(0),
        lat,
        lon,
        altitude_ft: aircraft.altitude_baro_ft.or(aircraft.altitude_geometric_ft),
        vertical_rate_fpm: aircraft.vertical_rate_fpm,
    })
}

fn infer_route(observations: &[Observation], callsign: Option<String>) -> Option<RouteLookup> {
    let candidates = observations
        .iter()
        .filter_map(nearest_airport_candidate)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    let mut origin = candidates
        .iter()
        .find(|candidate| {
            candidate
                .vertical_rate_fpm
                .is_some_and(|rate| rate >= CLIMB_FPM)
        })
        .cloned();
    let mut destination = candidates
        .iter()
        .rev()
        .find(|candidate| {
            candidate
                .vertical_rate_fpm
                .is_some_and(|rate| rate <= DESCENT_FPM)
        })
        .cloned();

    let first = candidates.first().cloned();
    let last = candidates.last().cloned();
    if origin.is_none()
        && destination.is_none()
        && let (Some(first), Some(last)) = (&first, &last)
        && first.airport.icao_code != last.airport.icao_code
    {
        origin = Some(first.clone());
        destination = Some(last.clone());
    }

    if origin.is_none() {
        origin = first.filter(|candidate| {
            candidate
                .vertical_rate_fpm
                .is_none_or(|rate| rate >= DESCENT_FPM)
        });
    }
    if destination.is_none() {
        destination = last.filter(|candidate| {
            candidate
                .vertical_rate_fpm
                .is_none_or(|rate| rate <= CLIMB_FPM)
        });
    }

    let origin = origin.map(|candidate| candidate.airport);
    let destination = destination
        .filter(|candidate| {
            origin
                .as_ref()
                .is_none_or(|origin| origin.icao_code != candidate.airport.icao_code)
        })
        .map(|candidate| candidate.airport);

    if origin.is_none() && destination.is_none() {
        return None;
    }

    let confidence = if origin.is_some() && destination.is_some() {
        "medium"
    } else {
        "low"
    };

    Some(RouteLookup {
        callsign: callsign.clone(),
        callsign_icao: callsign,
        callsign_iata: None,
        origin,
        destination,
        source: "rsdb_observed",
        source_label: "RSDB observed",
        confidence,
    })
}

fn nearest_airport_candidate(observation: &Observation) -> Option<AirportCandidate> {
    if observation
        .altitude_ft
        .is_some_and(|altitude_ft| altitude_ft > LOW_ALTITUDE_FT)
    {
        return None;
    }

    AIRPORTS
        .iter()
        .filter_map(|airport| {
            let distance_km =
                haversine_km(observation.lat, observation.lon, airport.lat, airport.lon);
            (distance_km <= AIRPORT_MATCH_KM).then_some((airport, distance_km))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(airport, _)| AirportCandidate {
            airport: route_airport(*airport),
            vertical_rate_fpm: observation.vertical_rate_fpm,
        })
}

fn route_airport(airport: Airport) -> RouteAirport {
    RouteAirport {
        icao_code: airport.icao,
        iata_code: airport.iata,
        name: airport.name,
        municipality: airport.municipality,
        country_name: airport.country,
        country_iso_name: airport.country_iso,
        latitude: airport.lat,
        longitude: airport.lon,
        elevation: airport.elevation_ft,
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let delta_phi = (lat2 - lat1).to_radians();
    let delta_lambda = (lon2 - lon1).to_radians();
    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    EARTH_RADIUS_KM * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

const AIRPORTS: &[Airport] = &[
    Airport {
        icao: "KSFO",
        iata: "SFO",
        name: "San Francisco International Airport",
        municipality: "San Francisco",
        country: "United States",
        country_iso: "US",
        lat: 37.6190,
        lon: -122.3750,
        elevation_ft: 13,
    },
    Airport {
        icao: "KOAK",
        iata: "OAK",
        name: "Metropolitan Oakland International Airport",
        municipality: "Oakland",
        country: "United States",
        country_iso: "US",
        lat: 37.7213,
        lon: -122.2210,
        elevation_ft: 9,
    },
    Airport {
        icao: "KSJC",
        iata: "SJC",
        name: "Norman Y. Mineta San Jose International Airport",
        municipality: "San Jose",
        country: "United States",
        country_iso: "US",
        lat: 37.3626,
        lon: -121.9290,
        elevation_ft: 62,
    },
    Airport {
        icao: "KSMF",
        iata: "SMF",
        name: "Sacramento International Airport",
        municipality: "Sacramento",
        country: "United States",
        country_iso: "US",
        lat: 38.6954,
        lon: -121.5908,
        elevation_ft: 27,
    },
    Airport {
        icao: "KSTS",
        iata: "STS",
        name: "Charles M. Schulz Sonoma County Airport",
        municipality: "Santa Rosa",
        country: "United States",
        country_iso: "US",
        lat: 38.5089,
        lon: -122.8129,
        elevation_ft: 129,
    },
    Airport {
        icao: "KAPC",
        iata: "APC",
        name: "Napa County Airport",
        municipality: "Napa",
        country: "United States",
        country_iso: "US",
        lat: 38.2132,
        lon: -122.2807,
        elevation_ft: 35,
    },
    Airport {
        icao: "KCCR",
        iata: "CCR",
        name: "Buchanan Field",
        municipality: "Concord",
        country: "United States",
        country_iso: "US",
        lat: 37.9897,
        lon: -122.0569,
        elevation_ft: 26,
    },
    Airport {
        icao: "KSQL",
        iata: "SQL",
        name: "San Carlos Airport",
        municipality: "San Carlos",
        country: "United States",
        country_iso: "US",
        lat: 37.5119,
        lon: -122.2495,
        elevation_ft: 5,
    },
    Airport {
        icao: "KHWD",
        iata: "HWD",
        name: "Hayward Executive Airport",
        municipality: "Hayward",
        country: "United States",
        country_iso: "US",
        lat: 37.6592,
        lon: -122.1225,
        elevation_ft: 52,
    },
    Airport {
        icao: "KLVK",
        iata: "LVK",
        name: "Livermore Municipal Airport",
        municipality: "Livermore",
        country: "United States",
        country_iso: "US",
        lat: 37.6934,
        lon: -121.8204,
        elevation_ft: 400,
    },
    Airport {
        icao: "KMRY",
        iata: "MRY",
        name: "Monterey Regional Airport",
        municipality: "Monterey",
        country: "United States",
        country_iso: "US",
        lat: 36.5870,
        lon: -121.8429,
        elevation_ft: 257,
    },
    Airport {
        icao: "KRNO",
        iata: "RNO",
        name: "Reno Tahoe International Airport",
        municipality: "Reno",
        country: "United States",
        country_iso: "US",
        lat: 39.4991,
        lon: -119.7681,
        elevation_ft: 4415,
    },
    Airport {
        icao: "KLAX",
        iata: "LAX",
        name: "Los Angeles International Airport",
        municipality: "Los Angeles",
        country: "United States",
        country_iso: "US",
        lat: 33.9425,
        lon: -118.4081,
        elevation_ft: 125,
    },
    Airport {
        icao: "KBUR",
        iata: "BUR",
        name: "Hollywood Burbank Airport",
        municipality: "Burbank",
        country: "United States",
        country_iso: "US",
        lat: 34.2007,
        lon: -118.3587,
        elevation_ft: 778,
    },
    Airport {
        icao: "KSNA",
        iata: "SNA",
        name: "John Wayne Airport",
        municipality: "Santa Ana",
        country: "United States",
        country_iso: "US",
        lat: 33.6757,
        lon: -117.8682,
        elevation_ft: 56,
    },
    Airport {
        icao: "KSAN",
        iata: "SAN",
        name: "San Diego International Airport",
        municipality: "San Diego",
        country: "United States",
        country_iso: "US",
        lat: 32.7338,
        lon: -117.1933,
        elevation_ft: 17,
    },
    Airport {
        icao: "KLAS",
        iata: "LAS",
        name: "Harry Reid International Airport",
        municipality: "Las Vegas",
        country: "United States",
        country_iso: "US",
        lat: 36.0801,
        lon: -115.1522,
        elevation_ft: 2181,
    },
    Airport {
        icao: "KPHX",
        iata: "PHX",
        name: "Phoenix Sky Harbor International Airport",
        municipality: "Phoenix",
        country: "United States",
        country_iso: "US",
        lat: 33.4343,
        lon: -112.0116,
        elevation_ft: 1135,
    },
    Airport {
        icao: "KSEA",
        iata: "SEA",
        name: "Seattle Tacoma International Airport",
        municipality: "Seattle",
        country: "United States",
        country_iso: "US",
        lat: 47.4490,
        lon: -122.3093,
        elevation_ft: 433,
    },
    Airport {
        icao: "KPDX",
        iata: "PDX",
        name: "Portland International Airport",
        municipality: "Portland",
        country: "United States",
        country_iso: "US",
        lat: 45.5887,
        lon: -122.5975,
        elevation_ft: 31,
    },
    Airport {
        icao: "KDEN",
        iata: "DEN",
        name: "Denver International Airport",
        municipality: "Denver",
        country: "United States",
        country_iso: "US",
        lat: 39.8617,
        lon: -104.6730,
        elevation_ft: 5431,
    },
    Airport {
        icao: "KEWR",
        iata: "EWR",
        name: "Newark Liberty International Airport",
        municipality: "New York",
        country: "United States",
        country_iso: "US",
        lat: 40.6925,
        lon: -74.1687,
        elevation_ft: 18,
    },
    Airport {
        icao: "PHNL",
        iata: "HNL",
        name: "Daniel K. Inouye International Airport",
        municipality: "Honolulu",
        country: "United States",
        country_iso: "US",
        lat: 21.3187,
        lon: -157.9224,
        elevation_ft: 13,
    },
    Airport {
        icao: "PHOG",
        iata: "OGG",
        name: "Kahului Airport",
        municipality: "Kahului",
        country: "United States",
        country_iso: "US",
        lat: 20.8986,
        lon: -156.4305,
        elevation_ft: 54,
    },
    Airport {
        icao: "PHKO",
        iata: "KOA",
        name: "Ellison Onizuka Kona International Airport",
        municipality: "Kailua-Kona",
        country: "United States",
        country_iso: "US",
        lat: 19.7388,
        lon: -156.0456,
        elevation_ft: 47,
    },
    Airport {
        icao: "PHLI",
        iata: "LIH",
        name: "Lihue Airport",
        municipality: "Lihue",
        country: "United States",
        country_iso: "US",
        lat: 21.9760,
        lon: -159.3389,
        elevation_ft: 153,
    },
    Airport {
        icao: "MMPR",
        iata: "PVR",
        name: "Licenciado Gustavo Diaz Ordaz International Airport",
        municipality: "Puerto Vallarta",
        country: "Mexico",
        country_iso: "MX",
        lat: 20.6801,
        lon: -105.2540,
        elevation_ft: 23,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_departure_origin_from_climb_near_sfo() {
        let aircraft = AircraftSnapshot {
            icao: "ADA167".to_owned(),
            callsign: Some("ASA1379".to_owned()),
            callsign_last_seen_ms: None,
            category: None,
            altitude_baro_ft: Some(4_500),
            altitude_geometric_ft: None,
            altitude_last_seen_ms: None,
            lat: Some(37.63),
            lon: Some(-122.38),
            distance_km: None,
            bearing_deg: None,
            seen_seconds_ago: None,
            position_status: rsdb::PositionStatus::Fresh,
            position_last_seen_ms: Some(1_000),
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
            vertical_rate_fpm: Some(1_024),
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
            last_seen_ms: 1_000,
            message_count: 1,
            last_type_code: None,
            last_decode_status: rsdb::DecodeStatus::Updated,
            last_raw: String::new(),
            raw_messages: Vec::new(),
        };
        let response = route_lookup(
            &[FeedMessage::aircraft(1_000, aircraft)],
            Some("ADA167"),
            Some("ASA1379"),
        );

        let route = response.route.expect("route hint");
        assert_eq!(route.origin.expect("origin").iata_code, "SFO");
        assert_eq!(route.source, "rsdb_observed");
    }
}
