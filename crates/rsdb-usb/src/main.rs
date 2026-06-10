use std::env;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Read, Write};
#[cfg(feature = "websocket")]
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::ExitCode;
#[cfg(feature = "websocket")]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(feature = "websocket")]
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod config;
mod decoder;
mod usb;

use config::{
    DEFAULT_COLLECTOR_HOST, DEFAULT_CONFIG_PATH, DEFAULT_HEARTBEAT_SECONDS, FeedRuntimeConfig,
    RuntimeConfig, SubmissionConfig, parse_index, parse_optional_seconds, parse_port,
    parse_required_path, parse_required_seconds, parse_seconds, take_config_path,
};
use decoder::{FrameDecoderKind, bytes_per_second_for_millis, ensure_feed_decoder, run_feed};
#[cfg(all(test, feature = "websocket"))]
use rsdb::PendingSubmission;
#[cfg(feature = "websocket")]
use rsdb::SubmissionOutbox;
use rsdb::{
    AdsbMessage, ExtendedSquitter, FeedMessage, Frame, FrameRecord, FrameRecordBatch,
    FrameReplayConfig, RadioConfig, ReceiverAllowlist, ReceiverIdentity, SignedSubmission,
    SubmissionPayload, SubmissionSigner, SubmissionStatus, iq_chunk_metrics, replay_frame_records,
};
use usb::{GainMode, IqStream, RtlSdrConfig, RtlSdrSource, list_rtl_sdr_devices};

#[cfg(feature = "websocket")]
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "websocket")]
static TLS_CLIENT_CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
#[cfg(feature = "websocket")]
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
#[cfg(feature = "websocket")]
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "websocket")]
fn websocket_accept_key(key: &str) -> String {
    use sha1::{Digest, Sha1};

    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WEBSOCKET_GUID.as_bytes());
    base64_encode(&hasher.finalize())
}

#[cfg(feature = "websocket")]
fn write_websocket_text_frame(stream: &mut impl Write, message: &str) -> std::io::Result<()> {
    let payload = message.as_bytes();
    let payload_len = payload.len();
    let mut header = Vec::with_capacity(10);

    header.push(0x81);
    match payload_len {
        0..=125 => header.push(u8::try_from(payload_len).expect("payload length fits u8")),
        126..=65_535 => {
            header.push(126);
            header.extend_from_slice(
                &u16::try_from(payload_len)
                    .expect("payload length fits u16")
                    .to_be_bytes(),
            );
        }
        _ => {
            header.push(127);
            header.extend_from_slice(
                &u64::try_from(payload_len)
                    .expect("payload length fits u64")
                    .to_be_bytes(),
            );
        }
    }

    stream.write_all(&header)?;
    stream.write_all(payload)
}

#[cfg(feature = "websocket")]
fn base64_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let value = (b0 << 16) | (b1 << 8) | b2;

        encoded.push(base64_char((value >> 18) & 0x3f));
        encoded.push(base64_char((value >> 12) & 0x3f));
        if chunk.len() > 1 {
            encoded.push(base64_char((value >> 6) & 0x3f));
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(base64_char(value & 0x3f));
        } else {
            encoded.push('=');
        }
    }

    encoded
}

#[cfg(feature = "websocket")]
fn base64_char(index: u32) -> char {
    let index = usize::try_from(index).expect("base64 index fits usize");
    char::from(BASE64_ALPHABET[index])
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let config_path = take_config_path(&mut args)?;
    let runtime = RuntimeConfig::load(config_path)?;

    match args.first().map(String::as_str) {
        None | Some("list") => list_devices(),
        Some("open") => {
            let index = parse_index(args.get(1), runtime.device_index)?;
            open_device(runtime.rtl_sdr_config(index))
        }
        Some("stream") => {
            let index = parse_index(args.get(1), runtime.device_index)?;
            let seconds = parse_seconds(args.get(2), runtime.stream_seconds)?;
            stream_samples(runtime.rtl_sdr_config(index), seconds)
        }
        Some("decode") => {
            let index = parse_index(args.get(1), runtime.device_index)?;
            let seconds = parse_seconds(args.get(2), runtime.stream_seconds)?;
            decode_live_frames(runtime.rtl_sdr_config(index), seconds)
        }
        Some("json") => {
            let index = parse_index(args.get(1), runtime.device_index)?;
            let seconds = parse_optional_seconds(args.get(2), runtime.json_seconds)?;
            let feed_config = runtime.feed_config()?;
            emit_json(runtime.rtl_sdr_config(index), &feed_config, seconds)
        }
        Some("serve") => {
            let index = parse_index(args.get(1), runtime.device_index)?;
            let host = args.get(2).map_or(DEFAULT_COLLECTOR_HOST, String::as_str);
            let port = args.get(3).map_or(Ok(runtime.collector_port), |port| {
                parse_port(port, "collector port")
            })?;
            if args.len() > 4 {
                return Err("serve accepts at most device index, host, and port".to_owned());
            }
            let bind = format!("{host}:{port}");
            let feed_config = runtime.feed_config()?;
            let submission_config = runtime.submission_config()?;
            serve_websocket(
                runtime.rtl_sdr_config(index),
                &feed_config,
                &bind,
                submission_config,
            )
        }
        Some("sign-submission") => sign_submission_command(&runtime, args.get(1)),
        Some("allowlist-entry") => allowlist_entry_command(&runtime),
        Some("record-frames") => {
            let seconds = parse_required_seconds(args.get(1), "record-frames seconds")?;
            let path = parse_required_path(args.get(2), "record-frames path")?;
            let index = parse_index(args.get(3), runtime.device_index)?;
            let feed_config = runtime.feed_config()?;
            record_frame_records(runtime.rtl_sdr_config(index), &feed_config, seconds, &path)
        }
        Some("replay") => {
            let path = parse_required_path(args.get(1), "replay path")?;
            replay_feed_file(&path)
        }
        Some("replay-frames") => {
            let path = parse_required_path(args.get(1), "replay-frames path")?;
            let feed_config = runtime.feed_config()?;
            replay_frame_record_file(&path, &feed_config)
        }
        Some("verify-submission") => {
            let allowlist_path = parse_required_path(args.get(1), "allowlist path")?;
            let submission_path = parse_required_path(args.get(2), "submission path")?;
            verify_submission_file(&allowlist_path, &submission_path)
        }
        Some("-h" | "--help" | "help") => {
            print_usage();
            Ok(())
        }
        Some(command) => {
            print_usage();
            Err(format!("unknown command: {command}"))
        }
    }
}

fn sign_submission_command(runtime: &RuntimeConfig, path: Option<&String>) -> Result<(), String> {
    let path = parse_required_path(path, "feed message path")?;
    let signer = runtime.submission_signer()?;
    let receiver_identity = runtime.required_receiver_identity()?;

    sign_submission_file(&path, &signer, &receiver_identity)
}

fn allowlist_entry_command(runtime: &RuntimeConfig) -> Result<(), String> {
    let signer = runtime.submission_signer()?;

    print_allowlist_entry(&signer);
    Ok(())
}

fn list_devices() -> Result<(), String> {
    let devices = list_rtl_sdr_devices().map_err(|error| error.to_string())?;

    if devices.is_empty() {
        println!("No RTL-SDR USB devices found.");
        return Ok(());
    }

    for device in devices {
        println!(
            "#{index}: {vendor:04x}:{product:04x} bus={bus} address={address}",
            index = device.index,
            vendor = device.vendor_id,
            product = device.product_id,
            bus = device.bus,
            address = device.address
        );

        if let Some(manufacturer) = device.manufacturer {
            println!("  manufacturer: {manufacturer}");
        }
        if let Some(product) = device.product {
            println!("  product:      {product}");
        }
        if let Some(serial) = device.serial {
            println!("  serial:       {serial}");
        }
    }

    Ok(())
}

fn open_device(config: RtlSdrConfig) -> Result<(), String> {
    let source = RtlSdrSource::open(config).map_err(|error| error.to_string())?;

    println!("Opened RTL-SDR device #{}", config.device_index);
    println!("  protocol:    {}", config.protocol);
    println!("  tuner:       {}", source.tuner_name());
    println!("  center:      {} Hz", source.center_frequency_hz());
    println!("  sample rate: {} Hz", source.sample_rate_hz());
    println!("  gains:       {:?}", source.supported_gains_tenth_db());

    Ok(())
}

fn stream_samples(config: RtlSdrConfig, seconds: u64) -> Result<(), String> {
    let mut source = RtlSdrSource::open(config).map_err(|error| error.to_string())?;

    println!(
        "Streaming RTL-SDR device #{} for {} at {} Hz for {seconds}s",
        config.device_index, config.protocol, config.center_frequency_hz
    );

    let stream = source
        .start_streaming()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    let mut total_bytes = 0_u64;
    let mut chunks = 0_u64;

    while start.elapsed().as_secs() < seconds {
        let Some(data) = stream.recv() else {
            break;
        };
        total_bytes += data.len() as u64;
        chunks += 1;
    }

    stream.stop();

    let elapsed = start.elapsed();
    let megabytes = format_megabytes(total_bytes);
    let throughput = format_megabytes(bytes_per_second(total_bytes, elapsed));
    let elapsed_seconds = format_seconds(elapsed);

    println!("Captured {megabytes} MB in {chunks} chunks over {elapsed_seconds}s");
    println!("Throughput: {throughput} MB/s");
    println!("Dropped chunks: {}", stream.dropped_chunks());

    Ok(())
}

fn decode_live_frames(config: RtlSdrConfig, seconds: u64) -> Result<(), String> {
    let decoder_kind = FrameDecoderKind::for_protocol(config.protocol, "decode")?;
    let mut source = RtlSdrSource::open(config).map_err(|error| error.to_string())?;

    println!(
        "Decoding RTL-SDR device #{} for {} at {} Hz for {seconds}s",
        config.device_index, config.protocol, config.center_frequency_hz
    );

    let stream = source
        .start_streaming()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    let mut decoder = decoder_kind.build_decoder();
    let mut decoded_count = 0_u64;

    while start.elapsed().as_secs() < seconds {
        let Some(data) = stream.recv() else {
            break;
        };

        for decoded in decoder.decode_chunk(&data) {
            decoded_count += 1;
            print_decoded_frame(decoded.sample_index, &decoded.frame);
        }
    }

    stream.stop();

    println!(
        "Decoded {decoded_count} frames; dropped USB chunks: {}",
        stream.dropped_chunks()
    );

    Ok(())
}

fn emit_json(
    config: RtlSdrConfig,
    feed_config: &FeedRuntimeConfig,
    seconds: Option<u64>,
) -> Result<(), String> {
    ensure_feed_decoder(config.protocol, "json")?;

    run_feed(
        config,
        feed_config,
        seconds,
        |message| {
            println!(
                "{}",
                serde_json::to_string(&message).map_err(|error| error.to_string())?
            );
            Ok(())
        },
        |_| Ok(()),
        |_, _| Ok(()),
    )
}

#[cfg(feature = "websocket")]
fn serve_websocket(
    config: RtlSdrConfig,
    feed_config: &FeedRuntimeConfig,
    bind: &str,
    submission_config: Option<SubmissionConfig>,
) -> Result<(), String> {
    websocket::serve(config, feed_config, bind, submission_config)
}

