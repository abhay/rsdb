use std::time::Instant;

use rsdb::{
    AircraftSnapshot, AircraftStore, FeedMessage, FeedStats, FrameRecord, FrameRecordBatch,
    ModesFrameDecoder, Protocol, RadioConfig, ReceiverIdentity, SubmissionStatus,
    enrich_aircraft_snapshot, enrich_aircraft_snapshots, iq_chunk_metrics,
};

use crate::config::FeedRuntimeConfig;
use crate::usb::{GainMode, IqStream, RtlSdrConfig, RtlSdrSource};

pub(crate) fn run_feed(
    config: RtlSdrConfig,
    feed_config: &FeedRuntimeConfig,
    seconds: Option<u64>,
    mut publish: impl FnMut(FeedMessage) -> Result<(), String>,
    mut submit_frame_batch: impl FnMut(FrameRecordBatch) -> Result<(), String>,
    mut observe_stream: impl FnMut(&IqStream, FeedCounters) -> Result<(), String>,
) -> Result<(), String> {
    let mut source = RtlSdrSource::open(config).map_err(|error| error.to_string())?;
    let radio = RadioConfig::for_protocol(config.protocol)
        .with_center_frequency_hz(source.center_frequency_hz())
        .with_sample_rate_hz(source.sample_rate_hz());
    let source_metadata = FrameRecordSourceMetadata::new(config, source.tuner_name())?;
    let stream = source
        .start_streaming()
        .map_err(|error| error.to_string())?;
    let result = run_streaming_feed(
        &stream,
        StreamingFeedContext {
            feed_config,
            radio,
            source_metadata,
        },
        seconds,
        &mut publish,
        &mut submit_frame_batch,
        &mut observe_stream,
    );

    stream.stop();
    result
}

fn run_streaming_feed(
    stream: &IqStream,
    context: StreamingFeedContext<'_>,
    seconds: Option<u64>,
    publish: &mut impl FnMut(FeedMessage) -> Result<(), String>,
    submit_frame_batch: &mut impl FnMut(FrameRecordBatch) -> Result<(), String>,
    observe_stream: &mut impl FnMut(&IqStream, FeedCounters) -> Result<(), String>,
) -> Result<(), String> {
    let stream_start_ms = crate::unix_time_ms();
    let mut decoder = FeedDecoderKind::for_protocol(context.feed_config.radio.protocol, "feed")?
        .build_decoder(
            context.feed_config,
            context.radio,
            context.source_metadata,
            stream_start_ms,
        );
    run_streaming_feed_with_decoder(
        stream,
        &mut decoder,
        seconds,
        publish,
        submit_frame_batch,
        observe_stream,
    )
}

#[derive(Debug, Clone)]
struct StreamingFeedContext<'a> {
    feed_config: &'a FeedRuntimeConfig,
    radio: RadioConfig,
    source_metadata: FrameRecordSourceMetadata,
}

fn run_streaming_feed_with_decoder(
    stream: &IqStream,
    decoder: &mut impl RadioFeedDecoder,
    seconds: Option<u64>,
    publish: &mut impl FnMut(FeedMessage) -> Result<(), String>,
    submit_frame_batch: &mut impl FnMut(FrameRecordBatch) -> Result<(), String>,
    observe_stream: &mut impl FnMut(&IqStream, FeedCounters) -> Result<(), String>,
) -> Result<(), String> {
    let start = Instant::now();
    let initial_now_ms = crate::unix_time_ms();
    let mut last_heartbeat_ms = initial_now_ms;
    let mut counters = FeedCounters::default();

    publish(decoder.initial_snapshot(initial_now_ms))?;

    while should_keep_streaming(start, seconds) {
        let Some(data) = stream.recv() else {
            break;
        };

        counters.record_usb_chunk(data.len(), crate::unix_time_ms());
        counters.dropped_usb_chunks = stream.dropped_chunks();

        let output = decoder.decode_chunk(&data, stream.dropped_chunks(), &mut counters)?;
        for message in output.messages {
            publish(message)?;
        }
        if let Some(batch) = output.frame_batch {
            submit_frame_batch(batch)?;
        }

        counters.dropped_usb_chunks = stream.dropped_chunks();
        for message in decoder.housekeeping_messages(
            crate::unix_time_ms(),
            initial_now_ms,
            &mut counters,
            &mut last_heartbeat_ms,
        ) {
            publish(message)?;
        }
        observe_stream(stream, counters)?;
    }

    Ok(())
}

