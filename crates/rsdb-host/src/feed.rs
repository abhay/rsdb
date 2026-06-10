use serde::{Deserialize, Serialize};

use crate::{
    AircraftSnapshot, AircraftStore, FrameRecord, FrameRecordError, Protocol, RadioConfig,
    ReceiverHandle, ReceiverSite, SubmissionHealth, SubmissionStatus,
};

/// Current JSON feed schema version.
pub const FEED_SCHEMA_VERSION: u32 = 1;

/// Current API schema document version.
pub const API_SCHEMA_VERSION: u32 = 1;

pub const FEED_RECENT_MESSAGE_WINDOW_MS: u64 = 5 * 60 * 1_000;

/// Versioned JSON message emitted by live aircraft feeds.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FeedMessage {
    Snapshot {
        schema_version: u32,
        #[serde(default)]
        protocol: Protocol,
        now_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receiver: Option<ReceiverIdentity>,
        aircraft: Vec<AircraftSnapshot>,
    },
    Aircraft {
        schema_version: u32,
        #[serde(default)]
        protocol: Protocol,
        now_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receiver: Option<ReceiverIdentity>,
        aircraft: AircraftSnapshot,
    },
    StaleAircraft {
        schema_version: u32,
        #[serde(default)]
        protocol: Protocol,
        now_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receiver: Option<ReceiverIdentity>,
        icao: String,
    },
    Heartbeat {
        schema_version: u32,
        #[serde(default)]
        protocol: Protocol,
        now_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receiver: Option<ReceiverIdentity>,
        aircraft_count: usize,
        stats: FeedStats,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct ReceiverIdentity {
    pub id: String,
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<ReceiverHandle>,
}

impl ReceiverIdentity {
    #[must_use]
    pub fn new(id: String) -> Self {
        let handle = Some(ReceiverHandle::from_receiver_id(&id));
        Self {
            id,
            name: None,
            handle,
        }
    }

    #[must_use]
    pub fn named(id: String, name: String) -> Self {
        let handle = Some(ReceiverHandle::from_receiver_id(&id));
        Self {
            id,
            name: Some(name),
            handle,
        }
    }

    #[must_use]
    pub fn with_generated_handle(mut self) -> Self {
        if self.handle.is_none() {
            self.handle = Some(ReceiverHandle::from_receiver_id(&self.id));
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FeedStats {
    pub uptime_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_site: Option<ReceiverSite>,
    pub receiver_connected: bool,
    pub last_frame_ms: Option<u64>,
    pub last_usb_chunk_ms: Option<u64>,
    #[serde(default)]
    pub usb_chunks: u64,
    #[serde(default)]
    pub usb_chunks_per_second: f64,
    #[serde(default)]
    pub usb_bytes: u64,
    #[serde(default)]
    pub usb_bytes_per_second: u64,
    pub dropped_usb_chunks: u64,
    #[serde(default)]
    pub dropped_usb_chunk_ratio: f64,
    pub decoded_frames: u64,
    pub decoded_frames_per_second: f64,
    #[serde(default)]
    pub decoded_frames_per_megabyte: f64,
    pub aircraft_updates: u64,
    pub aircraft_updates_per_second: f64,
    #[serde(default)]
    pub aircraft_updates_per_frame: f64,
    pub stale_aircraft_removed: u64,
    pub last_error: Option<String>,
    pub websocket_clients: usize,
    #[serde(default)]
    pub submission: SubmissionHealth,
}

/// JSON status document returned by `/status.json`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ServiceStatus {
    pub schema_version: u32,
    pub now_ms: u64,
    #[serde(default)]
    pub radio: RadioConfig,
    pub uptime_ms: u64,
    pub receiver_connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ReceiverIdentity>,
    pub receiver_site: Option<ReceiverSite>,
    pub aircraft_count: usize,
    pub last_frame_ms: Option<u64>,
    pub last_usb_chunk_ms: Option<u64>,
    #[serde(default)]
    pub usb_chunks: u64,
    #[serde(default)]
    pub usb_chunks_per_second: f64,
    #[serde(default)]
    pub usb_bytes: u64,
    #[serde(default)]
    pub usb_bytes_per_second: u64,
    pub dropped_usb_chunks: u64,
    #[serde(default)]
    pub dropped_usb_chunk_ratio: f64,
    pub decoded_frames: u64,
    pub decoded_frames_per_second: f64,
    #[serde(default)]
    pub decoded_frames_per_megabyte: f64,
    pub aircraft_updates: u64,
    pub aircraft_updates_per_second: f64,
    #[serde(default)]
    pub aircraft_updates_per_frame: f64,
    pub stale_aircraft_removed: u64,
    pub last_error: Option<String>,
    pub websocket_clients: usize,
    #[serde(default)]
    pub persistence: PersistenceStatus,
    #[serde(default)]
    pub submission: SubmissionStatus,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FeedBootstrap {
    pub schema_version: u32,
    pub now_ms: u64,
    pub recent_message_window_ms: u64,
    pub snapshot: FeedMessage,
    pub recent_messages: Vec<FeedMessage>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct PersistenceStatus {
    pub enabled: bool,
    pub messages_written: u64,
    pub bytes_written: u64,
    pub current_feed_bytes: u64,
    pub feed_max_bytes: u64,
    pub rotations: u64,
    pub last_write_ms: Option<u64>,
    pub last_error: Option<String>,
}

/// Machine-readable contract for HTTP and WebSocket clients.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ApiSchema {
    pub schema_version: u32,
    pub feed_schema_version: u32,
    pub endpoints: Vec<ApiEndpoint>,
    pub websocket: WebSocketSchema,
    pub feed_messages: Vec<FeedMessageSchema>,
    pub aircraft_fields: Vec<FieldSchema>,
    pub status_fields: Vec<FieldSchema>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ApiEndpoint {
    pub path: String,
    pub method: String,
    pub response_type: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WebSocketSchema {
    pub path: String,
    pub message_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FeedMessageSchema {
    pub message_type: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FieldSchema {
    pub name: String,
    pub json_type: String,
    pub nullable: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameReplayConfig {
    pub protocol: Protocol,
    pub initial_now_ms: u64,
    pub stale_after_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub receiver_identity: Option<ReceiverIdentity>,
    pub receiver_site: Option<ReceiverSite>,
}

impl FrameReplayConfig {
    #[must_use]
    pub fn new(protocol: Protocol, initial_now_ms: u64) -> Self {
        Self {
            protocol,
            initial_now_ms,
            stale_after_ms: 0,
            heartbeat_interval_ms: 0,
            receiver_identity: None,
            receiver_site: None,
        }
    }
}

impl Default for FrameReplayConfig {
    fn default() -> Self {
        Self::new(Protocol::Adsb1090, 0)
    }
}

impl Default for FeedStats {
    fn default() -> Self {
        Self {
            uptime_ms: 0,
            receiver_site: None,
            receiver_connected: false,
            last_frame_ms: None,
            last_usb_chunk_ms: None,
            usb_chunks: 0,
            usb_chunks_per_second: 0.0,
            usb_bytes: 0,
            usb_bytes_per_second: 0,
            dropped_usb_chunks: 0,
            dropped_usb_chunk_ratio: 0.0,
            decoded_frames: 0,
            decoded_frames_per_second: 0.0,
            decoded_frames_per_megabyte: 0.0,
            aircraft_updates: 0,
            aircraft_updates_per_second: 0.0,
            aircraft_updates_per_frame: 0.0,
            stale_aircraft_removed: 0,
            last_error: None,
            websocket_clients: 0,
            submission: SubmissionHealth::default(),
        }
    }
}

impl FeedMessage {
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        match self {
            Self::Snapshot { schema_version, .. }
            | Self::Aircraft { schema_version, .. }
            | Self::StaleAircraft { schema_version, .. }
            | Self::Heartbeat { schema_version, .. } => *schema_version,
        }
    }

    #[must_use]
    pub const fn now_ms(&self) -> u64 {
        match self {
            Self::Snapshot { now_ms, .. }
            | Self::Aircraft { now_ms, .. }
            | Self::StaleAircraft { now_ms, .. }
            | Self::Heartbeat { now_ms, .. } => *now_ms,
        }
    }

    #[must_use]
    pub const fn protocol(&self) -> Protocol {
        match self {
            Self::Snapshot { protocol, .. }
            | Self::Aircraft { protocol, .. }
            | Self::StaleAircraft { protocol, .. }
            | Self::Heartbeat { protocol, .. } => *protocol,
        }
    }

    #[must_use]
    pub const fn is_supported_schema_version(&self) -> bool {
        self.schema_version() == FEED_SCHEMA_VERSION
    }

    #[must_use]
    pub const fn snapshot(now_ms: u64, aircraft: Vec<AircraftSnapshot>) -> Self {
        Self::snapshot_for_protocol(Protocol::Adsb1090, now_ms, aircraft)
    }

    #[must_use]
    pub const fn snapshot_for_protocol(
        protocol: Protocol,
        now_ms: u64,
        aircraft: Vec<AircraftSnapshot>,
    ) -> Self {
        Self::Snapshot {
            schema_version: FEED_SCHEMA_VERSION,
            protocol,
            now_ms,
            receiver: None,
            aircraft,
        }
    }

    #[must_use]
    pub fn aircraft(now_ms: u64, aircraft: AircraftSnapshot) -> Self {
        Self::aircraft_for_protocol(Protocol::Adsb1090, now_ms, aircraft)
    }

    #[must_use]
    pub fn aircraft_for_protocol(
        protocol: Protocol,
        now_ms: u64,
        aircraft: AircraftSnapshot,
    ) -> Self {
        Self::Aircraft {
            schema_version: FEED_SCHEMA_VERSION,
            protocol,
            now_ms,
            receiver: None,
            aircraft,
        }
    }

    #[must_use]
    pub const fn stale_aircraft(now_ms: u64, icao: String) -> Self {
        Self::stale_aircraft_for_protocol(Protocol::Adsb1090, now_ms, icao)
    }

    #[must_use]
    pub const fn stale_aircraft_for_protocol(
        protocol: Protocol,
        now_ms: u64,
        icao: String,
    ) -> Self {
        Self::StaleAircraft {
            schema_version: FEED_SCHEMA_VERSION,
            protocol,
            now_ms,
            receiver: None,
            icao,
        }
    }

    #[must_use]
    pub const fn heartbeat(now_ms: u64, aircraft_count: usize, stats: FeedStats) -> Self {
        Self::heartbeat_for_protocol(Protocol::Adsb1090, now_ms, aircraft_count, stats)
    }

    #[must_use]
    pub const fn heartbeat_for_protocol(
        protocol: Protocol,
        now_ms: u64,
        aircraft_count: usize,
        stats: FeedStats,
    ) -> Self {
        Self::Heartbeat {
            schema_version: FEED_SCHEMA_VERSION,
            protocol,
            now_ms,
            receiver: None,
            aircraft_count,
            stats,
        }
    }

    #[must_use]
    pub fn with_receiver(self, receiver: Option<ReceiverIdentity>) -> Self {
        match self {
            Self::Snapshot {
                schema_version,
                protocol,
                now_ms,
                aircraft,
                ..
            } => Self::Snapshot {
                schema_version,
                protocol,
                now_ms,
                receiver,
                aircraft,
            },
            Self::Aircraft {
                schema_version,
                protocol,
                now_ms,
                aircraft,
                ..
            } => Self::Aircraft {
                schema_version,
                protocol,
                now_ms,
                receiver,
                aircraft,
            },
            Self::StaleAircraft {
                schema_version,
                protocol,
                now_ms,
                icao,
                ..
            } => Self::StaleAircraft {
                schema_version,
                protocol,
                now_ms,
                receiver,
                icao,
            },
            Self::Heartbeat {
                schema_version,
                protocol,
                now_ms,
                aircraft_count,
                stats,
                ..
            } => Self::Heartbeat {
                schema_version,
                protocol,
                now_ms,
                receiver,
                aircraft_count,
                stats,
            },
        }
    }

    #[must_use]
    pub fn with_protocol(self, protocol: Protocol) -> Self {
        match self {
            Self::Snapshot {
                schema_version,
                now_ms,
                receiver,
                aircraft,
                ..
            } => Self::Snapshot {
                schema_version,
                protocol,
                now_ms,
                receiver,
                aircraft,
            },
            Self::Aircraft {
                schema_version,
                now_ms,
                receiver,
                aircraft,
                ..
            } => Self::Aircraft {
                schema_version,
                protocol,
                now_ms,
                receiver,
                aircraft,
            },
            Self::StaleAircraft {
                schema_version,
                now_ms,
                receiver,
                icao,
                ..
            } => Self::StaleAircraft {
                schema_version,
                protocol,
                now_ms,
                receiver,
                icao,
            },
            Self::Heartbeat {
                schema_version,
                now_ms,
                receiver,
                aircraft_count,
                stats,
                ..
            } => Self::Heartbeat {
                schema_version,
                protocol,
                now_ms,
                receiver,
                aircraft_count,
                stats,
            },
        }
    }

    #[must_use]
    pub fn receiver(&self) -> Option<&ReceiverIdentity> {
        match self {
            Self::Snapshot { receiver, .. }
            | Self::Aircraft { receiver, .. }
            | Self::StaleAircraft { receiver, .. }
            | Self::Heartbeat { receiver, .. } => receiver.as_ref(),
        }
    }
}

impl ServiceStatus {
    #[must_use]
    pub const fn is_supported_schema_version(&self) -> bool {
        self.schema_version == FEED_SCHEMA_VERSION
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn api_schema() -> ApiSchema {
    ApiSchema {
        schema_version: API_SCHEMA_VERSION,
        feed_schema_version: FEED_SCHEMA_VERSION,
        endpoints: vec![
            endpoint(
                "/",
                "text/html",
                "Browser viewer for live receiver state and aircraft.",
            ),
            endpoint(
                "/status.json",
                "ServiceStatus",
                "Current receiver identity, health, and aggregate feed statistics.",
            ),
            endpoint(
                "/aircraft.json",
                "FeedMessage::Snapshot",
                "Current aircraft snapshot using the same envelope as the WebSocket feed.",
            ),
            endpoint(
                "/bootstrap.json",
                "FeedBootstrap",
                "Initial UI/API state containing current aircraft and recent feed messages.",
            ),
            endpoint(
                "/schema.json",
                "ApiSchema",
                "Machine-readable contract for HTTP endpoints, WebSocket messages, and fields.",
            ),
            endpoint(
                "/history.ndjson",
                "FeedMessage NDJSON",
                "Current rolling persisted feed log when persistence is enabled.",
            ),
            endpoint(
                "/ws",
                "FeedMessage",
                "WebSocket stream of live feed messages.",
            ),
        ],
        websocket: WebSocketSchema {
            path: "/ws".to_owned(),
            message_types: vec![
                "snapshot".to_owned(),
                "aircraft".to_owned(),
                "stale_aircraft".to_owned(),
                "heartbeat".to_owned(),
            ],
        },
        feed_messages: vec![
            message_schema(
                "snapshot",
                "Full aircraft list for initial state or snapshot polling, with optional receiver identity.",
            ),
            message_schema(
                "aircraft",
                "One updated aircraft snapshot, with optional receiver identity.",
            ),
            message_schema(
                "stale_aircraft",
                "ICAO address removed after stale timeout, with optional receiver identity.",
            ),
            message_schema(
                "heartbeat",
                "Receiver statistics emitted periodically, with optional receiver identity.",
            ),
        ],
        aircraft_fields: aircraft_field_schema(),
        status_fields: status_field_schema(),
    }
}

fn endpoint(path: &str, response_type: &str, description: &str) -> ApiEndpoint {
    ApiEndpoint {
        path: path.to_owned(),
        method: "GET".to_owned(),
        response_type: response_type.to_owned(),
        description: description.to_owned(),
    }
}

fn message_schema(message_type: &str, description: &str) -> FeedMessageSchema {
    FeedMessageSchema {
        message_type: message_type.to_owned(),
        description: description.to_owned(),
    }
}

fn field(name: &str, json_type: &str, nullable: bool, description: &str) -> FieldSchema {
    FieldSchema {
        name: name.to_owned(),
        json_type: json_type.to_owned(),
        nullable,
        description: description.to_owned(),
    }
}

#[allow(clippy::too_many_lines)]
fn aircraft_field_schema() -> Vec<FieldSchema> {
    vec![
        field(
            "icao",
            "string",
            false,
            "Uppercase six-hex aircraft ICAO address.",
        ),
        field(
            "callsign",
            "string",
            true,
            "ADS-B callsign when aircraft identification is observed.",
        ),
        field(
            "callsign_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when callsign last updated.",
        ),
        field("category", "integer", true, "ADS-B aircraft category code."),
        field(
            "altitude_baro_ft",
            "integer",
            true,
            "Barometric altitude in feet.",
        ),
        field(
            "altitude_geometric_ft",
            "integer",
            true,
            "Geometric altitude in feet when transmitted.",
        ),
        field(
            "altitude_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when altitude last updated.",
        ),
        field(
            "lat",
            "number",
            true,
            "Decoded latitude in decimal degrees.",
        ),
        field(
            "lon",
            "number",
            true,
            "Decoded longitude in decimal degrees.",
        ),
        field(
            "distance_km",
            "number",
            true,
            "Horizontal receiver-relative range in kilometers.",
        ),
        field(
            "bearing_deg",
            "number",
            true,
            "Initial bearing from receiver to aircraft in degrees.",
        ),
        field(
            "seen_seconds_ago",
            "integer",
            true,
            "Age of the last aircraft update.",
        ),
        field(
            "position_status",
            "string",
            false,
            "Position freshness: unavailable, fresh, stale, or rejected_jump.",
        ),
        field(
            "position_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when position last updated.",
        ),
        field(
            "surveillance_status",
            "integer",
            true,
            "ADS-B surveillance status code.",
        ),
        field(
            "nic_supplement_b",
            "boolean",
            true,
            "ADS-B NIC supplement B flag.",
        ),
        field(
            "time_flag",
            "boolean",
            true,
            "ADS-B airborne position time flag.",
        ),
        field(
            "cpr_format",
            "string",
            true,
            "CPR frame format: even or odd.",
        ),
        field(
            "ground_speed_kt",
            "number",
            true,
            "Ground speed in knots for ADS-B ground-speed velocity subtypes.",
        ),
        field(
            "airspeed_kt",
            "number",
            true,
            "Airspeed in knots for ADS-B airspeed velocity subtypes.",
        ),
        field("track_deg", "number", true, "Ground track in degrees."),
        field(
            "heading_deg",
            "number",
            true,
            "Heading in degrees for airspeed subtypes.",
        ),
        field(
            "speed_type",
            "string",
            true,
            "Velocity source: ground_speed or airspeed.",
        ),
        field(
            "velocity_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when velocity last updated.",
        ),
        field(
            "vertical_rate_source",
            "string",
            true,
            "Vertical rate source: barometric or geometric.",
        ),
        field(
            "vertical_rate_fpm",
            "integer",
            true,
            "Vertical rate in feet per minute.",
        ),
        field(
            "aircraft_status_subtype",
            "integer",
            true,
            "ADS-B aircraft status subtype.",
        ),
        field(
            "aircraft_status_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when aircraft status last updated.",
        ),
        field(
            "emergency_state",
            "string",
            true,
            "Decoded ADS-B emergency state.",
        ),
        field(
            "emergency_state_code",
            "integer",
            true,
            "Raw ADS-B emergency state code.",
        ),
        field(
            "mode_a_identity_code",
            "integer",
            true,
            "Raw Mode A identity bits from aircraft status.",
        ),
        field(
            "target_state_subtype",
            "integer",
            true,
            "ADS-B target state and status subtype.",
        ),
        field(
            "target_state_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when target state last updated.",
        ),
        field(
            "operational_status_subtype",
            "integer",
            true,
            "ADS-B operational status subtype.",
        ),
        field(
            "operational_status_last_seen_ms",
            "integer",
            true,
            "Unix milliseconds when operational status last updated.",
        ),
        field(
            "capability_class_code",
            "integer",
            true,
            "ADS-B operational capability class bits.",
        ),
        field(
            "operational_mode_code",
            "integer",
            true,
            "ADS-B operational mode bits.",
        ),
        field(
            "adsb_version",
            "integer",
            true,
            "ADS-B version from operational status.",
        ),
        field(
            "nic_supplement_a",
            "boolean",
            true,
            "ADS-B NIC supplement A flag.",
        ),
        field(
            "nac_p",
            "integer",
            true,
            "Navigation accuracy category for position.",
        ),
        field(
            "geometric_vertical_accuracy",
            "integer",
            true,
            "Geometric vertical accuracy code.",
        ),
        field(
            "source_integrity_level",
            "integer",
            true,
            "Source integrity level code.",
        ),
        field(
            "baro_altitude_integrity",
            "boolean",
            true,
            "Barometric altitude integrity flag.",
        ),
        field(
            "horizontal_reference_direction",
            "boolean",
            true,
            "Horizontal reference direction flag.",
        ),
        field("sil_supplement", "boolean", true, "SIL supplement flag."),
        field(
            "last_seen_ms",
            "integer",
            false,
            "Unix milliseconds when any message was last seen.",
        ),
        field(
            "message_count",
            "integer",
            false,
            "Messages accepted into this aircraft state.",
        ),
        field(
            "last_type_code",
            "integer",
            true,
            "Last ADS-B type code observed.",
        ),
        field(
            "last_decode_status",
            "string",
            false,
            "Decode result for the latest ADS-B payload: unknown, updated, partial, rejected, or unsupported.",
        ),
        field(
            "last_raw",
            "string",
            false,
            "Last raw Mode S frame in uppercase hex.",
        ),
        field(
            "raw_messages",
            "array<string>",
            false,
            "Recent raw Mode S frames in oldest-to-newest order.",
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn status_field_schema() -> Vec<FieldSchema> {
    vec![
        field(
            "schema_version",
            "integer",
            false,
            "Feed schema version for this status document.",
        ),
        field(
            "now_ms",
            "integer",
            false,
            "Server Unix time in milliseconds.",
        ),
        field(
            "uptime_ms",
            "integer",
            false,
            "Service uptime in milliseconds.",
        ),
        field(
            "radio",
            "object",
            false,
            "Configured protocol, center frequency, and sample rate for the active radio job.",
        ),
        field(
            "receiver_connected",
            "boolean",
            false,
            "Whether the USB receiver stream is connected.",
        ),
        field(
            "receiver",
            "object",
            true,
            "Configured receiver identity for source attribution.",
        ),
        field(
            "receiver_site",
            "object",
            true,
            "Configured receiver latitude and longitude.",
        ),
        field(
            "aircraft_count",
            "integer",
            false,
            "Current aircraft count.",
        ),
        field(
            "last_frame_ms",
            "integer",
            true,
            "Unix milliseconds when the last frame updated state.",
        ),
        field(
            "last_usb_chunk_ms",
            "integer",
            true,
            "Unix milliseconds when the last USB sample chunk arrived.",
        ),
        field(
            "usb_chunks",
            "integer",
            false,
            "USB sample chunks received since service start.",
        ),
        field(
            "usb_chunks_per_second",
            "number",
            false,
            "Average USB sample chunk rate since service start.",
        ),
        field(
            "usb_bytes",
            "integer",
            false,
            "USB sample bytes received since service start.",
        ),
        field(
            "usb_bytes_per_second",
            "integer",
            false,
            "Average USB sample byte rate since service start.",
        ),
        field(
            "dropped_usb_chunks",
            "integer",
            false,
            "USB chunks dropped by the capture queue.",
        ),
        field(
            "dropped_usb_chunk_ratio",
            "number",
            false,
            "Dropped USB chunks divided by received plus dropped USB chunks.",
        ),
        field(
            "decoded_frames",
            "integer",
            false,
            "Decoded Mode S frame count since service start.",
        ),
        field(
            "decoded_frames_per_second",
            "number",
            false,
            "Average decoded frame rate since service start.",
        ),
        field(
            "decoded_frames_per_megabyte",
            "number",
            false,
            "Decoded Mode S frames per decimal megabyte of USB sample data.",
        ),
        field(
            "aircraft_updates",
            "integer",
            false,
            "Aircraft state update count since service start.",
        ),
        field(
            "aircraft_updates_per_second",
            "number",
            false,
            "Average aircraft update rate since service start.",
        ),
        field(
            "aircraft_updates_per_frame",
            "number",
            false,
            "Aircraft state updates per decoded Mode S frame.",
        ),
        field(
            "stale_aircraft_removed",
            "integer",
            false,
            "Aircraft removed by stale timeout.",
        ),
        field(
            "last_error",
            "string",
            true,
            "Last receiver error while retrying.",
        ),
        field(
            "websocket_clients",
            "integer",
            false,
            "Currently connected WebSocket clients.",
        ),
        field(
            "persistence",
            "object",
            false,
            "Persistence state for the rolling feed history.",
        ),
        field(
            "submission",
            "object",
            false,
            "Signed aggregate submission state for this receiver.",
        ),
    ]
}

#[must_use]
pub fn enrich_aircraft_snapshot(
    aircraft: AircraftSnapshot,
    receiver_site: Option<&ReceiverSite>,
    now_ms: u64,
) -> AircraftSnapshot {
    aircraft.enriched_for_receiver(receiver_site, now_ms)
}

#[must_use]
pub fn enrich_aircraft_snapshots(
    aircraft: Vec<AircraftSnapshot>,
    receiver_site: Option<&ReceiverSite>,
    now_ms: u64,
) -> Vec<AircraftSnapshot> {
    aircraft
        .into_iter()
        .map(|aircraft| enrich_aircraft_snapshot(aircraft, receiver_site, now_ms))
        .collect()
}

/// Replays decoded frame records through aircraft state and emits feed messages.
///
/// # Errors
///
/// Returns an error when any frame record has an unsupported schema version or
/// invalid raw frame data.
pub fn replay_frame_records(
    records: &[FrameRecord],
    config: &FrameReplayConfig,
) -> Result<Vec<FeedMessage>, FrameRecordError> {
    let mut store = AircraftStore::default();
    let mut counters = FrameReplayCounters::default();
    let mut last_heartbeat_ms = config.initial_now_ms;
    let receiver_identity = config.receiver_identity.clone();
    let receiver_site = config.receiver_site.as_ref();
    let mut messages = vec![
        FeedMessage::snapshot_for_protocol(
            config.protocol,
            config.initial_now_ms,
            enrich_aircraft_snapshots(store.snapshots(), receiver_site, config.initial_now_ms),
        )
        .with_receiver(receiver_identity.clone()),
    ];

    for record in records {
        record.validate_protocol(config.protocol)?;
        let snapshot = store.update_frame_record(record)?;
        counters.decoded_frames += 1;

        if let Some(snapshot) = snapshot {
            counters.aircraft_updates += 1;
            counters.last_frame_ms = Some(record.now_ms);
            messages.push(
                FeedMessage::aircraft_for_protocol(
                    config.protocol,
                    record.now_ms,
                    enrich_aircraft_snapshot(snapshot, receiver_site, record.now_ms),
                )
                .with_receiver(receiver_identity.clone()),
            );
        }

        for removed in store.evict_stale(record.now_ms, config.stale_after_ms) {
            counters.stale_aircraft_removed += 1;
            messages.push(
                FeedMessage::stale_aircraft_for_protocol(
                    config.protocol,
                    record.now_ms,
                    removed.icao,
                )
                .with_receiver(receiver_identity.clone()),
            );
        }

        if config.heartbeat_interval_ms != 0
            && record.now_ms.saturating_sub(last_heartbeat_ms) >= config.heartbeat_interval_ms
        {
            let mut stats = counters.stats(record.now_ms, config.initial_now_ms);
            stats.receiver_site.clone_from(&config.receiver_site);
            messages.push(
                FeedMessage::heartbeat_for_protocol(
                    config.protocol,
                    record.now_ms,
                    store.aircraft_count(),
                    stats,
                )
                .with_receiver(receiver_identity.clone()),
            );
            last_heartbeat_ms = record.now_ms;
        }
    }

    Ok(messages)
}

#[derive(Debug, Clone, Copy, Default)]
struct FrameReplayCounters {
    decoded_frames: u64,
    aircraft_updates: u64,
    stale_aircraft_removed: u64,
    last_frame_ms: Option<u64>,
}

impl FrameReplayCounters {
    fn stats(self, now_ms: u64, started_ms: u64) -> FeedStats {
        let uptime_ms = now_ms.saturating_sub(started_ms);

        FeedStats {
            uptime_ms,
            receiver_site: None,
            receiver_connected: true,
            last_frame_ms: self.last_frame_ms,
            last_usb_chunk_ms: None,
            usb_chunks: 0,
            usb_chunks_per_second: 0.0,
            usb_bytes: 0,
            usb_bytes_per_second: 0,
            dropped_usb_chunks: 0,
            dropped_usb_chunk_ratio: 0.0,
            decoded_frames: self.decoded_frames,
            decoded_frames_per_second: replay_rate_per_second(self.decoded_frames, uptime_ms),
            decoded_frames_per_megabyte: 0.0,
            aircraft_updates: self.aircraft_updates,
            aircraft_updates_per_second: replay_rate_per_second(self.aircraft_updates, uptime_ms),
            aircraft_updates_per_frame: replay_ratio_per_frame(
                self.aircraft_updates,
                self.decoded_frames,
            ),
            stale_aircraft_removed: self.stale_aircraft_removed,
            last_error: None,
            websocket_clients: 0,
            submission: SubmissionHealth::default(),
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn replay_rate_per_second(count: u64, elapsed_ms: u64) -> f64 {
    if elapsed_ms == 0 {
        return 0.0;
    }

    let rate = count as f64 * 1_000.0 / elapsed_ms as f64;
    (rate * 10.0).round() / 10.0
}

#[allow(clippy::cast_precision_loss)]
fn replay_ratio_per_frame(numerator: u64, frames: u64) -> f64 {
    if frames == 0 {
        return 0.0;
    }

    let ratio = numerator as f64 / frames as f64;
    (ratio * 1_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{CprFormat, EmergencyState, PositionStatus, SpeedType, VerticalRateSource};

    #[test]
    fn serializes_snapshot_envelope() {
        let value = serde_json::to_value(FeedMessage::snapshot(42, Vec::new())).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "snapshot",
                "schema_version": 1,
                "protocol": "adsb1090",
                "now_ms": 42,
                "aircraft": [],
            })
        );
    }

    #[test]
    fn serializes_status_endpoint_contract() {
        let value = serde_json::to_value(sample_status()).unwrap();

        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "now_ms": 101,
                "radio": {
                    "protocol": "adsb1090",
                    "center_frequency_hz": 1_090_000_000,
                    "sample_rate_hz": 2_000_000,
                },
                "uptime_ms": 12345,
                "receiver_connected": true,
                "receiver": {
                    "id": "sf-rsdb-pi",
                    "name": "SF",
                    "handle": {
                        "base": "alert-lime-burst",
                        "suffix": "674710c9",
                    },
                },
                "receiver_site": {
                    "name": "SF",
                    "lat": 37.753,
                    "lon": -122.447,
                },
                "aircraft_count": 3,
                "last_frame_ms": 98,
                "last_usb_chunk_ms": 97,
                "usb_chunks": 2000,
                "usb_chunks_per_second": 162.0,
                "usb_bytes": 256_000_000,
                "usb_bytes_per_second": 20_737_140,
                "dropped_usb_chunks": 2,
                "dropped_usb_chunk_ratio": 0.001,
                "decoded_frames": 500,
                "decoded_frames_per_second": 40.5,
                "decoded_frames_per_megabyte": 2.0,
                "aircraft_updates": 450,
                "aircraft_updates_per_second": 36.4,
                "aircraft_updates_per_frame": 0.9,
                "stale_aircraft_removed": 7,
                "last_error": null,
                "websocket_clients": 2,
                "persistence": {
                    "enabled": true,
                    "messages_written": 12,
                    "bytes_written": 4096,
                    "current_feed_bytes": 4096,
                    "feed_max_bytes": 100_000_000,
                    "rotations": 1,
                    "last_write_ms": 100,
                    "last_error": null,
                },
                "submission": sample_submission_status_json(),
            })
        );
    }

    #[test]
    fn reports_feed_schema_version_support() {
        let message = FeedMessage::snapshot(42, Vec::new());

        assert_eq!(message.schema_version(), FEED_SCHEMA_VERSION);
        assert!(message.is_supported_schema_version());

        let status = sample_status();
        assert!(status.is_supported_schema_version());
    }

    #[test]
    fn serializes_api_schema_contract() {
        let schema = api_schema();
        let value = serde_json::to_value(&schema).unwrap();

        assert_eq!(schema.schema_version, API_SCHEMA_VERSION);
        assert_eq!(schema.feed_schema_version, FEED_SCHEMA_VERSION);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["feed_schema_version"], 1);
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| endpoint.path == "/schema.json")
        );
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| endpoint.path == "/bootstrap.json")
        );
        assert!(
            schema
                .websocket
                .message_types
                .iter()
                .any(|value| value == "aircraft")
        );
        assert!(
            schema
                .aircraft_fields
                .iter()
                .any(|field| field.name == "emergency_state" && field.nullable)
        );
        assert!(
            schema
                .status_fields
                .iter()
                .any(|field| field.name == "receiver_connected" && !field.nullable)
        );
        assert!(
            schema
                .status_fields
                .iter()
                .any(|field| field.name == "receiver" && field.nullable)
        );
        assert!(
            schema
                .status_fields
                .iter()
                .any(|field| field.name == "decoded_frames_per_megabyte" && !field.nullable)
        );
    }

    #[test]
    fn serializes_enriched_snapshot_aircraft_fields() {
        let value =
            serde_json::to_value(FeedMessage::snapshot(42, vec![sample_aircraft()])).unwrap();
        let expected = serde_json::from_str::<serde_json::Value>(
            r#"{
                "icao": "A062EF",
                "callsign": "DAL2809",
                "callsign_last_seen_ms": 1,
                "category": 3,
                "altitude_baro_ft": 10900,
                "altitude_geometric_ft": 11100,
                "altitude_last_seen_ms": 2,
                "lat": 37.631153,
                "lon": -122.38871,
                "distance_km": 1.8,
                "bearing_deg": 319.5,
                "seen_seconds_ago": 0,
                "position_status": "fresh",
                "position_last_seen_ms": 41,
                "surveillance_status": 0,
                "nic_supplement_b": true,
                "time_flag": false,
                "cpr_format": "even",
                "ground_speed_kt": 261.0,
                "airspeed_kt": null,
                "track_deg": 154.1,
                "heading_deg": null,
                "speed_type": "ground_speed",
                "velocity_last_seen_ms": 3,
                "vertical_rate_source": "barometric",
                "vertical_rate_fpm": -1024,
                "aircraft_status_subtype": 1,
                "aircraft_status_last_seen_ms": 4,
                "emergency_state": "no_communications",
                "emergency_state_code": 4,
                "mode_a_identity_code": 0,
                "target_state_subtype": 1,
                "target_state_last_seen_ms": 5,
                "operational_status_subtype": 0,
                "operational_status_last_seen_ms": 6,
                "capability_class_code": 8960,
                "operational_mode_code": 1536,
                "adsb_version": 2,
                "nic_supplement_a": false,
                "nac_p": 10,
                "geometric_vertical_accuracy": 1,
                "source_integrity_level": 3,
                "baro_altitude_integrity": true,
                "horizontal_reference_direction": false,
                "sil_supplement": true,
                "last_seen_ms": 42,
                "message_count": 8,
                "last_type_code": 29,
                "last_decode_status": "updated",
                "last_raw": "8DA062EFEA15984C015C08F76FB3",
                "raw_messages": ["8DA062EFEA15984C015C08F76FB3"]
            }"#,
        )
        .unwrap();

        assert_eq!(value["aircraft"][0], expected);
    }

    #[test]
    fn serializes_aircraft_endpoint_contract() {
        let value =
            serde_json::to_value(FeedMessage::snapshot(42, vec![sample_aircraft()])).unwrap();

        assert_eq!(value["type"], "snapshot");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["now_ms"], 42);
        assert_eq!(value["aircraft"].as_array().unwrap().len(), 1);
        assert_eq!(value["aircraft"][0]["icao"], "A062EF");
        assert_eq!(value["aircraft"][0]["distance_km"], 1.8);
        assert_eq!(value["aircraft"][0]["seen_seconds_ago"], 0);
    }

    #[test]
    fn serializes_aircraft_envelope() {
        let value = serde_json::to_value(FeedMessage::aircraft(43, sample_aircraft())).unwrap();

        assert_eq!(value["type"], "aircraft");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["protocol"], "adsb1090");
        assert_eq!(value["now_ms"], 43);
        assert_eq!(value["aircraft"]["icao"], "A062EF");
        assert_eq!(value["aircraft"]["adsb_version"], 2);
        assert_eq!(value["aircraft"]["position_status"], "fresh");
    }

    #[test]
    fn serializes_receiver_identity_on_feed_messages() {
        let message = FeedMessage::aircraft(43, sample_aircraft())
            .with_receiver(Some(sample_receiver_identity()));
        let value = serde_json::to_value(&message).unwrap();

        assert_eq!(value["receiver"]["id"], "sf-rsdb-pi");
        assert_eq!(value["receiver"]["name"], "SF");
        assert_eq!(value["receiver"]["handle"]["base"], "alert-lime-burst");
        assert_eq!(value["receiver"]["handle"]["suffix"], "674710c9");
        assert_eq!(message.receiver(), Some(&sample_receiver_identity()));
    }

    #[test]
    fn parses_receiver_identity_without_handle() {
        let receiver = serde_json::from_value::<ReceiverIdentity>(json!({
            "id": "old-rsdb-pi",
            "name": "Old",
        }))
        .unwrap();

        assert_eq!(receiver.id, "old-rsdb-pi");
        assert_eq!(receiver.name.as_deref(), Some("Old"));
        assert_eq!(receiver.handle, None);
    }

    #[test]
    fn serializes_stale_aircraft_envelope() {
        let value =
            serde_json::to_value(FeedMessage::stale_aircraft(44, "A062EF".to_owned())).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "stale_aircraft",
                "schema_version": 1,
                "protocol": "adsb1090",
                "now_ms": 44,
                "icao": "A062EF",
            })
        );
    }

    #[test]
    fn serializes_heartbeat_envelope() {
        let value = serde_json::to_value(FeedMessage::heartbeat(99, 3, sample_stats())).unwrap();

        assert_eq!(
            value,
            json!({
                "type": "heartbeat",
                "schema_version": 1,
                "protocol": "adsb1090",
                "now_ms": 99,
                "aircraft_count": 3,
                "stats": {
                    "uptime_ms": 12345,
                    "receiver_site": {
                        "name": "SF",
                        "lat": 37.753,
                        "lon": -122.447,
                    },
                    "receiver_connected": true,
                    "last_frame_ms": 98,
                    "last_usb_chunk_ms": 97,
                    "usb_chunks": 2000,
                    "usb_chunks_per_second": 162.0,
                    "usb_bytes": 256_000_000,
                    "usb_bytes_per_second": 20_737_140,
                    "dropped_usb_chunks": 2,
                    "dropped_usb_chunk_ratio": 0.001,
                    "decoded_frames": 500,
                    "decoded_frames_per_second": 40.5,
                    "decoded_frames_per_megabyte": 2.0,
                    "aircraft_updates": 450,
                    "aircraft_updates_per_second": 36.4,
                    "aircraft_updates_per_frame": 0.9,
                    "stale_aircraft_removed": 7,
                    "last_error": null,
                    "websocket_clients": 2,
                    "submission": sample_submission_health_json(),
                },
            })
        );
    }

    #[test]
    fn round_trips_feed_message_contract() {
        let message = FeedMessage::aircraft(43, sample_aircraft());
        let encoded = serde_json::to_string(&message).unwrap();
        let decoded = serde_json::from_str::<FeedMessage>(&encoded).unwrap();

        assert_eq!(decoded, message);
        assert!(decoded.is_supported_schema_version());
    }

    fn sample_status() -> ServiceStatus {
        ServiceStatus {
            schema_version: FEED_SCHEMA_VERSION,
            now_ms: 101,
            radio: RadioConfig::default(),
            uptime_ms: 12_345,
            receiver_connected: true,
            receiver: Some(sample_receiver_identity()),
            receiver_site: Some(ReceiverSite {
                name: Some("SF".to_owned()),
                lat: 37.753,
                lon: -122.447,
            }),
            aircraft_count: 3,
            last_frame_ms: Some(98),
            last_usb_chunk_ms: Some(97),
            usb_chunks: 2_000,
            usb_chunks_per_second: 162.0,
            usb_bytes: 256_000_000,
            usb_bytes_per_second: 20_737_140,
            dropped_usb_chunks: 2,
            dropped_usb_chunk_ratio: 0.001,
            decoded_frames: 500,
            decoded_frames_per_second: 40.5,
            decoded_frames_per_megabyte: 2.0,
            aircraft_updates: 450,
            aircraft_updates_per_second: 36.4,
            aircraft_updates_per_frame: 0.9,
            stale_aircraft_removed: 7,
            last_error: None,
            websocket_clients: 2,
            persistence: sample_persistence(),
            submission: sample_submission(),
        }
    }

    fn sample_stats() -> FeedStats {
        FeedStats {
            uptime_ms: 12_345,
            receiver_site: Some(ReceiverSite {
                name: Some("SF".to_owned()),
                lat: 37.753,
                lon: -122.447,
            }),
            receiver_connected: true,
            last_frame_ms: Some(98),
            last_usb_chunk_ms: Some(97),
            usb_chunks: 2_000,
            usb_chunks_per_second: 162.0,
            usb_bytes: 256_000_000,
            usb_bytes_per_second: 20_737_140,
            dropped_usb_chunks: 2,
            dropped_usb_chunk_ratio: 0.001,
            decoded_frames: 500,
            decoded_frames_per_second: 40.5,
            decoded_frames_per_megabyte: 2.0,
            aircraft_updates: 450,
            aircraft_updates_per_second: 36.4,
            aircraft_updates_per_frame: 0.9,
            stale_aircraft_removed: 7,
            last_error: None,
            websocket_clients: 2,
            submission: sample_submission().health(),
        }
    }

    fn sample_persistence() -> PersistenceStatus {
        PersistenceStatus {
            enabled: true,
            messages_written: 12,
            bytes_written: 4_096,
            current_feed_bytes: 4_096,
            feed_max_bytes: 100_000_000,
            rotations: 1,
            last_write_ms: Some(100),
            last_error: None,
        }
    }

    fn sample_submission() -> SubmissionStatus {
        SubmissionStatus {
            enabled: true,
            urls: vec!["http://aggregate.local/submit".to_owned()],
            targets: vec![crate::SubmissionTargetStatus {
                url: "http://aggregate.local/submit".to_owned(),
                delivered: 9,
                failed_attempts: 1,
                outbox_pending: 1,
                last_delivered_ms: Some(100),
                last_error: None,
            }],
            received: 11,
            signed: 10,
            delivered: 9,
            failed_attempts: 1,
            outbox_enabled: true,
            outbox_queued: 10,
            outbox_pending: 1,
            outbox_delivered: 9,
            outbox_dropped: 0,
            last_queued_ms: Some(99),
            last_delivered_ms: Some(100),
            last_error: None,
        }
    }

    fn sample_submission_status_json() -> Value {
        json!({
            "enabled": true,
            "urls": ["http://aggregate.local/submit"],
            "targets": [
                {
                    "url": "http://aggregate.local/submit",
                    "delivered": 9,
                    "failed_attempts": 1,
                    "outbox_pending": 1,
                    "last_delivered_ms": 100,
                    "last_error": null,
                },
            ],
            "received": 11,
            "signed": 10,
            "delivered": 9,
            "failed_attempts": 1,
            "outbox_enabled": true,
            "outbox_queued": 10,
            "outbox_pending": 1,
            "outbox_delivered": 9,
            "outbox_dropped": 0,
            "last_queued_ms": 99,
            "last_delivered_ms": 100,
            "last_error": null,
        })
    }

    fn sample_submission_health_json() -> Value {
        json!({
            "enabled": true,
            "delivered": 9,
            "outbox_pending": 1,
            "has_error": false,
            "target_count": 1,
            "targets_with_error": 0,
        })
    }

    fn sample_receiver_identity() -> ReceiverIdentity {
        ReceiverIdentity::named("sf-rsdb-pi".to_owned(), "SF".to_owned())
    }

    fn sample_aircraft() -> AircraftSnapshot {
        AircraftSnapshot {
            icao: "A062EF".to_owned(),
            callsign: Some("DAL2809".to_owned()),
            callsign_last_seen_ms: Some(1),
            category: Some(3),
            altitude_baro_ft: Some(10_900),
            altitude_geometric_ft: Some(11_100),
            altitude_last_seen_ms: Some(2),
            lat: Some(37.631_153),
            lon: Some(-122.388_710),
            distance_km: Some(1.8),
            bearing_deg: Some(319.5),
            seen_seconds_ago: Some(0),
            position_status: PositionStatus::Fresh,
            position_last_seen_ms: Some(41),
            surveillance_status: Some(0),
            nic_supplement_b: Some(true),
            time_flag: Some(false),
            cpr_format: Some(CprFormat::Even),
            ground_speed_kt: Some(261.0),
            airspeed_kt: None,
            track_deg: Some(154.1),
            heading_deg: None,
            speed_type: Some(SpeedType::GroundSpeed),
            velocity_last_seen_ms: Some(3),
            vertical_rate_source: Some(VerticalRateSource::Barometric),
            vertical_rate_fpm: Some(-1_024),
            aircraft_status_subtype: Some(1),
            aircraft_status_last_seen_ms: Some(4),
            emergency_state: Some(EmergencyState::NoCommunications),
            emergency_state_code: Some(4),
            mode_a_identity_code: Some(0),
            target_state_subtype: Some(1),
            target_state_last_seen_ms: Some(5),
            operational_status_subtype: Some(0),
            operational_status_last_seen_ms: Some(6),
            capability_class_code: Some(0x2300),
            operational_mode_code: Some(0x0600),
            adsb_version: Some(2),
            nic_supplement_a: Some(false),
            nac_p: Some(10),
            geometric_vertical_accuracy: Some(1),
            source_integrity_level: Some(3),
            baro_altitude_integrity: Some(true),
            horizontal_reference_direction: Some(false),
            sil_supplement: Some(true),
            last_seen_ms: 42,
            message_count: 8,
            last_type_code: Some(29),
            last_decode_status: crate::DecodeStatus::Updated,
            last_raw: "8DA062EFEA15984C015C08F76FB3".to_owned(),
            raw_messages: vec!["8DA062EFEA15984C015C08F76FB3".to_owned()],
        }
    }
}
