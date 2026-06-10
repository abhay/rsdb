use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    FEED_SCHEMA_VERSION, FRAME_RECORD_BATCH_SCHEMA_VERSION, FeedMessage, FrameRecordBatch,
    ReceiverIdentity,
};

pub const SIGNED_SUBMISSION_SCHEMA_VERSION: u32 = 1;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;
const ED25519_SECRET_KEY_BYTES: usize = 32;
const RECEIVER_ID_DOMAIN: &[u8] = b"rsdb.receiver-id.ed25519.v1";
const SUBMISSION_ID_DOMAIN: &[u8] = b"rsdb.submission-id.v1";

#[derive(Clone)]
pub struct SubmissionSigner {
    receiver_id: String,
    signing_key: SigningKey,
}

impl SubmissionSigner {
    /// Builds an Ed25519 submission signer from a 32-byte hex seed.
    ///
    /// # Errors
    ///
    /// Returns an error if `secret_key_hex` is not exactly 32 bytes of hex.
    pub fn from_ed25519_secret_hex(secret_key_hex: &str) -> Result<Self, SubmissionSigningError> {
        let secret_key = decode_secret_key_hex(secret_key_hex)?;
        let signing_key = SigningKey::from_bytes(&secret_key);
        let receiver_id =
            receiver_id_from_ed25519_public_key_bytes(&signing_key.verifying_key().to_bytes());
        Ok(Self {
            receiver_id,
            signing_key,
        })
    }

    #[must_use]
    pub fn receiver_id(&self) -> &str {
        &self.receiver_id
    }

    #[must_use]
    pub fn public_key_hex(&self) -> String {
        encode_hex(&self.signing_key.verifying_key().to_bytes())
    }

    /// Signs a receiver-attributed feed message as a submission envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload is missing receiver identity, has a
    /// different receiver ID than this signer, or cannot be serialized for
    /// signing.
    pub fn sign(
        &self,
        payload: FeedMessage,
        submitted_at_ms: u64,
    ) -> Result<SignedSubmission, SubmissionSigningError> {
        self.sign_payload(payload, submitted_at_ms)
    }

    /// Signs a receiver-attributed frame-record batch as a submission envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload is missing receiver identity, has a
    /// different receiver ID than this signer, or cannot be serialized for
    /// signing.
    pub fn sign_frame_records(
        &self,
        payload: FrameRecordBatch,
        submitted_at_ms: u64,
    ) -> Result<SignedSubmission, SubmissionSigningError> {
        self.sign_payload(payload, submitted_at_ms)
    }

    /// Signs a receiver-attributed submission payload.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload is missing receiver identity, has a
    /// different receiver ID than this signer, or cannot be serialized for
    /// signing.
    pub fn sign_payload(
        &self,
        payload: impl Into<SubmissionPayload>,
        submitted_at_ms: u64,
    ) -> Result<SignedSubmission, SubmissionSigningError> {
        let payload = payload.into();
        let payload_receiver = payload
            .receiver()
            .ok_or(SubmissionSigningError::MissingPayloadReceiver)?;
        if payload_receiver.id != self.receiver_id {
            return Err(SubmissionSigningError::ReceiverMismatch {
                signer_receiver_id: self.receiver_id.clone(),
                payload_receiver_id: payload_receiver.id.clone(),
            });
        }

        let mut submission = SignedSubmission::new_ed25519(
            self.receiver_id.clone(),
            submitted_at_ms,
            payload,
            String::new(),
        );
        let signature = self.signing_key.sign(
            &submission
                .signing_bytes()
                .map_err(|error| SubmissionSigningError::Serialization(error.to_string()))?,
        );
        submission.signature = encode_hex(&signature.to_bytes());

        Ok(submission)
    }
}

/// Derives the default receiver ID from an Ed25519 signing seed.
///
/// # Errors
///
/// Returns an error if `secret_key_hex` is not exactly 32 bytes of hex.
pub fn receiver_id_from_ed25519_secret_hex(
    secret_key_hex: &str,
) -> Result<String, SubmissionSigningError> {
    let secret_key = decode_secret_key_hex(secret_key_hex)?;
    let signing_key = SigningKey::from_bytes(&secret_key);

    Ok(receiver_id_from_ed25519_public_key_bytes(
        &signing_key.verifying_key().to_bytes(),
    ))
}

