use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_CONFIG_PATH: &str = "/etc/rsdb/rsdb.env";
const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8090;
const DEFAULT_AGGREGATE_RETENTION_HOURS: u64 = 72;
const DEFAULT_AGGREGATE_HOT_MAX_MB: u64 = 250;
const BYTES_PER_MEGABYTE: u64 = 1_000_000;
const MILLIS_PER_HOUR: u64 = 60 * 60 * 1_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let config_path = take_config_path(&mut args)?;
    let mut config = AggregateRuntimeConfig::load(config_path)?;

    if args.first().is_some_and(|arg| arg == "serve") {
        args.remove(0);
    }
    if args
        .first()
        .is_some_and(|arg| arg == "-h" || arg == "--help" || arg == "help")
    {
        print_usage();
        return Ok(());
    }

    if let Some(path) = args.first() {
        config.allowlist = Some(path.clone());
    }
    if let Some(host) = args.get(1) {
        parse_non_empty(host, "aggregate host")?.clone_into(&mut config.host);
    }
    if let Some(port) = args.get(2) {
        config.port = parse_port(port, "aggregate port")?;
    }
    if args.len() > 3 {
        print_usage();
        return Err("too many arguments".to_owned());
    }

    let allowlist = config
        .allowlist
        .as_deref()
        .ok_or_else(|| "RSDB_ALLOWLIST or allowlist path argument is required".to_owned())?;

    let bind = config.bind();

    rsdb_aggregate::serve(rsdb_aggregate::ServeConfig {
        allowlist,
        bind: &bind,
        data_dir: config.data_dir.as_deref(),
        retention_ms: config.retention_ms,
        max_bytes: config.max_bytes,
    })
}

#[derive(Debug, Clone)]
struct AggregateRuntimeConfig {
    host: String,
    port: u16,
    allowlist: Option<String>,
    data_dir: Option<PathBuf>,
    retention_ms: u64,
    max_bytes: u64,
}

impl Default for AggregateRuntimeConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: default_port(),
            allowlist: None,
            data_dir: None,
            retention_ms: DEFAULT_AGGREGATE_RETENTION_HOURS * MILLIS_PER_HOUR,
            max_bytes: DEFAULT_AGGREGATE_HOT_MAX_MB * BYTES_PER_MEGABYTE,
        }
    }
}

impl AggregateRuntimeConfig {
    fn load(explicit_path: Option<PathBuf>) -> Result<Self, String> {
        let mut config = Self::default();

        if let Some(path) = config_file_path(explicit_path) {
            config.apply_file(&path)?;
        }
        config.apply_environment()?;

        Ok(config)
    }

    fn apply_file(&mut self, path: &Path) -> Result<(), String> {
        let contents = std::fs::read_to_string(path)
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
            "RSDB_AGGREGATE_HOST" => {
                parse_non_empty(value, key)?.clone_into(&mut self.host);
            }
            "RSDB_AGGREGATE_PORT" => self.port = parse_port(value, key)?,
            "RSDB_ALLOWLIST" => self.allowlist = parse_optional_string(value),
            "RSDB_AGGREGATE_DATA_DIR" => self.data_dir = parse_optional_path(value),
            "RSDB_AGGREGATE_RETENTION_HOURS" => {
                self.retention_ms = parse_positive_u64(value, key)?.saturating_mul(MILLIS_PER_HOUR);
            }
            "RSDB_AGGREGATE_HOT_MAX_MB" => {
                self.max_bytes = parse_positive_u64(value, key)?.saturating_mul(BYTES_PER_MEGABYTE);
            }
            _ => {}
        }

        Ok(())
    }

    fn bind(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

const CONFIG_KEYS: &[&str] = &[
    "RSDB_AGGREGATE_HOST",
    "RSDB_AGGREGATE_PORT",
    "RSDB_ALLOWLIST",
    "RSDB_AGGREGATE_DATA_DIR",
    "RSDB_AGGREGATE_RETENTION_HOURS",
    "RSDB_AGGREGATE_HOT_MAX_MB",
];

fn default_port() -> u16 {
    port_from_alias(env::var("PORT").ok().as_deref())
}

fn port_from_alias(port: Option<&str>) -> u16 {
    port.map_or(Ok(DEFAULT_PORT), |port| parse_port(port, "PORT"))
        .unwrap_or(DEFAULT_PORT)
}

fn take_config_path(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
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

fn parse_non_empty<'a>(value: &'a str, key: &str) -> Result<&'a str, String> {
    if value.is_empty() {
        Err(format!("{key} must not be empty"))
    } else {
        Ok(value)
    }
}

fn parse_optional_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn parse_optional_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_port(value: &str, key: &str) -> Result<u16, String> {
    parse_non_empty(value, key)?
        .parse::<u16>()
        .map_err(|_| format!("{key} must be a TCP port between 0 and 65535"))
}

fn parse_positive_u64(value: &str, key: &str) -> Result<u64, String> {
    let value = parse_non_empty(value, key)?;
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{key} must be a positive integer"))?;
    if parsed == 0 {
        Err(format!("{key} must be greater than zero"))
    } else {
        Ok(parsed)
    }
}

fn print_usage() {
    println!("Usage:");
    println!("  rsdb-aggregate [--config path] [serve] [allowlist.txt] [host] [port]");
    println!();
    println!("Config defaults load from RSDB_CONFIG or {DEFAULT_CONFIG_PATH} when present.");
    println!("PORT is used as RSDB_AGGREGATE_PORT when RSDB_AGGREGATE_PORT is absent.");
    println!("Use RSDB_ALLOWLIST for a public-key list or a path to one.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_uses_port_for_default_bind() {
        assert_eq!(port_from_alias(Some("9090")), 9090);
    }

    #[test]
    fn config_parses_aggregate_keys() {
        let mut config = AggregateRuntimeConfig::default();

        config
            .apply_pair("RSDB_AGGREGATE_HOST", "127.0.0.1")
            .unwrap();
        config.apply_pair("RSDB_AGGREGATE_PORT", "8091").unwrap();
        config
            .apply_pair("RSDB_ALLOWLIST", "/tmp/allowlist.txt")
            .unwrap();
        config
            .apply_pair("RSDB_AGGREGATE_DATA_DIR", "/tmp/aggregate")
            .unwrap();
        config
            .apply_pair("RSDB_AGGREGATE_RETENTION_HOURS", "12")
            .unwrap();
        config.apply_pair("RSDB_AGGREGATE_HOT_MAX_MB", "7").unwrap();

        assert_eq!(config.bind(), "127.0.0.1:8091");
        assert_eq!(config.allowlist, Some("/tmp/allowlist.txt".to_owned()));
        assert_eq!(config.data_dir, Some(PathBuf::from("/tmp/aggregate")));
        assert_eq!(config.retention_ms, 12 * MILLIS_PER_HOUR);
        assert_eq!(config.max_bytes, 7 * BYTES_PER_MEGABYTE);
    }
}
