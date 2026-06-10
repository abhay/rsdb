use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::SignedSubmission;

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct SubmissionStatus {
    pub enabled: bool,
    pub urls: Vec<String>,
    #[serde(default)]
    pub targets: Vec<SubmissionTargetStatus>,
    pub received: u64,
    pub signed: u64,
    pub delivered: u64,
    pub failed_attempts: u64,
    pub outbox_enabled: bool,
    pub outbox_queued: u64,
    pub outbox_pending: u64,
    pub outbox_delivered: u64,
    pub outbox_dropped: u64,
    pub last_queued_ms: Option<u64>,
    pub last_delivered_ms: Option<u64>,
    pub last_error: Option<String>,
}

impl SubmissionStatus {
    #[must_use]
    pub fn enabled(urls: Vec<String>, outbox_enabled: bool) -> Self {
        let targets = urls
            .iter()
            .map(|url| SubmissionTargetStatus {
                url: url.clone(),
                ..SubmissionTargetStatus::default()
            })
            .collect();

        Self {
            enabled: true,
            urls,
            targets,
            outbox_enabled,
            ..Self::default()
        }
    }

    pub fn record_delivery(&mut self, url: &str, now_ms: u64) {
        self.delivered = self.delivered.saturating_add(1);
        self.last_delivered_ms = Some(now_ms);
        self.last_error = None;

        let target = self.target_mut(url);
        target.delivered = target.delivered.saturating_add(1);
        target.last_delivered_ms = Some(now_ms);
        target.last_error = None;
    }

    pub fn record_failure(&mut self, url: &str, error: String) {
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        self.last_error = Some(error.clone());

        let target = self.target_mut(url);
        target.failed_attempts = target.failed_attempts.saturating_add(1);
        target.last_error = Some(error);
    }

    pub fn update_outbox_pending(&mut self, entries: &[PendingSubmission]) {
        self.outbox_pending = u64::try_from(entries.len()).unwrap_or(u64::MAX);

        for target in &mut self.targets {
            target.outbox_pending = 0;
        }
        for entry in entries {
            for url in &entry.pending_urls {
                let target = self.target_mut(url);
                target.outbox_pending = target.outbox_pending.saturating_add(1);
            }
        }
    }

    #[must_use]
    pub fn health(&self) -> SubmissionHealth {
        SubmissionHealth {
            enabled: self.enabled,
            delivered: self.delivered,
            outbox_pending: u32::try_from(self.outbox_pending).unwrap_or(u32::MAX),
            has_error: self.last_error.is_some(),
            target_count: u32::try_from(self.targets.len()).unwrap_or(u32::MAX),
            targets_with_error: u32::try_from(
                self.targets
                    .iter()
                    .filter(|target| target.last_error.is_some())
                    .count(),
            )
            .unwrap_or(u32::MAX),
        }
    }

