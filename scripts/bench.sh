#!/usr/bin/env bash
# Time syncit commands on two vaults holding the same files: "flat" has one
# snapshot, "deep" has a long history. The gap between the two columns is
# what the log costs; the flat column is everything else.
#
#   scripts/bench.sh
#   FILES=1000 ROUNDS=10 EDITS=100 scripts/bench.sh    # a smaller vault
#
# To compare a change: run on main, run on the branch, compare the tables.
# The vaults are kept in target/bench and reused, so both runs read the same
# bytes. After a change to the on-disk format, `rm -rf target/bench`.
set -euo pipefail
cd "$(dirname "$0")/.."

files=${FILES:-10000} rounds=${ROUNDS:-100} edits=${EDITS:-1000} runs=${RUNS:-5}

cargo build --release --quiet
syncit=$PWD/target/release/syncit

flat=target/bench/flat-$files
deep=target/bench/deep-$files-$rounds-$edits

# Build in a .part folder, so an interrupted run is not mistaken for a vault.
vault() {
    [ -d "$1" ] && return
    rm -rf "$1.part"
    scripts/bench-vault.sh "$1.part" "$files" "$2" "$3"
    mv "$1.part" "$1"
}
vault "$flat" 0 0
vault "$deep" "$rounds" "$edits"

# The fastest of $runs runs, in seconds.
best() {
    local dir=$1
    shift
    for ((n = 0; n < runs; n++)); do
        (cd "$dir" && TIMEFORMAT=%R && time "$@" >/dev/null 2>&1) 2>&1
    done | sort -n | head -1
}

echo "$files files; deep has $(find "$deep/.syncit/log" -type f | wc -l | tr -d ' ') segments; best of $runs, seconds"
printf '%-16s %8s %8s\n' command flat deep
for cmd in "status" "snapshot" "log d0/f0.txt"; do
    # shellcheck disable=SC2086
    printf '%-16s %8s %8s\n' "$cmd" "$(best "$flat" "$syncit" $cmd)" "$(best "$deep" "$syncit" $cmd)"
done
