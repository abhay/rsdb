#![forbid(unsafe_code)]

pub mod aggregate;
pub mod audit;
pub mod capture;
pub mod feed;
pub mod names;
pub mod protocol;
pub mod signing;
pub mod state;
pub mod submission;

pub use aggregate::{
    AGGREGATE_SCHEMA_VERSION, AGGREGATE_STORE_PERSISTENCE_SCHEMA_VERSION,
    AggregateAircraftLifecycle, AggregateAircraftSnapshot, AggregateBootstrap,
    AggregateIngestResult, AggregatePersistenceStatus, AggregateReceiverSummary, AggregateSnapshot,
    AggregateStatus, AggregateStore, AggregateStoreError, AggregateStorePersistence,
    AggregateStorePersistenceError, aggregate_api_schema,
};
pub use audit::{CodeCount, FrameAuditReport, NameCount, audit_frame_records, audit_frames};
pub use capture::{
    FRAME_RECORD_BATCH_SCHEMA_VERSION, FRAME_RECORD_SCHEMA_VERSION, FrameRecord, FrameRecordBatch,
    FrameRecordError, FrameRecordSequenceValidator, FrameSignalMetrics,
    IQ_CAPTURE_RECORD_SCHEMA_VERSION, IqCaptureRecord, IqCaptureRecordError, IqChunkMetrics,
    ModesFrameDecoder, iq_chunk_metrics, replay_iq_capture_records, validate_frame_record_sequence,
};
pub use feed::{
    API_SCHEMA_VERSION, ApiEndpoint, ApiSchema, FEED_RECENT_MESSAGE_WINDOW_MS, FEED_SCHEMA_VERSION,
    FeedBootstrap, FeedMessage, FeedMessageSchema, FeedStats, FieldSchema, FrameReplayConfig,
    PersistenceStatus, ReceiverIdentity, ServiceStatus, WebSocketSchema, api_schema,
    enrich_aircraft_snapshot, enrich_aircraft_snapshots, replay_frame_records,
};
pub use names::ReceiverHandle;
pub use protocol::{Protocol, ProtocolParseError, RadioConfig};
pub use rsdb_core::{
    AdsbMessage, AirbornePosition, AirborneVelocity, AircraftIdentification,
    AircraftOperationalStatus, AircraftStatus, CprFormat, DecodedFrame, DecodedFrameSignal,
    DemodConfig, DownlinkFormat, EmergencyState, ExtendedSquitter, Frame, FrameError, FrameLength,
    IcaoAddress, LONG_FRAME_TOTAL_SAMPLES, MODES_SAMPLE_RATE_HZ, ModeSChecksum, SpeedType,
    TargetStateAndStatus, VerticalRateSource, crc24_modes, decode_frames_from_iq,
    decode_frames_from_magnitudes, unsigned_iq_to_magnitudes,
};
pub use signing::{
    ReceiverAllowlist, SIGNED_SUBMISSION_SCHEMA_VERSION, SignatureAlgorithm, SignedSubmission,
    SubmissionPayload, SubmissionSigner, SubmissionSigningError, SubmissionVerificationError,
    receiver_id_from_ed25519_public_key_hex, receiver_id_from_ed25519_secret_hex,
};
pub use state::{AircraftSnapshot, AircraftStore, DecodeStatus, PositionStatus, ReceiverSite};
pub use submission::{
    OutboxAppend, OutboxLoad, PendingSubmission, SubmissionHealth, SubmissionOutbox,
    SubmissionOutboxConfig, SubmissionOutboxError, SubmissionStatus, SubmissionTargetStatus,
};
