#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use rsdb::{
    AggregateIngestResult, AggregatePersistenceStatus, AggregateStore, AggregateStorePersistence,
    FeedMessage, ReceiverAllowlist, ReceiverIdentity, SignedSubmission,
};
use serde::Serialize;

const INDEX_HTML: &str = include_str!("../../../web/static/index.html");
const APP_CSS: &str = include_str!("../../../web/static/app.css");
const APP_JS: &str = include_str!("../../../web/dist/app.js");
const MAX_REQUEST_BODY_BYTES: usize = 1_048_576;
const AGGREGATE_CHECKPOINT_RECORDS: u64 = 250;
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, Clone, Copy)]
pub struct ServeConfig<'a> {
    pub allowlist: &'a str,
    pub bind: &'a str,
    pub data_dir: Option<&'a Path>,
    pub retention_ms: u64,
    pub max_bytes: u64,
}

/// Runs the aggregate ingest HTTP/WebSocket server.
///
/// # Errors
///
/// Returns an error when the allowlist or persisted store cannot be loaded, the
/// listener cannot bind, or persistence initialization fails.
pub fn serve(config: ServeConfig<'_>) -> Result<(), String> {
    let allowlist = read_allowlist(config.allowlist)?;
    let persistence = config
        .data_dir
        .map(|dir| AggregatePersistence::open(dir, config.retention_ms, config.max_bytes))
        .transpose()?;
    let store = persistence.as_ref().map_or_else(
        || Ok(AggregateStore::new()),
        |persistence| persistence.load_store(&allowlist),
    )?;
    let listener = TcpListener::bind(config.bind).map_err(|error| error.to_string())?;
    let hub = Arc::new(Hub::with_store(allowlist, store, persistence));

    eprintln!(
        "Serving aggregate ingest and feed on http://{}",
        config.bind
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let client_hub = Arc::clone(&hub);
        let _ = thread::Builder::new()
            .name("rsdb-aggregate-client".to_owned())
            .spawn(move || serve_connection(stream, &client_hub));
    }

    Ok(())
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
    let path = request.path_without_query();
    match (request.method.as_str(), path) {
        ("GET" | "HEAD", "/" | "/index.html") => {
            let _ = write_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
            );
        }
        ("GET" | "HEAD", "/app.css") => {
            let _ = write_response(
                &mut stream,
                "200 OK",
                "text/css; charset=utf-8",
                APP_CSS.as_bytes(),
            );
        }
        ("GET" | "HEAD", "/app.js") => {
            let _ = write_response(
                &mut stream,
                "200 OK",
                "application/javascript; charset=utf-8",
                APP_JS.as_bytes(),
            );
        }
        ("GET" | "HEAD", "/status.json") => {
            write_json_response(&mut stream, "200 OK", &hub.status_json());
        }
        ("GET" | "HEAD", "/aircraft.json") => {
            write_json_response(&mut stream, "200 OK", &hub.aircraft_json());
        }
        ("GET" | "HEAD", "/bootstrap.json") => {
            write_json_response(&mut stream, "200 OK", &hub.bootstrap_json());
        }
        ("GET" | "HEAD", "/receivers.json") => {
            write_json_response(&mut stream, "200 OK", &hub.receivers_json());
        }
        ("GET" | "HEAD", "/schema.json") => {
            write_json_response(&mut stream, "200 OK", &rsdb::aggregate_api_schema());
        }
        ("POST", "/submit") => match hub.submit(&request.body) {
            Ok(response) => write_json_response(&mut stream, "202 Accepted", &response),
            Err(error) => write_submit_error(&mut stream, &error),
        },
        (_, "/submit") => {
            let _ = write_response(
                &mut stream,
                "405 Method Not Allowed",
                "text/plain; charset=utf-8",
                b"method not allowed\n",
            );
        }
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

fn write_json_response<T: Serialize>(stream: &mut TcpStream, status: &str, value: &T) {
    match serde_json::to_vec(value) {
        Ok(body) => {
            let _ = write_response(stream, status, "application/json; charset=utf-8", &body);
        }
        Err(error) => {
            let _ = write_response(
                stream,
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                error.to_string().as_bytes(),
            );
        }
    }
}

fn write_submit_error(stream: &mut TcpStream, error: &SubmitError) {
    let status = match error {
        SubmitError::InvalidJson(_) | SubmitError::InvalidPayload(_) => "400 Bad Request",
        SubmitError::Rejected(_) => "403 Forbidden",
        SubmitError::Persistence(_) => "500 Internal Server Error",
        SubmitError::Writer(_) => "503 Service Unavailable",
    };
    write_json_response(
        stream,
        status,
        &ErrorResponse {
            error: error.to_string(),
        },
    );
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

fn read_allowlist(source: &str) -> Result<ReceiverAllowlist, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("RSDB_ALLOWLIST or allowlist argument is required".to_owned());
    }

    let path = Path::new(source);
    if path.exists() || looks_like_path(source) {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("{}: read failed: {error}", path.display()))?;
        return parse_allowlist_text(&contents, &path.display().to_string());
    }

    parse_allowlist_text(source, "RSDB_ALLOWLIST")
}

fn looks_like_path(source: &str) -> bool {
    source.contains('/')
        || Path::new(source).extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("txt") || extension.eq_ignore_ascii_case("list")
        })
}

fn parse_allowlist_text(text: &str, kind: &str) -> Result<ReceiverAllowlist, String> {
    let public_keys = text
        .lines()
        .flat_map(|line| {
            line.split_once('#')
                .map_or(line, |(before, _)| before)
                .split([',', ' ', '\t'])
        })
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(validate_public_key)
        .collect::<Result<BTreeSet<_>, _>>()?
        .into_iter()
        .collect::<Vec<_>>();

    if public_keys.is_empty() {
        return Err(format!(
            "{kind}: allowlist must contain at least one public key"
        ));
    }

    Ok(ReceiverAllowlist::new(public_keys))
}