/// Derives the default receiver ID from an Ed25519 public key.
///
/// # Errors
///
/// Returns an error if `public_key_hex` is not exactly 32 bytes of hex.
pub fn receiver_id_from_ed25519_public_key_hex(
    public_key_hex: &str,
) -> Result<String, SubmissionVerificationError> {
    let public_key = decode_fixed_hex::<ED25519_PUBLIC_KEY_BYTES>(public_key_hex, "public_key")?;

    Ok(receiver_id_from_ed25519_public_key_bytes(&public_key))
}

#[must_use]
fn receiver_id_from_ed25519_public_key_bytes(public_key: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(RECEIVER_ID_DOMAIN);
    hasher.update([0]);
    hasher.update(public_key);
    let digest = hasher.finalize();
    let digest_hex = encode_hex(&digest);

    format!("ed25519-{}", &digest_hex[..24])
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SignedSubmission {
    pub schema_version: u32,
    pub submission_id: String,
    pub receiver_id: String,
    pub algorithm: SignatureAlgorithm,
    pub submitted_at_ms: u64,
    pub payload: SubmissionPayload,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum SubmissionPayload {
    FeedMessage(FeedMessage),
    FrameRecords(FrameRecordBatch),
}

impl SubmissionPayload {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::FeedMessage(message) => match message {
                FeedMessage::Snapshot { .. } => "snapshot",
                FeedMessage::Aircraft { .. } => "aircraft",
                FeedMessage::StaleAircraft { .. } => "stale_aircraft",
                FeedMessage::Heartbeat { .. } => "heartbeat",
            },
            Self::FrameRecords(_) => "frame_records",
        }
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        match self {
            Self::FeedMessage(message) => message.schema_version(),
            Self::FrameRecords(batch) => batch.schema_version(),
        }
    }

    #[must_use]
    pub const fn expected_schema_version(&self) -> u32 {
        match self {
            Self::FeedMessage(_) => FEED_SCHEMA_VERSION,
            Self::FrameRecords(_) => FRAME_RECORD_BATCH_SCHEMA_VERSION,
        }
    }

    #[must_use]
    pub fn is_supported_schema_version(&self) -> bool {
        match self {
            Self::FeedMessage(message) => message.is_supported_schema_version(),
            Self::FrameRecords(batch) => batch.is_supported_schema_version(),
        }
    }

    #[must_use]
    pub fn receiver(&self) -> Option<&ReceiverIdentity> {
        match self {
            Self::FeedMessage(message) => message.receiver(),
            Self::FrameRecords(batch) => Some(&batch.receiver),
        }
    }

    /// Validates the payload body after envelope-level schema checks.
    ///
    /// # Errors
    ///
    /// Returns an error when a frame-record batch has invalid schema, receiver,
    /// protocol, frame metadata, timing, signal, or sequence fields.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::FeedMessage(_) => Ok(()),
            Self::FrameRecords(batch) => batch.validate().map_err(|error| error.to_string()),
        }
    }
}

impl From<FeedMessage> for SubmissionPayload {
    fn from(message: FeedMessage) -> Self {
        Self::FeedMessage(message)
    }
}

impl From<FrameRecordBatch> for SubmissionPayload {
    fn from(batch: FrameRecordBatch) -> Self {
        Self::FrameRecords(batch)
    }
}

