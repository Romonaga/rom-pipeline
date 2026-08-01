#!/usr/bin/env bash

set -uo pipefail

if (( $# < 3 || $# > 4 )); then
  printf 'usage: %s MANIFEST DESTINATION STATE_DIR [ARCHIVE_IDENTIFIER]\n' "$0" >&2
  exit 2
fi

readonly manifest=$1
readonly destination=$2
readonly state_dir=$3
readonly identifier=${4:-3ds-cia-undatted-encrypted}
readonly parallel=3
readonly progress_log="$state_dir/progress.log"

mkdir -p "$destination" "$state_dir"
exec 9>"$state_dir/lock"
if ! flock -n 9; then
  printf 'another exact CIA replacement download holds the lock\n'
  exit 1
fi

log_line() {
  printf '%s\n' "$1" | tee -a "$progress_log"
}

readonly total=$(awk -F '\t' '!/^#/ && $1 != "title_id" && NF {count++} END {print count}' "$manifest")
readonly total_bytes=$(awk -F '\t' '!/^#/ && $1 != "title_id" && NF {bytes += $4} END {printf "%.0f", bytes}' "$manifest")
readonly available_bytes=$(df -PB1 "$destination" | awk 'NR == 2 {print $4}')
remaining_bytes=0
while IFS=$'\t' read -r title_id selection remote_name expected_size expected_md5 expected_sha1; do
  if [[ -z "$title_id" || "$title_id" == \#* || "$title_id" == 'title_id' ]]; then
    continue
  fi
  target="$destination/$remote_name"
  part="$destination/.$remote_name.part"
  if [[ -f "$target" && $(stat -c %s "$target" 2>/dev/null || printf '0') == "$expected_size" ]]; then
    continue
  fi
  part_size=$(stat -c %s "$part" 2>/dev/null || printf '0')
  if (( part_size > expected_size )); then
    part_size=0
  fi
  remaining_bytes=$((remaining_bytes + expected_size - part_size))
done <"$manifest"
readonly remaining_bytes
if (( available_bytes < remaining_bytes )); then
  log_line "ERROR insufficient space required=$remaining_bytes available=$available_bytes"
  exit 1
fi

: >"$state_dir/failures.tsv"
: >"$progress_log"
log_line "START files=$total bytes=$total_bytes remaining_bytes=$remaining_bytes parallel=$parallel destination=$destination"

download_one() {
  local index=$1 title_id=$2 remote_name=$3 expected_size=$4 expected_md5=$5 expected_sha1=$6
  local encoded url target part actual_size actual_md5 actual_sha1
  encoded=$(jq -nr --arg name "$remote_name" '$name | @uri')
  url="https://archive.org/download/$identifier/$encoded"
  target="$destination/$remote_name"
  part="$destination/.$remote_name.part"

  if [[ -f "$target" ]]; then
    actual_size=$(stat -c %s "$target" 2>/dev/null || printf '0')
    actual_sha1=$(sha1sum -- "$target" 2>/dev/null | awk '{print $1}')
    if [[ "$actual_size" == "$expected_size" && "$actual_sha1" == "$expected_sha1" ]]; then
      log_line "SKIP [$index/$total] title_id=$title_id bytes=$actual_size: $remote_name"
      return 0
    fi
  fi

  if [[ -f "$part" ]]; then
    actual_size=$(stat -c %s "$part" 2>/dev/null || printf '0')
    if (( actual_size > expected_size )); then
      truncate -s 0 "$part"
    fi
  fi

  if ! curl --fail --location --silent --show-error \
    --retry 50 --retry-all-errors --retry-delay 10 \
    --connect-timeout 30 --speed-time 300 --speed-limit 1024 \
    --continue-at - --remote-time --output "$part" "$url"; then
    printf '%s\t%s\ttransfer_failed\n' "$title_id" "$remote_name" >>"$state_dir/failures.tsv"
    log_line "FAIL [$index/$total] transfer: $remote_name"
    return 1
  fi

  actual_size=$(stat -c %s "$part" 2>/dev/null || printf '0')
  actual_md5=$(md5sum -- "$part" | awk '{print $1}')
  actual_sha1=$(sha1sum -- "$part" | awk '{print $1}')
  if [[ "$actual_size" != "$expected_size" || "$actual_md5" != "$expected_md5" || "$actual_sha1" != "$expected_sha1" ]]; then
    printf '%s\t%s\tchecksum_failed\n' "$title_id" "$remote_name" >>"$state_dir/failures.tsv"
    log_line "FAIL [$index/$total] verification: $remote_name"
    return 1
  fi
  mv -f -- "$part" "$target"
  log_line "DONE [$index/$total] title_id=$title_id bytes=$actual_size: $remote_name"
}

active=0
index=0
while IFS=$'\t' read -r title_id failed_zip remote_name expected_size expected_md5 expected_sha1; do
  if [[ -z "$title_id" || "$title_id" == \#* || "$title_id" == 'title_id' ]]; then
    continue
  fi
  index=$((index + 1))
  download_one "$index" "$title_id" "$remote_name" "$expected_size" "$expected_md5" "$expected_sha1" &
  active=$((active + 1))
  if (( active >= parallel )); then
    wait -n || true
    active=$((active - 1))
  fi
done <"$manifest"

while (( active > 0 )); do
  wait -n || true
  active=$((active - 1))
done

failure_count=$(wc -l <"$state_dir/failures.tsv")
if (( failure_count > 0 )); then
  log_line "INCOMPLETE failures=$failure_count"
  exit 1
fi
log_line "COMPLETE files=$total destination=$destination"
