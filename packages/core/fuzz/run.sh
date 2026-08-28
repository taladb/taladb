#!/usr/bin/env bash
# Time-boxed fuzz run over every target.
#
# This is a smoke run, not a soak: it exists to catch a regression that any
# seeded input reaches, and to keep the targets building. Finding genuinely new
# bugs needs hours, not the seconds CI can spend — run this locally with a large
# SECONDS_PER_TARGET when that is the goal.
#
#   ./run.sh            # 60s per target
#   SECONDS_PER_TARGET=1800 ./run.sh
set -euo pipefail

cd "$(dirname "$0")"
: "${SECONDS_PER_TARGET:=60}"

# Seeds are committed; the working corpus is not. Copy rather than point
# libFuzzer at `seeds/` directly, so a run never writes derived inputs back into
# the committed set.
targets=$(sed -n 's/^name = "\(fuzz_[a-z_]*\)"$/\1/p' Cargo.toml)

status=0
for t in $targets; do
  mkdir -p "corpus/$t"
  if [ -d "seeds/$t" ]; then
    cp -n seeds/"$t"/* "corpus/$t/" 2>/dev/null || true
  fi
  echo "=== $t (${SECONDS_PER_TARGET}s) ==="
  if ! cargo +nightly fuzz run "$t" -- \
        -max_total_time="$SECONDS_PER_TARGET" \
        -rss_limit_mb=4096 \
        -print_final_stats=1; then
    echo "FAILED: $t"
    status=1
  fi
done

# `cargo fuzz` exits non-zero on a crash, but check for artifacts too: a target
# that dies in a way libFuzzer reports without a non-zero exit still leaves one.
if compgen -G "artifacts/*/*" > /dev/null; then
  echo "crash artifacts present:"
  ls -1 artifacts/*/*
  status=1
fi

exit $status
