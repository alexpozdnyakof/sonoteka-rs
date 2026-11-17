#!/bin/bash
# Cross-build for the Intel MBP and redeploy through the launchd agent.
set -euo pipefail
cd "$(dirname "$0")"
TARGET=x86_64-apple-darwin

echo "==> build ($TARGET)"
cargo build --release --target "$TARGET"

echo "==> scp to macbookpro"
scp -q "target/$TARGET/release/sonoteka-rs" macbookpro:sonoteka/

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
    pgrep -x sonoteka-rs >/dev/null && echo "deploy: OK" || { echo "deploy: FAIL"; exit 1; }'