#[cfg(not(feature = "websocket"))]
fn serve_websocket(
    _config: RtlSdrConfig,
    _feed_config: &FeedRuntimeConfig,
    _bind: &str,
    _submission_config: Option<SubmissionConfig>,
) -> Result<(), String> {
    Err("rebuild rsdb-usb with the websocket feature to enable WebSocket serving".to_owned())
}

fn record_frame_records(
    config: RtlSdrConfig,
    feed_config: &FeedRuntimeConfig,
    seconds: u64,
    path: &Path,
) -> Result<(), String> {
    let decoder_kind = FrameDecoderKind::for_protocol(config.protocol, "record-frames")?;
    let file = fs::File::create(path)
        .map_err(|error| format!("{}: create failed: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);

    if seconds == 0 {
        writer
            .flush()
            .map_err(|error| format!("{}: flush failed: {error}", path.display()))?;
        eprintln!("Recorded 0 decoded frames to {}", path.display());
        return Ok(());
    }

    let mut source = RtlSdrSource::open(config).map_err(|error| error.to_string())?;
    let radio = RadioConfig::for_protocol(config.protocol)
        .with_center_frequency_hz(source.center_frequency_hz())
        .with_sample_rate_hz(source.sample_rate_hz());
    let source_metadata = FrameRecordSourceMetadata::new(config, source.tuner_name())?;
    let stream = source
        .start_streaming()
        .map_err(|error| error.to_string())?;
    let context = FrameRecordStreamContext {
        radio,
        decoder_kind,
        feed_config,
        source_metadata: &source_metadata,
    };
    let result = record_frame_records_from_stream(context, &stream, seconds, path, &mut writer);

    stream.stop();

    let recorded = result?;
    writer
        .flush()
        .map_err(|error| format!("{}: flush failed: {error}", path.display()))?;
    eprintln!(
        "Recorded {recorded} decoded frames to {}; dropped USB chunks: {}",
        path.display(),
        stream.dropped_chunks()
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct FrameRecordStreamContext<'a> {
    radio: RadioConfig,
    decoder_kind: FrameDecoderKind,
    feed_config: &'a FeedRuntimeConfig,
    source_metadata: &'a FrameRecordSourceMetadata,
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

fn record_frame_records_from_stream(
    context: FrameRecordStreamContext<'_>,
    stream: &IqStream,
    seconds: u64,
    path: &Path,
    writer: &mut impl Write,
) -> Result<u64, String> {
    let start = Instant::now();
    let mut decoder = context.decoder_kind.build_decoder();
    let mut recorded = 0_u64;
    let stream_start_ms = unix_time_ms();
    let stream_id = format!("{}-{stream_start_ms}", context.radio.protocol.key());
    let mut chunk_sequence = 0_u64;
    let mut chunk_sample_index = 0_u64;
    let mut dropped_samples_before = 0_u64;
    let mut dropped_chunks = 0_u64;

    while start.elapsed().as_secs() < seconds {
        let Some(data) = stream.recv() else {
            break;
        };

        let current_dropped_chunks = stream.dropped_chunks();
        if current_dropped_chunks > dropped_chunks {
            let missed_chunks = current_dropped_chunks - dropped_chunks;
            let chunk_samples = u64::try_from(data.len() / 2)
                .map_err(|_| "USB chunk sample count overflowed u64".to_owned())?;
            dropped_samples_before =
                dropped_samples_before.saturating_add(missed_chunks.saturating_mul(chunk_samples));
            dropped_chunks = current_dropped_chunks;
        }

        let chunk_metrics = iq_chunk_metrics(&data);
        for decoded in decoder.decode_chunk(&data) {
            let mut record =
                FrameRecord::from_decoded_frame(context.radio, unix_time_ms(), &decoded)
                    .map_err(|error| error.to_string())?;
            record.frame_sequence = Some(recorded);
            record.stream_start_ms = Some(stream_start_ms);
            record
                .receiver
                .clone_from(&context.feed_config.receiver_identity);
            record
                .receiver_site
                .clone_from(&context.feed_config.receiver_site);
            record.gain_mode = Some(context.source_metadata.gain_mode.clone());
            record.gain_tenth_db = context.source_metadata.gain_tenth_db;
            record.bias_t = Some(context.source_metadata.bias_t);
            record.device_index = Some(context.source_metadata.device_index);
            record.tuner_name = Some(context.source_metadata.tuner_name.clone());
            record.stream_id = Some(stream_id.clone());
            record.chunk_sequence = Some(chunk_sequence);
            record.chunk_sample_index = Some(chunk_sample_index);
            record.set_dropped_samples_before(dropped_samples_before);
            if let Some(metrics) = chunk_metrics {
                record.apply_iq_chunk_metrics(metrics);
            }

            serde_json::to_writer(&mut *writer, &record).map_err(|error| error.to_string())?;
            writer
                .write_all(b"\n")
                .map_err(|error| format!("{}: write failed: {error}", path.display()))?;
            recorded += 1;
        }

        let chunk_samples = u64::try_from(data.len() / 2)
            .map_err(|_| "USB chunk sample count overflowed u64".to_owned())?;
        chunk_sample_index = chunk_sample_index.saturating_add(chunk_samples);
        chunk_sequence = chunk_sequence.saturating_add(1);
    }

    Ok(recorded)
}

fn replay_feed_file(path: &Path) -> Result<(), String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("{}: open failed: {error}", path.display()))?;
    let reader = BufReader::new(file);

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| {
            format!(
                "{}:{}: read failed: {error}",
                path.display(),
                line_index + 1
            )
        })?;
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        let message = parse_feed_line(path, line_index + 1, line)?;
        println!(
            "{}",
            serde_json::to_string(&message).map_err(|error| error.to_string())?
        );
    }

    Ok(())
}

fn parse_feed_line(path: &Path, line_number: usize, line: &str) -> Result<FeedMessage, String> {
    let message = serde_json::from_str::<FeedMessage>(line).map_err(|error| {
        format!(
            "{}:{line_number}: invalid feed message JSON: {error}",
            path.display()
        )
    })?;
    validate_feed_schema(path, line_number, message)
}

fn validate_feed_schema(
    path: &Path,
    line_number: usize,
    message: FeedMessage,
) -> Result<FeedMessage, String> {
    if message.is_supported_schema_version() {
        Ok(message)
    } else {
        let location = if line_number == 0 {
            path.display().to_string()
        } else {
            format!("{}:{line_number}", path.display())
        };

        Err(format!(
            "{location}: unsupported feed schema_version {}; expected {}",
            message.schema_version(),
            rsdb::FEED_SCHEMA_VERSION
        ))
    }
}

fn replay_frame_record_file(path: &Path, feed_config: &FeedRuntimeConfig) -> Result<(), String> {
    let records = read_frame_record_file(path)?;
    let protocol = records
        .first()
        .map_or(feed_config.radio.protocol, |record| record.protocol);
    let mut replay_config =
        FrameReplayConfig::new(protocol, records.first().map_or(0, |record| record.now_ms));
    replay_config.stale_after_ms = feed_config.stale_after;
    replay_config.heartbeat_interval_ms = feed_config.heartbeat_interval;
    replay_config
        .receiver_identity
        .clone_from(&feed_config.receiver_identity);
    replay_config
        .receiver_site
        .clone_from(&feed_config.receiver_site);
    let messages = replay_frame_records(&records, &replay_config)
        .map_err(|error| format!("{}: frame replay failed: {error}", path.display()))?;

    for message in messages {
        println!(
            "{}",
            serde_json::to_string(&message).map_err(|error| error.to_string())?
        );
    }

    Ok(())
}

fn verify_submission_file(allowlist_path: &Path, submission_path: &Path) -> Result<(), String> {
    let allowlist = read_allowlist_file(allowlist_path)?;
    let submission = read_json_file::<SignedSubmission>(submission_path, "submission")?;

    allowlist.verify_submission(&submission).map_err(|error| {
        format!(
            "{}: verification failed: {error}",
            submission_path.display()
        )
    })?;

    println!(
        "ok receiver={} payload={}",
        submission.receiver_id,
        submission.payload.kind()
    );
    Ok(())
}

fn sign_submission_file(
    path: &Path,
    signer: &SubmissionSigner,
    receiver_identity: &ReceiverIdentity,
) -> Result<(), String> {
    let message = read_feed_message_file(path)?;
    let message = message_for_signing(message, receiver_identity)?;
    let submission = signer
        .sign(message, unix_time_ms())
        .map_err(|error| format!("{}: signing failed: {error}", path.display()))?;

    println!(
        "{}",
        serde_json::to_string_pretty(&submission).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn print_allowlist_entry(signer: &SubmissionSigner) {
    println!("{}", signer.public_key_hex());
}

fn read_allowlist_file(path: &Path) -> Result<ReceiverAllowlist, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("{}: read failed: {error}", path.display()))?;
    let public_keys = text
        .lines()
        .flat_map(|line| {
            line.split_once('#')
                .map_or(line, |(before, _)| before)
                .split(|character: char| character == ',' || character.is_whitespace())
        })
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            if entry.len() == 64 && entry.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Ok(entry.to_ascii_lowercase())
            } else {
                Err(format!(
                    "{}: allowlist public keys must be 64 hex characters",
                    path.display()
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    if public_keys.is_empty() {
        return Err(format!(
            "{}: allowlist must contain at least one public key",
            path.display()
        ));
    }

    Ok(ReceiverAllowlist::new(public_keys))
}

fn read_feed_message_file(path: &Path) -> Result<FeedMessage, String> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("stdin: read failed: {error}"))?;
        text
    } else {
        fs::read_to_string(path)
            .map_err(|error| format!("{}: read failed: {error}", path.display()))?
    };
    let message = serde_json::from_str::<FeedMessage>(&text)
        .map_err(|error| format!("{}: invalid feed message JSON: {error}", path.display()))?;

    validate_feed_schema(path, 0, message)
}

fn message_for_signing(
    message: FeedMessage,
    receiver_identity: &ReceiverIdentity,
) -> Result<FeedMessage, String> {
    match message.receiver() {
        Some(receiver) if receiver.id == receiver_identity.id => Ok(message),
        Some(receiver) => Err(format!(
            "feed message receiver_id {} does not match configured receiver_id {}",
            receiver.id, receiver_identity.id
        )),
        None => Ok(message.with_receiver(Some(receiver_identity.clone()))),
    }
}

#[cfg(feature = "websocket")]
fn payload_for_signing(
    payload: SubmissionPayload,
    receiver_identity: &ReceiverIdentity,
) -> Result<SubmissionPayload, String> {
    match payload {
        SubmissionPayload::FeedMessage(message) => {
            validate_feed_schema(Path::new("receiver"), 0, message)
                .and_then(|message| message_for_signing(message, receiver_identity))
                .map(SubmissionPayload::FeedMessage)
        }
        SubmissionPayload::FrameRecords(batch) => {
            frame_batch_for_signing(batch, receiver_identity).map(SubmissionPayload::FrameRecords)
        }
    }
}