trait RadioFeedDecoder {
    fn initial_snapshot(&self, now_ms: u64) -> FeedMessage;

    fn decode_chunk(
        &mut self,
        data: &[u8],
        dropped_chunks: u64,
        counters: &mut FeedCounters,
    ) -> Result<FeedChunkOutput, String>;

    fn housekeeping_messages(
        &mut self,
        now_ms: u64,
        started_ms: u64,
        counters: &mut FeedCounters,
        last_heartbeat_ms: &mut u64,
    ) -> Vec<FeedMessage>;
}

#[derive(Debug, Default)]
struct FeedChunkOutput {
    messages: Vec<FeedMessage>,
    frame_batch: Option<FrameRecordBatch>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FeedDecoderKind {
    ModesAircraft,
}

impl FeedDecoderKind {
    pub(crate) fn for_protocol(protocol: Protocol, command: &str) -> Result<Self, String> {
        if protocol.uses_current_modes_decoder() {
            Ok(Self::ModesAircraft)
        } else {
            Err(unsupported_decoder_message(command, protocol))
        }
    }

    fn build_decoder(
        self,
        feed_config: &FeedRuntimeConfig,
        radio: RadioConfig,
        source_metadata: FrameRecordSourceMetadata,
        stream_start_ms: u64,
    ) -> ActiveFeedDecoder<'_> {
        match self {
            Self::ModesAircraft => ActiveFeedDecoder::ModesAircraft(ModesAircraftFeedDecoder::new(
                feed_config,
                radio,
                source_metadata,
                stream_start_ms,
            )),
        }
    }
}

enum ActiveFeedDecoder<'a> {
    ModesAircraft(ModesAircraftFeedDecoder<'a>),
}