fn validate_public_key(value: &str) -> Result<String, String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err("allowlist public keys must be 64 hex characters".to_owned())
    }
}

fn unix_time_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn feed_message_kind(message: &FeedMessage) -> &'static str {
    match message {
        FeedMessage::Snapshot { .. } => "snapshot",
        FeedMessage::Aircraft { .. } => "aircraft",
        FeedMessage::StaleAircraft { .. } => "stale_aircraft",
        FeedMessage::Heartbeat { .. } => "heartbeat",
    }
}

fn verified_submission_payload(
    allowlist: &ReceiverAllowlist,
    submission: &SignedSubmission,
) -> Result<FeedMessage, String> {
    allowlist
        .verify_submission(submission)
        .map_err(|error| error.to_string())?;
    Ok(submission
        .payload
        .clone()
        .with_receiver(Some(ReceiverIdentity::new(submission.receiver_id.clone()))))
}

fn websocket_accept_key(key: &str) -> String {
    use sha1::{Digest, Sha1};

    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WEBSOCKET_GUID.as_bytes());
    base64_encode(&hasher.finalize())
}

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

fn base64_char(index: u32) -> char {
    let index = usize::try_from(index).expect("base64 index fits usize");
    char::from(BASE64_ALPHABET[index])
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn read_from(stream: &mut TcpStream) -> Result<Self, String> {
        let mut buffer = Vec::with_capacity(1024);
        let mut byte = [0_u8; 1];

        while !buffer.ends_with(b"\r\n\r\n") {
            if buffer.len() >= 16_384 {
                return Err("HTTP request headers too large".to_owned());
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
        let mut request = Self {
            method,
            path,
            headers,
            body: Vec::new(),
        };
        let content_length = request.content_length()?;

        request.body.resize(content_length, 0);
        stream
            .read_exact(&mut request.body)
            .map_err(|error| error.to_string())?;

        Ok(request)
    }

    fn content_length(&self) -> Result<usize, String> {
        let Some(value) = self.header("content-length") else {
            return Ok(0);
        };
        let length = value
            .parse::<usize>()
            .map_err(|_| format!("invalid Content-Length: {value}"))?;
        if length > MAX_REQUEST_BODY_BYTES {
            Err(format!(
                "request body is {length} bytes; limit is {MAX_REQUEST_BODY_BYTES}"
            ))
        } else {
            Ok(length)
        }
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
struct AggregatePersistence {
    data_dir: PathBuf,
    snapshot_path: PathBuf,
    submissions_path: PathBuf,
    retention_ms: u64,
    max_bytes: u64,
    state: Mutex<AggregatePersistenceState>,
}

impl AggregatePersistence {
    fn open(data_dir: &Path, retention_ms: u64, max_bytes: u64) -> Result<Self, String> {
        fs::create_dir_all(data_dir)
            .map_err(|error| format!("{}: create failed: {error}", data_dir.display()))?;

        Ok(Self {
            data_dir: data_dir.to_owned(),
            snapshot_path: data_dir.join("snapshot.json"),
            submissions_path: data_dir.join("submissions.ndjson"),
            retention_ms,
            max_bytes,
            state: Mutex::new(AggregatePersistenceState::default()),
        })
    }

    fn status(&self) -> AggregatePersistenceStatus {
        let state = self
            .state
            .lock()
            .expect("aggregate persistence state mutex not poisoned");
        AggregatePersistenceStatus {
            enabled: true,
            path: Some(self.data_dir.display().to_string()),
            retention_ms: Some(self.retention_ms),
            max_bytes: Some(self.max_bytes),
            log_bytes: Some(state.log_bytes),
            log_records: Some(state.log_records),
            ..AggregatePersistenceStatus::default()
        }
    }

    fn load_store(&self, allowlist: &ReceiverAllowlist) -> Result<AggregateStore, String> {
        let mut store = self.load_snapshot()?;
        let compacted = self.compact_submission_log(unix_time_ms())?;

        self.replay_submissions(&mut store, allowlist, &compacted.entries)?;
        store.retain_accepted_submission_ids(&retained_submission_ids(&compacted.entries));
        if compacted.compacted {
            self.save_snapshot(&store)?;
        }
        *self
            .state
            .lock()
            .expect("aggregate persistence state mutex not poisoned") = compacted.state;

        Ok(store)
    }

    fn load_snapshot(&self) -> Result<AggregateStore, String> {
        let contents = match fs::read_to_string(&self.snapshot_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(AggregateStore::new());
            }
            Err(error) => {
                return Err(format!(
                    "{}: read failed: {error}",
                    self.snapshot_path.display()
                ));
            }
        };
        let persistence = serde_json::from_str::<AggregateStorePersistence>(&contents)
            .map_err(|error| format!("{}: invalid JSON: {error}", self.snapshot_path.display()))?;

        AggregateStore::from_persistence(persistence)
            .map_err(|error| format!("{}: {error}", self.snapshot_path.display()))
    }

    fn replay_submissions(
        &self,
        store: &mut AggregateStore,
        allowlist: &ReceiverAllowlist,
        submissions: &[SubmissionLogEntry],
    ) -> Result<(), String> {
        for (index, entry) in submissions.iter().enumerate() {
            let submission = &entry.submission;
            if store.has_accepted_submission_id(&submission.submission_id) {
                continue;
            }
            let payload = verified_submission_payload(allowlist, submission).map_err(|error| {
                format!("{}:{}: {error}", self.submissions_path.display(), index + 1)
            })?;
            store
                .replay_verified_submission(
                    &submission.submission_id,
                    payload,
                    submission.submitted_at_ms,
                )
                .map_err(|error| {
                    format!(
                        "{}:{}: invalid submission payload: {error}",
                        self.submissions_path.display(),
                        index + 1
                    )
                })?;
        }

        Ok(())
    }

    fn append_submission(
        &self,
        submission: &SignedSubmission,
    ) -> Result<PersistenceSaveResult, String> {
        let line_bytes = submission_line_bytes(submission)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.submissions_path)
            .map_err(|error| {
                format!("{}: open failed: {error}", self.submissions_path.display())
            })?;

        serde_json::to_writer(&mut file, submission).map_err(|error| {
            format!(
                "{}: write JSON failed: {error}",
                self.submissions_path.display()
            )
        })?;
        file.write_all(b"\n").map_err(|error| {
            format!("{}: write failed: {error}", self.submissions_path.display())
        })?;
        file.flush().map_err(|error| {
            format!("{}: flush failed: {error}", self.submissions_path.display())
        })?;

        let mut state = self
            .state
            .lock()
            .expect("aggregate persistence state mutex not poisoned");
        state.log_records = state.log_records.saturating_add(1);
        state.log_bytes = state.log_bytes.saturating_add(line_bytes);
        state.records_since_checkpoint = state.records_since_checkpoint.saturating_add(1);
        state.oldest_submitted_at_ms = state
            .oldest_submitted_at_ms
            .map_or(Some(submission.submitted_at_ms), |oldest| {
                Some(oldest.min(submission.submitted_at_ms))
            });

        Ok(PersistenceSaveResult {
            writes: 1,
            log_bytes: Some(state.log_bytes),
            log_records: Some(state.log_records),
            ..PersistenceSaveResult::default()
        })
    }

    fn maintain(
        &self,
        store: &mut AggregateStore,
        now_ms: u64,
    ) -> Result<PersistenceSaveResult, String> {
        let state = self
            .state
            .lock()
            .expect("aggregate persistence state mutex not poisoned");
        let checkpoint_due = state.records_since_checkpoint >= AGGREGATE_CHECKPOINT_RECORDS;
        let compact_due = state.log_bytes > self.max_bytes
            || state
                .oldest_submitted_at_ms
                .is_some_and(|oldest| oldest < now_ms.saturating_sub(self.retention_ms));
        drop(state);

        if compact_due {
            let compacted = self.compact_submission_log(now_ms)?;
            let mut result = PersistenceSaveResult {
                compacted: compacted.compacted,
                log_bytes: Some(compacted.state.log_bytes),
                log_records: Some(compacted.state.log_records),
                ..PersistenceSaveResult::default()
            };
            if compacted.compacted {
                store.retain_accepted_submission_ids(&retained_submission_ids(&compacted.entries));
                self.save_snapshot(store)?;
                result.writes = result.writes.saturating_add(2);
            }
            *self
                .state
                .lock()
                .expect("aggregate persistence state mutex not poisoned") = compacted.state;
            return Ok(result);
        }

        if checkpoint_due {
            self.sync_submission_log()?;
            self.save_snapshot(store)?;
            let mut state = self
                .state
                .lock()
                .expect("aggregate persistence state mutex not poisoned");
            state.records_since_checkpoint = 0;
            return Ok(PersistenceSaveResult {
                writes: 1,
                log_bytes: Some(state.log_bytes),
                log_records: Some(state.log_records),
                ..PersistenceSaveResult::default()
            });
        }

        let state = self
            .state
            .lock()
            .expect("aggregate persistence state mutex not poisoned");
        Ok(PersistenceSaveResult {
            log_bytes: Some(state.log_bytes),
            log_records: Some(state.log_records),
            ..PersistenceSaveResult::default()
        })
    }

    fn sync_submission_log(&self) -> Result<(), String> {
        let file = match fs::File::open(&self.submissions_path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "{}: open failed: {error}",
                    self.submissions_path.display()
                ));
            }
        };
        file.sync_data()
            .map_err(|error| format!("{}: sync failed: {error}", self.submissions_path.display()))
    }

    fn save_snapshot(&self, store: &AggregateStore) -> Result<(), String> {
        let tmp_path = self.snapshot_tmp_path();
        let file = fs::File::create(&tmp_path)
            .map_err(|error| format!("{}: create failed: {error}", tmp_path.display()))?;
        let mut writer = BufWriter::new(file);

        serde_json::to_writer(&mut writer, &store.persistence_snapshot())
            .map_err(|error| format!("{}: write JSON failed: {error}", tmp_path.display()))?;
        writer
            .write_all(b"\n")
            .map_err(|error| format!("{}: write failed: {error}", tmp_path.display()))?;
        writer
            .flush()
            .map_err(|error| format!("{}: flush failed: {error}", tmp_path.display()))?;
        writer
            .get_ref()
            .sync_data()
            .map_err(|error| format!("{}: sync failed: {error}", tmp_path.display()))?;
        fs::rename(&tmp_path, &self.snapshot_path).map_err(|error| {
            format!(
                "{} -> {}: rename failed: {error}",
                tmp_path.display(),
                self.snapshot_path.display()
            )
        })
    }

    fn compact_submission_log(&self, now_ms: u64) -> Result<CompactSubmissionLogResult, String> {
        let read = self.read_submission_log()?;
        let original_bytes = read.original_bytes;
        let mut entries = read.entries;
        let cutoff_ms = now_ms.saturating_sub(self.retention_ms);
        entries.sort_by_key(|entry| entry.submission.submitted_at_ms);

        entries.retain(|entry| entry.submission.submitted_at_ms >= cutoff_ms);
        let mut retained_bytes = entries
            .iter()
            .fold(0_u64, |bytes, entry| bytes.saturating_add(entry.line_bytes));
        let mut first_retained = 0_usize;
        while retained_bytes > self.max_bytes && first_retained + 1 < entries.len() {
            retained_bytes = retained_bytes.saturating_sub(entries[first_retained].line_bytes);
            first_retained += 1;
        }
        if first_retained > 0 {
            entries.drain(0..first_retained);
        }
        let state = AggregatePersistenceState::from_entries(&entries);
        let compacted = original_bytes != state.log_bytes;

        if compacted {
            self.write_submission_log(&entries)?;
        }

        Ok(CompactSubmissionLogResult {
            entries,
            state,
            compacted,
        })
    }

    fn read_submission_log(&self) -> Result<SubmissionLogRead, String> {
        let file = match fs::File::open(&self.submissions_path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(SubmissionLogRead::default());
            }
            Err(error) => {
                return Err(format!(
                    "{}: open failed: {error}",
                    self.submissions_path.display()
                ));
            }
        };
        let mut reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut original_bytes = 0_u64;
        let mut line_number = 0_usize;

        loop {
            let mut line = Vec::new();
            let read = reader.read_until(b'\n', &mut line).map_err(|error| {
                format!("{}: read failed: {error}", self.submissions_path.display())
            })?;
            if read == 0 {
                break;
            }

            line_number += 1;
            original_bytes = original_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            let complete_line = line.last().is_some_and(|byte| *byte == b'\n');
            let trimmed = trim_ascii_whitespace(&line);
            if is_ignorable_journal_line(trimmed) {
                continue;
            }
            let submission = match serde_json::from_slice::<SignedSubmission>(trimmed) {
                Ok(submission) => submission,
                Err(_) if !complete_line => continue,
                Err(error) => {
                    return Err(format!(
                        "{}:{line_number}: invalid signed submission JSON: {error}",
                        self.submissions_path.display()
                    ));
                }
            };
            entries.push(SubmissionLogEntry {
                line_bytes: submission_line_bytes(&submission)?,
                submission,
            });
        }

        Ok(SubmissionLogRead {
            entries,
            original_bytes,
        })
    }

    fn write_submission_log(&self, entries: &[SubmissionLogEntry]) -> Result<(), String> {
        let tmp_path = self.submissions_tmp_path();
        let file = fs::File::create(&tmp_path)
            .map_err(|error| format!("{}: create failed: {error}", tmp_path.display()))?;
        let mut writer = BufWriter::new(file);

        for entry in entries {
            serde_json::to_writer(&mut writer, &entry.submission)
                .map_err(|error| format!("{}: write JSON failed: {error}", tmp_path.display()))?;
            writer
                .write_all(b"\n")
                .map_err(|error| format!("{}: write failed: {error}", tmp_path.display()))?;
        }
        writer
            .flush()
            .map_err(|error| format!("{}: flush failed: {error}", tmp_path.display()))?;
        writer
            .get_ref()
            .sync_data()
            .map_err(|error| format!("{}: sync failed: {error}", tmp_path.display()))?;
        fs::rename(&tmp_path, &self.submissions_path).map_err(|error| {
            format!(
                "{} -> {}: rename failed: {error}",
                tmp_path.display(),
                self.submissions_path.display()
            )
        })
    }

    fn snapshot_tmp_path(&self) -> PathBuf {
        self.data_dir.join(".snapshot.json.tmp")
    }

    fn submissions_tmp_path(&self) -> PathBuf {
        self.data_dir.join(".submissions.ndjson.tmp")
    }
}

