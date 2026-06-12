#!/usr/bin/env sh
set -eu

TARGET="aarch64-unknown-linux-gnu"
DEFAULT_REPOSITORY="abhay/rsdb"
DEFAULT_REMOTE_AGGREGATE_URLS="https://rsdb.hackshare.com"
LOCAL_AGGREGATE_URL="http://127.0.0.1:8090"

INSTALL_ROOT="${RSDB_INSTALL_ROOT:-/opt/rsdb}"
CONFIG_DIR="${RSDB_CONFIG_DIR:-/etc/rsdb}"
STATE_DIR="${RSDB_STATE_DIR:-/var/lib/rsdb}"
CONFIG_FILE="$CONFIG_DIR/rsdb.env"
RELEASES_DIR="$INSTALL_ROOT/releases"
CURRENT_LINK="$INSTALL_ROOT/current"
INSTALLER_PATH="$INSTALL_ROOT/install-release.sh"

usage() {
    cat <<'USAGE'
Usage:
  ./pi/install-release.sh receiver [--enable-updater]
  ./pi/install-release.sh full [--enable-updater]

Release selection:
  RSDB_RELEASE_CHANNEL=nightly ./pi/install-release.sh receiver
  RSDB_VERSION=v0.1.0 ./pi/install-release.sh full

Environment:
  RSDB_GITHUB_REPOSITORY     GitHub repository, defaults to abhay/rsdb.
  RSDB_RELEASE_CHANNEL       stable or nightly; defaults to stable.
  RSDB_VERSION               release tag or latest; defaults to latest.
  RSDB_REMOTE_AGGREGATE_URLS Remote submit URL defaults for receiver profile.
USAGE
}

fail() {
    echo "install-release.sh: $*" >&2
    exit 1
}

profile=""
enable_updater=false

for arg in "$@"; do
    case "$arg" in
        receiver | full)
            if [ -n "$profile" ]; then
                fail "profile was provided more than once"
            fi
            profile="$arg"
            ;;
        --enable-updater)
            enable_updater=true
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            fail "unknown argument: $arg"
            ;;
    esac
done

if [ -z "$profile" ]; then
    usage >&2
    fail "choose receiver or full"
fi

arch="$(uname -m)"
case "$arch" in
    aarch64 | arm64) ;;
    armv6l | armv7l | armhf)
        fail "32-bit Raspberry Pi OS is not supported for release installs; boot a 64-bit aarch64 image"
        ;;
    *)
        fail "unsupported architecture $arch; release installs currently support aarch64 only"
        ;;
esac

if [ "$(id -u)" -eq 0 ]; then
    service_user="${RSDB_SERVICE_USER:-${SUDO_USER:-}}"
    if [ -z "$service_user" ] || [ "$service_user" = root ]; then
        fail "set RSDB_SERVICE_USER when running as root"
    fi
    service_group="${RSDB_SERVICE_GROUP:-$(id -gn "$service_user")}"
else
    if ! command -v sudo >/dev/null 2>&1; then
        fail "sudo is required when not running as root"
    fi
    service_user="${RSDB_SERVICE_USER:-$(id -un)}"
    service_group="${RSDB_SERVICE_GROUP:-$(id -gn)}"
fi

as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}

write_root_file() {
    mode="$1"
    path="$2"
    tmp_file="$(mktemp)"
    cat >"$tmp_file"
    as_root install -m "$mode" "$tmp_file" "$path"
    rm -f "$tmp_file"
}

config_value() {
    key="$1"
    if [ ! -f "$CONFIG_FILE" ]; then
        return 0
    fi

    awk -F= -v key="$key" '
        /^[[:space:]]*#/ { next }
        {
            lhs = $1
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", lhs)
            if (lhs == key) {
                sub(/^[^=]*=/, "", $0)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", $0)
                gsub(/^'\''|'\''$/, "", $0)
                gsub(/^"|"$/, "", $0)
                value = $0
            }
        }
        END { if (value != "") print value }
    ' "$CONFIG_FILE"
}

active_config_key() {
    [ -f "$CONFIG_FILE" ] && grep -Eq "^[[:space:]]*$1=[^#]*[^[:space:]#]" "$CONFIG_FILE"
}

config_changed=false

