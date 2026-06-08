use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"rsdb receiver handle v1\0";
const SUFFIX_BYTES: usize = 4;

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct ReceiverHandle {
    pub base: String,
    pub suffix: String,
}

impl ReceiverHandle {
    #[must_use]
    pub fn from_receiver_id(receiver_id: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(DOMAIN);
        hasher.update(receiver_id.as_bytes());
        let digest = hasher.finalize();
        Self::from_digest(&digest)
    }

    #[must_use]
    pub fn display_base(&self) -> &str {
        &self.base
    }

    #[must_use]
    pub fn display_with_suffix(&self) -> String {
        format!("{}-{}", self.base, self.suffix)
    }

    fn from_digest(digest: &[u8]) -> Self {
        let base = format!(
            "{}-{}-{}",
            TONES[word_index(digest, 0, TONES.len())],
            COLORS[word_index(digest, 2, COLORS.len())],
            SIGNALS[word_index(digest, 6, SIGNALS.len())],
        );
        let suffix = hex_suffix(&digest[8..8 + SUFFIX_BYTES]);

        Self { base, suffix }
    }
}

fn word_index(digest: &[u8], offset: usize, len: usize) -> usize {
    let value = u16::from_be_bytes([digest[offset], digest[offset + 1]]);
    usize::from(value) % len
}

fn hex_suffix(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }

    encoded
}

const TONES: &[&str] = &[
    "able", "alert", "ample", "brave", "brisk", "calm", "candid", "careful", "clever", "crisp",
    "dapper", "direct", "eager", "early", "even", "fair", "fleet", "fresh", "gentle", "glad",
    "graceful", "grand", "honest", "humble", "keen", "kind", "lively", "lucid", "merry", "mild",
    "modern", "neat", "noble", "open", "polished", "prompt", "quiet", "rapid", "ready", "resolute",
    "robust", "sharp", "simple", "smart", "smooth", "steady", "swift", "tidy", "true", "upbeat",
    "vivid", "warm", "wise", "zesty", "bright", "clear", "cozy", "firm", "light", "loyal", "prime",
    "sound", "stable", "witty",
];

const COLORS: &[&str] = &[
    "amber",
    "aqua",
    "ash",
    "azure",
    "basalt",
    "brass",
    "bronze",
    "cedar",
    "cerulean",
    "chalk",
    "cinder",
    "cobalt",
    "copper",
    "coral",
    "crimson",
    "cyan",
    "denim",
    "emerald",
    "flint",
    "gold",
    "granite",
    "graphite",
    "green",
    "indigo",
    "ivory",
    "jade",
    "linen",
    "magenta",
    "marble",
    "mint",
    "navy",
    "nickel",
    "ochre",
    "olive",
    "onyx",
    "opal",
    "pearl",
    "pine",
    "platinum",
    "quartz",
    "red",
    "rose",
    "ruby",
    "saffron",
    "sage",
    "silver",
    "slate",
    "steel",
    "stone",
    "teal",
    "tin",
    "topaz",
    "umber",
    "violet",
    "white",
    "yellow",
    "zinc",
    "blue",
    "brown",
    "charcoal",
    "glass",
    "iron",
    "lime",
    "vermilion",
];

const SIGNALS: &[&str] = &[
    "array", "azimuth", "beam", "beacon", "bearing", "blip", "carrier", "channel", "chirp", "code",
    "contact", "course", "downlink", "echo", "fix", "frame", "gain", "grid", "heading", "link",
    "marker", "message", "node", "packet", "ping", "pulse", "radial", "radio", "range", "relay",
    "route", "scope", "sector", "signal", "slot", "source", "spark", "squawk", "stream", "sweep",
    "sync", "trace", "track", "uplink", "vector", "waypoint", "window", "wire", "zone", "antenna",
    "band", "burst", "clock", "filter", "hub", "plot", "port", "reader", "sample", "tower",
    "tuner", "watch", "wave", "whisper",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_handle_is_deterministic() {
        let first = ReceiverHandle::from_receiver_id("rsdb-pi");
        let second = ReceiverHandle::from_receiver_id("rsdb-pi");

        assert_eq!(first, second);
        assert_eq!(first.base, "glad-umber-range");
        assert_eq!(first.suffix, "73d3b1bc");
    }

    #[test]
    fn receiver_handle_has_hidden_suffix() {
        let handle = ReceiverHandle::from_receiver_id("rsdb-pi");

        assert_eq!(handle.suffix.len(), 8);
        assert!(!handle.display_base().contains(&handle.suffix));
        assert_eq!(
            handle.display_with_suffix(),
            format!("{}-{}", handle.base, handle.suffix)
        );
    }

    #[test]
    fn receiver_handle_uses_three_words() {
        let handle = ReceiverHandle::from_receiver_id("rsdb-pi");

        assert_eq!(handle.base.split('-').count(), 3);
        assert!(
            handle
                .base
                .bytes()
                .all(|byte| { byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' })
        );
        assert!(handle.suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