#[derive(Debug, Default)]
struct AggregatePersistenceState {
    log_records: u64,
    log_bytes: u64,
    oldest_submitted_at_ms: Option<u64>,
    records_since_checkpoint: u64,
}

impl AggregatePersistenceState {
    fn from_entries(entries: &[SubmissionLogEntry]) -> Self {
        Self {
            log_records: u64::try_from(entries.len()).unwrap_or(u64::MAX),
            log_bytes: entries
                .iter()
                .fold(0_u64, |bytes, entry| bytes.saturating_add(entry.line_bytes)),
            oldest_submitted_at_ms: entries
                .iter()
                .map(|entry| entry.submission.submitted_at_ms)
                .min(),
            records_since_checkpoint: 0,
        }
    }
}

#[derive(Debug)]
struct SubmissionLogEntry {
    submission: SignedSubmission,
    line_bytes: u64,
}

#[derive(Debug, Default)]
struct SubmissionLogRead {
    entries: Vec<SubmissionLogEntry>,
    original_bytes: u64,
}

#[derive(Debug)]
struct CompactSubmissionLogResult {
    entries: Vec<SubmissionLogEntry>,
    state: AggregatePersistenceState,
    compacted: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct PersistenceSaveResult {
    writes: u64,
    compacted: bool,
    log_bytes: Option<u64>,
    log_records: Option<u64>,
}

fn retained_submission_ids(submissions: &[SubmissionLogEntry]) -> BTreeSet<String> {
    submissions
        .iter()
        .map(|entry| entry.submission.submission_id.clone())
        .collect()
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);

