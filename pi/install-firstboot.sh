#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_SCRIPT_DIR="$script_dir"
. "$script_dir/load-env.sh"

boot_mount="${1:-}"

if [ -z "$boot_mount" ] || [ ! -d "$boot_mount" ]; then
    echo "usage: $0 /Volumes/bootfs" >&2
    exit 1
fi

cmdline="$boot_mount/cmdline.txt"
firstrun="$boot_mount/firstrun.sh"
public_key_file="$RSDB_NODE_SSH_KEY.pub"
hostname="${RSDB_NODE_HOSTNAME:-}"
username="${RSDB_NODE_USER:-rsdb}"
timezone="${RSDB_TIMEZONE:-America/Los_Angeles}"
wifi_country="${RSDB_WIFI_COUNTRY:-US}"
wifi_ssid="${RSDB_WIFI_SSID:-}"
wifi_password="${RSDB_WIFI_PASSWORD:-}"

if [ ! -f "$cmdline" ]; then
    echo "missing cmdline.txt at $cmdline" >&2
    exit 1
fi

if [ -z "$hostname" ]; then
    echo "set RSDB_NODE_HOSTNAME in .env before installing first-boot config" >&2
    exit 1
fi

if [ ! -f "$public_key_file" ]; then
    case "$public_key_file" in
        /*) ;;
        *) public_key_file="$RSDB_REPO_ROOT/$public_key_file" ;;
    esac
fi

if [ ! -f "$public_key_file" ]; then
    echo "missing public key: $public_key_file" >&2
    echo "run ./pi/create-node-ssh-key.sh first" >&2
    exit 1
fi

case "$hostname" in
    *[!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-]* | "")
        echo "invalid RSDB_NODE_HOSTNAME: $hostname" >&2
        exit 1
        ;;
esac

case "$username" in
    *[!abcdefghijklmnopqrstuvwxyz_0123456789-]* | "" | -*)
        echo "invalid RSDB_NODE_USER: $username" >&2
        exit 1
        ;;
esac

shell_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

public_key="$(cat "$public_key_file")"

cat >"$firstrun" <<EOF
#!/bin/bash
set -e

hostname=$(shell_quote "$hostname")
username=$(shell_quote "$username")
public_key=$(shell_quote "$public_key")
timezone=$(shell_quote "$timezone")
wifi_country=$(shell_quote "$wifi_country")
wifi_ssid=$(shell_quote "$wifi_ssid")
wifi_password=$(shell_quote "$wifi_password")

echo "\$hostname" >/etc/hostname
if grep -q '^127\\.0\\.1\\.1' /etc/hosts; then
    sed -i "s/^127\\.0\\.1\\.1.*/127.0.1.1\\t\$hostname/" /etc/hosts
else
    printf '127.0.1.1\\t%s\\n' "\$hostname" >>/etc/hosts
fi

groups=""
for group in adm dialout cdrom sudo audio video plugdev games users input render netdev gpio i2c spi; do
    if getent group "\$group" >/dev/null 2>&1; then
        if [ -n "\$groups" ]; then
            groups="\$groups,\$group"
        else
            groups="\$group"
        fi
    fi
done

if ! id "\$username" >/dev/null 2>&1; then
    if [ -n "\$groups" ]; then
        useradd -m -s /bin/bash -G "\$groups" "\$username"
    else
        useradd -m -s /bin/bash "\$username"
    fi
fi

random_password="\$(openssl rand -base64 48 2>/dev/null || date +%s%N)"
printf '%s:%s\\n' "\$username" "\$random_password" | chpasswd

install -d -m 700 -o "\$username" -g "\$username" "/home/\$username/.ssh"
printf '%s\\n' "\$public_key" >"/home/\$username/.ssh/authorized_keys"
chown "\$username:\$username" "/home/\$username/.ssh/authorized_keys"
chmod 600 "/home/\$username/.ssh/authorized_keys"

printf '%s ALL=(ALL) NOPASSWD:ALL\\n' "\$username" >/etc/sudoers.d/010-rsdb-node
chmod 440 /etc/sudoers.d/010-rsdb-node

install -d /etc/ssh/sshd_config.d
cat >/etc/ssh/sshd_config.d/99-rsdb-node.conf <<'SSHD'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
SSHD

systemctl enable ssh || true
systemctl enable avahi-daemon || true
systemctl enable NetworkManager || true
timedatectl set-timezone "\$timezone" || true

if [ -n "\$wifi_country" ] && command -v raspi-config >/dev/null 2>&1; then
    raspi-config nonint do_wifi_country "\$wifi_country" || true
fi

if [ -n "\$wifi_ssid" ] && [ -n "\$wifi_password" ]; then
    rfkill unblock wifi >/dev/null 2>&1 || true
    install -d -m 700 /etc/NetworkManager/system-connections
    cat >/etc/NetworkManager/system-connections/rsdb-wifi.nmconnection <<WIFI
[connection]
id=rsdb-wifi
uuid=11111111-2222-4333-8444-555555555555
type=wifi
interface-name=wlan0
autoconnect=true

[wifi]
mode=infrastructure
ssid=\$wifi_ssid

[wifi-security]
key-mgmt=wpa-psk
psk=\$wifi_password

[ipv4]
method=auto

[ipv6]
addr-gen-mode=default
method=auto
WIFI
    chmod 600 /etc/NetworkManager/system-connections/rsdb-wifi.nmconnection
    systemctl restart NetworkManager || true
fi

for file in /boot/cmdline.txt /boot/firmware/cmdline.txt; do
    if [ -f "\$file" ]; then
        sed -i 's| init=/usr/lib/raspberrypi-sys-mods/firstboot||g; s| systemd.run=[^ ]*||g; s| systemd.run_success_action=[^ ]*||g; s| systemd.unit=kernel-command-line.target||g; s|  *| |g' "\$file"
    fi
done

rm -f /boot/firstrun.sh /boot/firmware/firstrun.sh
exit 0
EOF

chmod 755 "$firstrun"

cmdline_contents="$(tr -d '\n' <"$cmdline" \
    | sed 's| init=/usr/lib/raspberrypi-sys-mods/firstboot||g; s| systemd.run=[^ ]*||g; s| systemd.run_success_action=[^ ]*||g; s| systemd.unit=kernel-command-line.target||g; s|  *| |g; s|^ *||; s| *$||')"
printf '%s systemd.run=/boot/firmware/firstrun.sh systemd.run_success_action=reboot systemd.unit=kernel-command-line.target\n' "$cmdline_contents" >"$cmdline"

echo "Installed first-boot customization into $boot_mount"
echo "Hostname: $hostname"
echo "User:     $username"
