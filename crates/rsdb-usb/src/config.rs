use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsdb::{
    Protocol, RadioConfig, ReceiverIdentity, ReceiverSite, SubmissionOutboxConfig,
    SubmissionSigner, receiver_id_from_ed25519_secret_hex,
};

use crate::usb::{GainMode, RtlSdrConfig};

pub(crate) const DEFAULT_CONFIG_PATH: &str = "/etc/rsdb/rsdb.env";
pub(crate) const DEFAULT_COLLECTOR_HOST: &str = "127.0.0.1";
pub(crate) const DEFAULT_COLLECTOR_PORT: u16 = 8080;
const DEFAULT_STREAM_SECONDS: u64 = 5;
const DEFAULT_STALE_AFTER_SECONDS: u64 = 60;
pub(crate) const DEFAULT_HEARTBEAT_SECONDS: u64 = 15;
const DEFAULT_RETRY_SECONDS: u64 = 10;
const DEFAULT_SUBMIT_RETRY_SECONDS: u64 = 5;
const DEFAULT_SUBMIT_MAX_LAG_SECONDS: u64 = 60;
const DEFAULT_SUBMIT_OUTBOX_MAX_MB: u64 = 25;
const DEFAULT_PERSIST_FEED_MAX_MB: u64 = 100;
pub(crate) const BYTES_PER_MEGABYTE: u64 = 1_000_000;

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfig {
    pub(crate) device_index: usize,
    pub(crate) collector_port: u16,
    pub(crate) stream_seconds: u64,
    pub(crate) json_seconds: Option<u64>,
    protocol: Protocol,
    center_frequency_hz: Option<u32>,
    sample_rate_hz: Option<u32>,
    gain: GainMode,
    bias_t: bool,
    stale_after_ms: u64,
    heartbeat_interval_ms: u64,
    retry_after_ms: u64,
    receiver_lat: Option<f64>,
    receiver_lon: Option<f64>,
    persist_dir: Option<PathBuf>,
    persist_feed_max_bytes: u64,
    signing_key_path: Option<PathBuf>,
    submit_urls: Vec<String>,
    submit_retry_ms: u64,
    submit_max_lag_ms: u64,
    submission_outbox_dir: Option<PathBuf>,
    submission_outbox_max_bytes: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            device_index: 0,
            protocol: Protocol::Adsb1090,
            center_frequency_hz: None,
            sample_rate_hz: None,
            collector_port: DEFAULT_COLLECTOR_PORT,
            gain: RtlSdrConfig::default().gain,
            bias_t: false,
            stream_seconds: DEFAULT_STREAM_SECONDS,
            json_seconds: None,
            stale_after_ms: seconds_to_ms(DEFAULT_STALE_AFTER_SECONDS),
            heartbeat_interval_ms: seconds_to_ms(DEFAULT_HEARTBEAT_SECONDS),
            retry_after_ms: seconds_to_ms(DEFAULT_RETRY_SECONDS),
            receiver_lat: None,
            receiver_lon: None,
            persist_dir: None,
            persist_feed_max_bytes: DEFAULT_PERSIST_FEED_MAX_MB * BYTES_PER_MEGABYTE,
            signing_key_path: None,
            submit_urls: Vec::new(),
            submit_retry_ms: seconds_to_ms(DEFAULT_SUBMIT_RETRY_SECONDS),
            submit_max_lag_ms: seconds_to_ms(DEFAULT_SUBMIT_MAX_LAG_SECONDS),
            submission_outbox_dir: None,
            submission_outbox_max_bytes: DEFAULT_SUBMIT_OUTBOX_MAX_MB * BYTES_PER_MEGABYTE,
        }
    }
}

impl RuntimeConfig {
    pub(crate) fn load(explicit_path: Option<PathBuf>) -> Result<Self, String> {
        let mut config = Self::default();

        if let Some(path) = config_file_path(explicit_path) {
            config.apply_file(&path)?;
        }
        config.apply_environment()?;

        Ok(config)
    }