#[cfg(feature = "websocket")]
fn frame_batch_for_signing(
    mut batch: FrameRecordBatch,
    receiver_identity: &ReceiverIdentity,
) -> Result<FrameRecordBatch, String> {
    if batch.receiver.id != receiver_identity.id {
        return Err(format!(
            "frame batch receiver_id {} does not match configured receiver_id {}",
            batch.receiver.id, receiver_identity.id
        ));
    }

    batch.receiver = receiver_identity.clone();
    for record in &mut batch.records {
        if let Some(record_receiver) = &record.receiver
            && record_receiver.id != receiver_identity.id
        {
            return Err(format!(
                "frame record receiver_id {} does not match configured receiver_id {}",
                record_receiver.id, receiver_identity.id
            ));
        }
        record.receiver = Some(receiver_identity.clone());
    }
    batch.validate().map_err(|error| error.to_string())?;

    Ok(batch)
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path, kind: &str) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("{}: read failed: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("{}: invalid {kind} JSON: {error}", path.display()))
}

fn read_frame_record_file(path: &Path) -> Result<Vec<FrameRecord>, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("{}: open failed: {error}", path.display()))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| {
            format!(
                "{}:{}: read failed: {error}",
                path.display(),
                line_index + 1
            )
        })?;
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        records.push(parse_frame_record_line(path, line_index + 1, line)?);
    }

    Ok(records)
}

fn parse_frame_record_line(
    path: &Path,
    line_number: usize,
    line: &str,
) -> Result<FrameRecord, String> {
    let record = serde_json::from_str::<FrameRecord>(line).map_err(|error| {
        format!(
            "{}:{line_number}: invalid frame record JSON: {error}",
            path.display()
        )
    })?;

    record
        .parse_frame()
        .map_err(|error| format!("{}:{line_number}: {error}", path.display()))?;
    Ok(record)
}

#[derive(Debug, Clone)]
#[cfg(feature = "websocket")]
struct SubmissionWorker {
    sender: std::sync::mpsc::Sender<SubmissionPayload>,
    status: Arc<Mutex<SubmissionStatus>>,
}

#[cfg(feature = "websocket")]
fn start_submission_worker(config: SubmissionConfig) -> Result<SubmissionWorker, String> {
    let aggregate_urls = config
        .aggregate_urls
        .iter()
        .map(|url| aggregate_submit_url(url))
        .collect::<Vec<_>>();
    let outbox = config
        .outbox
        .as_ref()
        .map(SubmissionOutbox::open)
        .transpose()
        .map_err(|error| error.to_string())?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let status = Arc::new(Mutex::new(SubmissionStatus::enabled(
        aggregate_urls.clone(),
        outbox.is_some(),
    )));
    let worker_status = Arc::clone(&status);

    thread::Builder::new()
        .name("rsdb-submit".to_owned())
        .spawn(move || {
            run_submission_worker(
                receiver,
                &config,
                &aggregate_urls,
                outbox.as_ref(),
                &worker_status,
            );
        })
        .map_err(|error| error.to_string())?;

    Ok(SubmissionWorker { sender, status })
}

#[cfg(feature = "websocket")]
fn run_submission_worker(
    receiver: std::sync::mpsc::Receiver<SubmissionPayload>,
    config: &SubmissionConfig,
    aggregate_urls: &[String],
    outbox: Option<&SubmissionOutbox>,
    shared_status: &Arc<Mutex<SubmissionStatus>>,
) {
    let mut stats = SubmissionStatus::enabled(aggregate_urls.to_vec(), outbox.is_some());
    let mut last_report = Instant::now();
    let mut next_outbox_flush = Instant::now();

    publish_submission_status(shared_status, &stats);
    eprintln!(
        "Submitting signed receiver payloads to {}",
        aggregate_urls.join(", ")
    );
    if let Some(outbox) = &outbox {
        eprintln!(
            "Using durable submission outbox {}",
            outbox.path().display()
        );
        if let Err(error) = flush_submission_outbox(outbox, aggregate_urls, &mut stats) {
            next_outbox_flush = Instant::now() + config.retry_after;
            stats.last_error = Some(error.clone());
            eprintln!(
                "{}: outbox replay paused: {error}; retrying after {:?}",
                outbox.path().display(),
                config.retry_after
            );
        }
        publish_submission_status(shared_status, &stats);
    }

    for payload in receiver {
        stats.received = stats.received.saturating_add(1);
        let payload = match payload_for_signing(payload, &config.receiver_identity) {
            Ok(payload) => payload,
            Err(error) => {
                stats.last_error = Some(error.clone());
                eprintln!("receiver submission skipped: {error}");
                publish_submission_status(shared_status, &stats);
                continue;
            }
        };
        let submission = match config.signer.sign_payload(payload, unix_time_ms()) {
            Ok(submission) => submission,
            Err(error) => {
                let error = format!("failed to sign submission payload: {error}");
                stats.last_error = Some(error.clone());
                eprintln!("{error}");
                publish_submission_status(shared_status, &stats);
                continue;
            }
        };
        stats.signed = stats.signed.saturating_add(1);

        if let Some(outbox) = &outbox {
            let append = match outbox.append(&submission, aggregate_urls) {
                Ok(append) => append,
                Err(error) => {
                    let error = error.to_string();
                    stats.last_error = Some(error.clone());
                    eprintln!(
                        "{}: submission outbox append failed: {error}",
                        outbox.path().display()
                    );
                    publish_submission_status(shared_status, &stats);
                    continue;
                }
            };
            stats.outbox_queued = stats.outbox_queued.saturating_add(1);
            stats.outbox_pending = append.pending;
            refresh_submission_outbox_pending(outbox, &mut stats);
            stats.outbox_dropped = stats.outbox_dropped.saturating_add(append.dropped);
            stats.last_queued_ms = Some(unix_time_ms());

            if Instant::now() >= next_outbox_flush {
                match flush_submission_outbox(outbox, aggregate_urls, &mut stats) {
                    Ok(()) => next_outbox_flush = Instant::now(),
                    Err(error) => {
                        next_outbox_flush = Instant::now() + config.retry_after;
                        stats.last_error = Some(error);
                    }
                }
            }
        } else {
            for url in aggregate_urls {
                if let Err(error) = submit_with_retry(
                    url,
                    &submission,
                    config.retry_after,
                    &mut stats,
                    shared_status,
                ) {
                    stats.last_error = Some(error.clone());
                    eprintln!("{url}: submission stopped: {error}");
                }
            }
        }

        publish_submission_status(shared_status, &stats);
        if last_report.elapsed() >= Duration::from_secs(DEFAULT_HEARTBEAT_SECONDS) {
            print_submission_status(&stats);
            last_report = Instant::now();
        }
    }

    eprintln!("receiver submission worker stopped");
}

#[cfg(feature = "websocket")]
fn flush_submission_outbox(
    outbox: &SubmissionOutbox,
    aggregate_urls: &[String],
    stats: &mut SubmissionStatus,
) -> Result<(), String> {
    flush_submission_outbox_with_submitter(outbox, aggregate_urls, stats, submit_to_aggregate_url)
}

#[cfg(feature = "websocket")]
fn flush_submission_outbox_with_submitter(
    outbox: &SubmissionOutbox,
    aggregate_urls: &[String],
    stats: &mut SubmissionStatus,
    mut submitter: impl FnMut(&str, &SignedSubmission, &mut SubmissionStatus) -> Result<(), String>,
) -> Result<(), String> {
    let load = outbox.load().map_err(|error| error.to_string())?;
    let loaded_count = load.entries.len();
    let mut pending = Vec::new();
    let mut first_error = None;
    let mut failed_urls = Vec::<String>::new();
    let mut changed = load.discarded != 0;

    stats.update_outbox_pending(&load.entries);

    for mut entry in load.entries {
        let original_pending_urls = entry.pending_urls.clone();
        let urls = if entry.pending_urls.is_empty() {
            aggregate_urls.to_vec()
        } else {
            entry.pending_urls.clone()
        };
        let mut pending_urls = Vec::new();

        for url in &urls {
            if failed_urls.iter().any(|failed_url| failed_url == url) {
                pending_urls.push(url.clone());
                continue;
            }

            match submitter(url, &entry.submission, stats) {
                Ok(()) => {}
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    failed_urls.push(url.clone());
                    pending_urls.push(url.clone());
                }
            }
        }

        if pending_urls.is_empty() {
            stats.outbox_delivered = stats.outbox_delivered.saturating_add(1);
            changed = true;
        } else {
            if pending_urls != original_pending_urls {
                changed = true;
            }
            entry.pending_urls = pending_urls;
            pending.push(entry);
        }
    }

    if changed || first_error.is_none() || pending.len() != loaded_count {
        outbox
            .replace(&pending)
            .map_err(|error| error.to_string())?;
    }
    stats.update_outbox_pending(&pending);
    stats.outbox_dropped = stats.outbox_dropped.saturating_add(load.discarded);

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(feature = "websocket")]
fn refresh_submission_outbox_pending(outbox: &SubmissionOutbox, stats: &mut SubmissionStatus) {
    match outbox.load() {
        Ok(load) => stats.update_outbox_pending(&load.entries),
        Err(error) => stats.last_error = Some(error.to_string()),
    }
}

#[cfg(feature = "websocket")]
fn submit_to_aggregate_url(
    url: &str,
    submission: &SignedSubmission,
    stats: &mut SubmissionStatus,
) -> Result<(), String> {
    match submit_signed_submission(url, submission) {
        Ok(()) => {
            stats.record_delivery(url, unix_time_ms());
            Ok(())
        }
        Err(error) => {
            stats.record_failure(url, error.clone());
            Err(error)
        }
    }
}

#[cfg(feature = "websocket")]
fn submit_with_retry(
    url: &str,
    submission: &SignedSubmission,
    initial_retry_after: Duration,
    stats: &mut SubmissionStatus,
    shared_status: &Arc<Mutex<SubmissionStatus>>,
) -> Result<(), String> {
    let mut retry_after = initial_retry_after;

    loop {
        match submit_to_aggregate_url(url, submission, stats) {
            Ok(()) => {
                publish_submission_status(shared_status, stats);
                return Ok(());
            }
            Err(error) => {
                publish_submission_status(shared_status, stats);
                eprintln!("{url}: submit failed: {error}; retrying in {retry_after:?}");
                thread::sleep(retry_after);
                retry_after = retry_after.saturating_mul(2).min(Duration::from_mins(1));
            }
        }
    }
}

#[cfg(feature = "websocket")]
fn submit_signed_submission(url: &str, submission: &SignedSubmission) -> Result<(), String> {
    let body = serde_json::to_string(submission).map_err(|error| error.to_string())?;
    post_http_body(url, "application/json", body.as_bytes()).map(|_| ())
}

#[cfg(feature = "websocket")]
fn publish_submission_status(
    shared_status: &Arc<Mutex<SubmissionStatus>>,
    status: &SubmissionStatus,
) {
    *shared_status
        .lock()
        .expect("submission status mutex not poisoned") = status.clone();
}

#[cfg(feature = "websocket")]
fn print_submission_status(status: &SubmissionStatus) {
    eprintln!(
        "submission stats received={} signed={} delivered={} failed_attempts={} outbox_queued={} outbox_pending={} outbox_delivered={} outbox_dropped={} last_error={}",
        status.received,
        status.signed,
        status.delivered,
        status.failed_attempts,
        status.outbox_queued,
        status.outbox_pending,
        status.outbox_delivered,
        status.outbox_dropped,
        status.last_error.as_deref().unwrap_or("none")
    );
}