    &bytes[start..end]
}

fn is_ignorable_journal_line(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| *byte == 0 || byte.is_ascii_whitespace())
}

fn submission_line_bytes(submission: &SignedSubmission) -> Result<u64, String> {
    let bytes = serde_json::to_vec(submission).map_err(|error| error.to_string())?;
    Ok(u64::try_from(bytes.len() + 1).unwrap_or(u64::MAX))
}

#[derive(Debug)]
struct Hub {
    started_ms: u64,
    allowlist: ReceiverAllowlist,
    store: Arc<RwLock<AggregateStore>>,
    writer: mpsc::Sender<WriterCommand>,
    clients: Mutex<Vec<mpsc::Sender<String>>>,
    persistence_status: Arc<Mutex<AggregatePersistenceStatus>>,
}

#[allow(clippy::large_enum_variant)]
enum WriterCommand {
    Submit {
        submission_id: String,
        submission: SignedSubmission,
        payload: FeedMessage,
        submitted_at_ms: u64,
        reply: mpsc::Sender<Result<AggregateIngestResult, SubmitError>>,
    },
    Rejection {
        error: String,
        reply: mpsc::Sender<()>,
    },
}

fn spawn_writer(
    store: Arc<RwLock<AggregateStore>>,
    persistence: Option<AggregatePersistence>,
    persistence_status: Arc<Mutex<AggregatePersistenceStatus>>,
) -> mpsc::Sender<WriterCommand> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("rsdb-aggregate-writer".to_owned())
        .spawn(move || {
            for command in receiver {
                match command {
                    WriterCommand::Submit {
                        submission_id,
                        submission,
                        payload,
                        submitted_at_ms,
                        reply,
                    } => {
                        let result = writer_submit(
                            &store,
                            persistence.as_ref(),
                            &persistence_status,
                            &submission_id,
                            &submission,
                            payload,
                            submitted_at_ms,
                        );
                        let _ = reply.send(result);
                    }
                    WriterCommand::Rejection { error, reply } => {
                        store
                            .write()
                            .expect("aggregate store rwlock not poisoned")
                            .record_rejection(error);
                        let _ = reply.send(());
                    }
                }
            }
        })
        .expect("aggregate writer thread starts");
    sender
}

