#!/bin/bash
# Cross-build for the Intel MBP and redeploy through the launchd agent.
set -euo pipefail
cd "$(dirname "$0")"
TARGET=x86_64-apple-darwin

echo "==> build ($TARGET)"
cargo build --release --target "$TARGET"

echo "==> scp to macbookpro"
# The launchd job runs a stable launcher binary (sonoteka-rs-launcher) so
# macOS TCC grants survive rebuilds; the real binary it execs is replaced.
scp -q "target/$TARGET/release/sonoteka-rs" macbookpro:sonoteka/sonoteka-server

echo "==> restart service"
ssh macbookpro 'UID_NUM=$(id -u)
    LOG=~/sonoteka/server.log
    BEFORE=$(wc -l < "$LOG" 2>/dev/null || echo 0)
    launchctl kickstart -k gui/$UID_NUM/ru.sonoteka.rs
    # wait for a FRESH "listening" line (scan of a big library takes a while)
    for i in $(seq 1 45); do
        sleep 1
        NOW=$(wc -l < "$LOG" 2>/dev/null || echo 0)
        if [ "$NOW" -gt "$BEFORE" ] && tail -1 "$LOG" | grep -q "listening on"; then
            break
        fi
    done
    tail -2 "$LOG"
    lsof -nP -iTCP:4533 -sTCP:LISTEN >/dev/null 2>&1 && echo "deploy: OK" || { echo "deploy: FAIL"; exit 1; }'