impl SignedSubmission {
    /// Builds an Ed25519 signed-submission envelope.
    ///
    /// # Panics
    ///
    /// Panics if the submission payload cannot be serialized while deriving the
    /// deterministic submission ID.
    #[must_use]
    pub fn new_ed25519(
        receiver_id: String,
        submitted_at_ms: u64,
        payload: impl Into<SubmissionPayload>,
        signature: String,
    ) -> Self {
        let algorithm = SignatureAlgorithm::Ed25519;
        let payload = payload.into();
        let submission_id = submission_id_for(
            SIGNED_SUBMISSION_SCHEMA_VERSION,
            &receiver_id,
            algorithm,
            submitted_at_ms,
            &payload,
        )
        .expect("submission payload serialization cannot fail");

        Self {
            schema_version: SIGNED_SUBMISSION_SCHEMA_VERSION,
            submission_id,
            receiver_id,
            algorithm,
            submitted_at_ms,
            payload,
            signature,
        }
    }

    /// Returns the canonical JSON bytes covered by `signature`.
    ///
    /// # Errors
    ///
    /// Returns an error if the signing payload cannot be serialized.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, SubmissionVerificationError> {
        serde_json::to_vec(&self.signing_payload())
            .map_err(|error| SubmissionVerificationError::Serialization(error.to_string()))
    }

    fn signing_payload(&self) -> SubmissionSigningPayload<'_> {
        SubmissionSigningPayload {
            schema_version: self.schema_version,
            submission_id: &self.submission_id,
            receiver_id: &self.receiver_id,
            algorithm: self.algorithm,
            submitted_at_ms: self.submitted_at_ms,
            payload: &self.payload,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    Ed25519,
}