fn writer_submit(
    store: &RwLock<AggregateStore>,
    persistence: Option<&AggregatePersistence>,
    persistence_status: &Mutex<AggregatePersistenceStatus>,
    submission_id: &str,
    submission: &SignedSubmission,
    payload: FeedMessage,
    submitted_at_ms: u64,
) -> Result<AggregateIngestResult, SubmitError> {
    let is_new_submission = !store
        .read()
        .expect("aggregate store rwlock not poisoned")
        .has_accepted_submission_id(submission_id);

    if is_new_submission {
        persist_submission(persistence, persistence_status, submission)?;
    }

    let mut store = store.write().expect("aggregate store rwlock not poisoned");
    let ingest = store
        .ingest_verified_submission(submission_id, payload, submitted_at_ms)
        .map_err(|error| SubmitError::InvalidPayload(error.to_string()))?;
    if !ingest.duplicate {
        maintain_persistence(persistence, persistence_status, &mut store);
    }

    Ok(ingest)
}

fn persist_submission(
    persistence: Option<&AggregatePersistence>,
    persistence_status: &Mutex<AggregatePersistenceStatus>,
    submission: &SignedSubmission,
) -> Result<(), SubmitError> {
    let Some(persistence) = persistence else {
        return Ok(());
    };

    match persistence.append_submission(submission) {
        Ok(result) => {
            record_persistence_result(persistence_status, result);
            Ok(())
        }
        Err(error) => {
            record_persistence_error(persistence_status, error.clone());
            Err(SubmitError::Persistence(error))
        }
    }
}

fn maintain_persistence(
    persistence: Option<&AggregatePersistence>,
    persistence_status: &Mutex<AggregatePersistenceStatus>,
    store: &mut AggregateStore,
) {
    let Some(persistence) = persistence else {
        return;
    };

    match persistence.maintain(store, unix_time_ms()) {
        Ok(result) => record_persistence_result(persistence_status, result),
        Err(error) => {
            record_persistence_error(persistence_status, error.clone());
            eprintln!("aggregate persistence maintenance failed: {error}");
        }
    }
}

fn record_persistence_result(
    persistence_status: &Mutex<AggregatePersistenceStatus>,
    result: PersistenceSaveResult,
) {
    let now_ms = unix_time_ms();
    let mut status = persistence_status
        .lock()
        .expect("aggregate persistence status mutex not poisoned");
    status.writes = status.writes.saturating_add(result.writes);
    if result.compacted {
        status.compactions = status.compactions.saturating_add(1);
        status.last_compaction_ms = Some(now_ms);
    }
    if result.writes > 0 {
        status.last_write_ms = Some(now_ms);
    }
    status.log_bytes = result.log_bytes;
    status.log_records = result.log_records;
    status.last_error = None;
}

fn record_persistence_error(persistence_status: &Mutex<AggregatePersistenceStatus>, error: String) {
    persistence_status
        .lock()
        .expect("aggregate persistence status mutex not poisoned")
        .last_error = Some(error);
}

impl Hub {
    #[cfg(test)]
    fn new(allowlist: ReceiverAllowlist) -> Self {
        Self::with_store(allowlist, AggregateStore::new(), None)
    }

    fn with_store(
        allowlist: ReceiverAllowlist,
        store: AggregateStore,
        persistence: Option<AggregatePersistence>,
    ) -> Self {
        let persistence_status = persistence
            .as_ref()
            .map(AggregatePersistence::status)
            .unwrap_or_default();
        let store = Arc::new(RwLock::new(store));
        let persistence_status = Arc::new(Mutex::new(persistence_status));
        let writer = spawn_writer(
            Arc::clone(&store),
            persistence,
            Arc::clone(&persistence_status),
        );

        Self {
            started_ms: unix_time_ms(),
            allowlist,
            store,
            writer,
            clients: Mutex::new(Vec::new()),
            persistence_status,
        }
    }

