#!/usr/bin/env bash
# Generate a throw-away vault for benchmarking.
#
#   scripts/bench-vault.sh <dir> <files> <rounds> <edits>
#
# Writes <files> small files and snapshots them, then <rounds> times rewrites
# the next <edits> files (round-robin) and snapshots again. So the log ends
# with 1 + <rounds> segments and <files> + <rounds> * <edits> entries.
#
# The same arguments always give the same paths, contents and history shape.
# Segment names still differ from run to run, because entries record mtimes.
set -euo pipefail

dir=$1 files=$2 rounds=$3 edits=$4
syncit=${SYNCIT:-$(cd "$(dirname "$0")/.." && pwd)/target/release/syncit}

mkdir -p "$dir"
cd "$dir"
"$syncit" init . >/dev/null

# 100 files to a folder.
for ((d = 0; d <= (files - 1) / 100; d++)); do mkdir "d$d"; done
for ((i = 0; i < files; i++)); do
    echo "file $i version 0" >"d$((i / 100))/f$i.txt"
done
"$syncit" snapshot >/dev/null

for ((r = 1; r <= rounds; r++)); do
    for ((j = 0; j < edits; j++)); do
        i=$((((r - 1) * edits + j) % files))
        echo "file $i version $r" >"d$((i / 100))/f$i.txt"
    done
    "$syncit" snapshot >/dev/null
done

echo "$dir: $files files, $(find .syncit/log -type f | wc -l | tr -d ' ') segments" >&2