append_config_line() {
    line="$1"
    tmp_config="$(mktemp)"

    if [ -f "$CONFIG_FILE" ]; then
        cat "$CONFIG_FILE" >"$tmp_config"
    fi
    printf '%s\n' "$line" >>"$tmp_config"

    as_root install -m 0644 "$tmp_config" "$CONFIG_FILE"
    rm -f "$tmp_config"
    config_changed=true
}

ensure_config_key() {
    key="$1"
    line="$2"

    if [ ! -f "$CONFIG_FILE" ] || ! grep -Eq "^[[:space:]#]*${key}=" "$CONFIG_FILE"; then
        append_config_line "$line"
    fi
}

set_config_key() {
    key="$1"
    value="$2"

    if active_config_key "$key" && [ "$(config_value "$key")" = "$value" ]; then
        return 0
    fi

    tmp_config="$(mktemp)"
    awk -v key="$key" -v line="$key=$value" '
        BEGIN { written = 0 }
        {
            lhs = $0
            sub(/=.*/, "", lhs)
            gsub(/^[[:space:]#]+|[[:space:]]+$/, "", lhs)
            if (lhs == key) {
                if (!written) {
                    print line
                    written = 1
                }
                next
            }
            print
        }
        END { if (!written) print line }
    ' "$CONFIG_FILE" >"$tmp_config"

    as_root install -m 0644 "$tmp_config" "$CONFIG_FILE"
    rm -f "$tmp_config"
    config_changed=true
}

validate_coordinate_pair() {
    lat="$1"
    lon="$2"

    if [ -z "$lat" ] || [ -z "$lon" ]; then
        fail "RSDB_RECEIVER_LAT and RSDB_RECEIVER_LON must be set together"
    fi

    if ! awk -v lat="$lat" -v lon="$lon" '
        BEGIN {
            numeric = "^-?[0-9]+(\\.[0-9]+)?$"
            if (lat !~ numeric || lon !~ numeric) exit 1
            if (lat < -90 || lat > 90 || lon < -180 || lon > 180) exit 1
        }
    '; then
        fail "invalid receiver coordinates: RSDB_RECEIVER_LAT=$lat RSDB_RECEIVER_LON=$lon"
    fi
}

validate_hostname() {
    hostname="$1"

    case "$hostname" in
        *[!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-]* | "")
            fail "invalid RSDB_NODE_HOSTNAME: $hostname"
            ;;
    esac
}

normalize_urls() {
    printf '%s' "$1" | tr '[:space:]' ',' | sed 's/,,*/,/g; s/^,//; s/,$//'
}

urls_contain() {
    haystack="$(normalize_urls "$1")"
    needle="$2"

    case ",$haystack," in
        *,"$needle",*) return 0 ;;
        *) return 1 ;;
    esac
}

urls_only_local() {
    [ "$(normalize_urls "$1")" = "$LOCAL_AGGREGATE_URL" ]
}

urls_without_local() {
    urls="$(normalize_urls "$1")"
    old_ifs="$IFS"
    IFS=,
    result=""

    for url in $urls; do
        if [ "$url" = "$LOCAL_AGGREGATE_URL" ]; then
            continue
        fi
        if [ -n "$result" ]; then
            result="$result,$url"
        else
            result="$url"
        fi
    done

    IFS="$old_ifs"
    printf '%s\n' "$result"
}

release_env_value() {
    file="$1"
    key="$2"

    if [ ! -f "$file" ]; then
        return 0
    fi

    awk -F= -v key="$key" '
        $1 == key {
            sub(/^[^=]*=/, "", $0)
            print
            exit
        }
    ' "$file"
}

validate_path_component() {
    value="$1"
    label="$2"

    case "$value" in
        "" | *[!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-]*)
            fail "$label contains unsupported characters: $value"
            ;;
    esac
}

config_release_channel="$(config_value RSDB_RELEASE_CHANNEL || true)"
config_version="$(config_value RSDB_VERSION || true)"

repository="${RSDB_GITHUB_REPOSITORY:-$DEFAULT_REPOSITORY}"
release_channel="${RSDB_RELEASE_CHANNEL:-${config_release_channel:-stable}}"
requested_version="${RSDB_VERSION:-${config_version:-latest}}"

case "$release_channel" in
    stable | nightly) ;;
    *) fail "RSDB_RELEASE_CHANNEL must be stable or nightly" ;;