    pub(crate) fn rtl_sdr_config(&self, device_index: usize) -> RtlSdrConfig {
        let radio = self.radio_config();

        RtlSdrConfig {
            device_index,
            protocol: radio.protocol,
            center_frequency_hz: radio.center_frequency_hz,
            sample_rate_hz: radio.sample_rate_hz,
            gain: self.gain,
            bias_t: self.bias_t,
        }
    }

    fn radio_config(&self) -> RadioConfig {
        let mut radio = RadioConfig::for_protocol(self.protocol);

        if let Some(center_frequency_hz) = self.center_frequency_hz {
            radio = radio.with_center_frequency_hz(center_frequency_hz);
        }
        if let Some(sample_rate_hz) = self.sample_rate_hz {
            radio = radio.with_sample_rate_hz(sample_rate_hz);
        }

        radio
    }

    pub(crate) fn feed_config(&self) -> Result<FeedRuntimeConfig, String> {
        Ok(FeedRuntimeConfig {
            radio: self.radio_config(),
            stale_after: self.stale_after_ms,
            heartbeat_interval: self.heartbeat_interval_ms,
            retry_after: self.retry_after_ms,
            receiver_identity: self.receiver_identity()?,
            receiver_site: self.receiver_site()?,
            persistence: PersistenceRuntimeConfig {
                dir: self.persist_dir.clone(),
                feed_max_bytes: self.persist_feed_max_bytes,
            },
        })
    }

    fn receiver_site(&self) -> Result<Option<ReceiverSite>, String> {
        match (self.receiver_lat, self.receiver_lon) {
            (Some(lat), Some(lon)) => Ok(Some(ReceiverSite {
                name: None,
                lat,
                lon,
            })),
            (None, None) => Ok(None),
            _ => Err("RSDB_RECEIVER_LAT and RSDB_RECEIVER_LON must be set together".to_owned()),
        }
    }

    fn receiver_identity(&self) -> Result<Option<ReceiverIdentity>, String> {
        let Some(id) = self.resolved_receiver_id()? else {
            return Ok(None);
        };

        Ok(Some(ReceiverIdentity::new(id)))
    }

    pub(crate) fn required_receiver_identity(&self) -> Result<ReceiverIdentity, String> {
        self.receiver_identity()?
            .ok_or_else(|| "an Ed25519 signing key is required for receiver identity".to_owned())
    }

    pub(crate) fn submission_signer(&self) -> Result<SubmissionSigner, String> {
        let secret_key_hex = self.signing_key_hex()?;

        SubmissionSigner::from_ed25519_secret_hex(&secret_key_hex)
            .map_err(|error| error.to_string())
    }

    fn resolved_receiver_id(&self) -> Result<Option<String>, String> {
        if !self.signing_key_configured() {
            return Ok(None);
        }

        let secret_key_hex = self.signing_key_hex()?;
        receiver_id_from_ed25519_secret_hex(&secret_key_hex)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn signing_key_configured(&self) -> bool {
        self.signing_key_path.is_some()
    }

    fn signing_key_hex(&self) -> Result<String, String> {
        if let Some(path) = &self.signing_key_path {
            return fs::read_to_string(path)
                .map(|value| value.trim().to_owned())
                .map_err(|error| format!("{}: read failed: {error}", path.display()));
        }

        Err("RSDB_SIGNING_KEY_PATH is required".to_owned())
    }

    pub(crate) fn submission_config(&self) -> Result<Option<SubmissionConfig>, String> {
        let submit_urls = self.submit_urls();
        if submit_urls.is_empty() {
            return Ok(None);
        }

        Ok(Some(SubmissionConfig {
            aggregate_urls: submit_urls,
            signer: self.submission_signer()?,
            receiver_identity: self.required_receiver_identity()?,
            retry_after: Duration::from_millis(self.submit_retry_ms.max(1_000)),
            max_payload_lag: Duration::from_millis(self.submit_max_lag_ms.max(1_000)),
            outbox: self
                .submission_outbox_dir
                .clone()
                .map(|dir| SubmissionOutboxConfig {
                    dir,
                    max_bytes: self.submission_outbox_max_bytes,
                }),
        }))
    }

    fn submit_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();

        for url in &self.submit_urls {
            if !urls.contains(url) {
                urls.push(url.clone());
            }
        }

        urls
    }