impl fmt::Display for SignatureAlgorithm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ed25519 => formatter.write_str("ed25519"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ReceiverAllowlist {
    pub public_keys: Vec<String>,
}

impl ReceiverAllowlist {
    #[must_use]
    pub const fn new(public_keys: Vec<String>) -> Self {
        Self { public_keys }
    }

    /// Verifies that a submission is attributed to an allowlisted public key.
    ///
    /// # Errors
    ///
    /// Returns an error when schema versions are unsupported, receiver identity
    /// does not match, the public key is not allowlisted, or the Ed25519
    /// signature does not verify.
    pub fn verify_submission(
        &self,
        submission: &SignedSubmission,
    ) -> Result<(), SubmissionVerificationError> {
        if submission.schema_version != SIGNED_SUBMISSION_SCHEMA_VERSION {
            return Err(SubmissionVerificationError::UnsupportedSchemaVersion {
                actual: submission.schema_version,
                expected: SIGNED_SUBMISSION_SCHEMA_VERSION,
            });
        }
        if !submission.payload.is_supported_schema_version() {
            return Err(
                SubmissionVerificationError::UnsupportedPayloadSchemaVersion {
                    actual: submission.payload.schema_version(),
                    expected: submission.payload.expected_schema_version(),
                },
            );
        }
        submission
            .payload
            .validate()
            .map_err(SubmissionVerificationError::InvalidPayload)?;

        let payload_receiver = submission
            .payload
            .receiver()
            .ok_or(SubmissionVerificationError::MissingPayloadReceiver)?;
        if payload_receiver.id != submission.receiver_id {
            return Err(SubmissionVerificationError::ReceiverMismatch {
                envelope_receiver_id: submission.receiver_id.clone(),
                payload_receiver_id: payload_receiver.id.clone(),
            });
        }

        let public_key = self.allowed_ed25519_public_key(&submission.receiver_id)?;
        verify_ed25519(submission, &public_key)?;
        let expected_submission_id = submission
            .expected_submission_id()
            .map_err(|error| SubmissionVerificationError::Serialization(error.to_string()))?;
        if submission.submission_id != expected_submission_id {
            return Err(SubmissionVerificationError::SubmissionIdMismatch {
                submission_id: submission.submission_id.clone(),
                expected_submission_id,
            });
        }

        Ok(())
    }

    fn allowed_ed25519_public_key(
        &self,
        receiver_id: &str,
    ) -> Result<[u8; ED25519_PUBLIC_KEY_BYTES], SubmissionVerificationError> {
        for public_key in &self.public_keys {
            let public_key =
                decode_fixed_hex::<ED25519_PUBLIC_KEY_BYTES>(public_key, "public_key")?;
            if receiver_id_from_ed25519_public_key_bytes(&public_key) == receiver_id {
                return Ok(public_key);
            }
        }

        Err(SubmissionVerificationError::PublicKeyNotAllowed {
            receiver_id: receiver_id.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionVerificationError {
    UnsupportedSchemaVersion {
        actual: u32,
        expected: u32,
    },
    UnsupportedPayloadSchemaVersion {
        actual: u32,
        expected: u32,
    },
    MissingPayloadReceiver,
    ReceiverMismatch {
        envelope_receiver_id: String,
        payload_receiver_id: String,
    },
    InvalidPayload(String),
    SubmissionIdMismatch {
        submission_id: String,
        expected_submission_id: String,
    },
    PublicKeyNotAllowed {
        receiver_id: String,
    },
    MalformedHex {
        field: &'static str,
        expected_bytes: usize,
    },
    InvalidPublicKey {
        receiver_id: String,
    },
    SignatureMismatch {
        receiver_id: String,
    },
    Serialization(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionSigningError {
    MalformedSecretKeyHex,
    MissingPayloadReceiver,
    ReceiverMismatch {
        signer_receiver_id: String,
        payload_receiver_id: String,
    },
    Serialization(String),
}

impl fmt::Display for SubmissionSigningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedSecretKeyHex => formatter
                .write_str("secret key must be lowercase or uppercase hex for exactly 32 bytes"),
            Self::MissingPayloadReceiver => {
                formatter.write_str("payload is missing receiver identity")
            }
            Self::ReceiverMismatch {
                signer_receiver_id,
                payload_receiver_id,
            } => write!(
                formatter,
                "payload receiver_id {payload_receiver_id} does not match signer receiver_id {signer_receiver_id}"
            ),
            Self::Serialization(error) => {
                write!(formatter, "failed to serialize signing payload: {error}")
            }
        }
    }
}

impl std::error::Error for SubmissionSigningError {}

impl fmt::Display for SubmissionVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { actual, expected } => write!(
                formatter,
                "unsupported signed submission schema_version {actual}; expected {expected}"
            ),
            Self::UnsupportedPayloadSchemaVersion { actual, expected } => write!(
                formatter,
                "unsupported payload schema_version {actual}; expected {expected}"
            ),
            Self::MissingPayloadReceiver => {
                formatter.write_str("payload is missing receiver identity")
            }
            Self::ReceiverMismatch {
                envelope_receiver_id,
                payload_receiver_id,
            } => write!(
                formatter,
                "payload receiver_id {payload_receiver_id} does not match envelope receiver_id {envelope_receiver_id}"
            ),
            Self::InvalidPayload(error) => {
                write!(formatter, "invalid submission payload: {error}")
            }
            Self::SubmissionIdMismatch {
                submission_id,
                expected_submission_id,
            } => write!(
                formatter,
                "submission_id {submission_id} does not match expected submission_id {expected_submission_id}"
            ),
            Self::PublicKeyNotAllowed { receiver_id } => write!(
                formatter,
                "receiver_id {receiver_id} does not match an allowlisted public key"
            ),
            Self::MalformedHex {
                field,
                expected_bytes,
            } => write!(
                formatter,
                "{field} must be lowercase or uppercase hex for exactly {expected_bytes} bytes"
            ),
            Self::InvalidPublicKey { receiver_id } => {
                write!(
                    formatter,
                    "public key for receiver_id {receiver_id} is invalid"
                )
            }
            Self::SignatureMismatch { receiver_id } => {
                write!(
                    formatter,
                    "signature did not verify for receiver_id {receiver_id}"
                )
            }
            Self::Serialization(error) => {
                write!(formatter, "failed to serialize signing payload: {error}")
            }
        }
    }
}

impl std::error::Error for SubmissionVerificationError {}

#[derive(Serialize)]
struct SubmissionSigningPayload<'a> {
    schema_version: u32,
    submission_id: &'a str,
    receiver_id: &'a str,
    algorithm: SignatureAlgorithm,
    submitted_at_ms: u64,
    payload: &'a SubmissionPayload,
}

