#!/usr/bin/env sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    echo "This helper is macOS-only." >&2
    exit 1
fi

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_SCRIPT_DIR="$script_dir"
. "$script_dir/load-env.sh"
. "$script_dir/macos-disk.sh"

if [ -n "$RSDB_NODE_HOST" ]; then
    node_target="$RSDB_NODE_USER@$RSDB_NODE_HOST"
else
    node_target=""
fi

disk="$(resolve_external_physical_disk "${1:-}")"
image="${2:-pi/images/raspios-lite-arm64.img.xz}"

case "$image" in
    /*) ;;
    *) image="$RSDB_REPO_ROOT/$image" ;;
esac

if [ ! -f "$image" ]; then
    echo "missing image: $image" >&2
    echo "run $script_dir/download-rpios-lite.sh first, or pass an image path" >&2
    exit 1
fi

public_key_file="$RSDB_NODE_SSH_KEY.pub"
case "$public_key_file" in
    /*) ;;
    *) public_key_file="$RSDB_REPO_ROOT/$public_key_file" ;;
esac

if [ ! -f "$public_key_file" ]; then
    "$script_dir/create-node-ssh-key.sh"
fi

assert_external_whole_disk "$disk"

echo "About to erase and write:"
diskutil info "$disk"
echo
printf 'Type exactly "flash %s" to continue: ' "$disk"
read -r confirmation

if [ "$confirmation" != "flash $disk" ]; then
    echo "aborted"
    exit 1
fi

sudo -v

raw_disk="/dev/r$(basename "$disk")"

diskutil unmountDisk force "$disk" >/dev/null

case "$image" in
    *.xz)
        if ! command -v xz >/dev/null 2>&1; then
            echo "xz is required for .xz images. Install with: brew install xz" >&2
            exit 1
        fi
        xz -dc "$image" | sudo dd of="$raw_disk" bs=4m status=progress
        ;;
    *)
        sudo dd if="$image" of="$raw_disk" bs=4m status=progress
        ;;
esac

sync
diskutil unmountDisk "$disk" >/dev/null 2>&1 || true
diskutil mount "${disk}s1" >/dev/null 2>&1 || true

mount_point="$(diskutil info "${disk}s1" | awk -F': *' '/Mount Point/ { print $2 }')"

if [ -z "$mount_point" ] || [ ! -d "$mount_point" ]; then
    echo "could not mount ${disk}s1 automatically" >&2
    echo "reinsert the SD card, then run: $script_dir/install-firstboot.sh /Volumes/bootfs" >&2
    exit 1
fi

"$script_dir/install-firstboot.sh" "$mount_point"
sync
mdutil -i off "$mount_point" >/dev/null 2>&1 || true

if ! diskutil eject "$disk" >/dev/null; then
    sleep 2
    diskutil unmountDisk force "$disk" >/dev/null
    diskutil eject "$disk" >/dev/null
fi

echo
echo "Done. Insert the SD card into the Pi and boot it."
echo "Connect with:"
echo "  ssh -i $RSDB_NODE_SSH_KEY -o ForwardAgent=no $node_target"