    fn apply_file(&mut self, path: &Path) -> Result<(), String> {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;

        for (line_index, line) in contents.lines().enumerate() {
            let Some((key, value)) = parse_config_line(line) else {
                continue;
            };
            self.apply_pair(&key, &value)
                .map_err(|error| format!("{}:{}: {error}", path.display(), line_index + 1))?;
        }

        Ok(())
    }

    fn apply_environment(&mut self) -> Result<(), String> {
        for key in CONFIG_KEYS {
            let Ok(value) = env::var(key) else {
                continue;
            };
            self.apply_pair(key, value.trim())
                .map_err(|error| format!("{key}: {error}"))?;
        }

        Ok(())
    }

    fn apply_pair(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "RSDB_DEVICE_INDEX" => self.device_index = parse_usize(value, key)?,
            "RSDB_PROTOCOL" => self.protocol = parse_protocol(value, key)?,
            "RSDB_CENTER_FREQUENCY_HZ" => {
                self.center_frequency_hz = parse_optional_u32_value(value, key)?;
            }
            "RSDB_SAMPLE_RATE_HZ" => {
                self.sample_rate_hz = parse_optional_u32_value(value, key)?;
            }
            "RSDB_COLLECTOR_PORT" => self.collector_port = parse_port(value, key)?,
            "RSDB_GAIN" => self.gain = parse_gain(value, key)?,
            "RSDB_GAIN_TENTH_DB" => self.gain = GainMode::Manual(parse_i32(value, key)?),
            "RSDB_BIAS_T" => self.bias_t = parse_bool(value, key)?,
            "RSDB_STREAM_SECONDS" => self.stream_seconds = parse_u64(value, key)?,
            "RSDB_JSON_SECONDS" => self.json_seconds = parse_optional_u64_value(value, key)?,
            "RSDB_STALE_AFTER_SECONDS" => {
                self.stale_after_ms = seconds_to_ms(parse_u64(value, key)?);
            }
            "RSDB_HEARTBEAT_SECONDS" => {
                self.heartbeat_interval_ms = seconds_to_ms(parse_u64(value, key)?);
            }
            "RSDB_RETRY_SECONDS" => {
                self.retry_after_ms = seconds_to_ms(parse_u64(value, key)?);
            }
            "RSDB_RECEIVER_LAT" => self.receiver_lat = parse_optional_lat(value, key)?,
            "RSDB_RECEIVER_LON" => self.receiver_lon = parse_optional_lon(value, key)?,
            "RSDB_PERSIST_DIR" => self.persist_dir = parse_optional_path(value),
            "RSDB_PERSIST_FEED_MAX_MB" => {
                self.persist_feed_max_bytes =
                    parse_u64(value, key)?.saturating_mul(BYTES_PER_MEGABYTE);
            }
            "RSDB_SIGNING_KEY_PATH" => self.signing_key_path = parse_optional_path(value),
            "RSDB_SUBMIT_URLS" => {
                self.submit_urls = parse_url_list(value);
            }
            "RSDB_SUBMIT_RETRY_SECONDS" => {
                self.submit_retry_ms = seconds_to_ms(parse_u64(value, key)?);
            }
            "RSDB_SUBMIT_MAX_LAG_SECONDS" => {
                let max_lag_seconds = parse_u64(value, key)?;
                if max_lag_seconds == 0 {
                    return Err(format!("{key} must be greater than zero"));
                }
                self.submit_max_lag_ms = seconds_to_ms(max_lag_seconds);
            }
            "RSDB_SUBMIT_OUTBOX_DIR" => self.submission_outbox_dir = parse_optional_path(value),
            "RSDB_SUBMIT_OUTBOX_MAX_MB" => {
                let max_mb = parse_u64(value, key)?;
                if max_mb == 0 {
                    return Err(format!("{key} must be greater than zero"));
                }
                self.submission_outbox_max_bytes = max_mb.saturating_mul(BYTES_PER_MEGABYTE);
            }
            _ => {}
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FeedRuntimeConfig {
    pub(crate) radio: RadioConfig,
    pub(crate) stale_after: u64,
    pub(crate) heartbeat_interval: u64,
    #[cfg_attr(not(feature = "websocket"), allow(dead_code))]
    pub(crate) retry_after: u64,
    pub(crate) receiver_identity: Option<ReceiverIdentity>,
    pub(crate) receiver_site: Option<ReceiverSite>,
    #[cfg_attr(not(feature = "websocket"), allow(dead_code))]
    pub(crate) persistence: PersistenceRuntimeConfig,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "websocket"), allow(dead_code))]