#[derive(Serialize)]
struct SubmissionIdPayload<'a> {
    schema_version: u32,
    receiver_id: &'a str,
    algorithm: SignatureAlgorithm,
    submitted_at_ms: u64,
    payload: &'a SubmissionPayload,
}

impl SignedSubmission {
    fn expected_submission_id(&self) -> Result<String, serde_json::Error> {
        submission_id_for(
            self.schema_version,
            &self.receiver_id,
            self.algorithm,
            self.submitted_at_ms,
            &self.payload,
        )
    }
}

fn submission_id_for(
    schema_version: u32,
    receiver_id: &str,
    algorithm: SignatureAlgorithm,
    submitted_at_ms: u64,
    payload: &SubmissionPayload,
) -> Result<String, serde_json::Error> {
    let encoded = serde_json::to_vec(&SubmissionIdPayload {
        schema_version,
        receiver_id,
        algorithm,
        submitted_at_ms,
        payload,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(SUBMISSION_ID_DOMAIN);
    hasher.update([0]);
    hasher.update(encoded);
    let digest = hasher.finalize();

    Ok(encode_hex(&digest))
}

fn verify_ed25519(
    submission: &SignedSubmission,
    public_key: &[u8; ED25519_PUBLIC_KEY_BYTES],
) -> Result<(), SubmissionVerificationError> {
    let verifying_key = VerifyingKey::from_bytes(public_key).map_err(|_| {
        SubmissionVerificationError::InvalidPublicKey {
            receiver_id: submission.receiver_id.clone(),
        }
    })?;
    let signature = Signature::from_bytes(&decode_fixed_hex::<ED25519_SIGNATURE_BYTES>(
        &submission.signature,
        "signature",
    )?);
    let signing_bytes = submission.signing_bytes()?;

    verifying_key
        .verify(&signing_bytes, &signature)
        .map_err(|_| SubmissionVerificationError::SignatureMismatch {
            receiver_id: submission.receiver_id.clone(),
        })
}

fn decode_fixed_hex<const N: usize>(
    value: &str,
    field: &'static str,
) -> Result<[u8; N], SubmissionVerificationError> {
    if value.len() != N * 2 {
        return Err(SubmissionVerificationError::MalformedHex {
            field,
            expected_bytes: N,
        });
    }

    let mut output = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or(SubmissionVerificationError::MalformedHex {
            field,
            expected_bytes: N,
        })?;
        let low = hex_nibble(pair[1]).ok_or(SubmissionVerificationError::MalformedHex {
            field,
            expected_bytes: N,
        })?;
        output[index] = (high << 4) | low;
    }

    Ok(output)
}

fn decode_secret_key_hex(
    value: &str,
) -> Result<[u8; ED25519_SECRET_KEY_BYTES], SubmissionSigningError> {
    let value = value.trim();
    if value.len() != ED25519_SECRET_KEY_BYTES * 2 {
        return Err(SubmissionSigningError::MalformedSecretKeyHex);
    }

    let mut output = [0_u8; ED25519_SECRET_KEY_BYTES];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or(SubmissionSigningError::MalformedSecretKeyHex)?;
        let low = hex_nibble(pair[1]).ok_or(SubmissionSigningError::MalformedSecretKeyHex)?;
        output[index] = (high << 4) | low;
    }

    Ok(output)
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

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;
    use crate::{FeedMessage, Frame, FrameRecord, FrameRecordBatch, Protocol, ReceiverIdentity};

    #[test]
    fn verifies_signed_submission() {
        let signing_key = sample_signing_key();
        let submission = signed_submission(&signing_key);
        let allowlist = allowlist_for(&signing_key);

        assert_eq!(allowlist.verify_submission(&submission), Ok(()));
    }

    #[test]
    fn submission_signer_signs_allowlisted_payload() {
        let signer = SubmissionSigner::from_ed25519_secret_hex(
            "0707070707070707070707070707070707070707070707070707070707070707",
        )
        .unwrap();
        let payload = FeedMessage::snapshot(42, Vec::new())
            .with_receiver(Some(ReceiverIdentity::new(signer.receiver_id().to_owned())));
        let submission = signer.sign(payload, 1_717_000_000_000).unwrap();
        let allowlist = ReceiverAllowlist::new(vec![signer.public_key_hex()]);

        assert_eq!(submission.receiver_id, signer.receiver_id());
        assert_eq!(submission.submission_id.len(), 64);
        assert_eq!(allowlist.verify_submission(&submission), Ok(()));
    }

    #[test]
    fn submission_signer_signs_frame_record_batch() {
        let signer = SubmissionSigner::from_ed25519_secret_hex(
            "0707070707070707070707070707070707070707070707070707070707070707",
        )
        .unwrap();
        let receiver = ReceiverIdentity::new(signer.receiver_id().to_owned());
        let frame = Frame::from_hex("8DA062EF9910B19A38040ACE2B14").unwrap();
        let record = FrameRecord::new(42, 0, &frame).with_receiver(Some(receiver.clone()));
        let batch = FrameRecordBatch::new(Protocol::Adsb1090, receiver, vec![record]);

        let submission = signer.sign_frame_records(batch, 1_717_000_000_000).unwrap();
        let allowlist = ReceiverAllowlist::new(vec![signer.public_key_hex()]);

        assert_eq!(submission.payload.kind(), "frame_records");
        assert_eq!(allowlist.verify_submission(&submission), Ok(()));
    }

    #[test]
    fn derives_receiver_id_from_secret_key() {
        let first = receiver_id_from_ed25519_secret_hex(
            "0707070707070707070707070707070707070707070707070707070707070707",
        )
        .unwrap();
        let second = receiver_id_from_ed25519_secret_hex(
            "0707070707070707070707070707070707070707070707070707070707070707",
        )
        .unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("ed25519-"));
        assert_eq!(first.len(), 32);
        assert_eq!(
            receiver_id_from_ed25519_secret_hex("bad"),
            Err(SubmissionSigningError::MalformedSecretKeyHex)
        );
    }

    #[test]
    fn submission_id_is_deterministic_for_same_envelope() {
        let first = unsigned_submission();
        let second = unsigned_submission();

        assert_eq!(first.submission_id, second.submission_id);
    }

    #[test]
    fn submission_signer_rejects_receiver_mismatch() {
        let signer = SubmissionSigner::from_ed25519_secret_hex(
            "0707070707070707070707070707070707070707070707070707070707070707",
        )
        .unwrap();
        let payload = FeedMessage::snapshot(42, Vec::new())
            .with_receiver(Some(ReceiverIdentity::new("other-rsdb-pi".to_owned())));

        assert_eq!(
            signer.sign(payload, 1_717_000_000_000),
            Err(SubmissionSigningError::ReceiverMismatch {
                signer_receiver_id: signer.receiver_id().to_owned(),
                payload_receiver_id: "other-rsdb-pi".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_tampered_payload() {
        let signing_key = sample_signing_key();
        let mut submission = signed_submission(&signing_key);
        let allowlist = allowlist_for(&signing_key);

        if let SubmissionPayload::FeedMessage(FeedMessage::Snapshot { now_ms, .. }) =
            &mut submission.payload
        {
            *now_ms += 1;
        }

        assert_eq!(
            allowlist.verify_submission(&submission),
            Err(SubmissionVerificationError::SignatureMismatch {
                receiver_id: sample_receiver_id(),
            })
        );
    }

    #[test]
    fn rejects_tampered_submission_id() {
        let signing_key = sample_signing_key();
        let mut submission = signed_submission(&signing_key);
        let allowlist = allowlist_for(&signing_key);

        submission.submission_id = "0".repeat(64);

        assert_eq!(
            allowlist.verify_submission(&submission),
            Err(SubmissionVerificationError::SignatureMismatch {
                receiver_id: sample_receiver_id(),
            })
        );
    }

    #[test]
    fn rejects_resigned_submission_id_mismatch() {
        let signing_key = sample_signing_key();
        let mut submission = signed_submission(&signing_key);
        submission.submission_id = "0".repeat(64);
        submission.signature = signature_hex(&signing_key, &submission);
        let allowlist = allowlist_for(&signing_key);

        assert_eq!(
            allowlist.verify_submission(&submission),
            Err(SubmissionVerificationError::SubmissionIdMismatch {
                submission_id: "0".repeat(64),
                expected_submission_id: unsigned_submission().submission_id,
            })
        );
    }

    #[test]
    fn rejects_missing_payload_receiver() {
        let signing_key = sample_signing_key();
        let mut submission = signed_submission(&signing_key);
        submission.payload = FeedMessage::snapshot(42, Vec::new()).into();
        submission.signature = signature_hex(&signing_key, &submission);
        let allowlist = allowlist_for(&signing_key);

        assert_eq!(
            allowlist.verify_submission(&submission),
            Err(SubmissionVerificationError::MissingPayloadReceiver)
        );
    }

    #[test]
    fn rejects_unallowlisted_key() {
        let signing_key = sample_signing_key();
        let submission = signed_submission(&signing_key);
        let other_key = SigningKey::from_bytes(&[8; 32]);
        let allowlist = ReceiverAllowlist::new(vec![public_key_hex(&other_key)]);

        assert_eq!(
            allowlist.verify_submission(&submission),
            Err(SubmissionVerificationError::PublicKeyNotAllowed {
                receiver_id: sample_receiver_id(),
            })
        );
    }

    #[test]
    fn signing_bytes_exclude_signature() {
        let mut submission = unsigned_submission();
        let unsigned = submission.signing_bytes().unwrap();

        submission.signature = "abc123".to_owned();

        assert_eq!(submission.signing_bytes().unwrap(), unsigned);
    }

    #[test]
    fn signing_bytes_include_submission_id() {
        let mut submission = unsigned_submission();
        let original = submission.signing_bytes().unwrap();

        submission.submission_id = "0".repeat(64);

        assert_ne!(submission.signing_bytes().unwrap(), original);
    }

    #[test]
    fn parses_allowlist_json() {
        let json = r#"{
            "public_keys": [
                "0000000000000000000000000000000000000000000000000000000000000000"
            ]
        }"#;

        let allowlist = serde_json::from_str::<ReceiverAllowlist>(json).unwrap();

        assert_eq!(
            allowlist.public_keys,
            vec!["0000000000000000000000000000000000000000000000000000000000000000"]
        );
    }

    #[test]
    fn derives_receiver_id_from_public_key() {
        let signing_key = sample_signing_key();
        let public_key = public_key_hex(&signing_key);

        assert_eq!(
            receiver_id_from_ed25519_public_key_hex(&public_key).unwrap(),
            sample_receiver_id()
        );
    }

    fn sample_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn allowlist_for(signing_key: &SigningKey) -> ReceiverAllowlist {
        ReceiverAllowlist::new(vec![public_key_hex(signing_key)])
    }

    fn signed_submission(signing_key: &SigningKey) -> SignedSubmission {
        let mut submission = unsigned_submission_for(signing_key);
        submission.signature = signature_hex(signing_key, &submission);
        submission
    }

    fn unsigned_submission() -> SignedSubmission {
        unsigned_submission_for(&sample_signing_key())
    }

    fn unsigned_submission_for(signing_key: &SigningKey) -> SignedSubmission {
        SignedSubmission::new_ed25519(
            receiver_id_for(signing_key),
            1_717_000_000_000,
            FeedMessage::snapshot(42, Vec::new()).with_receiver(Some(receiver_for(signing_key))),
            String::new(),
        )
    }

    fn sample_receiver_id() -> String {
        receiver_id_for(&sample_signing_key())
    }

    fn receiver_id_for(signing_key: &SigningKey) -> String {
        receiver_id_from_ed25519_public_key_bytes(&signing_key.verifying_key().to_bytes())
    }

    fn receiver_for(signing_key: &SigningKey) -> ReceiverIdentity {
        ReceiverIdentity::new(receiver_id_for(signing_key))
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
}