    fn submit(&self, body: &[u8]) -> Result<SubmitResponse, SubmitError> {
        let submission = serde_json::from_slice::<SignedSubmission>(body)
            .map_err(|error| SubmitError::InvalidJson(error.to_string()))?;

        let payload = match verified_submission_payload(&self.allowlist, &submission) {
            Ok(payload) => payload,
            Err(error) => {
                self.record_rejection(error.clone());
                return Err(SubmitError::Rejected(error));
            }
        };

        let receiver_id = submission.receiver_id.clone();
        let submission_id = submission.submission_id.clone();
        let submitted_at_ms = submission.submitted_at_ms;
        let payload_type = feed_message_kind(&payload).to_owned();
        let (reply, receiver) = mpsc::channel();
        self.writer
            .send(WriterCommand::Submit {
                submission_id: submission_id.clone(),
                submission,
                payload,
                submitted_at_ms,
                reply,
            })
            .map_err(|_| SubmitError::Writer("aggregate writer is unavailable".to_owned()))?;
        let ingest = receiver
            .recv()
            .map_err(|_| SubmitError::Writer("aggregate writer stopped".to_owned()))??;

        if let Some(message) = &ingest.message {
            self.broadcast(message);
        }

        Ok(SubmitResponse {
            accepted: true,
            duplicate: ingest.duplicate,
            submission_id,
            receiver_id,
            payload_type,
        })
    }

    fn record_rejection(&self, error: String) {
        let (reply, receiver) = mpsc::channel();
        if self
            .writer
            .send(WriterCommand::Rejection { error, reply })
            .is_err()
        {
            eprintln!("aggregate writer is unavailable while recording rejection");
            return;
        }
        if receiver.recv().is_err() {
            eprintln!("aggregate writer stopped while recording rejection");
        }
    }

    fn subscribe(&self) -> mpsc::Receiver<String> {
        let (sender, receiver) = mpsc::channel();
        let now_ms = unix_time_ms();
        for message in self
            .store
            .read()
            .expect("aggregate store rwlock not poisoned")
            .feed_messages(now_ms)
        {
            if let Ok(encoded) = serde_json::to_string(&message) {
                let _ = sender.send(encoded);
            }
        }

        self.clients
            .lock()
            .expect("aggregate client list mutex not poisoned")
            .push(sender);

        receiver
    }

    fn broadcast(&self, message: &FeedMessage) {
        let Ok(encoded) = serde_json::to_string(message) else {
            return;
        };
        let mut clients = self
            .clients
            .lock()
            .expect("aggregate client list mutex not poisoned");

        clients.retain(|client| client.send(encoded.clone()).is_ok());
    }

    fn status_json(&self) -> rsdb::AggregateStatus {
        let now_ms = unix_time_ms();
        let uptime_ms = now_ms.saturating_sub(self.started_ms);
        let websocket_clients = self
            .clients
            .lock()
            .expect("aggregate client list mutex not poisoned")
            .len();

        let persistence = self
            .persistence_status
            .lock()
            .expect("aggregate persistence status mutex not poisoned")
            .clone();

        self.store
            .read()
            .expect("aggregate store rwlock not poisoned")
            .status_with_persistence(now_ms, uptime_ms, websocket_clients, persistence)
    }

    fn aircraft_json(&self) -> rsdb::AggregateSnapshot {
        self.store
            .read()
            .expect("aggregate store rwlock not poisoned")
            .snapshot(unix_time_ms())
    }

    fn bootstrap_json(&self) -> rsdb::AggregateBootstrap {
        self.store
            .read()
            .expect("aggregate store rwlock not poisoned")
            .bootstrap(unix_time_ms())
    }

    fn receivers_json(&self) -> Vec<rsdb::AggregateReceiverSummary> {
        self.status_json().receivers
    }
}

#[derive(Debug, Serialize)]
struct SubmitResponse {
    accepted: bool,
    duplicate: bool,
    submission_id: String,
    receiver_id: String,
    payload_type: String,
}

#[derive(Debug)]
enum SubmitError {
    InvalidJson(String),
    InvalidPayload(String),
    Persistence(String),
    Rejected(String),
    Writer(String),
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(formatter, "invalid submission JSON: {error}"),
            Self::InvalidPayload(error) => {
                write!(formatter, "invalid submission payload: {error}")
            }
            Self::Persistence(error) => {
                write!(formatter, "aggregate persistence failed: {error}")
            }
            Self::Rejected(error) => write!(formatter, "submission rejected: {error}"),
            Self::Writer(error) => write!(formatter, "aggregate writer failed: {error}"),
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ed25519_dalek::{Signer, SigningKey};
    use rsdb::{FeedMessage, ReceiverIdentity};

    use super::*;

    #[test]
    fn hub_accepts_allowlisted_submission() {
        let signing_key = sample_signing_key();
        let hub = Hub::new(allowlist_for(&signing_key));
        let submission = signed_submission(&signing_key);

        let response = hub
            .submit(&serde_json::to_vec(&submission).unwrap())
            .unwrap();

        assert!(response.accepted);
        assert!(!response.duplicate);
        assert_eq!(response.submission_id, submission.submission_id);
        assert_eq!(response.receiver_id, receiver_id_for(&signing_key));
        assert_eq!(response.payload_type, "aircraft");
        assert_eq!(hub.status_json().submissions_accepted, 1);
        assert_eq!(hub.aircraft_json().aircraft.len(), 1);
    }