#[cfg(any(feature = "websocket", test))]
fn aggregate_submit_url(value: &str) -> String {
    let base = value.trim();
    let mut url = if base.starts_with("http://") || base.starts_with("https://") {
        base.to_owned()
    } else {
        format!("http://{base}")
    };

    if scheme_path(&url).is_none_or(|path| path == "/" || path.is_empty()) {
        url = format!("{}/submit", url.trim_end_matches('/'));
    }

    url
}

#[cfg(feature = "websocket")]
fn post_http_body(url: &str, content_type: &str, body: &[u8]) -> Result<String, String> {
    if url.starts_with("https://") {
        return post_https_body(url, content_type, body);
    }

    let target = HttpTarget::parse_http(url)?;
    let mut stream = connect_http_target(url, &target, DEFAULT_HTTP_TIMEOUT)?;
    write_http_post_request(&mut stream, &target, content_type, body)
        .map_err(|error| format!("{url}: request failed: {error}"))?;

    read_http_response(url, stream)
}

#[cfg(feature = "websocket")]
fn post_https_body(url: &str, content_type: &str, body: &[u8]) -> Result<String, String> {
    let target = HttpTarget::parse_https(url)?;
    let stream = connect_http_target(url, &target, DEFAULT_HTTP_TIMEOUT)?;
    let server_name = rustls::pki_types::ServerName::try_from(target.host.clone())
        .map_err(|_| format!("{url}: invalid TLS server name {}", target.host))?;
    let connection = rustls::ClientConnection::new(tls_client_config(url)?, server_name)
        .map_err(|error| format!("{url}: TLS setup failed: {error}"))?;
    let mut stream = rustls::StreamOwned::new(connection, stream);

    write_http_post_request(&mut stream, &target, content_type, body)
        .map_err(|error| format!("{url}: request failed: {error}"))?;

    read_http_response(url, stream)
}

#[cfg(feature = "websocket")]
fn tls_client_config(url: &str) -> Result<Arc<rustls::ClientConfig>, String> {
    TLS_CLIENT_CONFIG
        .get_or_init(load_tls_client_config)
        .clone()
        .map_err(|error| format!("{url}: {error}"))
}

#[cfg(feature = "websocket")]
fn load_tls_client_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let roots = webpki_roots::TLS_SERVER_ROOTS
        .iter()
        .cloned()
        .collect::<rustls::RootCertStore>();

    if roots.is_empty() {
        return Err("no usable Mozilla TLS root certificates loaded".to_owned());
    }

    let config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|error| format!("TLS protocol setup failed: {error}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();

    Ok(Arc::new(config))
}

#[cfg(feature = "websocket")]
fn write_http_post_request(
    stream: &mut impl Write,
    target: &HttpTarget,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let request = format!(
        "POST {} HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        target.path,
        target.host_header(),
        body.len()
    );

    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(feature = "websocket")]
fn connect_http_target(
    url: &str,
    target: &HttpTarget,
    timeout: Duration,
) -> Result<TcpStream, String> {
    let addresses = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|error| format!("{url}: failed to resolve host: {error}"))?
        .collect::<Vec<_>>();
    let mut last_error = None;
    let mut stream = None;

    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }

    let stream = stream.ok_or_else(|| {
        last_error.map_or_else(
            || format!("{url}: host did not resolve to an address"),
            |error| format!("{url}: connect failed: {error}"),
        )
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("{url}: failed to set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("{url}: failed to set write timeout: {error}"))?;

    Ok(stream)
}

#[cfg(feature = "websocket")]
fn read_http_response(url: &str, mut stream: impl Read) -> Result<String, String> {
    let response = read_http_response_bytes(url, &mut stream, DEFAULT_HTTP_TIMEOUT)?;
    let response = String::from_utf8(response)
        .map_err(|error| format!("{url}: response was not valid UTF-8: {error}"))?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("{url}: invalid HTTP response"))?;
    let status = headers.lines().next().unwrap_or_default();

    if !status.contains(" 2") {
        return Err(format!("{url}: {status}"));
    }

    Ok(body.to_owned())
}

#[cfg(feature = "websocket")]
fn read_http_response_bytes(
    url: &str,
    stream: &mut impl Read,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];

    loop {
        if http_response_complete(&response).map_err(|error| format!("{url}: {error}"))? {
            return Ok(response);
        }

        match stream.read(&mut chunk) {
            Ok(0) => {
                if response.is_empty() {
                    return Err(format!("{url}: empty HTTP response"));
                }
                return Ok(response);
            }
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if Instant::now() >= deadline {
                    return Err(format!("{url}: response read timed out"));
                }
            }
            Err(error) => return Err(format!("{url}: response read failed: {error}")),
        }
    }
}

#[cfg(feature = "websocket")]
fn http_response_complete(response: &[u8]) -> Result<bool, String> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(false);
    };
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|error| format!("invalid HTTP response headers: {error}"))?;
    let Some(content_length) = headers.lines().find_map(http_content_length) else {
        return Ok(false);
    };

    Ok(response.len() >= header_end + 4 + content_length)
}

#[cfg(feature = "websocket")]
fn http_content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    name.eq_ignore_ascii_case("content-length")
        .then(|| value.trim().parse::<usize>().ok())
        .flatten()
}

#[cfg(feature = "websocket")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpTarget {
    host: String,
    port: u16,
    path: String,
    default_port: u16,
}

#[cfg(feature = "websocket")]
impl HttpTarget {
    fn parse_http(url: &str) -> Result<Self, String> {
        Self::parse_with_scheme(url, "http://", 80)
    }

    #[cfg(feature = "websocket")]
    fn parse_https(url: &str) -> Result<Self, String> {
        Self::parse_with_scheme(url, "https://", 443)
    }

    fn parse_with_scheme(url: &str, scheme: &str, default_port: u16) -> Result<Self, String> {
        let Some(rest) = url.strip_prefix(scheme) else {
            return Err(format!(
                "{url}: only http:// and https:// URLs are supported"
            ));
        };
        let (authority, path) = rest.split_once('/').map_or((rest, "/"), |(host, path)| {
            (host, path.strip_prefix('/').unwrap_or(path))
        });
        let (host, port) =
            authority
                .rsplit_once(':')
                .map_or((authority, Ok(default_port)), |(host, port)| {
                    (
                        host,
                        port.parse()
                            .map_err(|_| format!("{url}: invalid port {port}")),
                    )
                });
        let port = port?;

        if host.is_empty() {
            return Err(format!("{url}: missing host"));
        }

        Ok(Self {
            host: host.to_owned(),
            port,
            path: format!("/{path}"),
            default_port,
        })
    }