impl RadioFeedDecoder for ActiveFeedDecoder<'_> {
    fn initial_snapshot(&self, now_ms: u64) -> FeedMessage {
        match self {
            Self::ModesAircraft(decoder) => decoder.initial_snapshot(now_ms),
        }
    }

    fn decode_chunk(
        &mut self,
        data: &[u8],
        dropped_chunks: u64,
        counters: &mut FeedCounters,
    ) -> Result<FeedChunkOutput, String> {
        match self {
            Self::ModesAircraft(decoder) => decoder.decode_chunk(data, dropped_chunks, counters),
        }
    }

    fn housekeeping_messages(
        &mut self,
        now_ms: u64,
        started_ms: u64,
        counters: &mut FeedCounters,
        last_heartbeat_ms: &mut u64,
    ) -> Vec<FeedMessage> {
        match self {
            Self::ModesAircraft(decoder) => {
                decoder.housekeeping_messages(now_ms, started_ms, counters, last_heartbeat_ms)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FrameDecoderKind {
    Modes,
}

impl FrameDecoderKind {
    pub(crate) fn for_protocol(protocol: Protocol, command: &str) -> Result<Self, String> {
        if protocol.uses_current_modes_decoder() {
            Ok(Self::Modes)
        } else {
            Err(unsupported_decoder_message(command, protocol))
        }
    }

    pub(crate) fn build_decoder(self) -> ModesFrameDecoder {
        match self {
            Self::Modes => ModesFrameDecoder::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct FrameRecordSourceMetadata {
    gain_mode: String,
    gain_tenth_db: Option<i32>,
    bias_t: bool,
    device_index: u64,
    tuner_name: String,
}

impl FrameRecordSourceMetadata {
    fn new(config: RtlSdrConfig, tuner_name: String) -> Result<Self, String> {
        let device_index = u64::try_from(config.device_index)
            .map_err(|_| "RTL-SDR device index overflowed u64".to_owned())?;
        let (gain_mode, gain_tenth_db) = match config.gain {
            GainMode::Auto => ("auto".to_owned(), None),
            GainMode::Manual(gain_tenth_db) => ("manual".to_owned(), Some(gain_tenth_db)),
        };

        Ok(Self {
            gain_mode,
            gain_tenth_db,
            bias_t: config.bias_t,
            device_index,
            tuner_name,
        })
    }
}

struct ModesAircraftFeedDecoder<'a> {
    feed_config: &'a FeedRuntimeConfig,
    frames: ModesFrameDecoder,
    store: AircraftStore,
    radio: RadioConfig,
    source_metadata: FrameRecordSourceMetadata,
    stream_start_ms: u64,
    stream_id: String,
    frame_sequence: u64,
    chunk_sequence: u64,
    chunk_sample_index: u64,
    dropped_chunks: u64,
    dropped_samples_before: u64,
}

impl<'a> ModesAircraftFeedDecoder<'a> {
    fn new(
        feed_config: &'a FeedRuntimeConfig,
        radio: RadioConfig,
        source_metadata: FrameRecordSourceMetadata,
        stream_start_ms: u64,
    ) -> Self {
        let stream_id = format!("{}-{stream_start_ms}", radio.protocol.key());

        Self {
            feed_config,
            frames: ModesFrameDecoder::default(),
            store: AircraftStore::default(),
            radio,
            source_metadata,
            stream_start_ms,
            stream_id,
            frame_sequence: 0,
            chunk_sequence: 0,
            chunk_sample_index: 0,
            dropped_chunks: 0,
            dropped_samples_before: 0,
        }
    }

    fn protocol(&self) -> Protocol {
        self.feed_config.radio.protocol
    }

    fn receiver_identity(&self) -> Option<ReceiverIdentity> {
        self.feed_config.receiver_identity.clone()
    }

    fn enrich_aircraft(&self, aircraft: AircraftSnapshot, now_ms: u64) -> AircraftSnapshot {
        enrich_aircraft_snapshot(aircraft, self.feed_config.receiver_site.as_ref(), now_ms)
    }

    fn enrich_aircraft_list(
        &self,
        aircraft: Vec<AircraftSnapshot>,
        now_ms: u64,
    ) -> Vec<AircraftSnapshot> {
        enrich_aircraft_snapshots(aircraft, self.feed_config.receiver_site.as_ref(), now_ms)
    }
}

impl RadioFeedDecoder for ModesAircraftFeedDecoder<'_> {
    fn initial_snapshot(&self, now_ms: u64) -> FeedMessage {
        FeedMessage::snapshot_for_protocol(
            self.protocol(),
            now_ms,
            self.enrich_aircraft_list(self.store.snapshots(), now_ms),
        )
        .with_receiver(self.receiver_identity())
    }

    fn decode_chunk(
        &mut self,
        data: &[u8],
        dropped_chunks: u64,
        counters: &mut FeedCounters,
    ) -> Result<FeedChunkOutput, String> {
        let mut messages = Vec::new();
        let mut records = Vec::new();
        let chunk_samples = u64::try_from(data.len() / 2)
            .map_err(|_| "USB chunk sample count overflowed u64".to_owned())?;
        if dropped_chunks > self.dropped_chunks {
            let missed_chunks = dropped_chunks - self.dropped_chunks;
            self.dropped_samples_before = self
                .dropped_samples_before
                .saturating_add(missed_chunks.saturating_mul(chunk_samples));
            self.dropped_chunks = dropped_chunks;
        }
        let chunk_metrics = iq_chunk_metrics(data);

        for decoded in self.frames.decode_chunk(data) {
            counters.decoded_frames += 1;
            let now_ms = crate::unix_time_ms();
            let mut record = FrameRecord::from_decoded_frame(self.radio, now_ms, &decoded)
                .map_err(|error| error.to_string())?;
            record.frame_sequence = Some(self.frame_sequence);
            record.stream_start_ms = Some(self.stream_start_ms);
            record
                .receiver
                .clone_from(&self.feed_config.receiver_identity);
            record
                .receiver_site
                .clone_from(&self.feed_config.receiver_site);
            record.gain_mode = Some(self.source_metadata.gain_mode.clone());
            record.gain_tenth_db = self.source_metadata.gain_tenth_db;
            record.bias_t = Some(self.source_metadata.bias_t);
            record.device_index = Some(self.source_metadata.device_index);
            record.tuner_name = Some(self.source_metadata.tuner_name.clone());
            record.stream_id = Some(self.stream_id.clone());
            record.chunk_sequence = Some(self.chunk_sequence);
            record.chunk_sample_index = Some(self.chunk_sample_index);
            record.set_dropped_samples_before(self.dropped_samples_before);
            if let Some(metrics) = chunk_metrics {
                record.apply_iq_chunk_metrics(metrics);
            }

            let Some(snapshot) = self.store.update_frame(&decoded.frame, now_ms) else {
                records.push(record);
                self.frame_sequence = self.frame_sequence.saturating_add(1);
                continue;
            };

            records.push(record);
            self.frame_sequence = self.frame_sequence.saturating_add(1);
            counters.aircraft_updates += 1;
            counters.last_frame_ms = Some(now_ms);
            messages.push(
                FeedMessage::aircraft_for_protocol(
                    self.protocol(),
                    now_ms,
                    self.enrich_aircraft(snapshot, now_ms),
                )
                .with_receiver(self.receiver_identity()),
            );
        }

        self.chunk_sample_index = self.chunk_sample_index.saturating_add(chunk_samples);
        self.chunk_sequence = self.chunk_sequence.saturating_add(1);

        let frame_batch = match (
            records.is_empty(),
            self.feed_config.receiver_identity.clone(),
        ) {
            (false, Some(receiver)) => {
                Some(FrameRecordBatch::new(self.protocol(), receiver, records))
            }
            _ => None,
        };

        Ok(FeedChunkOutput {
            messages,
            frame_batch,
        })
    }

    fn housekeeping_messages(
        &mut self,
        now_ms: u64,
        started_ms: u64,
        counters: &mut FeedCounters,
        last_heartbeat_ms: &mut u64,
    ) -> Vec<FeedMessage> {
        let mut messages = Vec::new();

        for removed in self.store.evict_stale(now_ms, self.feed_config.stale_after) {
            counters.stale_aircraft_removed += 1;
            messages.push(
                FeedMessage::stale_aircraft_for_protocol(self.protocol(), now_ms, removed.icao)
                    .with_receiver(self.receiver_identity()),
            );
        }

        if self.feed_config.heartbeat_interval != 0
            && now_ms.saturating_sub(*last_heartbeat_ms) >= self.feed_config.heartbeat_interval
        {
            let stats = counters.stats(now_ms, started_ms, true, None, 0);
            messages.push(
                FeedMessage::heartbeat_for_protocol(
                    self.protocol(),
                    now_ms,
                    self.store.aircraft_count(),
                    stats,
                )
                .with_receiver(self.receiver_identity()),
            );
            *last_heartbeat_ms = now_ms;
        }

        messages
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FeedCounters {
    pub(crate) decoded_frames: u64,
    pub(crate) aircraft_updates: u64,
    pub(crate) stale_aircraft_removed: u64,
    pub(crate) dropped_usb_chunks: u64,
    pub(crate) last_frame_ms: Option<u64>,
    pub(crate) last_usb_chunk_ms: Option<u64>,
    pub(crate) usb_chunks: u64,
    pub(crate) usb_bytes: u64,
}

impl FeedCounters {
    pub(crate) fn record_usb_chunk(&mut self, byte_len: usize, now_ms: u64) {
        let byte_len = u64::try_from(byte_len).unwrap_or(u64::MAX);

        self.last_usb_chunk_ms = Some(now_ms);
        self.usb_chunks = self.usb_chunks.saturating_add(1);
        self.usb_bytes = self.usb_bytes.saturating_add(byte_len);
    }

    pub(crate) fn stats(
        self,
        now_ms: u64,
        started_ms: u64,
        receiver_connected: bool,
        last_error: Option<String>,
        websocket_clients: usize,
    ) -> FeedStats {
        let uptime_ms = now_ms.saturating_sub(started_ms);

        FeedStats {
            uptime_ms,
            receiver_site: None,
            receiver_connected,
            last_frame_ms: self.last_frame_ms,
            last_usb_chunk_ms: self.last_usb_chunk_ms,
            usb_chunks: self.usb_chunks,
            usb_chunks_per_second: rate_per_second(self.usb_chunks, uptime_ms),
            usb_bytes: self.usb_bytes,
            usb_bytes_per_second: bytes_per_second_for_millis(self.usb_bytes, uptime_ms),
            dropped_usb_chunks: self.dropped_usb_chunks,
            dropped_usb_chunk_ratio: dropped_usb_chunk_ratio(
                self.dropped_usb_chunks,
                self.usb_chunks,
            ),
            decoded_frames: self.decoded_frames,
            decoded_frames_per_second: rate_per_second(self.decoded_frames, uptime_ms),
            decoded_frames_per_megabyte: frames_per_megabyte(self.decoded_frames, self.usb_bytes),
            aircraft_updates: self.aircraft_updates,
            aircraft_updates_per_second: rate_per_second(self.aircraft_updates, uptime_ms),
            aircraft_updates_per_frame: ratio_per_frame(self.aircraft_updates, self.decoded_frames),
            stale_aircraft_removed: self.stale_aircraft_removed,
            last_error,
            websocket_clients,
            submission: SubmissionStatus::default().health(),
        }
    }
}

pub(crate) fn ensure_feed_decoder(protocol: Protocol, command: &str) -> Result<(), String> {
    FeedDecoderKind::for_protocol(protocol, command).map(|_| ())
}

fn unsupported_decoder_message(command: &str, protocol: Protocol) -> String {
    format!(
        "{command} is configured for {protocol}, but rsdb-usb only implements the ADS-B / Mode S decoder today"
    )
}

fn should_keep_streaming(start: Instant, seconds: Option<u64>) -> bool {
    seconds.is_none_or(|seconds| start.elapsed().as_secs() < seconds)
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn rate_per_second(count: u64, elapsed_ms: u64) -> f64 {
    if elapsed_ms == 0 {
        return 0.0;
    }

    let rate = count as f64 * 1_000.0 / elapsed_ms as f64;
    (rate * 10.0).round() / 10.0
}

pub(crate) fn bytes_per_second_for_millis(bytes: u64, elapsed_ms: u64) -> u64 {
    if elapsed_ms == 0 {
        return 0;
    }

    let bytes_per_second = u128::from(bytes) * 1_000 / u128::from(elapsed_ms);
    u64::try_from(bytes_per_second).unwrap_or(u64::MAX)
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn frames_per_megabyte(frames: u64, bytes: u64) -> f64 {
    if bytes == 0 {
        return 0.0;
    }

    let frames_per_megabyte = frames as f64 * 1_000_000.0 / bytes as f64;
    (frames_per_megabyte * 10.0).round() / 10.0
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn ratio_per_frame(numerator: u64, frames: u64) -> f64 {
    if frames == 0 {
        return 0.0;
    }

    let ratio = numerator as f64 / frames as f64;
    (ratio * 1_000.0).round() / 1_000.0
}

pub(crate) fn dropped_usb_chunk_ratio(dropped_chunks: u64, received_chunks: u64) -> f64 {
    let total_chunks = dropped_chunks.saturating_add(received_chunks);
    if total_chunks == 0 {
        return 0.0;
    }

    ratio_per_frame(dropped_chunks, total_chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_counters_report_receiver_quality() {
        let mut counters = FeedCounters::default();
        counters.record_usb_chunk(1_000_000, 100);
        counters.record_usb_chunk(1_000_000, 200);
        counters.dropped_usb_chunks = 1;
        counters.decoded_frames = 50;
        counters.aircraft_updates = 25;
        counters.last_frame_ms = Some(250);

        let stats = counters.stats(10_000, 0, true, None, 3);

        assert_eq!(stats.last_usb_chunk_ms, Some(200));
        assert_eq!(stats.usb_chunks, 2);
        assert_approx_eq(stats.usb_chunks_per_second, 0.2);
        assert_eq!(stats.usb_bytes, 2_000_000);
        assert_eq!(stats.usb_bytes_per_second, 200_000);
        assert_approx_eq(stats.dropped_usb_chunk_ratio, 0.333);
        assert_approx_eq(stats.decoded_frames_per_megabyte, 25.0);
        assert_approx_eq(stats.aircraft_updates_per_frame, 0.5);
        assert_eq!(stats.websocket_clients, 3);
    }

    #[test]
    fn decoder_registry_rejects_non_modes_protocols() {
        assert_eq!(
            FeedDecoderKind::for_protocol(Protocol::Adsb1090, "serve").unwrap(),
            FeedDecoderKind::ModesAircraft
        );
        assert_eq!(
            FrameDecoderKind::for_protocol(Protocol::Adsb1090, "record-frames").unwrap(),
            FrameDecoderKind::Modes
        );
        assert!(
            FeedDecoderKind::for_protocol(Protocol::Uat978, "serve")
                .unwrap_err()
                .contains("only implements the ADS-B / Mode S decoder today")
        );
    }

    fn assert_approx_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {actual} to be within 0.001 of {expected}"
        );
    }
}
