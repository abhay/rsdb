#!/usr/bin/env sh

list_external_physical_disks() {
    diskutil list external physical | awk '/^\/dev\/disk[0-9]+[[:space:]]/ { print $1 }'
}

validate_disk_path() {
    requested_disk="$1"
    disk_number="${requested_disk#/dev/disk}"

    if [ "$requested_disk" = "$disk_number" ] || [ -z "$disk_number" ]; then
        echo "disk must look like /dev/diskN, got: $requested_disk" >&2
        exit 1
    fi

    case "$disk_number" in
        *[!0-9]*)
            echo "disk must be a whole disk like /dev/diskN, got: $requested_disk" >&2
            exit 1
            ;;
    esac
}

resolve_external_physical_disk() {
    requested_disk="${1:-}"

    if [ -n "$requested_disk" ]; then
        validate_disk_path "$requested_disk"
        printf '%s\n' "$requested_disk"
        return
    fi

    disks="$(list_external_physical_disks | sed '/^$/d')"
    count="$(printf '%s\n' "$disks" | sed '/^$/d' | wc -l | tr -d ' ')"

    case "$count" in
        0)
            echo "no external physical disks found" >&2
            echo "insert an SD card or pass /dev/diskN explicitly" >&2
            exit 1
            ;;
        1)
            disk="$disks"
            echo "auto-discovered external disk: $disk" >&2
            printf '%s\n' "$disk"
            ;;
        *)
            echo "multiple external physical disks found; pass /dev/diskN explicitly" >&2
            diskutil list external physical >&2
            exit 1
            ;;
    esac
}

assert_external_whole_disk() {
    disk="$1"
    validate_disk_path "$disk"
    info="$(diskutil info "$disk")"
    whole="$(printf '%s\n' "$info" | awk -F': *' '/Whole:/ { print $2; exit }')"
    location="$(printf '%s\n' "$info" | awk -F': *' '/Device Location:/ { print $2; exit }')"

    if [ "$whole" != "Yes" ]; then
        echo "refusing to use non-whole disk: $disk" >&2
        exit 1
    fi

    if [ "$location" != "External" ]; then
        echo "refusing to use non-external disk: $disk" >&2
        echo "Device Location: ${location:-unknown}" >&2
        exit 1
    fi
}