    fn target_mut(&mut self, url: &str) -> &mut SubmissionTargetStatus {
        if let Some(index) = self.targets.iter().position(|target| target.url == url) {
            return &mut self.targets[index];
        }

        self.targets.push(SubmissionTargetStatus {
            url: url.to_owned(),
            ..SubmissionTargetStatus::default()
        });
        self.targets.last_mut().expect("target was just appended")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct SubmissionTargetStatus {
    pub url: String,
    pub delivered: u64,
    pub failed_attempts: u64,
    pub outbox_pending: u64,
    pub last_delivered_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct SubmissionHealth {
    pub enabled: bool,
    pub delivered: u64,
    pub outbox_pending: u32,
    pub has_error: bool,
    #[serde(default)]
    pub target_count: u32,
    #[serde(default)]
    pub targets_with_error: u32,
}

#[derive(Debug, Clone)]
pub struct SubmissionOutboxConfig {
    pub dir: PathBuf,
    pub max_bytes: u64,
}

#[derive(Debug)]
pub struct SubmissionOutbox {
    path: PathBuf,
    max_bytes: u64,
}

#[derive(Debug, Default)]
pub struct OutboxAppend {
    pub pending: u64,
    pub dropped: u64,
}

#[derive(Debug, Default)]
pub struct OutboxLoad {
    pub entries: Vec<PendingSubmission>,
    pub discarded: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PendingSubmission {
    pub submission: SignedSubmission,
    pub pending_urls: Vec<String>,
}

impl PendingSubmission {
    #[must_use]
    pub fn new(submission: SignedSubmission, urls: &[String]) -> Self {
        Self {
            submission,
            pending_urls: urls.to_vec(),
        }
    }
}

#[derive(Debug)]
pub enum SubmissionOutboxError {
    Io {
        path: PathBuf,
        action: &'static str,
        source: io::Error,
    },
    Json {
        path: Option<PathBuf>,
        action: &'static str,
        source: serde_json::Error,
    },
    Replace {
        tmp_path: PathBuf,
        path: PathBuf,
        source: io::Error,
    },
}

impl SubmissionOutboxError {
    fn io(path: &Path, action: &'static str, source: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            action,
            source,
        }
    }

    fn json(path: Option<&Path>, action: &'static str, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.map(Path::to_owned),
            action,
            source,
        }
    }
}

impl fmt::Display for SubmissionOutboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                path,
                action,
                source,
            } => write!(formatter, "{}: {action}: {source}", path.display()),
            Self::Json {
                path: Some(path),
                action,
                source,
            } => write!(formatter, "{}: {action}: {source}", path.display()),
            Self::Json {
                path: None,
                action,
                source,
            } => write!(formatter, "{action}: {source}"),
            Self::Replace {
                tmp_path,
                path,
                source,
            } => write!(
                formatter,
                "{}: replace outbox {} failed: {source}",
                tmp_path.display(),
                path.display()
            ),
        }
    }
}

impl Error for SubmissionOutboxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Replace { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
        }
    }
}

type SubmissionOutboxResult<T> = Result<T, SubmissionOutboxError>;