pub(crate) struct PersistenceRuntimeConfig {
    pub(crate) dir: Option<PathBuf>,
    pub(crate) feed_max_bytes: u64,
}

#[derive(Clone)]
#[cfg_attr(not(feature = "websocket"), allow(dead_code))]
pub(crate) struct SubmissionConfig {
    pub(crate) aggregate_urls: Vec<String>,
    pub(crate) signer: SubmissionSigner,
    pub(crate) receiver_identity: ReceiverIdentity,
    pub(crate) retry_after: Duration,
    pub(crate) max_payload_lag: Duration,
    pub(crate) outbox: Option<SubmissionOutboxConfig>,
}

const CONFIG_KEYS: &[&str] = &[
    "RSDB_DEVICE_INDEX",
    "RSDB_PROTOCOL",
    "RSDB_CENTER_FREQUENCY_HZ",
    "RSDB_SAMPLE_RATE_HZ",
    "RSDB_COLLECTOR_PORT",
    "RSDB_GAIN",
    "RSDB_GAIN_TENTH_DB",
    "RSDB_BIAS_T",
    "RSDB_STREAM_SECONDS",
    "RSDB_JSON_SECONDS",
    "RSDB_STALE_AFTER_SECONDS",
    "RSDB_HEARTBEAT_SECONDS",
    "RSDB_RETRY_SECONDS",
    "RSDB_RECEIVER_LAT",
    "RSDB_RECEIVER_LON",
    "RSDB_PERSIST_DIR",
    "RSDB_PERSIST_FEED_MAX_MB",
    "RSDB_SIGNING_KEY_PATH",
    "RSDB_SUBMIT_URLS",
    "RSDB_SUBMIT_RETRY_SECONDS",
    "RSDB_SUBMIT_MAX_LAG_SECONDS",
    "RSDB_SUBMIT_OUTBOX_DIR",
    "RSDB_SUBMIT_OUTBOX_MAX_MB",
];

pub(crate) fn take_config_path(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let Some(index) = args.iter().position(|arg| arg == "--config" || arg == "-c") else {
        return Ok(None);
    };

    if index + 1 >= args.len() {
        return Err(format!("{} requires a path", args[index]));
    }

    let path = PathBuf::from(args.remove(index + 1));
    args.remove(index);

    Ok(Some(path))
}