    #[test]
    fn hub_accepts_duplicate_submission_without_reapplying() {
        let signing_key = sample_signing_key();
        let hub = Hub::new(allowlist_for(&signing_key));
        let submission = signed_submission(&signing_key);
        let body = serde_json::to_vec(&submission).unwrap();

        let first = hub.submit(&body).unwrap();
        let second = hub.submit(&body).unwrap();
        let status = hub.status_json();

        assert!(!first.duplicate);
        assert!(second.accepted);
        assert!(second.duplicate);
        assert_eq!(second.submission_id, submission.submission_id);
        assert_eq!(status.submissions_accepted, 1);
        assert_eq!(status.submissions_duplicate, 1);
        assert_eq!(status.submissions_rejected, 0);
        assert_eq!(hub.aircraft_json().aircraft.len(), 1);
    }

    #[test]
    fn hub_normalizes_receiver_identity_from_public_key() {
        let signing_key = sample_signing_key();
        let hub = Hub::new(allowlist_for(&signing_key));
        let submission = signed_submission(&signing_key);

        hub.submit(&serde_json::to_vec(&submission).unwrap())
            .unwrap();
        let snapshot = hub.aircraft_json();
        let receiver = &snapshot.aircraft[0].receiver;

        assert_eq!(receiver.id, receiver_id_for(&signing_key));
        assert_eq!(receiver.name, None);
        assert!(receiver.handle.is_some());
    }

