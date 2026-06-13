use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    AircraftSnapshot, AircraftStore, FeedMessage, FeedStats, FrameRecordBatch, Protocol,
    ReceiverIdentity, ReceiverSite, SubmissionHealth, enrich_aircraft_snapshot,
    feed::{
        API_SCHEMA_VERSION, ApiEndpoint, ApiSchema, FeedMessageSchema, FieldSchema, WebSocketSchema,
    },
};

pub const AGGREGATE_SCHEMA_VERSION: u32 = 1;
pub const AGGREGATE_STORE_PERSISTENCE_SCHEMA_VERSION: u32 = 1;
pub const AGGREGATE_RECENT_FEED_WINDOW_MS: u64 = 5 * 60 * 1_000;
pub const AGGREGATE_AIRCRAFT_STALE_AFTER_MS: u64 = 10_000;
pub const AGGREGATE_AIRCRAFT_EXPIRED_AFTER_MS: u64 = 60_000;

#[must_use]
pub fn aggregate_api_schema() -> ApiSchema {
    ApiSchema {
        schema_version: API_SCHEMA_VERSION,
        feed_schema_version: AGGREGATE_SCHEMA_VERSION,
        endpoints: vec![
            aggregate_endpoint(
                "/status.json",
                "GET",
                "AggregateStatus",
                "Current aggregate write status, receiver counts, and ingest counters.",
            ),
            aggregate_endpoint(
                "/aircraft.json",
                "GET",
                "AggregateSnapshot",
                "Receiver-scoped aircraft snapshots with aggregate lifecycle metadata.",
            ),
            aggregate_endpoint(
                "/bootstrap.json",
                "GET",
                "AggregateBootstrap",
                "Initial UI/API state containing current aircraft and recent feed messages.",
            ),
            aggregate_endpoint(
                "/receivers.json",
                "GET",
                "AggregateReceiverSummary[]",
                "Per-receiver aggregate summaries.",
            ),
            aggregate_endpoint(
                "/route-lookup.json",
                "GET",
                "RouteLookupResponse",
                "Best-effort free route hint inferred from RSDB-observed tracks near known airports.",
            ),
            aggregate_endpoint(
                "/schema.json",
                "GET",
                "ApiSchema",
                "Machine-readable aggregate endpoint and field contract.",
            ),
            aggregate_endpoint(
                "/agents.md",
                "GET",
                "text/markdown",
                "Agent-facing project guide generated from committed repository docs.",
            ),
            aggregate_endpoint(
                "/llms.txt",
                "GET",
                "text/markdown",
                "Agent-facing project guide alias for clients that discover llms.txt.",
            ),
            aggregate_endpoint(
                "/robots.txt",
                "GET",
                "text/plain",
                "Crawler policy that allows safe read-only agent endpoints and disallows submit.",
            ),
            aggregate_endpoint(
                "/submit",
                "POST",
                "SubmitResponse",
                "SignedSubmission ingest endpoint.",
            ),
            aggregate_endpoint(
                "/ws",
                "GET",
                "FeedMessage",
                "WebSocket stream of verified aggregate feed messages.",
            ),
        ],
        websocket: WebSocketSchema {
            path: "/ws".to_owned(),
            message_types: vec!["feed_message".to_owned()],
        },
        feed_messages: vec![
            aggregate_message_schema(
                "signed_submission",
                "Signed receiver submission accepted by POST /submit. Payloads may be FeedMessage or FrameRecordBatch JSON.",
            ),
            aggregate_message_schema(
                "feed_message",
                "Verified receiver FeedMessage broadcast to aggregate WebSocket clients.",
            ),
        ],
        aircraft_fields: aggregate_aircraft_field_schema(),
        status_fields: aggregate_status_field_schema(),
    }
}

fn aggregate_endpoint(
    path: &str,
    method: &str,
    response_type: &str,
    description: &str,
) -> ApiEndpoint {
    ApiEndpoint {
        path: path.to_owned(),
        method: method.to_owned(),
        response_type: response_type.to_owned(),
        description: description.to_owned(),
    }
}

fn aggregate_message_schema(message_type: &str, description: &str) -> FeedMessageSchema {
    FeedMessageSchema {
        message_type: message_type.to_owned(),
        description: description.to_owned(),
    }
}

fn aggregate_field(name: &str, json_type: &str, nullable: bool, description: &str) -> FieldSchema {
    FieldSchema {
        name: name.to_owned(),
        json_type: json_type.to_owned(),
        nullable,
        description: description.to_owned(),
    }
}

fn aggregate_aircraft_field_schema() -> Vec<FieldSchema> {
    vec![
        aggregate_field(
            "receiver",
            "object",
            false,
            "Receiver identity that submitted this aircraft state.",
        ),
        aggregate_field(
            "aircraft",
            "object",
            false,
            "Latest AircraftSnapshot reported by that receiver.",
        ),
        aggregate_field(
            "lifecycle",
            "string",
            false,
            "Aggregate aircraft lifecycle: fresh, stale, or expired.",
        ),
        aggregate_field(
            "message_age_ms",
            "integer",
            false,
            "Milliseconds since the latest accepted message for this aircraft.",
        ),
        aggregate_field(
            "position_age_ms",
            "integer",
            true,
            "Milliseconds since the latest accepted position for this aircraft.",
        ),
    ]
}

