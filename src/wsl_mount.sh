#!/bin/sh
set -eu
export LC_ALL=C
export PATH=/usr/sbin:/usr/bin:/sbin:/bin

find_disk() {
    size="$1"
    remote="$2"
    previous="$3"
    usb=''
    if [ "$remote" != '-' ]; then
        # Match the host USB/IP device number to the local USB bus and port.
        usb=$(awk -v id="$remote" '$5 == id && $3 == "006" { print $7 }' \
            /sys/devices/platform/vhci_hcd.*/status* 2>/dev/null || :)
        [ -n "$usb" ] || return 1
        usb=$(readlink -e "/sys/bus/usb/devices/$usb") || return 1
    fi
    for item in /sys/class/block/*; do
        [ ! -e "$item/partition" ] && [ -e "$item/device" ] || continue
        name=$(basename "$item")
        [ -b "/dev/$name" ] || continue
        [ "$(cat "$item/size")" -eq "$((size / 512))" ] || continue
        if [ -n "$usb" ]; then
            case "$(readlink -f "$item")" in "$usb"/*) ;; *) continue ;; esac
        else
            case " $previous " in *" $name "*) continue ;; esac
        fi
        printf '/dev/%s\n' "$name"
    done
}

unmount_volumes() {
    case "$1" in /mnt/wsl/disk-copy-98.*) ;; *) return 1 ;; esac
    [ -d "$1" ] || return 0
    for dir in "$1"/volume-*; do
        [ -d "$dir" ] || continue
        if mountpoint -q "$dir"; then
            umount -- "$dir" || return 1
        fi
        rmdir -- "$dir" || return 1
    done
    for record in "$1"/loop-*; do
        [ -f "$record" ] || continue
        loop=$(cat "$record")
        if losetup "$loop" >/dev/null 2>&1; then
            losetup -d "$loop" || return 1
        fi
        rm -- "$record"
    done
    for plan in "$1"/move-*; do
        [ ! -f "$plan" ] || rm -- "$plan"
    done
    rmdir -- "$1"
}

probe() {
    timeout 20 blkid -p -o export "$@" || :
}

ensure_tool() {
    command -v "$1" >/dev/null && return 0
    if command -v apt-get >/dev/null; then
        export DEBIAN_FRONTEND=noninteractive
        if ! timeout 90 apt-get -q -y install "$2" >&2; then
            timeout 90 apt-get -q update >&2 &&
                timeout 90 apt-get -q -y install "$2" >&2
        fi
    fi
    if ! command -v "$1" >/dev/null; then
        printf 'The WSL distribution requires the %s package to open this volume.\n' "$2" >&2
        return 1
    fi
}

read_only_loop() (
    backing="$1"
    offset="$2"
    length="$3"
    record="$4"
    loop=$(losetup --find --show --read-only --offset "$offset" --sizelimit "$length" "$backing") || exit 1
    printf '%s\n' "$loop" > "$record"
    printf '%s\n' "$loop"
)

mount_fuse_ext() (
    device="$1"
    dir="$2"
    ensure_tool fuse2fs fuse2fs || exit 1
    fuse2fs "$device" "$dir" -o ro,norecovery,allow_other,nosuid,nodev,noexec >&2
)

clean_ext() {
    header=$(dumpe2fs -h "$1" 2>/dev/null) || return 1
    printf '%s\n' "$header" | grep -Eq '^Filesystem state:[[:space:]]+clean$' &&
        ! printf '%s\n' "$header" | grep -Eq '^Filesystem features:.*[[:space:]]needs_recovery([[:space:]]|$)'
}

switch_write_mount() (
    action="$1"
    dir="$2"
    base=$(dirname "$dir")
    name=$(basename "$dir")
    case "$base:$name" in /mnt/wsl/disk-copy-98.*:volume-[0-9]*) ;; *) exit 1 ;; esac
    IFS=' ' read -r backing sequence offset length loop_record < "$base/move-$name" || exit 1
    original=$(cat "$loop_record")
    write_record="$loop_record-write"
    mounted=$(findmnt -n -o SOURCE --mountpoint "$dir")

    if [ "$action" = begin-write ]; then
        [ "$mounted" = "$original" ] && [ ! -e "$write_record" ] || exit 1
        if ! clean_ext "$original"; then
            echo 'Il filesystem richiede un controllo. Il volume resta in sola lettura.' >&2
            exit 1
        fi
        if ! timeout 40 e2fsck -n "$original" >/dev/null 2>&1; then
            echo 'Il controllo in sola lettura del filesystem non è riuscito.' >&2
            exit 1
        fi
        if [ "$(blockdev --getdiskseq "$backing")" != "$sequence" ]; then
            echo 'Il disco fisico è cambiato. Montalo di nuovo.' >&2
            exit 1
        fi
        umount -- "$dir" || exit 1
        restore() {
            if ! mountpoint -q "$dir"; then
                mount_fuse_ext "$original" "$dir" || :
            fi
        }
        trap restore EXIT
        writable=$(losetup --find --show --offset "$offset" --sizelimit "$length" "$backing") || exit 1
        if [ "$(blockdev --getdiskseq "$backing")" != "$sequence" ] ||
            [ "$(blockdev --getro "$writable")" != 0 ]; then
            losetup -d "$writable"
            echo 'Il dispositivo di scrittura non corrisponde al disco montato.' >&2
            exit 1
        fi
        printf '%s\n' "$writable" > "$write_record"
        if ! fuse2fs "$writable" "$dir" -o rw,fakeroot,allow_other,nosuid,nodev,noexec,noatime >&2 ||
            ! mountpoint -q "$dir"; then
            losetup -d "$writable"
            rm -- "$write_record"
            echo 'WSL non ha aperto il volume in scrittura.' >&2
            exit 1
        fi
        trap - EXIT
    else
        writable=$(cat "$write_record")
        [ "$mounted" = "$writable" ] || exit 1
        sync -f "$dir" || exit 1
        umount -- "$dir" || exit 1
        flushed=0
        blockdev --flushbufs "$writable" || flushed=1
        losetup -d "$writable" || exit 1
        rm -- "$write_record"
        # Drop the read-only loop's page cache before reopening it after writes.
        blockdev --flushbufs "$original" || exit 1
        mount_fuse_ext "$original" "$dir"
        [ "$flushed" -eq 0 ]
    fi
)

mount_lvm() (
    backing="$1"
    pv_offset="$2"
    dir="$3"
    records="$4"
    ensure_tool pvs lvm2 || exit 1
    pv=$(read_only_loop "$backing" "$pv_offset" 0 "$records-pv") || exit 1
    config='devices { use_devicesfile=0 }'
    pv_info=$(pvs --readonly --config "$config" --devices "$pv" --noheadings \
        --units s --nosuffix --separator : -o vg_name,pe_start,vg_extent_size,pv_count "$pv") || exit 1
    segments=$(lvs --readonly --config "$config" --devices "$pv" --noheadings --segments \
        --units s --nosuffix --separator : -o vg_name,lv_name,segtype,seg_start,seg_size,devices,stripes) || exit 1
    # Map one complete linear LV without activating the volume group in WSL.
    range=$(printf '%s\n%s\n' "$pv_info" "$segments" | awk -F : -v pv="$pv" '
        { sub(/^[[:space:]]+/, "", $1); sub(/[[:space:]]+$/, "", $NF) }
        NR == 1 { vg=$1; pe=$2; extent=$3; valid=($4 == 1 && pe ~ /^[0-9]+$/ && extent ~ /^[0-9]+$/) }
        NR == 2 && valid && $1 == vg && $3 == "linear" && $4 == 0 &&
            $5 ~ /^[1-9][0-9]*$/ && $7 == 1 && index($6, pv "(") == 1 {
            start=substr($6, length(pv)+2)
            if (start ~ /^[0-9]+\)$/) {
                sub(/\)$/, "", start)
                printf "%.0f %.0f\n", pe + start * extent, $5
            }
        }
        END { if (NR != 2) exit 1 }') || exit 1
    if [ -z "$range" ]; then
        echo 'The LVM group needs multiple disks or uses a layout that cannot be reconstructed from this disk alone.' >&2
        exit 1
    fi
    set -- $range
    offset=$((pv_offset + $1 * 512))
    length=$(($2 * 512))
    if [ "$((offset + length))" -gt "$(blockdev --getsize64 "$backing")" ]; then
        echo 'The LVM data range extends beyond the selected disk.' >&2
        exit 1
    fi
    lv=$(read_only_loop "$backing" "$offset" "$length" "$records-lv") || exit 1
    signature=$(probe "$lv")
    type=$(printf '%s\n' "$signature" | sed -n 's/^TYPE=//p')
    case "$type" in
        ext2|ext3|ext4)
            mount_fuse_ext "$lv" "$dir" || exit 1
            ;;
        *)
            echo 'The LVM data volume does not contain a supported ext filesystem.' >&2
            exit 1
            ;;
    esac
    mode=ro
    if [ "$pv_offset" -gt 0 ] && clean_ext "$lv"; then
        sequence=$(blockdev --getdiskseq "$backing" 2>/dev/null || :)
        if [ -n "$sequence" ]; then
            printf '%s %s %s %s %s\n' "$backing" "$sequence" "$offset" "$length" "$records-lv" \
                > "$(dirname "$dir")/move-$(basename "$dir")"
            mode=move
        fi
    fi
    printf '%s\t%s\n' "$type" "$mode"
)

legacy_pv_offset() (
    # Legacy ReadyNAS SPARC stores the partition table in a logical RAID view.
    # Confirm both the small ext system volume and the adjacent LVM signature.
    for layout in '2 4608003' '32 4608040'; do
        set -- $layout
        system=$(probe -O "$(($1 * 512))" "$disk")
        printf '%s\n' "$system" | grep -qx 'FSBLOCKSIZE=16384' || continue
        offset=$(($2 * 512))
        signature=$(probe -O "$offset" "$disk")
        if printf '%s\n' "$signature" | grep -qx 'TYPE=LVM2_member'; then
            printf '%s\n' "$offset"
            exit 0
        fi
    done
)

case "$1" in
    prepare-usb)
        modprobe usb-storage 2>/dev/null || :
        modprobe uas 2>/dev/null || :
        ;;
    snapshot)
        for item in /sys/class/block/*; do basename "$item"; done
        ;;
    unmount)
        unmount_volumes "$2"
        ;;
    begin-write|end-write)
        switch_write_mount "$1" "$2"
        ;;
    mount)
        disk=''
        for attempt in $(seq 1 45); do
            disk=$(find_disk "$2" "$3" "$4" || :)
            [ -z "$disk" ] || break
            sleep 1
        done
        case "$disk" in
            '') echo 'WSL did not expose a block device of the expected size. Check the USB adapter, power and USB storage drivers.' >&2; exit 1 ;;
            *'
'*) echo 'Multiple Linux disks match the selected device.' >&2; exit 1 ;;
        esac
        base=$(mktemp -d /mnt/wsl/disk-copy-98.XXXXXXXX)
        trap 'unmount_volumes "$base"' EXIT
        chmod 755 "$base"
        candidates=$(lsblk -lnpo NAME,TYPE -- "$disk" | awk '$2 == "part" {print $1}')
        [ -n "$candidates" ] || candidates="$disk"
        count=0
        detected=''
        for device in $candidates; do
            signature=$(probe "$device")
            type=$(printf '%s\n' "$signature" | sed -n 's/^TYPE=//p')
            pv_offset=0
            if [ -z "$type" ] && [ "$device" = "$disk" ]; then
                pv_offset=$(legacy_pv_offset)
                [ -z "$pv_offset" ] || type=LVM2_member
            fi
            [ -n "$type" ] || continue
            detected="$detected $device ($type)"
            options=nosuid,nodev,noexec
            readonly=ro
            case "$type" in
                ext3|ext4) readonly=ro,noload ;;
                xfs) readonly=ro,norecovery ;;
                btrfs) readonly=ro,nologreplay ;;
                vfat|exfat|ntfs) options="$options,uid=$5,gid=$6,umask=022" ;;
                ext2|f2fs|hfsplus|iso9660|udf|squashfs|LVM2_member) ;;
                *) continue ;;
            esac
            number=$((count+1))
            dir="$base/volume-$number"
            mkdir "$dir"
            if [ "$type" = LVM2_member ]; then
                if details=$(mount_lvm "$device" "$pv_offset" "$dir" "$base/loop-$(basename "$device")"); then
                    printf '%s\tLVM\t%s\n' "$dir" "$details"
                    count=$number
                else
                    rmdir "$dir"
                fi
                continue
            fi
            mounted=''
            drivers="$type"
            [ "$type" != ntfs ] || drivers='ntfs3 ntfs'
            for driver in $drivers; do
                if mount -t "$driver" -o "rw,$options" -- "$device" "$dir"; then
                    mounted=1
                    break
                fi
                if mount -t "$driver" -o "$readonly,$options" -- "$device" "$dir"; then
                    mounted=1
                    break
                fi
            done
            mode=rw
            if [ -z "$mounted" ]; then
                case "$type" in
                    ext2|ext3|ext4)
                        loop=$(read_only_loop "$device" 0 0 "$base/loop-$(basename "$device")-ext") || exit 1
                        if mount_fuse_ext "$loop" "$dir"; then
                            mounted=1
                            mode=ro
                        fi
                        ;;
                esac
            fi
            if [ -n "$mounted" ]; then
                if findmnt -n -o OPTIONS --mountpoint "$dir" | grep -Eq '(^|,)ro(,|$)'; then
                    mode=ro
                fi
                printf '%s\t%s\t%s\t%s\n' "$dir" "$device" "$type" "$mode"
                count=$number
            else
                rmdir "$dir"
            fi
        done
        if [ "$count" -eq 0 ]; then
            if [ -n "$detected" ]; then
                printf 'WSL could not mount the detected filesystems:%s\n' "$detected" >&2
            else
                printf 'WSL sees %s, but found no readable filesystem on the disk or its partitions.\n' "$disk" >&2
            fi
            exit 1
        fi
        trap - EXIT
        ;;
    *) exit 2 ;;
esac