    #[test]
    fn hub_persists_accepted_submission_state() {
        let signing_key = sample_signing_key();
        let dir = temp_test_dir("rsdb-aggregate-persistence");
        let retention_ms = 24 * 60 * 60 * 1_000;
        let max_bytes = 1_000_000;
        let submission = signed_submission(&signing_key);
        let body = serde_json::to_vec(&submission).unwrap();
        let persistence = AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let hub = Hub::with_store(
            allowlist_for(&signing_key),
            AggregateStore::new(),
            Some(persistence),
        );

        let first = hub.submit(&body).unwrap();
        let write_status = hub.status_json();
        let restored_persistence =
            AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let restored_store = restored_persistence
            .load_store(&allowlist_for(&signing_key))
            .unwrap();
        let restored_hub = Hub::with_store(
            allowlist_for(&signing_key),
            restored_store,
            Some(restored_persistence),
        );
        let second = restored_hub.submit(&body).unwrap();
        let status = restored_hub.status_json();

        assert!(!first.duplicate);
        assert!(dir.join("submissions.ndjson").is_file());
        assert!(second.duplicate);
        assert_eq!(status.submissions_accepted, 1);
        assert_eq!(status.submissions_duplicate, 1);
        assert_eq!(restored_hub.aircraft_json().aircraft.len(), 1);
        assert_eq!(second.submission_id, submission.submission_id);
        assert_eq!(write_status.persistence.writes, 1);
        assert!(write_status.persistence.last_write_ms.is_some());
        assert!(status.persistence.enabled);
        assert_eq!(status.persistence.path, Some(dir.display().to_string()));
        assert_eq!(status.persistence.retention_ms, Some(retention_ms));
        assert_eq!(status.persistence.max_bytes, Some(max_bytes));
        assert!(status.persistence.log_bytes.is_some_and(|bytes| bytes > 0));
        assert_eq!(status.persistence.log_records, Some(1));
        assert_eq!(status.persistence.last_error, None);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistence_compacts_submission_log_by_disk_size() {
        let signing_key = sample_signing_key();
        let allowlist = allowlist_for(&signing_key);
        let dir = temp_test_dir("rsdb-aggregate-disk-size");
        let retention_ms = 60 * 60 * 1_000;
        let now_ms = unix_time_ms();
        let first_submission = signed_submission_at(&signing_key, now_ms);
        let fresh_submission = signed_submission_at(&signing_key, now_ms + 1);
        let max_bytes = submission_line_bytes(&fresh_submission).unwrap();
        let persistence = AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let hub = Hub::with_store(allowlist.clone(), AggregateStore::new(), Some(persistence));

        hub.submit(&serde_json::to_vec(&first_submission).unwrap())
            .unwrap();
        hub.submit(&serde_json::to_vec(&fresh_submission).unwrap())
            .unwrap();
        let compacted_status = hub.status_json();

        let log = fs::read_to_string(dir.join("submissions.ndjson")).unwrap();
        assert!(!log.contains(&first_submission.submission_id));
        assert!(log.contains(&fresh_submission.submission_id));
        assert!(compacted_status.persistence.compactions >= 1);
        assert!(compacted_status.persistence.last_compaction_ms.is_some());
        assert_eq!(compacted_status.persistence.log_records, Some(1));
        assert!(
            compacted_status
                .persistence
                .log_bytes
                .is_some_and(|bytes| bytes <= max_bytes)
        );

        let restored_persistence =
            AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let restored_store = restored_persistence.load_store(&allowlist).unwrap();
        let restored_hub = Hub::with_store(allowlist, restored_store, Some(restored_persistence));
        let replay = restored_hub
            .submit(&serde_json::to_vec(&fresh_submission).unwrap())
            .unwrap();

        assert!(replay.duplicate);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistence_recovers_from_trailing_journal_residue() {
        let signing_key = sample_signing_key();
        let allowlist = allowlist_for(&signing_key);
        let dir = temp_test_dir("rsdb-aggregate-journal-residue");
        let retention_ms = 24 * 60 * 60 * 1_000;
        let max_bytes = 1_000_000;
        let submission = signed_submission(&signing_key);
        let persistence = AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let hub = Hub::with_store(allowlist.clone(), AggregateStore::new(), Some(persistence));

        hub.submit(&serde_json::to_vec(&submission).unwrap())
            .unwrap();
        {
            let mut log = fs::OpenOptions::new()
                .append(true)
                .open(dir.join("submissions.ndjson"))
                .unwrap();
            log.write_all(b"\n\0\0\0").unwrap();
        }

        let restored_persistence =
            AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let restored_store = restored_persistence.load_store(&allowlist).unwrap();
        let restored_hub = Hub::with_store(allowlist, restored_store, Some(restored_persistence));
        let replay = restored_hub
            .submit(&serde_json::to_vec(&submission).unwrap())
            .unwrap();
        let log = fs::read(dir.join("submissions.ndjson")).unwrap();

        assert!(replay.duplicate);
        assert!(!log.contains(&0));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistence_rejects_complete_invalid_journal_line() {
        let signing_key = sample_signing_key();
        let allowlist = allowlist_for(&signing_key);
        let dir = temp_test_dir("rsdb-aggregate-invalid-journal");
        let retention_ms = 24 * 60 * 60 * 1_000;
        let max_bytes = 1_000_000;
        let submission = signed_submission(&signing_key);
        let persistence = AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let hub = Hub::with_store(allowlist.clone(), AggregateStore::new(), Some(persistence));

        hub.submit(&serde_json::to_vec(&submission).unwrap())
            .unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(dir.join("submissions.ndjson"))
            .unwrap()
            .write_all(b"not-json\n")
            .unwrap();

        let restored_persistence =
            AggregatePersistence::open(&dir, retention_ms, max_bytes).unwrap();
        let error = restored_persistence.load_store(&allowlist).unwrap_err();

        assert!(error.contains("invalid signed submission JSON"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hub_rejects_tampered_submission() {
        let signing_key = sample_signing_key();
        let hub = Hub::new(allowlist_for(&signing_key));
        let mut submission = signed_submission(&signing_key);
        submission.payload = FeedMessage::stale_aircraft(101, "A00001".to_owned())
            .with_receiver(Some(receiver(&signing_key)));

        let error = hub
            .submit(&serde_json::to_vec(&submission).unwrap())
            .unwrap_err();

        assert!(matches!(error, SubmitError::Rejected(_)));
        let status = hub.status_json();
        assert_eq!(status.submissions_accepted, 0);
        assert_eq!(status.submissions_rejected, 1);
    }

    #[test]
    fn allowlist_file_loads_for_server_startup() {
        let signing_key = sample_signing_key();
        let dir = temp_test_dir("rsdb-aggregate-allowlist");
        let path = dir.join("allowlist.txt");

        fs::write(&path, format!("{}\n", public_key_hex(&signing_key))).unwrap();

        let allowlist = read_allowlist(&path.display().to_string()).unwrap();

        assert_eq!(allowlist.public_keys, vec![public_key_hex(&signing_key)]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn inline_allowlist_loads_for_secret_managed_deploys() {
        let signing_key = sample_signing_key();
        let allowlist_text = public_key_hex(&signing_key);

        let allowlist = read_allowlist(&allowlist_text).unwrap();

        assert_eq!(allowlist.public_keys, vec![public_key_hex(&signing_key)]);
    }

    #[test]
    fn websocket_helpers_match_rfc_accept_key_and_text_frame() {
        assert_eq!(
            websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );

        let mut frame = Vec::new();
        write_websocket_text_frame(&mut frame, "hello").unwrap();

        assert_eq!(&frame[..2], &[0x81, 5]);
        assert_eq!(&frame[2..], b"hello");
    }

    fn signed_submission(signing_key: &SigningKey) -> SignedSubmission {
        signed_submission_at(signing_key, unix_time_ms())
    }

    fn signed_submission_at(signing_key: &SigningKey, submitted_at_ms: u64) -> SignedSubmission {
        let mut submission = SignedSubmission::new_ed25519(
            receiver_id_for(signing_key),
            submitted_at_ms,
            FeedMessage::aircraft(100, aircraft()).with_receiver(Some(receiver(signing_key))),
            String::new(),
        );
        submission.signature = encode_hex(
            &signing_key
                .sign(&submission.signing_bytes().unwrap())
                .to_bytes(),
        );
        submission
    }

    fn allowlist_for(signing_key: &SigningKey) -> ReceiverAllowlist {
        ReceiverAllowlist::new(vec![public_key_hex(signing_key)])
    }

    fn receiver(signing_key: &SigningKey) -> ReceiverIdentity {
        ReceiverIdentity::named(receiver_id_for(signing_key), "Submitted Name".to_owned())
    }

    fn receiver_id_for(signing_key: &SigningKey) -> String {
        rsdb::receiver_id_from_ed25519_public_key_hex(&public_key_hex(signing_key)).unwrap()
    }

    fn public_key_hex(signing_key: &SigningKey) -> String {
        encode_hex(&signing_key.verifying_key().to_bytes())
    }

    fn sample_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    fn aircraft() -> rsdb::AircraftSnapshot {
        rsdb::AircraftSnapshot {
            icao: "A00001".to_owned(),
            callsign: Some("TEST123".to_owned()),
            callsign_last_seen_ms: Some(100),
            category: None,
            altitude_baro_ft: Some(12_000),
            altitude_geometric_ft: None,
            altitude_last_seen_ms: Some(100),
            lat: None,
            lon: None,
            distance_km: None,
            bearing_deg: None,
            seen_seconds_ago: None,
            position_status: rsdb::PositionStatus::Unavailable,
            position_last_seen_ms: None,
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
            last_seen_ms: 100,
            message_count: 1,
            last_type_code: None,
            last_decode_status: rsdb::DecodeStatus::Updated,
            last_raw: String::new(),
            raw_messages: Vec::new(),
        }
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
        fs::create_dir_all(&path).unwrap();
        path
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
}