esac

case "$requested_version" in
    "" | latest)
        requested_version=latest
        if [ "$release_channel" = nightly ]; then
            release_tag=nightly
        else
            release_tag=latest
        fi
        ;;
    *)
        release_tag="$requested_version"
        ;;
esac

validate_path_component "$release_tag" "release tag"

ensure_os_packages() {
    if ! command -v apt-get >/dev/null 2>&1; then
        fail "apt-get is required on Raspberry Pi OS"
    fi

    missing_packages=""
    if ! command -v curl >/dev/null 2>&1; then
        missing_packages="$missing_packages curl"
    fi
    if ! command -v zstd >/dev/null 2>&1; then
        missing_packages="$missing_packages zstd"
    fi
    if ! command -v sha256sum >/dev/null 2>&1; then
        missing_packages="$missing_packages coreutils"
    fi
    if ! command -v sshd >/dev/null 2>&1; then
        missing_packages="$missing_packages openssh-server"
    fi
    if ! command -v avahi-daemon >/dev/null 2>&1; then
        missing_packages="$missing_packages avahi-daemon"
    fi

    if [ -n "$missing_packages" ]; then
        as_root apt-get update
        as_root apt-get install -y ca-certificates $missing_packages
    fi
}

install_self() {
    script_path="$0"
    case "$script_path" in
        /*) ;;
        */*) script_path="$(pwd)/$script_path" ;;
        *)
            script_path="$(command -v "$script_path" || printf '%s' "$script_path")"
            case "$script_path" in
                /*) ;;
                *) script_path="$(pwd)/$script_path" ;;
            esac
            ;;
    esac

    if [ -f "$script_path" ]; then
        if [ ! -f "$INSTALLER_PATH" ] || ! cmp -s "$script_path" "$INSTALLER_PATH"; then
            as_root install -m 0755 "$script_path" "$INSTALLER_PATH"
        fi
    fi
}

install_system_basics() {
    as_root install -d -m 0755 "$CONFIG_DIR"
    as_root install -d -m 0755 "$INSTALL_ROOT"
    as_root install -d -m 0755 "$RELEASES_DIR"
    as_root install -d -m 0755 -o "$service_user" -g "$service_group" "$STATE_DIR"
    as_root install -d -m 0755 -o "$service_user" -g "$service_group" "$STATE_DIR/aggregate"
    as_root install -d -m 0755 -o "$service_user" -g "$service_group" "$STATE_DIR/submit"

    if [ -n "${RSDB_NODE_HOSTNAME:-}" ]; then
        validate_hostname "$RSDB_NODE_HOSTNAME"
        as_root hostnamectl set-hostname "$RSDB_NODE_HOSTNAME"
    fi

    if command -v groupadd >/dev/null 2>&1; then
        as_root groupadd -f plugdev
    fi
    if getent group plugdev >/dev/null 2>&1; then
        as_root usermod -aG plugdev "$service_user"
    fi

    as_root install -d -m 0755 /etc/udev/rules.d
    write_root_file 0644 /etc/udev/rules.d/20-rsdb-rtlsdr.rules <<'RULES'
SUBSYSTEM=="usb", ATTR{idVendor}=="0bda", ATTR{idProduct}=="2832", MODE="0660", GROUP="plugdev", TAG+="uaccess"
SUBSYSTEM=="usb", ATTR{idVendor}=="0bda", ATTR{idProduct}=="2838", MODE="0660", GROUP="plugdev", TAG+="uaccess"
RULES

    if command -v udevadm >/dev/null 2>&1; then
        as_root udevadm control --reload-rules
        as_root udevadm trigger || true
    fi

    as_root systemctl enable --now ssh avahi-daemon >/dev/null 2>&1 || true
}

default_submit_urls() {
    remote_urls="${RSDB_REMOTE_AGGREGATE_URLS:-}"

    if [ "$profile" = receiver ]; then
        if [ -z "$remote_urls" ]; then
            remote_urls="$DEFAULT_REMOTE_AGGREGATE_URLS"
        fi
        printf '%s\n' "$remote_urls"
        return 0
    fi

    if [ -n "$remote_urls" ]; then
        printf '%s,%s\n' "$LOCAL_AGGREGATE_URL" "$remote_urls"
    else
        printf '%s\n' "$LOCAL_AGGREGATE_URL"
    fi
}

ensure_config_file() {
    if [ -f "$CONFIG_FILE" ]; then
        echo "Preserving existing $CONFIG_FILE"
        return 0
    fi

    submit_urls="$(default_submit_urls)"
    write_root_file 0644 "$CONFIG_FILE" <<CONFIG
RSDB_NODE_PROFILE=$profile
RSDB_RELEASE_CHANNEL=$release_channel
RSDB_VERSION=$requested_version
RSDB_DEVICE_INDEX=0
RSDB_PROTOCOL=adsb1090
# RSDB_CENTER_FREQUENCY_HZ=1090000000
# RSDB_SAMPLE_RATE_HZ=2000000
RSDB_COLLECTOR_PORT=8080
RSDB_GAIN=496
RSDB_BIAS_T=false
RSDB_STREAM_SECONDS=5
# RSDB_JSON_SECONDS=30
RSDB_STALE_AFTER_SECONDS=60
RSDB_HEARTBEAT_SECONDS=15
RSDB_RETRY_SECONDS=10
RSDB_PERSIST_DIR=$STATE_DIR
RSDB_PERSIST_FEED_MAX_MB=100
RSDB_AGGREGATE_SERVICE_ENABLED=auto
RSDB_AGGREGATE_HOST=0.0.0.0
RSDB_AGGREGATE_PORT=8090
RSDB_ALLOWLIST=$CONFIG_DIR/allowlist.txt
RSDB_AGGREGATE_DATA_DIR=$STATE_DIR/aggregate
RSDB_AGGREGATE_RETENTION_HOURS=72
RSDB_AGGREGATE_HOT_MAX_MB=250
# RSDB_SIGNING_KEY_PATH=$CONFIG_DIR/receiver.seed
RSDB_SUBMIT_URLS=$submit_urls
RSDB_SUBMIT_RETRY_SECONDS=5
RSDB_SUBMIT_MAX_LAG_SECONDS=60
RSDB_SUBMIT_OUTBOX_DIR=$STATE_DIR/submit
RSDB_SUBMIT_OUTBOX_MAX_MB=25
# RSDB_RECEIVER_LAT=fill-me-in
# RSDB_RECEIVER_LON=fill-me-in
CONFIG
    config_changed=true
}

apply_config_defaults() {
    ensure_config_file

    ensure_config_key RSDB_PERSIST_DIR "RSDB_PERSIST_DIR=$STATE_DIR"
    ensure_config_key RSDB_PERSIST_FEED_MAX_MB "RSDB_PERSIST_FEED_MAX_MB=100"
    ensure_config_key RSDB_PROTOCOL "RSDB_PROTOCOL=adsb1090"
    ensure_config_key RSDB_COLLECTOR_PORT "RSDB_COLLECTOR_PORT=8080"
    ensure_config_key RSDB_AGGREGATE_SERVICE_ENABLED "RSDB_AGGREGATE_SERVICE_ENABLED=auto"
    ensure_config_key RSDB_AGGREGATE_HOST "RSDB_AGGREGATE_HOST=0.0.0.0"
    ensure_config_key RSDB_AGGREGATE_PORT "RSDB_AGGREGATE_PORT=8090"
    ensure_config_key RSDB_ALLOWLIST "RSDB_ALLOWLIST=$CONFIG_DIR/allowlist.txt"
    ensure_config_key RSDB_AGGREGATE_DATA_DIR "RSDB_AGGREGATE_DATA_DIR=$STATE_DIR/aggregate"
    ensure_config_key RSDB_AGGREGATE_RETENTION_HOURS "RSDB_AGGREGATE_RETENTION_HOURS=72"
    ensure_config_key RSDB_AGGREGATE_HOT_MAX_MB "RSDB_AGGREGATE_HOT_MAX_MB=250"
    ensure_config_key RSDB_SIGNING_KEY_PATH "# RSDB_SIGNING_KEY_PATH=$CONFIG_DIR/receiver.seed"
    ensure_config_key RSDB_SUBMIT_RETRY_SECONDS "RSDB_SUBMIT_RETRY_SECONDS=5"
    ensure_config_key RSDB_SUBMIT_MAX_LAG_SECONDS "RSDB_SUBMIT_MAX_LAG_SECONDS=60"
    ensure_config_key RSDB_SUBMIT_OUTBOX_DIR "RSDB_SUBMIT_OUTBOX_DIR=$STATE_DIR/submit"
    ensure_config_key RSDB_SUBMIT_OUTBOX_MAX_MB "RSDB_SUBMIT_OUTBOX_MAX_MB=25"

    set_config_key RSDB_NODE_PROFILE "$profile"
    set_config_key RSDB_RELEASE_CHANNEL "$release_channel"
    set_config_key RSDB_VERSION "$requested_version"

    if [ "$profile" = receiver ]; then
        set_config_key RSDB_AGGREGATE_SERVICE_ENABLED false
    else
        set_config_key RSDB_AGGREGATE_SERVICE_ENABLED true
    fi

    receiver_lat="${RSDB_RECEIVER_LAT:-$(config_value RSDB_RECEIVER_LAT)}"
    receiver_lon="${RSDB_RECEIVER_LON:-$(config_value RSDB_RECEIVER_LON)}"
    if [ -n "$receiver_lat" ] || [ -n "$receiver_lon" ]; then
        validate_coordinate_pair "$receiver_lat" "$receiver_lon"
        set_config_key RSDB_RECEIVER_LAT "$receiver_lat"
        set_config_key RSDB_RECEIVER_LON "$receiver_lon"
    fi

    if [ -n "${RSDB_SUBMIT_URLS:-}" ]; then
        set_config_key RSDB_SUBMIT_URLS "$RSDB_SUBMIT_URLS"
    else
        current_submit_urls="$(config_value RSDB_SUBMIT_URLS)"
        if [ "$profile" = receiver ]; then
            receiver_submit_urls="$(urls_without_local "$current_submit_urls")"
            if [ -z "$receiver_submit_urls" ] || urls_only_local "$current_submit_urls"; then
                receiver_submit_urls="$(default_submit_urls)"
            fi
            if [ "$receiver_submit_urls" != "$current_submit_urls" ]; then
                set_config_key RSDB_SUBMIT_URLS "$receiver_submit_urls"
            fi
        elif [ -z "$current_submit_urls" ]; then
            set_config_key RSDB_SUBMIT_URLS "$(default_submit_urls)"
        elif ! urls_contain "$current_submit_urls" "$LOCAL_AGGREGATE_URL"; then
            set_config_key RSDB_SUBMIT_URLS "$LOCAL_AGGREGATE_URL,$current_submit_urls"
        fi
    fi

    if [ -n "${RSDB_SUBMIT_MAX_LAG_SECONDS:-}" ]; then
        set_config_key RSDB_SUBMIT_MAX_LAG_SECONDS "$RSDB_SUBMIT_MAX_LAG_SECONDS"
    fi

    if [ -f "$CONFIG_DIR/receiver.seed" ]; then
        as_root chown "$service_user:$service_group" "$CONFIG_DIR/receiver.seed"
        as_root chmod 0600 "$CONFIG_DIR/receiver.seed"
        if ! active_config_key RSDB_SIGNING_KEY_PATH; then
            set_config_key RSDB_SIGNING_KEY_PATH "$CONFIG_DIR/receiver.seed"
        fi
    fi
}

download_file() {
    url="$1"
    output="$2"

    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL --retry 3 \
            -H "Authorization: Bearer $GITHUB_TOKEN" \
            -H "X-GitHub-Api-Version: 2022-11-28" \
            -o "$output" \
            "$url"
    else
        curl -fsSL --retry 3 -o "$output" "$url"
    fi
}

download_release() {
    artifact="rsdb-$profile-$TARGET.tar.zst"
    tmp_dir="$(mktemp -d)"
    extract_dir="$tmp_dir/extract"
    mkdir -p "$extract_dir"

    if [ "$release_tag" = latest ]; then
        release_url="https://github.com/$repository/releases/latest/download"
    else
        release_url="https://github.com/$repository/releases/download/$release_tag"
    fi

    echo "Downloading $artifact from $repository release $release_tag"
    download_file "$release_url/$artifact" "$tmp_dir/$artifact"
    download_file "$release_url/checksums.txt" "$tmp_dir/checksums.txt"

    expected_sum="$(awk -v artifact="$artifact" '$2 == artifact { print $1; found = 1 } END { if (!found) exit 1 }' "$tmp_dir/checksums.txt")" \
        || fail "checksums.txt does not contain $artifact"
    actual_sum="$(sha256sum "$tmp_dir/$artifact" | awk '{ print $1 }')"

    if [ "$expected_sum" != "$actual_sum" ]; then
        fail "checksum mismatch for $artifact"
    fi

    tar --zstd -xf "$tmp_dir/$artifact" -C "$extract_dir"
}

install_release() {
    manifest="$extract_dir/share/rsdb/release.env"
    if [ ! -f "$manifest" ]; then
        fail "artifact is missing share/rsdb/release.env"
    fi

    build_version="$(release_env_value "$manifest" RSDB_BUILD_VERSION)"
    build_profile="$(release_env_value "$manifest" RSDB_BUILD_PROFILE)"
    build_target="$(release_env_value "$manifest" RSDB_BUILD_TARGET)"

    validate_path_component "$build_version" "build version"
    if [ "$build_profile" != "$profile" ]; then
        fail "downloaded $build_profile artifact for requested $profile profile"
    fi
    if [ "$build_target" != "$TARGET" ]; then
        fail "downloaded $build_target artifact for expected $TARGET"
    fi
    if [ ! -x "$extract_dir/bin/rsdb-usb" ]; then
        fail "artifact is missing bin/rsdb-usb"
    fi
    if [ "$profile" = full ] && [ ! -x "$extract_dir/bin/rsdb-aggregate" ]; then
        fail "full artifact is missing bin/rsdb-aggregate"
    fi

    current_manifest="$CURRENT_LINK/share/rsdb/release.env"
    current_version="$(release_env_value "$current_manifest" RSDB_BUILD_VERSION)"
    current_profile="$(release_env_value "$current_manifest" RSDB_BUILD_PROFILE)"
    release_changed=true

    if [ "$current_version" = "$build_version" ] &&
        [ "$current_profile" = "$profile" ] &&
        [ -x "$CURRENT_LINK/bin/rsdb-usb" ]; then
        if [ "$profile" = receiver ] || [ -x "$CURRENT_LINK/bin/rsdb-aggregate" ]; then
            release_changed=false
        fi
    fi

    if [ "$release_changed" = false ]; then
        echo "RSDB $build_version is already installed for profile $profile"
        return 0
    fi

    release_dir="$RELEASES_DIR/$build_version"
    staging_dir="$RELEASES_DIR/.$build_version.installing.$$"
    link_tmp="$INSTALL_ROOT/.current.$$"

    as_root rm -rf "$staging_dir"
    as_root install -d -m 0755 "$staging_dir"
    as_root cp -R "$extract_dir/." "$staging_dir/"
    as_root chmod 0755 "$staging_dir/bin" "$staging_dir/bin/rsdb-usb"
    if [ -f "$staging_dir/bin/rsdb-aggregate" ]; then
        as_root chmod 0755 "$staging_dir/bin/rsdb-aggregate"
    fi

    if [ -e "$release_dir" ]; then
        as_root rm -rf "$release_dir"
    fi
    as_root mv "$staging_dir" "$release_dir"
    as_root ln -sfn "$release_dir" "$link_tmp"
    as_root mv -Tf "$link_tmp" "$CURRENT_LINK"

    echo "Installed RSDB $build_version to $release_dir"
}

ensure_allowlist() {
    if [ "$profile" != full ]; then
        return 0
    fi

    allowlist_path="$(config_value RSDB_ALLOWLIST)"
    if [ -z "$allowlist_path" ]; then
        allowlist_path="$CONFIG_DIR/allowlist.txt"
        set_config_key RSDB_ALLOWLIST "$allowlist_path"
    fi

    if [ "$allowlist_path" != "$CONFIG_DIR/allowlist.txt" ] || [ -f "$allowlist_path" ]; then
        return 0
    fi

    if ! active_config_key RSDB_SIGNING_KEY_PATH; then
        echo "No signing key configured; create $allowlist_path before relying on the local aggregate" >&2
        return 0
    fi

    tmp_allowlist="$(mktemp)"
    if "$CURRENT_LINK/bin/rsdb-usb" --config "$CONFIG_FILE" allowlist-entry >"$tmp_allowlist"; then
        as_root install -m 0644 "$tmp_allowlist" "$allowlist_path"
        echo "Installed local aggregate allowlist at $allowlist_path"
    else
        echo "Could not generate $allowlist_path; rsdb-aggregate may not start until an allowlist exists" >&2
    fi
    rm -f "$tmp_allowlist"
}

install_service_units() {
    write_root_file 0644 /etc/systemd/system/rsdb.service <<SERVICE
[Unit]
Description=RSDB receiver collector
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=$service_user
Group=$service_group
WorkingDirectory=$STATE_DIR
EnvironmentFile=-$CONFIG_FILE
ExecStart=$CURRENT_LINK/bin/rsdb-usb serve
Restart=always
RestartSec=10
SupplementaryGroups=plugdev
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
SERVICE

    write_root_file 0644 /etc/systemd/system/rsdb-aggregate.service <<SERVICE
[Unit]
Description=RSDB local aggregate ingest
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=$service_user
Group=$service_group
WorkingDirectory=$STATE_DIR
EnvironmentFile=-$CONFIG_FILE
ExecStart=$CURRENT_LINK/bin/rsdb-aggregate serve
Restart=always
RestartSec=10
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
SERVICE
}

install_updater_units() {
    write_root_file 0644 /etc/systemd/system/rsdb-update.service <<SERVICE
[Unit]
Description=Update RSDB release install
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
EnvironmentFile=-$CONFIG_FILE
Environment=RSDB_GITHUB_REPOSITORY=$repository
Environment=RSDB_SERVICE_USER=$service_user
Environment=RSDB_SERVICE_GROUP=$service_group
ExecStart=$INSTALLER_PATH $profile
SERVICE

    write_root_file 0644 /etc/systemd/system/rsdb-update.timer <<'TIMER'
[Unit]
Description=Daily RSDB release update check

[Timer]
OnCalendar=daily
Persistent=true
RandomizedDelaySec=30m

[Install]
WantedBy=timers.target
TIMER
}

apply_services() {
    should_restart=false
    if [ "$release_changed" = true ] || [ "$config_changed" = true ]; then
        should_restart=true
    fi

    as_root systemctl daemon-reload
    as_root systemctl enable rsdb.service >/dev/null

    if [ "$profile" = full ]; then
        if [ ! -x "$CURRENT_LINK/bin/rsdb-aggregate" ]; then
            fail "full profile requires $CURRENT_LINK/bin/rsdb-aggregate"
        fi
        as_root systemctl enable rsdb-aggregate.service >/dev/null
    else
        as_root systemctl disable --now rsdb-aggregate.service >/dev/null 2>&1 || true
    fi

    if [ "$enable_updater" = true ]; then
        as_root systemctl enable --now rsdb-update.timer >/dev/null
    fi

    if [ "$should_restart" = true ]; then
        if ! as_root systemctl restart rsdb.service; then
            echo "rsdb.service did not start cleanly; check: journalctl -u rsdb.service" >&2
        fi

        if [ "$profile" = full ]; then
            if ! as_root systemctl restart rsdb-aggregate.service; then
                echo "rsdb-aggregate.service did not start cleanly; check: journalctl -u rsdb-aggregate.service" >&2
            fi
        fi
    else
        as_root systemctl start rsdb.service >/dev/null 2>&1 || true
        if [ "$profile" = full ]; then
            as_root systemctl start rsdb-aggregate.service >/dev/null 2>&1 || true
        fi
    fi
}

cleanup() {
    if [ -n "${tmp_dir:-}" ] && [ -d "$tmp_dir" ]; then
        rm -rf "$tmp_dir"
    fi
}
trap cleanup EXIT INT TERM

ensure_os_packages
install_system_basics
install_self
apply_config_defaults
download_release
install_release
ensure_allowlist
install_service_units
if [ "$enable_updater" = true ]; then
    install_updater_units
fi
apply_services

echo
echo "RSDB $profile profile is installed."
echo "Current release: $CURRENT_LINK"
echo "Receiver service: systemctl status rsdb.service"
if [ "$profile" = full ]; then
    echo "Aggregate service: systemctl status rsdb-aggregate.service"
else
    echo "Aggregate service: disabled for receiver profile"
fi
if [ "$enable_updater" = true ]; then
    echo "Updater timer: systemctl status rsdb-update.timer"
fi