fn config_file_path(explicit_path: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit_path {
        return Some(path);
    }

    if let Ok(path) = env::var("RSDB_CONFIG") {
        let path = path.trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    let default_path = PathBuf::from(DEFAULT_CONFIG_PATH);
    default_path.exists().then_some(default_path)
}

fn parse_config_line(line: &str) -> Option<(String, String)> {
    let line = line
        .split_once('#')
        .map_or(line, |(before, _)| before)
        .trim();
    let line = line.strip_prefix("export ").unwrap_or(line).trim();

    if line.is_empty() {
        return None;
    }

    let (key, value) = line.split_once('=')?;
    Some((key.trim().to_owned(), unquote(value.trim()).to_owned()))
}

fn unquote(value: &str) -> &str {
    if value.len() < 2 {
        return value;
    }

    let first = value.as_bytes()[0];
    let last = value.as_bytes()[value.len() - 1];

    if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

pub(crate) fn parse_index(value: Option<&String>, default: usize) -> Result<usize, String> {
    match value {
        Some(value) => value
            .parse()
            .map_err(|_| format!("invalid device index: {value}")),
        None => Ok(default),
    }
}

pub(crate) fn parse_seconds(value: Option<&String>, default: u64) -> Result<u64, String> {
    match value {
        Some(value) => value
            .parse()
            .map_err(|_| format!("invalid stream duration: {value}")),
        None => Ok(default),
    }
}

pub(crate) fn parse_optional_seconds(
    value: Option<&String>,
    default: Option<u64>,
) -> Result<Option<u64>, String> {
    match value {
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| format!("invalid stream duration: {value}")),
        None => Ok(default),
    }
}

pub(crate) fn parse_required_seconds(value: Option<&String>, key: &str) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("{key} is required"))?;
    parse_u64(value, key)
}

pub(crate) fn parse_required_path(value: Option<&String>, key: &str) -> Result<PathBuf, String> {
    let value = value.ok_or_else(|| format!("{key} is required"))?;
    Ok(PathBuf::from(parse_non_empty(value, key)?))
}

fn parse_non_empty<'a>(value: &'a str, key: &str) -> Result<&'a str, String> {
    if value.is_empty() {
        Err(format!("{key} must not be empty"))
    } else {
        Ok(value)
    }
}

fn parse_usize(value: &str, key: &str) -> Result<usize, String> {
    parse_non_empty(value, key)?
        .parse()
        .map_err(|_| format!("{key} must be a non-negative integer"))
}

fn parse_i32(value: &str, key: &str) -> Result<i32, String> {
    parse_non_empty(value, key)?
        .parse()
        .map_err(|_| format!("{key} must be an integer"))
}

fn parse_u64(value: &str, key: &str) -> Result<u64, String> {
    parse_non_empty(value, key)?
        .parse()
        .map_err(|_| format!("{key} must be a non-negative integer"))
}

fn parse_u32(value: &str, key: &str) -> Result<u32, String> {
    parse_non_empty(value, key)?
        .parse()
        .map_err(|_| format!("{key} must be a non-negative integer up to 4294967295"))
}

pub(crate) fn parse_port(value: &str, key: &str) -> Result<u16, String> {
    parse_non_empty(value, key)?
        .parse::<u16>()
        .map_err(|_| format!("{key} must be a TCP port between 0 and 65535"))
}

fn parse_optional_u64_value(value: &str, key: &str) -> Result<Option<u64>, String> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u64(value, key).map(Some)
    }
}

fn parse_optional_u32_value(value: &str, key: &str) -> Result<Option<u32>, String> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u32(value, key).map(Some)
    }
}

fn parse_optional_lat(value: &str, key: &str) -> Result<Option<f64>, String> {
    if value.is_empty() {
        return Ok(None);
    }

    let value = parse_f64(value, key)?;
    if (-90.0..=90.0).contains(&value) {
        Ok(Some(value))
    } else {
        Err(format!("{key} must be between -90 and 90"))
    }
}

fn parse_optional_lon(value: &str, key: &str) -> Result<Option<f64>, String> {
    if value.is_empty() {
        return Ok(None);
    }

    let value = parse_f64(value, key)?;
    if (-180.0..=180.0).contains(&value) {
        Ok(Some(value))
    } else {
        Err(format!("{key} must be between -180 and 180"))
    }
}