fn aggregate_status_field_schema() -> Vec<FieldSchema> {
    vec![
        aggregate_field(
            "schema_version",
            "integer",
            false,
            "Aggregate response schema version.",
        ),
        aggregate_field("now_ms", "integer", false, "Current aggregate Unix time."),
        aggregate_field(
            "uptime_ms",
            "integer",
            false,
            "Aggregate service uptime in milliseconds.",
        ),
        aggregate_field(
            "receiver_count",
            "integer",
            false,
            "Number of receivers represented in aggregate state.",
        ),
        aggregate_field(
            "aircraft_count",
            "integer",
            false,
            "Total receiver-scoped aircraft snapshots.",
        ),
        aggregate_field(
            "submissions_accepted",
            "integer",
            false,
            "Signed submissions accepted and applied.",
        ),
        aggregate_field(
            "submissions_duplicate",
            "integer",
            false,
            "Duplicate submission IDs accepted idempotently.",
        ),
        aggregate_field(
            "submissions_rejected",
            "integer",
            false,
            "Submissions rejected by signature or allowlist verification.",
        ),
        aggregate_field(
            "last_submission_ms",
            "integer",
            true,
            "Unix milliseconds for the last accepted or duplicate submission.",
        ),
        aggregate_field(
            "last_error",
            "string",
            true,
            "Most recent aggregate ingest error.",
        ),
        aggregate_field(
            "websocket_clients",
            "integer",
            false,
            "Current aggregate WebSocket client count.",
        ),
        aggregate_field(
            "recent_message_window_ms",
            "integer",
            false,
            "Recent feed-message retention window used by bootstrap and trail data.",
        ),
        aggregate_field(
            "aircraft_stale_after_ms",
            "integer",
            false,
            "Message age after which aggregate aircraft lifecycle becomes stale.",
        ),
        aggregate_field(
            "aircraft_expired_after_ms",
            "integer",
            false,
            "Message age after which aggregate aircraft lifecycle becomes expired.",
        ),
        aggregate_field(
            "persistence",
            "object",
            false,
            "Aggregate persistence health and last write state.",
        ),
        aggregate_field(
            "receivers",
            "array",
            false,
            "Per-receiver aggregate summaries.",
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AggregateAircraftSnapshot {
    pub receiver: ReceiverIdentity,
    pub aircraft: AircraftSnapshot,
    pub lifecycle: AggregateAircraftLifecycle,
    pub message_age_ms: u64,
    pub position_age_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AggregateSnapshot {
    pub schema_version: u32,
    pub now_ms: u64,
    pub aircraft: Vec<AggregateAircraftSnapshot>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateAircraftLifecycle {
    Fresh,
    Stale,
    Expired,
}

impl AggregateAircraftLifecycle {
    #[must_use]
    pub const fn from_message_age(message_age_ms: u64) -> Self {
        if message_age_ms > AGGREGATE_AIRCRAFT_EXPIRED_AFTER_MS {
            Self::Expired
        } else if message_age_ms > AGGREGATE_AIRCRAFT_STALE_AFTER_MS {
            Self::Stale
        } else {
            Self::Fresh
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AggregateBootstrap {
    pub schema_version: u32,
    pub now_ms: u64,
    pub recent_message_window_ms: u64,
    pub snapshot: AggregateSnapshot,
    pub recent_messages: Vec<FeedMessage>,
}

impl AggregateAircraftSnapshot {
    #[must_use]
    pub fn from_parts(receiver: ReceiverIdentity, aircraft: AircraftSnapshot, now_ms: u64) -> Self {
        let message_age_ms = now_ms.saturating_sub(aircraft.last_seen_ms);
        let position_age_ms = aircraft
            .position_last_seen_ms
            .map(|position_ms| now_ms.saturating_sub(position_ms));

        Self {
            receiver,
            aircraft,
            lifecycle: AggregateAircraftLifecycle::from_message_age(message_age_ms),
            message_age_ms,
            position_age_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AggregateReceiverSummary {
    pub receiver: ReceiverIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_site: Option<ReceiverSite>,
    pub aircraft_count: usize,
    pub messages_accepted: u64,
    pub last_message_ms: Option<u64>,
    pub last_submission_ms: Option<u64>,
    pub last_heartbeat_ms: Option<u64>,
    pub receiver_connected: Option<bool>,
    pub submission: Option<SubmissionHealth>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct AggregatePersistenceStatus {
    pub enabled: bool,
    pub path: Option<String>,
    pub retention_ms: Option<u64>,
    pub max_bytes: Option<u64>,
    pub log_bytes: Option<u64>,
    pub log_records: Option<u64>,
    pub writes: u64,
    pub compactions: u64,
    pub last_write_ms: Option<u64>,
    pub last_compaction_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AggregateStatus {
    pub schema_version: u32,
    pub now_ms: u64,
    pub uptime_ms: u64,
    pub receiver_count: usize,
    pub aircraft_count: usize,
    pub submissions_accepted: u64,
    pub submissions_duplicate: u64,
    pub submissions_rejected: u64,
    pub last_submission_ms: Option<u64>,
    pub last_error: Option<String>,
    pub websocket_clients: usize,
    pub recent_message_window_ms: u64,
    pub aircraft_stale_after_ms: u64,
    pub aircraft_expired_after_ms: u64,
    pub persistence: AggregatePersistenceStatus,
    pub receivers: Vec<AggregateReceiverSummary>,
}

#[derive(Debug, Default)]
pub struct AggregateStore {
    receivers: BTreeMap<String, ReceiverAggregate>,
    accepted_submission_ids: BTreeMap<String, u64>,
    submissions_accepted: u64,
    submissions_duplicate: u64,
    submissions_rejected: u64,
    last_submission_ms: Option<u64>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AggregateStorePersistence {
    schema_version: u32,
    receivers: Vec<PersistedReceiverAggregate>,
    accepted_submissions: Vec<PersistedAcceptedSubmission>,
    submissions_accepted: u64,
    submissions_duplicate: u64,
    submissions_rejected: u64,
    last_submission_ms: Option<u64>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct PersistedAcceptedSubmission {
    submission_id: String,
    submitted_at_ms: u64,
}

impl AggregateStorePersistence {
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct PersistedReceiverAggregate {
    receiver: ReceiverIdentity,
    protocol: Protocol,
    aircraft: Vec<AircraftSnapshot>,
    messages_accepted: u64,
    last_message_ms: Option<u64>,
    last_submission_ms: Option<u64>,
    #[serde(default)]
    last_heartbeat_ms: Option<u64>,
    #[serde(default)]
    latest_stats: Option<FeedStats>,
    #[serde(default)]
    recent_messages: Vec<FeedMessage>,
}

impl AggregateStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a previously verified feed message to aggregate state.
    ///
    /// # Errors
    ///
    /// Returns an error if the feed message does not carry receiver identity.
    pub fn ingest_verified(
        &mut self,
        message: FeedMessage,
        submitted_at_ms: u64,
    ) -> Result<FeedMessage, AggregateStoreError> {
        let message = self.apply_verified(message, submitted_at_ms)?;
        self.record_accepted_submission(submitted_at_ms);
        Ok(message)
    }

    /// Applies a previously verified signed submission once.
    ///
    /// # Errors
    ///
    /// Returns an error if the feed message does not carry receiver identity.
    pub fn ingest_verified_submission(
        &mut self,
        submission_id: &str,
        message: FeedMessage,
        submitted_at_ms: u64,
    ) -> Result<AggregateIngestResult, AggregateStoreError> {
        if self.accepted_submission_ids.contains_key(submission_id) {
            self.submissions_duplicate = self.submissions_duplicate.saturating_add(1);
            self.last_error = None;
            return Ok(AggregateIngestResult::duplicate());
        }

        let message = self.apply_verified(message, submitted_at_ms)?;
        self.record_accepted_submission(submitted_at_ms);
        self.accepted_submission_ids
            .insert(submission_id.to_owned(), submitted_at_ms);

        Ok(AggregateIngestResult::applied(message))
    }

    /// Applies a previously verified signed frame-record batch once.
    ///
    /// # Errors
    ///
    /// Returns an error if the batch is invalid or decoded feed messages do not
    /// carry receiver identity.
    pub fn ingest_verified_frame_records_submission(
        &mut self,
        submission_id: &str,
        batch: &FrameRecordBatch,
        submitted_at_ms: u64,
    ) -> Result<AggregateIngestResult, AggregateStoreError> {
        if self.accepted_submission_ids.contains_key(submission_id) {
            self.submissions_duplicate = self.submissions_duplicate.saturating_add(1);
            self.last_error = None;
            return Ok(AggregateIngestResult::duplicate());
        }

        let messages = self.apply_frame_record_batch(batch, submitted_at_ms)?;
        let mut applied_messages = Vec::with_capacity(messages.len());
        for message in messages {
            applied_messages.push(self.apply_verified(message, submitted_at_ms)?);
        }

        self.record_accepted_submission(submitted_at_ms);
        self.accepted_submission_ids
            .insert(submission_id.to_owned(), submitted_at_ms);

        Ok(AggregateIngestResult::applied_many(applied_messages))
    }

    /// Replays a persisted signed submission without counting already-applied
    /// entries as live duplicates.
    ///
    /// # Errors
    ///
    /// Returns an error if the feed message does not carry receiver identity.
    pub fn replay_verified_submission(
        &mut self,
        submission_id: &str,
        message: FeedMessage,
        submitted_at_ms: u64,
    ) -> Result<bool, AggregateStoreError> {
        if self.accepted_submission_ids.contains_key(submission_id) {
            return Ok(false);
        }

        self.apply_verified(message, submitted_at_ms)?;
        self.record_accepted_submission(submitted_at_ms);
        self.accepted_submission_ids
            .insert(submission_id.to_owned(), submitted_at_ms);
        Ok(true)
    }

    /// Replays a persisted signed frame-record batch without counting
    /// already-applied entries as live duplicates.
    ///
    /// # Errors
    ///
    /// Returns an error if the batch is invalid or decoded feed messages do not
    /// carry receiver identity.
    pub fn replay_verified_frame_records_submission(
        &mut self,
        submission_id: &str,
        batch: &FrameRecordBatch,
        submitted_at_ms: u64,
    ) -> Result<bool, AggregateStoreError> {
        if self.accepted_submission_ids.contains_key(submission_id) {
            return Ok(false);
        }

        for message in self.apply_frame_record_batch(batch, submitted_at_ms)? {
            self.apply_verified(message, submitted_at_ms)?;
        }
        self.record_accepted_submission(submitted_at_ms);
        self.accepted_submission_ids
            .insert(submission_id.to_owned(), submitted_at_ms);
        Ok(true)
    }

    pub fn retain_accepted_submission_ids(&mut self, retained_ids: &BTreeSet<String>) {
        self.accepted_submission_ids
            .retain(|submission_id, _| retained_ids.contains(submission_id));
    }

    #[must_use]
    pub fn has_accepted_submission_id(&self, submission_id: &str) -> bool {
        self.accepted_submission_ids.contains_key(submission_id)
    }

    fn apply_frame_record_batch(
        &mut self,
        batch: &FrameRecordBatch,
        submitted_at_ms: u64,
    ) -> Result<Vec<FeedMessage>, AggregateStoreError> {
        batch
            .validate()
            .map_err(|error| AggregateStoreError::InvalidFrameRecords(error.to_string()))?;

        let receiver = batch.receiver.clone().with_generated_handle();
        let receiver_site = batch.receiver_site();
        let mut messages = Vec::new();

        for record in &batch.records {
            let snapshot = {
                let aggregate = self
                    .receivers
                    .entry(receiver.id.clone())
                    .or_insert_with(|| ReceiverAggregate::new(receiver.clone()));
                aggregate.receiver.clone_from(&receiver);
                aggregate.protocol = batch.protocol;
                aggregate
                    .decoder
                    .update_frame_record(record)
                    .map_err(|error| AggregateStoreError::InvalidFrameRecords(error.to_string()))?
            };

            if let Some(snapshot) = snapshot {
                messages.push(
                    FeedMessage::aircraft_for_protocol(
                        batch.protocol,
                        record.now_ms,
                        enrich_aircraft_snapshot(snapshot, receiver_site.as_ref(), record.now_ms),
                    )
                    .with_receiver(Some(receiver.clone())),
                );
            }

            let stale_aircraft = {
                let aggregate = self
                    .receivers
                    .entry(receiver.id.clone())
                    .or_insert_with(|| ReceiverAggregate::new(receiver.clone()));
                aggregate
                    .decoder
                    .evict_stale(record.now_ms, AGGREGATE_AIRCRAFT_STALE_AFTER_MS)
            };
            for removed in stale_aircraft {
                messages.push(
                    FeedMessage::stale_aircraft_for_protocol(
                        batch.protocol,
                        record.now_ms,
                        removed.icao,
                    )
                    .with_receiver(Some(receiver.clone())),
                );
            }
        }

        if messages.is_empty() {
            self.last_submission_ms = Some(submitted_at_ms);
            self.last_error = None;
        }

        Ok(messages)
    }

    fn apply_verified(
        &mut self,
        message: FeedMessage,
        submitted_at_ms: u64,
    ) -> Result<FeedMessage, AggregateStoreError> {
        let receiver = message
            .receiver()
            .cloned()
            .ok_or(AggregateStoreError::MissingReceiver)?
            .with_generated_handle();
        let message = message.with_receiver(Some(receiver.clone()));
        let message_time_ms = message.now_ms();
        let protocol = message.protocol();
        let aggregate = self
            .receivers
            .entry(receiver.id.clone())
            .or_insert_with(|| ReceiverAggregate::new(receiver.clone()));

        aggregate.receiver = receiver;
        aggregate.protocol = protocol;
        aggregate.messages_accepted = aggregate.messages_accepted.saturating_add(1);
        aggregate.last_message_ms = Some(message_time_ms);
        aggregate.last_submission_ms = Some(submitted_at_ms);

        match &message {
            FeedMessage::Snapshot { aircraft, .. } => aggregate.replace_aircraft(aircraft),
            FeedMessage::Aircraft { aircraft, .. } => {
                aggregate.upsert_aircraft(aircraft);
                aggregate.record_recent_message(message.clone(), message_time_ms);
            }
            FeedMessage::StaleAircraft { icao, .. } => {
                aggregate.remove_aircraft(icao);
                aggregate.record_recent_message(message.clone(), message_time_ms);
            }
            FeedMessage::Heartbeat { stats, .. } => {
                aggregate.record_heartbeat(message_time_ms, stats.clone());
            }
        }

        Ok(message)
    }

    fn record_accepted_submission(&mut self, submitted_at_ms: u64) {
        self.submissions_accepted = self.submissions_accepted.saturating_add(1);
        self.last_submission_ms = Some(submitted_at_ms);
        self.last_error = None;
    }

    pub fn record_rejection(&mut self, error: impl Into<String>) {
        self.submissions_rejected = self.submissions_rejected.saturating_add(1);
        self.last_error = Some(error.into());
    }

    #[must_use]
    pub fn persistence_snapshot(&self) -> AggregateStorePersistence {
        AggregateStorePersistence {
            schema_version: AGGREGATE_STORE_PERSISTENCE_SCHEMA_VERSION,
            receivers: self
                .receivers
                .values()
                .map(|receiver| PersistedReceiverAggregate {
                    receiver: receiver.receiver.clone(),
                    protocol: receiver.protocol,
                    aircraft: receiver.aircraft.values().cloned().collect(),
                    messages_accepted: receiver.messages_accepted,
                    last_message_ms: receiver.last_message_ms,
                    last_submission_ms: receiver.last_submission_ms,
                    last_heartbeat_ms: receiver.last_heartbeat_ms,
                    latest_stats: receiver.latest_stats.clone(),
                    recent_messages: receiver.recent_messages.clone(),
                })
                .collect(),
            accepted_submissions: self
                .accepted_submission_ids
                .iter()
                .map(
                    |(submission_id, submitted_at_ms)| PersistedAcceptedSubmission {
                        submission_id: submission_id.clone(),
                        submitted_at_ms: *submitted_at_ms,
                    },
                )
                .collect(),
            submissions_accepted: self.submissions_accepted,
            submissions_duplicate: self.submissions_duplicate,
            submissions_rejected: self.submissions_rejected,
            last_submission_ms: self.last_submission_ms,
            last_error: self.last_error.clone(),
        }
    }

    /// Rebuilds aggregate state from a persistence snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot schema is unsupported.
    pub fn from_persistence(
        persistence: AggregateStorePersistence,
    ) -> Result<Self, AggregateStorePersistenceError> {
        if persistence.schema_version != AGGREGATE_STORE_PERSISTENCE_SCHEMA_VERSION {
            return Err(AggregateStorePersistenceError::UnsupportedSchemaVersion(
                persistence.schema_version,
            ));
        }

        Ok(Self {
            receivers: persistence
                .receivers
                .into_iter()
                .map(|receiver| {
                    (
                        receiver.receiver.id.clone(),
                        ReceiverAggregate {
                            receiver: receiver.receiver.with_generated_handle(),
                            protocol: receiver.protocol,
                            decoder: AircraftStore::default(),
                            aircraft: receiver
                                .aircraft
                                .into_iter()
                                .map(|aircraft| (aircraft.icao.clone(), aircraft))
                                .collect(),
                            messages_accepted: receiver.messages_accepted,
                            last_message_ms: receiver.last_message_ms,
                            last_submission_ms: receiver.last_submission_ms,
                            last_heartbeat_ms: receiver.last_heartbeat_ms,
                            latest_stats: receiver.latest_stats,
                            recent_messages: receiver.recent_messages,
                        },
                    )
                })
                .collect(),
            accepted_submission_ids: persistence
                .accepted_submissions
                .into_iter()
                .map(|submission| (submission.submission_id, submission.submitted_at_ms))
                .collect(),
            submissions_accepted: persistence.submissions_accepted,
            submissions_duplicate: persistence.submissions_duplicate,
            submissions_rejected: persistence.submissions_rejected,
            last_submission_ms: persistence.last_submission_ms,
            last_error: persistence.last_error,
        })
    }

    #[must_use]
    pub fn snapshot(&self, now_ms: u64) -> AggregateSnapshot {
        AggregateSnapshot {
            schema_version: AGGREGATE_SCHEMA_VERSION,
            now_ms,
            aircraft: self.aircraft(now_ms),
        }
    }

    #[must_use]
    pub fn bootstrap(&self, now_ms: u64) -> AggregateBootstrap {
        AggregateBootstrap {
            schema_version: AGGREGATE_SCHEMA_VERSION,
            now_ms,
            recent_message_window_ms: AGGREGATE_RECENT_FEED_WINDOW_MS,
            snapshot: self.snapshot(now_ms),
            recent_messages: self.recent_feed_messages(now_ms),
        }
    }

    #[must_use]
    pub fn status(&self, now_ms: u64, uptime_ms: u64, websocket_clients: usize) -> AggregateStatus {
        self.status_with_persistence(
            now_ms,
            uptime_ms,
            websocket_clients,
            AggregatePersistenceStatus::default(),
        )
    }

    #[must_use]
    pub fn status_with_persistence(
        &self,
        now_ms: u64,
        uptime_ms: u64,
        websocket_clients: usize,
        persistence: AggregatePersistenceStatus,
    ) -> AggregateStatus {
        AggregateStatus {
            schema_version: AGGREGATE_SCHEMA_VERSION,
            now_ms,
            uptime_ms,
            receiver_count: self.receivers.len(),
            aircraft_count: self.aircraft_count(),
            submissions_accepted: self.submissions_accepted,
            submissions_duplicate: self.submissions_duplicate,
            submissions_rejected: self.submissions_rejected,
            last_submission_ms: self.last_submission_ms,
            last_error: self.last_error.clone(),
            websocket_clients,
            recent_message_window_ms: AGGREGATE_RECENT_FEED_WINDOW_MS,
            aircraft_stale_after_ms: AGGREGATE_AIRCRAFT_STALE_AFTER_MS,
            aircraft_expired_after_ms: AGGREGATE_AIRCRAFT_EXPIRED_AFTER_MS,
            persistence,
            receivers: self.receiver_summaries(),
        }
    }

    #[must_use]
    pub fn feed_messages(&self, now_ms: u64) -> Vec<FeedMessage> {
        let mut messages = self.recent_feed_messages(now_ms);
        messages.extend(
            self.receivers
                .values()
                .flat_map(|aggregate| {
                    aggregate.aircraft.values().map(|aircraft| {
                        FeedMessage::aircraft_for_protocol(
                            aggregate.protocol,
                            now_ms,
                            aircraft.clone(),
                        )
                        .with_receiver(Some(aggregate.receiver.clone()))
                    })
                })
                .collect::<Vec<_>>(),
        );
        messages
    }

    #[must_use]
    pub fn aircraft_count(&self) -> usize {
        self.receivers
            .values()
            .map(|receiver| receiver.aircraft.len())
            .sum()
    }

    fn aircraft(&self, now_ms: u64) -> Vec<AggregateAircraftSnapshot> {
        self.receivers
            .values()
            .flat_map(|aggregate| {
                aggregate.aircraft.values().cloned().map(|aircraft| {
                    AggregateAircraftSnapshot::from_parts(
                        aggregate.receiver.clone(),
                        aircraft,
                        now_ms,
                    )
                })
            })
            .collect()
    }

    fn receiver_summaries(&self) -> Vec<AggregateReceiverSummary> {
        self.receivers
            .values()
            .map(|receiver| AggregateReceiverSummary {
                receiver: receiver.receiver.clone(),
                receiver_site: receiver
                    .latest_stats
                    .as_ref()
                    .and_then(|stats| stats.receiver_site.clone()),
                aircraft_count: receiver.aircraft.len(),
                messages_accepted: receiver.messages_accepted,
                last_message_ms: receiver.last_message_ms,
                last_submission_ms: receiver.last_submission_ms,
                last_heartbeat_ms: receiver.last_heartbeat_ms,
                receiver_connected: receiver
                    .latest_stats
                    .as_ref()
                    .map(|stats| stats.receiver_connected),
                submission: receiver
                    .latest_stats
                    .as_ref()
                    .map(|stats| stats.submission.clone()),
            })
            .collect()
    }

    fn recent_feed_messages(&self, now_ms: u64) -> Vec<FeedMessage> {
        let min_ms = now_ms.saturating_sub(AGGREGATE_RECENT_FEED_WINDOW_MS);
        let mut messages = self
            .receivers
            .values()
            .flat_map(|aggregate| {
                aggregate
                    .recent_messages
                    .iter()
                    .filter(move |message| message.now_ms() >= min_ms)
                    .cloned()
            })
            .collect::<Vec<_>>();

        messages.sort_by_key(FeedMessage::now_ms);
        messages
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AggregateStorePersistenceError {
    UnsupportedSchemaVersion(u32),
}

impl fmt::Display for AggregateStorePersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(schema_version) => write!(
                formatter,
                "unsupported aggregate store persistence schema_version {schema_version}; expected {AGGREGATE_STORE_PERSISTENCE_SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for AggregateStorePersistenceError {}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateIngestResult {
    pub message: Option<FeedMessage>,
    pub messages: Vec<FeedMessage>,
    pub duplicate: bool,
}

impl AggregateIngestResult {
    #[must_use]
    pub fn applied(message: FeedMessage) -> Self {
        Self {
            message: Some(message.clone()),
            messages: vec![message],
            duplicate: false,
        }
    }

    #[must_use]
    pub fn applied_many(messages: Vec<FeedMessage>) -> Self {
        Self {
            message: messages.first().cloned(),
            messages,
            duplicate: false,
        }
    }

    #[must_use]
    pub fn duplicate() -> Self {
        Self {
            message: None,
            messages: Vec::new(),
            duplicate: true,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AggregateStoreError {
    MissingReceiver,
    InvalidFrameRecords(String),
}

impl fmt::Display for AggregateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingReceiver => {
                formatter.write_str("feed message is missing receiver identity")
            }
            Self::InvalidFrameRecords(error) => {
                write!(formatter, "invalid frame records: {error}")
            }
        }
    }
}

impl std::error::Error for AggregateStoreError {}

#[derive(Debug)]
struct ReceiverAggregate {
    receiver: ReceiverIdentity,
    protocol: Protocol,
    decoder: AircraftStore,
    aircraft: BTreeMap<String, AircraftSnapshot>,
    messages_accepted: u64,
    last_message_ms: Option<u64>,
    last_submission_ms: Option<u64>,
    last_heartbeat_ms: Option<u64>,
    latest_stats: Option<FeedStats>,
    recent_messages: Vec<FeedMessage>,
}

impl ReceiverAggregate {
    fn new(receiver: ReceiverIdentity) -> Self {
        Self {
            receiver,
            protocol: Protocol::Adsb1090,
            decoder: AircraftStore::default(),
            aircraft: BTreeMap::new(),
            messages_accepted: 0,
            last_message_ms: None,
            last_submission_ms: None,
            last_heartbeat_ms: None,
            latest_stats: None,
            recent_messages: Vec::new(),
        }
    }

    fn replace_aircraft(&mut self, aircraft: &[AircraftSnapshot]) {
        self.aircraft = aircraft
            .iter()
            .cloned()
            .map(|aircraft| (aircraft.icao.clone(), aircraft))
            .collect();
    }

    fn upsert_aircraft(&mut self, aircraft: &AircraftSnapshot) {
        self.aircraft
            .insert(aircraft.icao.clone(), aircraft.clone());
    }

    fn remove_aircraft(&mut self, icao: &str) {
        self.aircraft.remove(icao);
    }

    fn record_heartbeat(&mut self, now_ms: u64, stats: FeedStats) {
        self.last_heartbeat_ms = Some(now_ms);
        self.latest_stats = Some(stats);
        self.prune_recent_messages(now_ms);
    }

    fn record_recent_message(&mut self, message: FeedMessage, now_ms: u64) {
        self.recent_messages.push(message);
        self.prune_recent_messages(now_ms);
    }

    fn prune_recent_messages(&mut self, now_ms: u64) {
        let min_ms = now_ms.saturating_sub(AGGREGATE_RECENT_FEED_WINDOW_MS);
        self.recent_messages
            .retain(|message| message.now_ms() >= min_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Frame, FrameRecord, FrameRecordBatch, PositionStatus, ReceiverHandle};

    #[test]
    fn aggregate_api_schema_describes_aggregate_contract() {
        let schema = aggregate_api_schema();

        assert_eq!(schema.schema_version, API_SCHEMA_VERSION);
        assert_eq!(schema.feed_schema_version, AGGREGATE_SCHEMA_VERSION);
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| { endpoint.path == "/submit" && endpoint.method == "POST" })
        );
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| { endpoint.path == "/schema.json" && endpoint.method == "GET" })
        );
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| { endpoint.path == "/bootstrap.json" && endpoint.method == "GET" })
        );
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| { endpoint.path == "/agents.md" && endpoint.method == "GET" })
        );
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| { endpoint.path == "/llms.txt" && endpoint.method == "GET" })
        );
        assert!(
            schema
                .endpoints
                .iter()
                .any(|endpoint| { endpoint.path == "/robots.txt" && endpoint.method == "GET" })
        );
        assert_eq!(schema.websocket.path, "/ws");
        assert!(
            schema
                .status_fields
                .iter()
                .any(|field| { field.name == "submissions_accepted" && !field.nullable })
        );
        assert!(
            schema
                .aircraft_fields
                .iter()
                .any(|field| { field.name == "lifecycle" && !field.nullable })
        );
    }

    #[test]
    fn aggregate_store_keeps_receiver_scoped_aircraft() {
        let mut store = AggregateStore::new();

        store
            .ingest_verified(
                FeedMessage::snapshot(100, vec![aircraft("A00001")])
                    .with_receiver(Some(receiver("sf-a"))),
                110,
            )
            .unwrap();
        store
            .ingest_verified(
                FeedMessage::aircraft(120, aircraft("A00001"))
                    .with_receiver(Some(receiver("sf-b"))),
                130,
            )
            .unwrap();

        let snapshot = store.snapshot(140);

        assert_eq!(snapshot.aircraft.len(), 2);
        assert_eq!(snapshot.aircraft[0].receiver.id, "sf-a");
        assert_eq!(snapshot.aircraft[1].receiver.id, "sf-b");
        assert_eq!(store.status(140, 40, 3).receiver_count, 2);
    }

    #[test]
    fn aggregate_snapshot_exposes_aircraft_lifecycle() {
        let mut store = AggregateStore::new();

        store
            .ingest_verified(
                FeedMessage::aircraft(100_000, aircraft_seen_at("A00001", 95_000, Some(90_000)))
                    .with_receiver(Some(receiver("sf-a"))),
                105_000,
            )
            .unwrap();

        let snapshot = store.snapshot(111_000);
        let aircraft = &snapshot.aircraft[0];

        assert_eq!(aircraft.lifecycle, AggregateAircraftLifecycle::Stale);
        assert_eq!(aircraft.message_age_ms, 16_000);
        assert_eq!(aircraft.position_age_ms, Some(21_000));
    }

    #[test]
    fn aggregate_store_applies_updates_and_stale_messages() {
        let mut store = AggregateStore::new();

        store
            .ingest_verified(
                FeedMessage::snapshot(100, vec![aircraft("A00001"), aircraft("A00002")])
                    .with_receiver(Some(receiver("sf-a"))),
                110,
            )
            .unwrap();
        store
            .ingest_verified(
                FeedMessage::stale_aircraft(120, "A00001".to_owned())
                    .with_receiver(Some(receiver("sf-a"))),
                130,
            )
            .unwrap();

        let snapshot = store.snapshot(140);

        assert_eq!(snapshot.aircraft.len(), 1);
        assert_eq!(snapshot.aircraft[0].aircraft.icao, "A00002");
        assert_eq!(store.status(140, 40, 0).submissions_accepted, 2);
    }

    #[test]
    fn aggregate_store_deduplicates_submission_ids() {
        let mut store = AggregateStore::new();
        let message =
            FeedMessage::aircraft(100, aircraft("A00001")).with_receiver(Some(receiver("sf-a")));

        let first = store
            .ingest_verified_submission("submission-1", message.clone(), 110)
            .unwrap();
        let duplicate = store
            .ingest_verified_submission("submission-1", message, 110)
            .unwrap();
        let status = store.status(140, 40, 0);

        assert!(!first.duplicate);
        assert!(first.message.is_some());
        assert!(duplicate.duplicate);
        assert!(duplicate.message.is_none());
        assert_eq!(status.submissions_accepted, 1);
        assert_eq!(status.submissions_duplicate, 1);
        assert_eq!(store.snapshot(140).aircraft.len(), 1);
    }

    #[test]
    fn aggregate_store_ingests_verified_frame_record_batch() {
        let mut store = AggregateStore::new();
        let receiver = receiver("sf-a");
        let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
        let record = FrameRecord::new(100, 0, &frame).with_receiver(Some(receiver.clone()));
        let batch = FrameRecordBatch::new(Protocol::Adsb1090, receiver, vec![record]);

        let first = store
            .ingest_verified_frame_records_submission("submission-1", &batch, 110)
            .unwrap();
        let duplicate = store
            .ingest_verified_frame_records_submission("submission-1", &batch, 120)
            .unwrap();
        let status = store.status(140, 40, 0);

        assert!(!first.duplicate);
        assert_eq!(first.messages.len(), 1);
        assert!(duplicate.duplicate);
        assert!(duplicate.messages.is_empty());
        assert_eq!(status.submissions_accepted, 1);
        assert_eq!(status.submissions_duplicate, 1);
        assert_eq!(status.receivers[0].messages_accepted, 1);
        assert_eq!(store.snapshot(140).aircraft.len(), 1);
    }

    #[test]
    fn aggregate_store_keeps_frame_decoder_state_across_batches() {
        let mut store = AggregateStore::new();
        let receiver = receiver("sf-a");
        let even = Frame::from_hex("8D40621D58C382D690C8AC2863A7").unwrap();
        let odd = Frame::from_hex("8D40621D58C386435CC412692AD6").unwrap();
        let even_batch = FrameRecordBatch::new(
            Protocol::Adsb1090,
            receiver.clone(),
            vec![FrameRecord::new(1_000, 0, &even).with_receiver(Some(receiver.clone()))],
        );
        let odd_batch = FrameRecordBatch::new(
            Protocol::Adsb1090,
            receiver.clone(),
            vec![FrameRecord::new(2_000, 1_000, &odd).with_receiver(Some(receiver))],
        );

        store
            .ingest_verified_frame_records_submission("submission-1", &even_batch, 1_100)
            .unwrap();
        store
            .ingest_verified_frame_records_submission("submission-2", &odd_batch, 2_100)
            .unwrap();

        let snapshot = store.snapshot(2_200);
        let aircraft = &snapshot.aircraft[0].aircraft;

        assert_eq!(aircraft.icao, "40621D");
        assert_eq!(aircraft.position_status, PositionStatus::Fresh);
        assert_eq!(aircraft.position_last_seen_ms, Some(2_000));
        assert!(aircraft.lat.is_some());
        assert!(aircraft.lon.is_some());
    }

    #[test]
    fn aggregate_store_exposes_receiver_heartbeat_health() {
        let mut store = AggregateStore::new();
        let mut stats = FeedStats {
            receiver_site: Some(ReceiverSite {
                name: None,
                lat: 37.753,
                lon: -122.447,
            }),
            receiver_connected: true,
            ..FeedStats::default()
        };
        stats.submission = SubmissionHealth {
            enabled: true,
            outbox_pending: 2,
            ..SubmissionHealth::default()
        };

        store
            .ingest_verified(
                FeedMessage::heartbeat(100, 0, stats).with_receiver(Some(receiver("sf-a"))),
                110,
            )
            .unwrap();

        let aggregate_status = store.status(120, 20, 0);
        let summary = &aggregate_status.receivers[0];

        assert_eq!(summary.last_heartbeat_ms, Some(100));
        assert_eq!(
            summary.receiver_site.as_ref().map(|site| site.lat),
            Some(37.753)
        );
        assert_eq!(summary.receiver_connected, Some(true));
        assert_eq!(
            summary
                .submission
                .as_ref()
                .map(|submission| submission.outbox_pending),
            Some(2)
        );
    }

    #[test]
    fn aggregate_store_replays_recent_messages_before_current_state() {
        let mut store = AggregateStore::new();

        store
            .ingest_verified(
                FeedMessage::aircraft(100, aircraft("A00001"))
                    .with_receiver(Some(receiver("sf-a"))),
                105,
            )
            .unwrap();
        store
            .ingest_verified(
                FeedMessage::aircraft(120, aircraft("A00001"))
                    .with_receiver(Some(receiver("sf-a"))),
                125,
            )
            .unwrap();

        let messages = store.feed_messages(130);

        assert_eq!(
            messages.iter().map(FeedMessage::now_ms).collect::<Vec<_>>(),
            vec![100, 120, 130]
        );
    }

    #[test]
    fn aggregate_feed_messages_preserve_receiver_protocol() {
        let mut store = AggregateStore::new();

        store
            .ingest_verified(
                FeedMessage::aircraft_for_protocol(Protocol::Uat978, 100, aircraft("A00001"))
                    .with_receiver(Some(receiver("sf-a"))),
                105,
            )
            .unwrap();

        let messages = store.feed_messages(130);

        assert!(
            messages
                .iter()
                .all(|message| message.protocol() == Protocol::Uat978)
        );
    }

    #[test]
    fn aggregate_bootstrap_carries_recent_history_and_current_snapshot() {
        let mut store = AggregateStore::new();

        store
            .ingest_verified(
                FeedMessage::aircraft(100, aircraft("A00001"))
                    .with_receiver(Some(receiver("sf-a"))),
                105,
            )
            .unwrap();
        store
            .ingest_verified(
                FeedMessage::aircraft(120, aircraft("A00001"))
                    .with_receiver(Some(receiver("sf-a"))),
                125,
            )
            .unwrap();

        let bootstrap = store.bootstrap(130);

        assert_eq!(bootstrap.schema_version, AGGREGATE_SCHEMA_VERSION);
        assert_eq!(
            bootstrap.recent_message_window_ms,
            AGGREGATE_RECENT_FEED_WINDOW_MS
        );
        assert_eq!(bootstrap.recent_messages.len(), 2);
        assert_eq!(bootstrap.snapshot.aircraft.len(), 1);
        assert_eq!(bootstrap.snapshot.aircraft[0].aircraft.icao, "A00001");
    }

    #[test]
    fn aggregate_store_persistence_round_trips_state_and_dedupes() {
        let mut store = AggregateStore::new();
        let message =
            FeedMessage::aircraft(100, aircraft("A00001")).with_receiver(Some(receiver("sf-a")));

        store
            .ingest_verified_submission("submission-1", message.clone(), 110)
            .unwrap();
        store
            .ingest_verified(
                FeedMessage::aircraft(120, aircraft("A00002"))
                    .with_receiver(Some(receiver("sf-a"))),
                130,
            )
            .unwrap();
        store.record_rejection("bad signature");

        let persistence = store.persistence_snapshot();
        let mut restored = AggregateStore::from_persistence(persistence).unwrap();
        let duplicate = restored
            .ingest_verified_submission("submission-1", message, 140)
            .unwrap();
        let status = restored.status(150, 50, 0);

        assert!(duplicate.duplicate);
        assert_eq!(restored.snapshot(150).aircraft.len(), 2);
        assert_eq!(status.receiver_count, 1);
        assert_eq!(status.receivers[0].messages_accepted, 2);
        assert_eq!(status.submissions_accepted, 2);
        assert_eq!(status.submissions_duplicate, 1);
        assert_eq!(status.submissions_rejected, 1);
        assert_eq!(status.last_error.as_deref(), None);
        assert_eq!(restored.bootstrap(150).recent_messages.len(), 2);
        assert_eq!(
            status.recent_message_window_ms,
            AGGREGATE_RECENT_FEED_WINDOW_MS
        );
        assert_eq!(
            status.aircraft_stale_after_ms,
            AGGREGATE_AIRCRAFT_STALE_AFTER_MS
        );
        assert_eq!(
            status.aircraft_expired_after_ms,
            AGGREGATE_AIRCRAFT_EXPIRED_AFTER_MS
        );
    }

    #[test]
    fn aggregate_store_rejects_unsupported_persistence_schema() {
        let mut persistence = AggregateStore::new().persistence_snapshot();
        persistence.schema_version = 2;

        let error = AggregateStore::from_persistence(persistence).unwrap_err();

        assert_eq!(
            error,
            AggregateStorePersistenceError::UnsupportedSchemaVersion(2)
        );
    }

    #[test]
    fn aggregate_store_generates_missing_receiver_handle() {
        let mut store = AggregateStore::new();
        let receiver = ReceiverIdentity {
            id: "legacy".to_owned(),
            name: None,
            handle: None,
        };

        let accepted = store
            .ingest_verified(
                FeedMessage::aircraft(100, aircraft("A00001")).with_receiver(Some(receiver)),
                110,
            )
            .unwrap();

        assert_eq!(
            accepted
                .receiver()
                .and_then(|receiver| receiver.handle.as_ref()),
            Some(&ReceiverHandle::from_receiver_id("legacy"))
        );
    }

    #[test]
    fn aggregate_store_records_rejections() {
        let mut store = AggregateStore::new();

        store.record_rejection("bad signature");

        let status = store.status(100, 10, 0);
        assert_eq!(status.submissions_rejected, 1);
        assert_eq!(status.last_error.as_deref(), Some("bad signature"));
    }

    fn receiver(id: &str) -> ReceiverIdentity {
        ReceiverIdentity::new(id.to_owned())
    }

    fn aircraft(icao: &str) -> AircraftSnapshot {
        aircraft_seen_at(icao, 0, None)
    }

    fn aircraft_seen_at(
        icao: &str,
        last_seen_ms: u64,
        position_last_seen_ms: Option<u64>,
    ) -> AircraftSnapshot {
        AircraftSnapshot {
            icao: icao.to_owned(),
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
            position_status: crate::PositionStatus::Unavailable,
            position_last_seen_ms,
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
            last_seen_ms,
            message_count: 1,
            last_type_code: None,
            last_decode_status: crate::DecodeStatus::Unknown,
            last_raw: String::new(),
            raw_messages: Vec::new(),
        }
    }
}