    fn host_header(&self) -> String {
        if self.port == self.default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[cfg(any(feature = "websocket", test))]
fn scheme_path(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let (_, path) = rest.split_once('/')?;
    Some(path)
}

fn print_usage() {
    println!("Usage:");
    println!("  rsdb-usb [--config path] <command>");
    println!("  rsdb-usb list");
    println!("  rsdb-usb open [index]");
    println!("  rsdb-usb stream [index] [seconds]");
    println!("  rsdb-usb decode [index] [seconds]");
    println!("  rsdb-usb json [index] [seconds]");
    println!("  rsdb-usb serve [index] [host] [port]");
    println!("  rsdb-usb sign-submission <feed-message.json|->");
    println!("  rsdb-usb allowlist-entry");
    println!("  rsdb-usb record-frames <seconds> <path> [index]");
    println!("  rsdb-usb replay <path>");
    println!("  rsdb-usb replay-frames <path>");
    println!("  rsdb-usb verify-submission <allowlist.txt> <submission.json>");
    println!();
    println!("Config defaults load from RSDB_CONFIG or {DEFAULT_CONFIG_PATH} when present.");
    println!("Persistence is enabled when RSDB_PERSIST_DIR is set.");
}

fn bytes_per_second(bytes: u64, elapsed: Duration) -> u64 {
    let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    bytes_per_second_for_millis(bytes, millis)
}

fn format_megabytes(bytes: u64) -> String {
    let whole = bytes / 1_000_000;
    let fractional = (bytes % 1_000_000) / 10_000;
    format!("{whole}.{fractional:02}")
}

fn format_seconds(duration: Duration) -> String {
    let millis = duration.as_millis();
    let whole = millis / 1_000;
    let fractional = (millis % 1_000) / 10;
    format!("{whole}.{fractional:02}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ed25519_dalek::{Signer, SigningKey};

    use super::*;
    #[cfg(feature = "websocket")]
    use crate::config::BYTES_PER_MEGABYTE;
    #[cfg(feature = "websocket")]
    use rsdb::{Protocol, SubmissionOutboxConfig};

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_helpers_match_rfc_accept_key_and_text_frame() {
        assert_eq!(
            websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );

        let mut frame = Vec::new();
        write_websocket_text_frame(&mut frame, "hello").unwrap();
        assert_eq!(frame, b"\x81\x05hello");
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn submission_outbox_persists_pending_submissions() {
        let dir = temp_test_dir("rsdb-submit-outbox-persist");
        let urls = test_submit_urls();
        let outbox = SubmissionOutbox::open(&SubmissionOutboxConfig {
            dir: dir.clone(),
            max_bytes: BYTES_PER_MEGABYTE,
        })
        .unwrap();
        let first = signed_test_submission(1);
        let second = signed_test_submission(2);

        let first_append = outbox.append(&first, &urls).unwrap();
        let second_append = outbox.append(&second, &urls).unwrap();
        let load = outbox.load().unwrap();

        assert_eq!(first_append.pending, 1);
        assert_eq!(second_append.pending, 2);
        assert_eq!(
            load.entries
                .iter()
                .map(|entry| entry.submission.submission_id.as_str())
                .collect::<Vec<_>>(),
            vec![first.submission_id.as_str(), second.submission_id.as_str()]
        );
        assert_eq!(load.entries[0].pending_urls, urls);

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn submission_outbox_replaces_delivered_submissions() {
        let dir = temp_test_dir("rsdb-submit-outbox-replace");
        let urls = test_submit_urls();
        let outbox = SubmissionOutbox::open(&SubmissionOutboxConfig {
            dir: dir.clone(),
            max_bytes: BYTES_PER_MEGABYTE,
        })
        .unwrap();
        let first = signed_test_submission(1);
        let second = signed_test_submission(2);
        let second_entry = PendingSubmission::new(second.clone(), &urls);

        outbox.append(&first, &urls).unwrap();
        outbox.append(&second, &urls).unwrap();
        outbox.replace(std::slice::from_ref(&second_entry)).unwrap();

        let load = outbox.load().unwrap();

        assert_eq!(load.entries.len(), 1);
        assert_eq!(
            load.entries[0].submission.submission_id,
            second.submission_id
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn submission_outbox_trims_oldest_submissions_to_limit() {
        let dir = temp_test_dir("rsdb-submit-outbox-trim");
        let urls = test_submit_urls();
        let second = signed_test_submission(2);
        let outbox = SubmissionOutbox::open(&SubmissionOutboxConfig {
            dir: dir.clone(),
            max_bytes: pending_submission_line_bytes(&PendingSubmission::new(
                second.clone(),
                &urls,
            )),
        })
        .unwrap();

        outbox.append(&signed_test_submission(1), &urls).unwrap();
        let append = outbox.append(&second, &urls).unwrap();
        let load = outbox.load().unwrap();

        assert_eq!(append.pending, 1);
        assert_eq!(append.dropped, 1);
        assert_eq!(load.entries.len(), 1);
        assert_eq!(
            load.entries[0].submission.submission_id,
            second.submission_id
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn submission_outbox_discards_invalid_lines() {
        let dir = temp_test_dir("rsdb-submit-outbox-invalid");
        let urls = test_submit_urls();
        let outbox = SubmissionOutbox::open(&SubmissionOutboxConfig {
            dir: dir.clone(),
            max_bytes: BYTES_PER_MEGABYTE,
        })
        .unwrap();
        let submission = signed_test_submission(1);

        fs::write(outbox.path(), b"{not json}\n").unwrap();
        let append = outbox.append(&submission, &urls).unwrap();
        let load = outbox.load().unwrap();

        assert_eq!(append.pending, 1);
        assert_eq!(append.dropped, 1);
        assert_eq!(load.entries.len(), 1);
        assert_eq!(
            load.entries[0].submission.submission_id,
            submission.submission_id
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn submission_outbox_discards_submission_id_mismatches() {
        let dir = temp_test_dir("rsdb-submit-outbox-id-mismatch");
        let urls = test_submit_urls();
        let outbox = SubmissionOutbox::open(&SubmissionOutboxConfig {
            dir: dir.clone(),
            max_bytes: BYTES_PER_MEGABYTE,
        })
        .unwrap();
        let mut submission = signed_test_submission(1);
        submission.submission_id = "bad-submission-id".to_owned();
        let entry = PendingSubmission::new(submission, &urls);

        fs::write(outbox.path(), serde_json::to_vec(&entry).unwrap()).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(outbox.path())
            .unwrap()
            .write_all(b"\n")
            .unwrap();

        let load = outbox.load().unwrap();

        assert_eq!(load.entries.len(), 0);
        assert_eq!(load.discarded, 1);

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn submission_outbox_continues_healthy_targets_after_target_failure() {
        let dir = temp_test_dir("rsdb-submit-outbox-partial-target-failure");
        let healthy_url = "http://healthy.example.test/submit".to_owned();
        let failing_url = "https://failing.example.test/submit".to_owned();
        let urls = vec![healthy_url.clone(), failing_url.clone()];
        let outbox = SubmissionOutbox::open(&SubmissionOutboxConfig {
            dir: dir.clone(),
            max_bytes: BYTES_PER_MEGABYTE,
        })
        .unwrap();
        outbox.append(&signed_test_submission(1), &urls).unwrap();
        outbox.append(&signed_test_submission(2), &urls).unwrap();
        let mut stats = SubmissionStatus::enabled(urls.clone(), true);

        let error =
            flush_submission_outbox_with_submitter(&outbox, &urls, &mut stats, |url, _, stats| {
                if url == healthy_url {
                    stats.record_delivery(url, unix_time_ms());
                    Ok(())
                } else {
                    let error = format!("{url}: simulated failure");
                    stats.record_failure(url, error.clone());
                    Err(error)
                }
            })
            .unwrap_err();
        let load = outbox.load().unwrap();

        assert!(error.contains("simulated failure"));
        assert_eq!(stats.targets[0].url, healthy_url);
        assert_eq!(stats.targets[0].delivered, 2);
        assert_eq!(stats.targets[0].failed_attempts, 0);
        assert_eq!(stats.targets[1].url, failing_url);
        assert_eq!(stats.targets[1].delivered, 0);
        assert_eq!(stats.targets[1].failed_attempts, 1);
        assert_eq!(load.entries.len(), 2);
        assert!(
            load.entries
                .iter()
                .all(|entry| entry.pending_urls == [failing_url.clone()])
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn message_for_signing_attaches_configured_receiver() {
        let receiver = ReceiverIdentity::named("sf-rsdb-pi".to_owned(), "SF".to_owned());
        let message = FeedMessage::snapshot(42, Vec::new());

        let signed_message = message_for_signing(message, &receiver).unwrap();

        assert_eq!(signed_message.receiver(), Some(&receiver));
    }

    #[test]
    fn message_for_signing_rejects_receiver_mismatch() {
        let receiver = ReceiverIdentity::new("sf-rsdb-pi".to_owned());
        let message = FeedMessage::snapshot(42, Vec::new())
            .with_receiver(Some(ReceiverIdentity::new("other-rsdb-pi".to_owned())));

        let error = message_for_signing(message, &receiver).unwrap_err();

        assert!(error.contains("does not match configured receiver_id sf-rsdb-pi"));
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn frame_batch_for_signing_attaches_configured_receiver_to_records() {
        let receiver = ReceiverIdentity::new("sf-rsdb-pi".to_owned());
        let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
        let batch = FrameRecordBatch::new(
            Protocol::Adsb1090,
            receiver.clone(),
            vec![FrameRecord::new(42, 0, &frame)],
        );

        let signed_batch = frame_batch_for_signing(batch, &receiver).unwrap();

        assert_eq!(signed_batch.receiver, receiver);
        assert_eq!(signed_batch.records[0].receiver.as_ref(), Some(&receiver));
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn frame_batch_for_signing_rejects_receiver_mismatch() {
        let receiver = ReceiverIdentity::new("sf-rsdb-pi".to_owned());
        let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
        let batch = FrameRecordBatch::new(
            Protocol::Adsb1090,
            ReceiverIdentity::new("other-rsdb-pi".to_owned()),
            vec![FrameRecord::new(42, 0, &frame)],
        );

        let error = frame_batch_for_signing(batch, &receiver).unwrap_err();

        assert!(error.contains("does not match configured receiver_id sf-rsdb-pi"));
    }

    #[test]
    fn aggregate_submit_url_defaults_to_submit_endpoint() {
        assert_eq!(
            aggregate_submit_url("127.0.0.1:8090"),
            "http://127.0.0.1:8090/submit"
        );
        assert_eq!(
            aggregate_submit_url("http://127.0.0.1:8090/"),
            "http://127.0.0.1:8090/submit"
        );
        assert_eq!(
            aggregate_submit_url("http://127.0.0.1:8090/custom-submit"),
            "http://127.0.0.1:8090/custom-submit"
        );
        assert_eq!(
            aggregate_submit_url("https://agg.example.com"),
            "https://agg.example.com/submit"
        );
        assert_eq!(
            aggregate_submit_url("https://agg.example.com/custom-submit"),
            "https://agg.example.com/custom-submit"
        );
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn https_targets_default_to_tls_port() {
        let target = HttpTarget::parse_https("https://agg.example.com/submit").unwrap();

        assert_eq!(target.host, "agg.example.com");
        assert_eq!(target.port, 443);
        assert_eq!(target.path, "/submit");
        assert_eq!(target.host_header(), "agg.example.com");
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn https_targets_include_non_default_host_port() {
        let target = HttpTarget::parse_https("https://agg.example.com:8443/submit").unwrap();

        assert_eq!(target.port, 8443);
        assert_eq!(target.host_header(), "agg.example.com:8443");
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn http_response_reader_stops_after_content_length() {
        struct WouldBlockAfterResponse {
            response: &'static [u8],
            offset: usize,
        }

        impl Read for WouldBlockAfterResponse {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.offset >= self.response.len() {
                    return Err(std::io::Error::from(ErrorKind::WouldBlock));
                }
                let read = buffer.len().min(self.response.len() - self.offset);
                buffer[..read].copy_from_slice(&self.response[self.offset..self.offset + read]);
                self.offset += read;
                Ok(read)
            }
        }

        let response = b"HTTP/1.1 202 Accepted\r\nContent-Length: 12\r\n\r\n{\"ok\":true}\n";
        let mut stream = WouldBlockAfterResponse {
            response,
            offset: 0,
        };

        let body = read_http_response("https://aggregate.example.com/submit", &mut stream).unwrap();

        assert_eq!(body, "{\"ok\":true}\n");
    }

    #[test]
    fn verify_submission_file_accepts_allowlisted_signature() {
        let signing_key = SigningKey::from_bytes(&[8; 32]);
        let public_key = public_key_hex(&signing_key);
        let receiver_id = rsdb::receiver_id_from_ed25519_public_key_hex(&public_key).unwrap();
        let mut submission = SignedSubmission::new_ed25519(
            receiver_id.clone(),
            1_717_000_000_000,
            FeedMessage::snapshot(42, Vec::new())
                .with_receiver(Some(ReceiverIdentity::named(receiver_id, "SF".to_owned()))),
            String::new(),
        );
        submission.signature = signature_hex(&signing_key, &submission);
        let allowlist_text = format!("{public_key}\n");
        let dir = temp_test_dir("rsdb-verify-submission");
        let allowlist_path = dir.join("allowlist.txt");
        let submission_path = dir.join("submission.json");

        fs::write(&allowlist_path, allowlist_text).unwrap();
        fs::write(&submission_path, serde_json::to_vec(&submission).unwrap()).unwrap();

        assert_eq!(
            verify_submission_file(&allowlist_path, &submission_path),
            Ok(())
        );

        fs::remove_dir_all(dir).unwrap();
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn signature_hex(signing_key: &SigningKey, submission: &SignedSubmission) -> String {
        encode_hex(
            &signing_key
                .sign(&submission.signing_bytes().unwrap())
                .to_bytes(),
        )
    }

    fn public_key_hex(signing_key: &SigningKey) -> String {
        encode_hex(&signing_key.verifying_key().to_bytes())
    }

    fn encode_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(bytes.len() * 2);

        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }

        encoded
    }

    #[cfg(feature = "websocket")]
    fn signed_test_submission(sequence: u64) -> SignedSubmission {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let mut submission = SignedSubmission::new_ed25519(
            "sf-rsdb-pi".to_owned(),
            1_717_000_000_000 + sequence,
            FeedMessage::snapshot(sequence, Vec::new()).with_receiver(Some(
                ReceiverIdentity::named("sf-rsdb-pi".to_owned(), "SF".to_owned()),
            )),
            String::new(),
        );
        submission.signature = signature_hex(&signing_key, &submission);
        submission
    }

    #[cfg(feature = "websocket")]
    fn pending_submission_line_bytes(entry: &PendingSubmission) -> u64 {
        let bytes = serde_json::to_vec(entry).unwrap().len();
        u64::try_from(bytes).unwrap_or(u64::MAX).saturating_add(1)
    }

    #[cfg(feature = "websocket")]
    fn test_submit_urls() -> Vec<String> {
        vec![
            "http://127.0.0.1:8090/submit".to_owned(),
            "https://aggregate.example.com/submit".to_owned(),
        ]
    }
}

fn print_decoded_frame(sample_index: usize, frame: &Frame) {
    if let Some(squitter) = ExtendedSquitter::parse(frame) {
        match squitter.message {
            AdsbMessage::AircraftIdentification(identification) => {
                println!(
                    "{sample_index}: df={} icao={} type={} callsign={} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    identification.callsign,
                    frame.to_hex()
                );
            }
            AdsbMessage::TargetStateAndStatus(status) => {
                println!(
                    "{sample_index}: df={} icao={} type={} target_subtype={} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    status.subtype,
                    frame.to_hex()
                );
            }
            AdsbMessage::AircraftStatus(status) => {
                println!(
                    "{sample_index}: df={} icao={} type={} aircraft_status_subtype={} emergency={:?} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    status.subtype,
                    status.emergency_state,
                    frame.to_hex()
                );
            }
            AdsbMessage::AircraftOperationalStatus(status) => {
                println!(
                    "{sample_index}: df={} icao={} type={} adsb_version={} nac_p={} sil={} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    status.adsb_version,
                    status.nac_p,
                    status.source_integrity_level,
                    frame.to_hex()
                );
            }
            AdsbMessage::Unknown { .. } => {
                println!(
                    "{sample_index}: df={} icao={} type={} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    frame.to_hex()
                );
            }
            AdsbMessage::AirbornePosition(position) => {
                println!(
                    "{sample_index}: df={} icao={} type={} altitude={:?} cpr={:?} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    position.altitude_baro_ft,
                    position.cpr_format,
                    frame.to_hex()
                );
            }
            AdsbMessage::AirborneVelocity(velocity) => {
                println!(
                    "{sample_index}: df={} icao={} type={} ground_speed={:?} airspeed={:?} track={:?} heading={:?} vr={:?} raw={}",
                    frame.downlink_format().bits(),
                    squitter.icao,
                    squitter.type_code,
                    velocity.ground_speed_kt,
                    velocity.airspeed_kt,
                    velocity.track_deg,
                    velocity.heading_deg,
                    velocity.vertical_rate_fpm,
                    frame.to_hex()
                );
            }
        }
    } else {
        println!(
            "{sample_index}: df={} raw={}",
            frame.downlink_format().bits(),
            frame.to_hex()
        );
    }
}

fn unix_time_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(feature = "websocket")]
mod websocket {
    use std::fs;
    use std::io::{BufWriter, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;

    use crate::config::{FeedRuntimeConfig, PersistenceRuntimeConfig, SubmissionConfig};
    use crate::decoder::{
        FeedCounters, bytes_per_second_for_millis, dropped_usb_chunk_ratio, ensure_feed_decoder,
        frames_per_megabyte, rate_per_second, ratio_per_frame, run_feed,
    };
    use crate::usb::RtlSdrConfig;
    use crate::{
        start_submission_worker, unix_time_ms, websocket_accept_key, write_websocket_text_frame,
    };
    use rsdb::{
        AircraftSnapshot, FeedBootstrap, FeedMessage, FeedStats, PersistenceStatus, Protocol,
        RadioConfig, ReceiverIdentity, ReceiverSite, ServiceStatus, SubmissionPayload,
        SubmissionStatus, enrich_aircraft_snapshots,
    };

    pub fn serve(
        config: RtlSdrConfig,
        feed_config: &FeedRuntimeConfig,
        bind: &str,
        submission_config: Option<SubmissionConfig>,
    ) -> Result<(), String> {
        ensure_feed_decoder(config.protocol, "serve")?;

        let listener = TcpListener::bind(bind).map_err(|error| error.to_string())?;
        let submission_worker = submission_config.map(start_submission_worker).transpose()?;
        let submission_sender = submission_worker
            .as_ref()
            .map(|worker| worker.sender.clone());
        let submission_status = submission_worker
            .as_ref()
            .map(|worker| Arc::clone(&worker.status));
        let hub = Arc::new(Hub::new(
            feed_config.radio,
            feed_config.receiver_identity.clone(),
            feed_config.receiver_site.clone(),
            feed_config.persistence.clone(),
            submission_status,
        ));
        let accept_hub = Arc::clone(&hub);

        thread::Builder::new()
            .name("rsdb-ws-accept".to_owned())
            .spawn(move || accept_connections(&listener, &accept_hub))
            .map_err(|error| error.to_string())?;

        eprintln!(
            "Serving {} collector diagnostics on http://{bind}",
            feed_config.radio.protocol
        );
        hub.publish(&FeedMessage::snapshot_for_protocol(
            feed_config.radio.protocol,
            unix_time_ms(),
            Vec::new(),
        ))?;

        loop {
            hub.set_receiver_connected(true, None);
            match run_feed(
                config,
                feed_config,
                None,
                |message| {
                    let message = hub.enrich_heartbeat(message);
                    hub.publish(&message)?;
                    if let Some(sender) = &submission_sender
                        && matches!(message, FeedMessage::Heartbeat { .. })
                    {
                        let _ = sender.send(SubmissionPayload::FeedMessage(message));
                    }
                    Ok(())
                },
                |batch| {
                    if let Some(sender) = &submission_sender {
                        let _ = sender.send(SubmissionPayload::FrameRecords(batch));
                    }
                    Ok(())
                },
                |stream, counters| {
                    hub.set_dropped_usb_chunks(stream.dropped_chunks());
                    hub.set_feed_counters(counters);
                    Ok(())
                },
            ) {
                Ok(()) => {
                    eprintln!("{} USB sample stream ended", feed_config.radio.protocol);
                    hub.set_receiver_connected(false, Some("USB sample stream ended".to_owned()));
                }
                Err(error) => {
                    eprintln!("{} feed unavailable: {error}", feed_config.radio.protocol);
                    hub.set_receiver_connected(false, Some(error));
                }
            }

            let now_ms = unix_time_ms();
            hub.publish(&FeedMessage::heartbeat_for_protocol(
                feed_config.radio.protocol,
                now_ms,
                hub.aircraft_count(),
                hub.feed_stats(now_ms),
            ))?;
            thread::sleep(Duration::from_millis(feed_config.retry_after.max(1_000)));
        }
    }

    fn accept_connections(listener: &TcpListener, hub: &Arc<Hub>) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let client_hub = Arc::clone(hub);
            let _ = thread::Builder::new()
                .name("rsdb-http-client".to_owned())
                .spawn(move || serve_connection(stream, &client_hub));
        }
    }

    fn serve_connection(mut stream: TcpStream, hub: &Hub) {
        let Ok(request) = HttpRequest::read_from(&mut stream) else {
            return;
        };

        if request.is_websocket_upgrade() {
            serve_websocket_client(stream, hub, &request);
            return;
        }

        serve_http(stream, hub, &request);
    }

    fn serve_websocket_client(mut stream: TcpStream, hub: &Hub, request: &HttpRequest) {
        let Some(key) = request.header("sec-websocket-key") else {
            let _ = write_response(
                &mut stream,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                b"missing Sec-WebSocket-Key\n",
            );
            return;
        };

        let accept_key = websocket_accept_key(key);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             \r\n"
        );

        if stream.write_all(response.as_bytes()).is_err() {
            return;
        }

        let receiver = hub.subscribe();

        for message in receiver {
            if write_websocket_text_frame(&mut stream, &message).is_err() {
                break;
            }
        }
    }

    fn serve_http(mut stream: TcpStream, hub: &Hub, request: &HttpRequest) {
        if request.method != "GET" && request.method != "HEAD" {
            let _ = write_response(
                &mut stream,
                "405 Method Not Allowed",
                "text/plain; charset=utf-8",
                b"method not allowed\n",
            );
            return;
        }

        let path = request.path_without_query();
        match path {
            "/status.json" => write_json_result(&mut stream, hub.status_json()),
            "/aircraft.json" => write_json_result(&mut stream, hub.aircraft_json()),
            "/bootstrap.json" => write_json_result(&mut stream, hub.bootstrap_json()),
            "/schema.json" => write_json_result(
                &mut stream,
                serde_json::to_string(&rsdb::api_schema()).map_err(|error| error.to_string()),
            ),
            "/history.ndjson" => write_history_result(&mut stream, hub.history_ndjson()),
            _ => {
                let _ = write_response(
                    &mut stream,
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    b"not found\n",
                );
            }
        }
    }

    fn write_json_result(stream: &mut TcpStream, result: Result<String, String>) {
        match result {
            Ok(body) => {
                let _ = write_response(
                    stream,
                    "200 OK",
                    "application/json; charset=utf-8",
                    body.as_bytes(),
                );
            }
            Err(error) => {
                let _ = write_response(
                    stream,
                    "500 Internal Server Error",
                    "text/plain; charset=utf-8",
                    error.as_bytes(),
                );
            }
        }
    }

    fn write_history_result(stream: &mut TcpStream, result: Result<Option<Vec<u8>>, String>) {
        match result {
            Ok(Some(body)) => {
                let _ = write_response(
                    stream,
                    "200 OK",
                    "application/x-ndjson; charset=utf-8",
                    &body,
                );
            }
            Ok(None) => {
                let _ = write_response(
                    stream,
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    b"persistence is not enabled\n",
                );
            }
            Err(error) => {
                let _ = write_response(
                    stream,
                    "500 Internal Server Error",
                    "text/plain; charset=utf-8",
                    error.as_bytes(),
                );
            }
        }
    }

    fn write_response(
        stream: &mut TcpStream,
        status: &str,
        content_type: &str,
        body: &[u8],
    ) -> std::io::Result<()> {
        let header = format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             Cache-Control: no-store\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        );

        stream.write_all(header.as_bytes())?;
        stream.write_all(body)
    }

    #[derive(Debug)]
    struct HttpRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
    }

    impl HttpRequest {
        fn read_from(stream: &mut TcpStream) -> Result<Self, String> {
            let mut buffer = Vec::with_capacity(1024);
            let mut byte = [0_u8; 1];

            while !buffer.ends_with(b"\r\n\r\n") {
                if buffer.len() >= 16_384 {
                    return Err("HTTP request too large".to_owned());
                }

                let read = stream.read(&mut byte).map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("connection closed before HTTP request".to_owned());
                }
                buffer.push(byte[0]);
            }

            let request = std::str::from_utf8(&buffer).map_err(|error| error.to_string())?;
            let mut lines = request.split("\r\n");
            let request_line = lines
                .next()
                .ok_or_else(|| "missing request line".to_owned())?;
            let mut request_parts = request_line.split_whitespace();
            let method = request_parts
                .next()
                .ok_or_else(|| "missing HTTP method".to_owned())?
                .to_owned();
            let path = request_parts
                .next()
                .ok_or_else(|| "missing HTTP path".to_owned())?
                .to_owned();
            let headers = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
                .collect();

            Ok(Self {
                method,
                path,
                headers,
            })
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(header_name, _)| header_name == name)
                .map(|(_, value)| value.as_str())
        }

        fn is_websocket_upgrade(&self) -> bool {
            self.header("upgrade")
                .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        }

        fn path_without_query(&self) -> &str {
            self.path
                .split_once('?')
                .map_or(self.path.as_str(), |(path, _)| path)
        }
    }

    #[derive(Debug)]
    struct Hub {
        started_ms: u64,
        radio: RadioConfig,
        receiver_identity: Option<ReceiverIdentity>,
        receiver_site: Option<ReceiverSite>,
        clients: Mutex<Vec<mpsc::Sender<String>>>,
        latest: Mutex<Vec<AircraftSnapshot>>,
        recent_messages: Mutex<Vec<FeedMessage>>,
        status: Mutex<ReceiverStatus>,
        persistence: Mutex<PersistenceState>,
        submission_status: Option<Arc<Mutex<SubmissionStatus>>>,
    }

    impl Hub {
        fn new(
            radio: RadioConfig,
            receiver_identity: Option<ReceiverIdentity>,
            receiver_site: Option<ReceiverSite>,
            persistence_config: PersistenceRuntimeConfig,
            submission_status: Option<Arc<Mutex<SubmissionStatus>>>,
        ) -> Self {
            let now_ms = unix_time_ms();

            Self {
                started_ms: now_ms,
                radio,
                receiver_identity,
                receiver_site,
                clients: Mutex::new(Vec::new()),
                latest: Mutex::new(Vec::new()),
                recent_messages: Mutex::new(Vec::new()),
                status: Mutex::new(ReceiverStatus {
                    receiver_connected: false,
                    last_frame_ms: None,
                    last_usb_chunk_ms: None,
                    usb_chunks: 0,
                    usb_bytes: 0,
                    dropped_usb_chunks: 0,
                    decoded_frames: 0,
                    aircraft_updates: 0,
                    stale_aircraft_removed: 0,
                    last_error: None,
                }),
                persistence: Mutex::new(PersistenceState::open(persistence_config)),
                submission_status,
            }
        }
    }

    #[derive(Debug, Clone)]
    struct ReceiverStatus {
        receiver_connected: bool,
        last_frame_ms: Option<u64>,
        last_usb_chunk_ms: Option<u64>,
        usb_chunks: u64,
        usb_bytes: u64,
        dropped_usb_chunks: u64,
        decoded_frames: u64,
        aircraft_updates: u64,
        stale_aircraft_removed: u64,
        last_error: Option<String>,
    }

    #[derive(Debug)]
    struct PersistenceState {
        writer: Option<PersistentFeed>,
        status: PersistenceStatus,
    }

    impl PersistenceState {
        fn open(config: PersistenceRuntimeConfig) -> Self {
            let Some(dir) = config.dir else {
                return Self {
                    writer: None,
                    status: PersistenceStatus::default(),
                };
            };

            match PersistentFeed::open(&dir, config.feed_max_bytes) {
                Ok(writer) => {
                    let status = PersistenceStatus {
                        enabled: true,
                        current_feed_bytes: writer.current_feed_bytes,
                        feed_max_bytes: config.feed_max_bytes,
                        ..PersistenceStatus::default()
                    };
                    Self {
                        writer: Some(writer),
                        status,
                    }
                }
                Err(error) => Self {
                    writer: None,
                    status: PersistenceStatus {
                        enabled: true,
                        feed_max_bytes: config.feed_max_bytes,
                        last_error: Some(error),
                        ..PersistenceStatus::default()
                    },
                },
            }
        }

        fn persist(
            &mut self,
            protocol: Protocol,
            message: &FeedMessage,
            latest: &[AircraftSnapshot],
            now_ms: u64,
        ) {
            if !self.status.enabled {
                return;
            }

            let Some(writer) = self.writer.as_mut() else {
                return;
            };

            match writer.persist(protocol, message, latest, now_ms) {
                Ok(bytes_written) => {
                    self.status.messages_written = self.status.messages_written.saturating_add(1);
                    self.status.bytes_written =
                        self.status.bytes_written.saturating_add(bytes_written);
                    self.status.current_feed_bytes = writer.current_feed_bytes;
                    self.status.rotations = writer.rotations;
                    self.status.last_write_ms = Some(now_ms);
                    self.status.last_error = None;
                }
                Err(error) => {
                    self.status.last_error = Some(error);
                }
            }
        }

        fn status(&self) -> PersistenceStatus {
            self.status.clone()
        }

        fn read_current_feed(&mut self) -> Result<Option<Vec<u8>>, String> {
            if !self.status.enabled {
                return Ok(None);
            }

            let Some(writer) = self.writer.as_mut() else {
                return Err(self.status.last_error.clone().unwrap_or_else(|| {
                    "persistence is enabled but the feed writer is unavailable".to_owned()
                }));
            };

            writer.flush()?;
            fs::read(&writer.feed_path)
                .map(Some)
                .map_err(|error| format!("{}: read failed: {error}", writer.feed_path.display()))
        }
    }

    #[derive(Debug)]
    struct PersistentFeed {
        feed_path: PathBuf,
        previous_feed_path: PathBuf,
        latest_path: PathBuf,
        feed: BufWriter<fs::File>,
        current_feed_bytes: u64,
        max_feed_bytes: u64,
        rotations: u64,
    }

    impl PersistentFeed {
        fn open(dir: &Path, max_feed_bytes: u64) -> Result<Self, String> {
            fs::create_dir_all(dir)
                .map_err(|error| format!("{}: create failed: {error}", dir.display()))?;

            let feed_path = dir.join("feed.ndjson");
            let current_feed_bytes = fs::metadata(&feed_path).map_or(0, |metadata| metadata.len());
            let feed = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&feed_path)
                .map_err(|error| format!("{}: open failed: {error}", feed_path.display()))?;

            Ok(Self {
                previous_feed_path: dir.join("feed.previous.ndjson"),
                latest_path: dir.join("latest-aircraft.json"),
                feed_path,
                feed: BufWriter::new(feed),
                current_feed_bytes,
                max_feed_bytes,
                rotations: 0,
            })
        }

        fn persist(
            &mut self,
            protocol: Protocol,
            message: &FeedMessage,
            latest: &[AircraftSnapshot],
            now_ms: u64,
        ) -> Result<u64, String> {
            let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
            line.push(b'\n');
            let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);

            self.rotate_if_needed(line_len)?;
            self.feed
                .write_all(&line)
                .map_err(|error| format!("{}: write failed: {error}", self.feed_path.display()))?;
            self.feed
                .flush()
                .map_err(|error| format!("{}: flush failed: {error}", self.feed_path.display()))?;
            self.current_feed_bytes = self.current_feed_bytes.saturating_add(line_len);
            self.write_latest_snapshot(protocol, latest, now_ms, message.receiver().cloned())?;

            Ok(line_len)
        }

        fn rotate_if_needed(&mut self, next_write_bytes: u64) -> Result<(), String> {
            if self.max_feed_bytes == 0
                || self.current_feed_bytes == 0
                || self.current_feed_bytes.saturating_add(next_write_bytes) <= self.max_feed_bytes
            {
                return Ok(());
            }

            self.feed
                .flush()
                .map_err(|error| format!("{}: flush failed: {error}", self.feed_path.display()))?;
            if self.previous_feed_path.exists() {
                fs::remove_file(&self.previous_feed_path).map_err(|error| {
                    format!(
                        "{}: remove failed: {error}",
                        self.previous_feed_path.display()
                    )
                })?;
            }
            fs::rename(&self.feed_path, &self.previous_feed_path).map_err(|error| {
                format!(
                    "{}: rotate to {} failed: {error}",
                    self.feed_path.display(),
                    self.previous_feed_path.display()
                )
            })?;
            let feed = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.feed_path)
                .map_err(|error| format!("{}: open failed: {error}", self.feed_path.display()))?;

            self.feed = BufWriter::new(feed);
            self.current_feed_bytes = 0;
            self.rotations = self.rotations.saturating_add(1);
            Ok(())
        }

        fn write_latest_snapshot(
            &self,
            protocol: Protocol,
            latest: &[AircraftSnapshot],
            now_ms: u64,
            receiver: Option<ReceiverIdentity>,
        ) -> Result<(), String> {
            let message = FeedMessage::snapshot_for_protocol(protocol, now_ms, latest.to_vec())
                .with_receiver(receiver);
            let bytes = serde_json::to_vec(&message).map_err(|error| error.to_string())?;
            let tmp_path = self.latest_path.with_extension("json.tmp");

            fs::write(&tmp_path, bytes)
                .map_err(|error| format!("{}: write failed: {error}", tmp_path.display()))?;
            fs::rename(&tmp_path, &self.latest_path).map_err(|error| {
                format!(
                    "{}: rename to {} failed: {error}",
                    tmp_path.display(),
                    self.latest_path.display()
                )
            })
        }

        fn flush(&mut self) -> Result<(), String> {
            self.feed
                .flush()
                .map_err(|error| format!("{}: flush failed: {error}", self.feed_path.display()))
        }
    }

    impl Hub {
        fn subscribe(&self) -> mpsc::Receiver<String> {
            let (sender, receiver) = mpsc::channel();
            let now_ms = unix_time_ms();
            let latest = enrich_aircraft_snapshots(
                self.latest
                    .lock()
                    .expect("latest aircraft mutex not poisoned")
                    .clone(),
                self.receiver_site.as_ref(),
                now_ms,
            );
            let snapshot_message = serde_json::to_string(
                &FeedMessage::snapshot_for_protocol(self.radio.protocol, now_ms, latest)
                    .with_receiver(self.receiver_identity.clone()),
            )
            .expect("snapshot message serializes");

            let _ = sender.send(snapshot_message);
            self.clients
                .lock()
                .expect("client list mutex not poisoned")
                .push(sender);

            receiver
        }

        fn publish(&self, message: &FeedMessage) -> Result<(), String> {
            let message = self.message_for_receiver(message.clone());

            self.update_latest(&message);
            self.update_status_from_feed(&message);
            self.persist_message(&message);
            self.record_recent_message(&message);
            let message = serde_json::to_string(&message).map_err(|error| error.to_string())?;
            let mut clients = self.clients.lock().expect("client list mutex not poisoned");

            clients.retain(|client| client.send(message.clone()).is_ok());
            Ok(())
        }

        fn message_for_receiver(&self, message: FeedMessage) -> FeedMessage {
            if message.receiver().is_some() {
                message
            } else {
                message.with_receiver(self.receiver_identity.clone())
            }
        }

        fn update_latest(&self, message: &FeedMessage) {
            let mut latest = self
                .latest
                .lock()
                .expect("latest aircraft mutex not poisoned");

            match message {
                FeedMessage::Snapshot { aircraft, .. } => {
                    latest.clone_from(aircraft);
                }
                FeedMessage::Aircraft { aircraft, .. } => {
                    if let Some(existing) = latest
                        .iter_mut()
                        .find(|existing| existing.icao == aircraft.icao)
                    {
                        *existing = aircraft.clone();
                    } else {
                        latest.push(aircraft.clone());
                        latest.sort_by(|left, right| left.icao.cmp(&right.icao));
                    }
                }
                FeedMessage::StaleAircraft { icao, .. } => {
                    latest.retain(|aircraft| aircraft.icao != *icao);
                }
                FeedMessage::Heartbeat { .. } => {}
            }
        }

        fn enrich_heartbeat(&self, message: FeedMessage) -> FeedMessage {
            match message {
                FeedMessage::Heartbeat {
                    protocol,
                    now_ms,
                    receiver,
                    aircraft_count,
                    mut stats,
                    ..
                } => {
                    stats.receiver_site.clone_from(&self.receiver_site);
                    stats.websocket_clients = self.websocket_clients();
                    stats.submission = self.submission_status().health();
                    FeedMessage::heartbeat_for_protocol(protocol, now_ms, aircraft_count, stats)
                        .with_receiver(receiver)
                }
                other => other,
            }
        }

        fn aircraft_count(&self) -> usize {
            self.latest
                .lock()
                .expect("latest aircraft mutex not poisoned")
                .len()
        }

        fn websocket_clients(&self) -> usize {
            self.clients
                .lock()
                .expect("client list mutex not poisoned")
                .len()
        }

        fn set_receiver_connected(&self, receiver_connected: bool, last_error: Option<String>) {
            let mut status = self
                .status
                .lock()
                .expect("receiver status mutex not poisoned");
            status.receiver_connected = receiver_connected;
            status.last_error = last_error;
        }

        fn set_dropped_usb_chunks(&self, dropped_usb_chunks: u64) {
            self.status
                .lock()
                .expect("receiver status mutex not poisoned")
                .dropped_usb_chunks = dropped_usb_chunks;
        }

        fn set_feed_counters(&self, counters: FeedCounters) {
            let mut status = self
                .status
                .lock()
                .expect("receiver status mutex not poisoned");
            status.last_frame_ms = counters.last_frame_ms;
            status.last_usb_chunk_ms = counters.last_usb_chunk_ms;
            status.usb_chunks = counters.usb_chunks;
            status.usb_bytes = counters.usb_bytes;
            status.dropped_usb_chunks = counters.dropped_usb_chunks;
            status.decoded_frames = counters.decoded_frames;
            status.aircraft_updates = counters.aircraft_updates;
            status.stale_aircraft_removed = counters.stale_aircraft_removed;
        }

        fn update_status_from_feed(&self, message: &FeedMessage) {
            match message {
                FeedMessage::Aircraft { now_ms, .. } => {
                    let mut status = self
                        .status
                        .lock()
                        .expect("receiver status mutex not poisoned");
                    status.receiver_connected = true;
                    status.last_frame_ms = Some(*now_ms);
                    status.last_error = None;
                }
                FeedMessage::StaleAircraft { .. } => {
                    self.status
                        .lock()
                        .expect("receiver status mutex not poisoned")
                        .stale_aircraft_removed += 1;
                }
                FeedMessage::Heartbeat { stats, .. } => {
                    let mut status = self
                        .status
                        .lock()
                        .expect("receiver status mutex not poisoned");
                    status.receiver_connected = stats.receiver_connected;
                    status.last_frame_ms = stats.last_frame_ms;
                    status.last_usb_chunk_ms = stats.last_usb_chunk_ms;
                    status.usb_chunks = stats.usb_chunks;
                    status.usb_bytes = stats.usb_bytes;
                    status.dropped_usb_chunks = stats.dropped_usb_chunks;
                    status.decoded_frames = stats.decoded_frames;
                    status.aircraft_updates = stats.aircraft_updates;
                    status.stale_aircraft_removed = stats.stale_aircraft_removed;
                    status.last_error.clone_from(&stats.last_error);
                }
                FeedMessage::Snapshot { .. } => {}
            }
        }

        fn status_json(&self) -> Result<String, String> {
            let now_ms = unix_time_ms();
            let receiver_status = self
                .status
                .lock()
                .expect("receiver status mutex not poisoned")
                .clone();
            let stats = self.feed_stats_from_status(now_ms, receiver_status);
            let response = ServiceStatus {
                schema_version: rsdb::FEED_SCHEMA_VERSION,
                now_ms,
                radio: self.radio,
                uptime_ms: stats.uptime_ms,
                receiver_connected: stats.receiver_connected,
                receiver: self.receiver_identity.clone(),
                receiver_site: self.receiver_site.clone(),
                aircraft_count: self.aircraft_count(),
                last_frame_ms: stats.last_frame_ms,
                last_usb_chunk_ms: stats.last_usb_chunk_ms,
                usb_chunks: stats.usb_chunks,
                usb_chunks_per_second: stats.usb_chunks_per_second,
                usb_bytes: stats.usb_bytes,
                usb_bytes_per_second: stats.usb_bytes_per_second,
                dropped_usb_chunks: stats.dropped_usb_chunks,
                dropped_usb_chunk_ratio: stats.dropped_usb_chunk_ratio,
                decoded_frames: stats.decoded_frames,
                decoded_frames_per_second: stats.decoded_frames_per_second,
                decoded_frames_per_megabyte: stats.decoded_frames_per_megabyte,
                aircraft_updates: stats.aircraft_updates,
                aircraft_updates_per_second: stats.aircraft_updates_per_second,
                aircraft_updates_per_frame: stats.aircraft_updates_per_frame,
                stale_aircraft_removed: stats.stale_aircraft_removed,
                last_error: stats.last_error,
                websocket_clients: stats.websocket_clients,
                persistence: self.persistence_status(),
                submission: self.submission_status(),
            };

            serde_json::to_string(&response).map_err(|error| error.to_string())
        }

        fn feed_stats(&self, now_ms: u64) -> FeedStats {
            let status = self
                .status
                .lock()
                .expect("receiver status mutex not poisoned")
                .clone();
            self.feed_stats_from_status(now_ms, status)
        }

        fn feed_stats_from_status(&self, now_ms: u64, status: ReceiverStatus) -> FeedStats {
            let uptime_ms = now_ms.saturating_sub(self.started_ms);

            FeedStats {
                uptime_ms,
                receiver_site: self.receiver_site.clone(),
                receiver_connected: status.receiver_connected,
                last_frame_ms: status.last_frame_ms,
                last_usb_chunk_ms: status.last_usb_chunk_ms,
                usb_chunks: status.usb_chunks,
                usb_chunks_per_second: rate_per_second(status.usb_chunks, uptime_ms),
                usb_bytes: status.usb_bytes,
                usb_bytes_per_second: bytes_per_second_for_millis(status.usb_bytes, uptime_ms),
                dropped_usb_chunks: status.dropped_usb_chunks,
                dropped_usb_chunk_ratio: dropped_usb_chunk_ratio(
                    status.dropped_usb_chunks,
                    status.usb_chunks,
                ),
                decoded_frames: status.decoded_frames,
                decoded_frames_per_second: rate_per_second(status.decoded_frames, uptime_ms),
                decoded_frames_per_megabyte: frames_per_megabyte(
                    status.decoded_frames,
                    status.usb_bytes,
                ),
                aircraft_updates: status.aircraft_updates,
                aircraft_updates_per_second: rate_per_second(status.aircraft_updates, uptime_ms),
                aircraft_updates_per_frame: ratio_per_frame(
                    status.aircraft_updates,
                    status.decoded_frames,
                ),
                stale_aircraft_removed: status.stale_aircraft_removed,
                last_error: status.last_error,
                websocket_clients: self.websocket_clients(),
                submission: self.submission_status().health(),
            }
        }

        fn aircraft_json(&self) -> Result<String, String> {
            let aircraft = self
                .latest
                .lock()
                .expect("latest aircraft mutex not poisoned")
                .clone();
            let now_ms = unix_time_ms();
            let aircraft = enrich_aircraft_snapshots(aircraft, self.receiver_site.as_ref(), now_ms);

            serde_json::to_string(
                &FeedMessage::snapshot_for_protocol(self.radio.protocol, now_ms, aircraft)
                    .with_receiver(self.receiver_identity.clone()),
            )
            .map_err(|error| error.to_string())
        }

        fn bootstrap_json(&self) -> Result<String, String> {
            let now_ms = unix_time_ms();
            let aircraft = self
                .latest
                .lock()
                .expect("latest aircraft mutex not poisoned")
                .clone();
            let aircraft = enrich_aircraft_snapshots(aircraft, self.receiver_site.as_ref(), now_ms);
            let snapshot =
                FeedMessage::snapshot_for_protocol(self.radio.protocol, now_ms, aircraft)
                    .with_receiver(self.receiver_identity.clone());
            let bootstrap = FeedBootstrap {
                schema_version: rsdb::FEED_SCHEMA_VERSION,
                now_ms,
                recent_message_window_ms: rsdb::FEED_RECENT_MESSAGE_WINDOW_MS,
                snapshot,
                recent_messages: self.recent_feed_messages(now_ms),
            };

            serde_json::to_string(&bootstrap).map_err(|error| error.to_string())
        }

        fn record_recent_message(&self, message: &FeedMessage) {
            if !matches!(
                message,
                FeedMessage::Aircraft { .. } | FeedMessage::StaleAircraft { .. }
            ) {
                return;
            }

            let now_ms = message.now_ms();
            let mut messages = self
                .recent_messages
                .lock()
                .expect("recent message mutex not poisoned");
            messages.push(message.clone());
            prune_recent_feed_messages(&mut messages, now_ms);
        }

        fn recent_feed_messages(&self, now_ms: u64) -> Vec<FeedMessage> {
            let mut messages = self
                .recent_messages
                .lock()
                .expect("recent message mutex not poisoned");
            prune_recent_feed_messages(&mut messages, now_ms);
            messages.clone()
        }

        fn persist_message(&self, message: &FeedMessage) {
            let now_ms = unix_time_ms();
            let latest = self
                .latest
                .lock()
                .expect("latest aircraft mutex not poisoned")
                .clone();
            self.persistence
                .lock()
                .expect("persistence mutex not poisoned")
                .persist(self.radio.protocol, message, &latest, now_ms);
        }

        fn persistence_status(&self) -> PersistenceStatus {
            self.persistence
                .lock()
                .expect("persistence mutex not poisoned")
                .status()
        }

        fn submission_status(&self) -> SubmissionStatus {
            self.submission_status
                .as_ref()
                .map(|status| {
                    status
                        .lock()
                        .expect("submission status mutex not poisoned")
                        .clone()
                })
                .unwrap_or_default()
        }

        fn history_ndjson(&self) -> Result<Option<Vec<u8>>, String> {
            self.persistence
                .lock()
                .expect("persistence mutex not poisoned")
                .read_current_feed()
        }
    }

    fn prune_recent_feed_messages(messages: &mut Vec<FeedMessage>, now_ms: u64) {
        let min_ms = now_ms.saturating_sub(rsdb::FEED_RECENT_MESSAGE_WINDOW_MS);
        messages.retain(|message| message.now_ms() >= min_ms);
    }

    #[cfg(test)]
    mod tests {
        use std::env;
        use std::process;

        use super::*;

        #[test]
        fn persistence_writes_feed_history_and_latest_snapshot() {
            let dir = test_dir("writes");
            let mut state = PersistenceState::open(PersistenceRuntimeConfig {
                dir: Some(dir.clone()),
                feed_max_bytes: 10_000,
            });
            let message = FeedMessage::snapshot(42, Vec::new());

            state.persist(Protocol::Adsb1090, &message, &[], 42);

            let status = state.status();
            assert!(status.enabled);
            assert_eq!(status.messages_written, 1);
            assert_eq!(status.last_write_ms, Some(42));
            assert_eq!(status.last_error, None);

            let history = String::from_utf8(state.read_current_feed().unwrap().unwrap()).unwrap();
            assert!(history.contains("\"type\":\"snapshot\""));

            let latest = fs::read_to_string(dir.join("latest-aircraft.json")).unwrap();
            assert!(matches!(
                serde_json::from_str::<FeedMessage>(&latest).unwrap(),
                FeedMessage::Snapshot { now_ms: 42, .. }
            ));

            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn persistence_rotates_current_feed() {
            let dir = test_dir("rotates");
            let mut state = PersistenceState::open(PersistenceRuntimeConfig {
                dir: Some(dir.clone()),
                feed_max_bytes: 32,
            });
            let message = FeedMessage::snapshot(42, Vec::new());

            state.persist(Protocol::Adsb1090, &message, &[], 42);
            state.persist(Protocol::Adsb1090, &message, &[], 43);

            let status = state.status();
            assert_eq!(status.rotations, 1);
            assert!(dir.join("feed.previous.ndjson").exists());
            assert!(dir.join("feed.ndjson").exists());

            fs::remove_dir_all(dir).unwrap();
        }

        fn test_dir(label: &str) -> PathBuf {
            let mut dir = env::temp_dir();
            dir.push(format!("rsdb-{label}-{}-{}", process::id(), unix_time_ms()));
            dir
        }
    }
}