fn parse_optional_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn parse_url_list(value: &str) -> Vec<String> {
    value
        .split(|character: char| character == ',' || character.is_whitespace())
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_f64(value: &str, key: &str) -> Result<f64, String> {
    let value = parse_non_empty(value, key)?;
    let parsed = value
        .parse()
        .map_err(|_| format!("{key} must be a number"))?;

    if f64::is_finite(parsed) {
        Ok(parsed)
    } else {
        Err(format!("{key} must be finite"))
    }
}

fn parse_gain(value: &str, key: &str) -> Result<GainMode, String> {
    let value = parse_non_empty(value, key)?;

    if value.eq_ignore_ascii_case("auto") {
        Ok(GainMode::Auto)
    } else {
        parse_i32(value, key).map(GainMode::Manual)
    }
}

fn parse_protocol(value: &str, key: &str) -> Result<Protocol, String> {
    parse_non_empty(value, key)?
        .parse::<Protocol>()
        .map_err(|error| error.to_string())
}

fn parse_bool(value: &str, key: &str) -> Result<bool, String> {
    match parse_non_empty(value, key)?.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{key} must be true or false")),
    }
}

const fn seconds_to_ms(seconds: u64) -> u64 {
    seconds.saturating_mul(1_000)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use rsdb::Protocol;

    use super::*;

    #[test]
    fn runtime_config_has_no_receiver_identity_without_signing_key() {
        let config = RuntimeConfig::default();

        assert_eq!(config.feed_config().unwrap().receiver_identity, None);
    }

    #[test]
    fn runtime_config_selects_protocol_radio_defaults() {
        let mut config = RuntimeConfig::default();

        config.apply_pair("RSDB_PROTOCOL", "uat978").unwrap();

        let radio = config.radio_config();
        assert_eq!(radio.protocol, Protocol::Uat978);
        assert_eq!(radio.center_frequency_hz, 978_000_000);
        assert_eq!(radio.sample_rate_hz, 2_400_000);
        assert_eq!(config.rtl_sdr_config(2).protocol, Protocol::Uat978);
    }

    #[test]
    fn runtime_config_allows_explicit_radio_overrides() {
        let mut config = RuntimeConfig::default();

        config.apply_pair("RSDB_PROTOCOL", "ais").unwrap();
        config
            .apply_pair("RSDB_CENTER_FREQUENCY_HZ", "161975000")
            .unwrap();
        config.apply_pair("RSDB_SAMPLE_RATE_HZ", "1536000").unwrap();

        let radio = config.radio_config();
        assert_eq!(radio.protocol, Protocol::Ais);
        assert_eq!(radio.center_frequency_hz, 161_975_000);
        assert_eq!(radio.sample_rate_hz, 1_536_000);
    }

    #[test]
    fn runtime_config_rejects_unknown_protocol() {
        let mut config = RuntimeConfig::default();

        assert!(
            config
                .apply_pair("RSDB_PROTOCOL", "weatherfax")
                .unwrap_err()
                .contains("unsupported protocol")
        );
    }

    #[test]
    fn runtime_config_derives_receiver_identity_from_signing_key() {
        let mut config = RuntimeConfig::default();
        let dir = temp_test_dir("rsdb-runtime-signing-key");
        let seed_path = dir.join("receiver.seed");
        fs::write(
            &seed_path,
            "0707070707070707070707070707070707070707070707070707070707070707\n",
        )
        .unwrap();

        config
            .apply_pair("RSDB_SIGNING_KEY_PATH", seed_path.to_str().unwrap())
            .unwrap();
        config
            .apply_pair("RSDB_SUBMIT_URLS", "http://127.0.0.1:8090")
            .unwrap();

        let receiver_identity = config
            .feed_config()
            .unwrap()
            .receiver_identity
            .expect("receiver identity");
        let submission = config.submission_config().unwrap().unwrap();

        assert!(receiver_identity.id.starts_with("ed25519-"));
        assert_eq!(receiver_identity.id.len(), 32);
        assert_eq!(submission.signer.receiver_id(), receiver_identity.id);
        assert_eq!(submission.receiver_identity.id, receiver_identity.id);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runtime_config_loads_signing_and_submission_settings() {
        let mut config = RuntimeConfig::default();
        let dir = temp_test_dir("rsdb-runtime-submission-settings");
        let seed_path = dir.join("receiver.seed");
        fs::write(
            &seed_path,
            "0707070707070707070707070707070707070707070707070707070707070707\n",
        )
        .unwrap();

        config
            .apply_pair("RSDB_SIGNING_KEY_PATH", seed_path.to_str().unwrap())
            .unwrap();
        config
            .apply_pair(
                "RSDB_SUBMIT_URLS",
                "http://127.0.0.1:8090, https://shared.example.com, https://shared.example.com, https://fly.example.com",
            )
            .unwrap();
        config.apply_pair("RSDB_SUBMIT_RETRY_SECONDS", "7").unwrap();
        config
            .apply_pair("RSDB_SUBMIT_MAX_LAG_SECONDS", "11")
            .unwrap();
        config
            .apply_pair("RSDB_SUBMIT_OUTBOX_DIR", "/tmp/rsdb-submit-outbox")
            .unwrap();
        config.apply_pair("RSDB_SUBMIT_OUTBOX_MAX_MB", "3").unwrap();

        let submission = config.submission_config().unwrap().unwrap();

        assert_eq!(
            submission.aggregate_urls,
            vec![
                "http://127.0.0.1:8090".to_owned(),
                "https://shared.example.com".to_owned(),
                "https://fly.example.com".to_owned()
            ]
        );
        assert!(submission.signer.receiver_id().starts_with("ed25519-"));
        assert_eq!(
            submission.signer.receiver_id(),
            submission.receiver_identity.id
        );
        assert_eq!(submission.receiver_identity.name, None);
        assert_eq!(submission.retry_after, Duration::from_secs(7));
        assert_eq!(submission.max_payload_lag, Duration::from_secs(11));
        assert_eq!(
            submission
                .outbox
                .as_ref()
                .map(|outbox| outbox.dir.as_path()),
            Some(Path::new("/tmp/rsdb-submit-outbox"))
        );
        assert_eq!(
            submission.outbox.as_ref().map(|outbox| outbox.max_bytes),
            Some(3 * BYTES_PER_MEGABYTE)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runtime_config_disables_submission_without_urls() {
        let config = RuntimeConfig::default();

        assert!(config.submission_config().unwrap().is_none());
    }

    #[test]
    fn runtime_config_requires_signing_key_for_submission_urls() {
        let mut config = RuntimeConfig::default();

        config
            .apply_pair("RSDB_SUBMIT_URLS", "http://127.0.0.1:8090")
            .unwrap();

        match config.submission_config() {
            Err(error) => assert!(error.contains("RSDB_SIGNING_KEY_PATH is required")),
            Ok(_) => panic!("submission config should require a signing key"),
        }
    }

    #[test]
    fn runtime_config_rejects_zero_submission_outbox_limit() {
        let mut config = RuntimeConfig::default();

        assert!(
            config
                .apply_pair("RSDB_SUBMIT_OUTBOX_MAX_MB", "0")
                .unwrap_err()
                .contains("must be greater than zero")
        );
    }

    #[test]
    fn runtime_config_rejects_zero_submission_max_lag() {
        let mut config = RuntimeConfig::default();

        assert!(
            config
                .apply_pair("RSDB_SUBMIT_MAX_LAG_SECONDS", "0")
                .unwrap_err()
                .contains("must be greater than zero")
        );
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = env::temp_dir().join(format!("{prefix}-{}-{millis}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