impl SubmissionOutbox {
    /// Opens a durable NDJSON outbox for signed submissions.
    ///
    /// # Errors
    ///
    /// Returns an error when the outbox directory or file cannot be created.
    pub fn open(config: &SubmissionOutboxConfig) -> SubmissionOutboxResult<Self> {
        fs::create_dir_all(&config.dir).map_err(|error| {
            SubmissionOutboxError::io(&config.dir, "create outbox dir failed", error)
        })?;
        let path = config.dir.join("submission-outbox.ndjson");
        if !path.exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|error| SubmissionOutboxError::io(&path, "create outbox failed", error))?;
        }

        Ok(Self {
            path,
            max_bytes: config.max_bytes,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one signed submission and trims the oldest entries when needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the outbox cannot be written, read, or replaced.
    pub fn append(
        &self,
        submission: &SignedSubmission,
        urls: &[String],
    ) -> SubmissionOutboxResult<OutboxAppend> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| SubmissionOutboxError::io(&self.path, "open outbox failed", error))?;
        let entry = PendingSubmission::new(submission.clone(), urls);
        let encoded = encode_pending_submission(&entry, Some(&self.path))?;
        file.write_all(&encoded).map_err(|error| {
            SubmissionOutboxError::io(&self.path, "append outbox failed", error)
        })?;
        file.write_all(b"\n").map_err(|error| {
            SubmissionOutboxError::io(&self.path, "append outbox failed", error)
        })?;
        file.flush()
            .map_err(|error| SubmissionOutboxError::io(&self.path, "flush outbox failed", error))?;

        self.trim_to_limit()
    }

    /// Loads valid submissions from the outbox.
    ///
    /// # Errors
    ///
    /// Returns an error when the outbox cannot be read.
    pub fn load(&self) -> SubmissionOutboxResult<OutboxLoad> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(OutboxLoad::default()),
            Err(error) => {
                return Err(SubmissionOutboxError::io(
                    &self.path,
                    "open outbox failed",
                    error,
                ));
            }
        };
        let reader = BufReader::new(file);
        let mut load = OutboxLoad::default();

        for (line_index, line) in reader.lines().enumerate() {
            let line = line.map_err(|error| {
                SubmissionOutboxError::io(&self.path, "read outbox failed", error)
            })?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<PendingSubmission>(line) {
                Ok(entry) => match validate_pending_submission(&entry) {
                    Ok(()) => load.entries.push(entry),
                    Err(error) => {
                        load.discarded = load.discarded.saturating_add(1);
                        eprintln!(
                            "{}:{}: discarded invalid outbox entry: {error}",
                            self.path.display(),
                            line_index + 1
                        );
                    }
                },
                Err(error) => {
                    load.discarded = load.discarded.saturating_add(1);
                    eprintln!(
                        "{}:{}: discarded invalid outbox entry: {error}",
                        self.path.display(),
                        line_index + 1
                    );
                }
            }
        }

        Ok(load)
    }

    /// Replaces outbox contents with the supplied submissions.
    ///
    /// # Errors
    ///
    /// Returns an error when the temporary outbox cannot be written or renamed.
    pub fn replace(&self, entries: &[PendingSubmission]) -> SubmissionOutboxResult<()> {
        let tmp_path = self.path.with_extension("ndjson.tmp");
        {
            let mut writer = BufWriter::new(fs::File::create(&tmp_path).map_err(|error| {
                SubmissionOutboxError::io(&tmp_path, "create temp outbox failed", error)
            })?);
            for entry in entries {
                let encoded = encode_pending_submission(entry, Some(&tmp_path))?;
                writer.write_all(&encoded).map_err(|error| {
                    SubmissionOutboxError::io(&tmp_path, "write temp outbox failed", error)
                })?;
                writer.write_all(b"\n").map_err(|error| {
                    SubmissionOutboxError::io(&tmp_path, "write temp outbox failed", error)
                })?;
            }
            writer.flush().map_err(|error| {
                SubmissionOutboxError::io(&tmp_path, "flush temp outbox failed", error)
            })?;
        }
        fs::rename(&tmp_path, &self.path).map_err(|error| SubmissionOutboxError::Replace {
            tmp_path,
            path: self.path.clone(),
            source: error,
        })
    }

    fn trim_to_limit(&self) -> SubmissionOutboxResult<OutboxAppend> {
        let load = self.load()?;
        let mut bytes = outbox_bytes(&load.entries)?;
        let mut first_retained = 0;

        while bytes > self.max_bytes && first_retained < load.entries.len() {
            bytes = bytes.saturating_sub(submission_line_bytes(&load.entries[first_retained])?);
            first_retained += 1;
        }

        let dropped = load
            .discarded
            .saturating_add(u64::try_from(first_retained).unwrap_or(u64::MAX));
        if dropped != 0 {
            self.replace(&load.entries[first_retained..])?;
        }

        Ok(OutboxAppend {
            pending: u64::try_from(load.entries.len().saturating_sub(first_retained))
                .unwrap_or(u64::MAX),
            dropped,
        })
    }
}

fn outbox_bytes(entries: &[PendingSubmission]) -> SubmissionOutboxResult<u64> {
    entries.iter().try_fold(0_u64, |total, entry| {
        submission_line_bytes(entry).map(|bytes| total.saturating_add(bytes))
    })
}

fn submission_line_bytes(entry: &PendingSubmission) -> SubmissionOutboxResult<u64> {
    let bytes = encode_pending_submission(entry, None)?.len();
    Ok(u64::try_from(bytes).unwrap_or(u64::MAX).saturating_add(1))
}

fn encode_pending_submission(
    entry: &PendingSubmission,
    path: Option<&Path>,
) -> SubmissionOutboxResult<Vec<u8>> {
    validate_pending_submission(entry).map_err(|error| {
        SubmissionOutboxError::json(path, "validate outbox submission failed", error)
    })?;
    let encoded = serde_json::to_vec(entry).map_err(|error| {
        SubmissionOutboxError::json(path, "encode outbox submission failed", error)
    })?;
    let decoded = serde_json::from_slice::<PendingSubmission>(&encoded).map_err(|error| {
        SubmissionOutboxError::json(path, "decode encoded outbox submission failed", error)
    })?;
    validate_pending_submission(&decoded).map_err(|error| {
        SubmissionOutboxError::json(path, "validate encoded outbox submission failed", error)
    })?;
    Ok(encoded)
}

fn validate_pending_submission(entry: &PendingSubmission) -> Result<(), serde_json::Error> {
    entry
        .submission
        .validate_envelope()
        .map_err(serde::ser::Error::custom)
}
