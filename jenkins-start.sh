#!/bin/bash
cd "$(dirname "$0")"

BUILD_BINARY="./target/release/tyclaw"
RUN_DIR="workspace"
WORKS_DIR="/home/tuyoo/tyclaw/works"
BINARY="$RUN_DIR/tyclaw"
LOG_FILE="$RUN_DIR/logs/tyclaw.log"
PID_FILE="$RUN_DIR/.tyclaw.pid"

if [ ! -x "$BUILD_BINARY" ]; then
    echo "Error: Binary not found at $BUILD_BINARY"
    exit 1
fi

if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "TyClaw is already running (pid=$(cat "$PID_FILE"))"
    exit 1
fi

cp "$BUILD_BINARY" "$BINARY"
echo "Copied binary to $BINARY"

mkdir -p "$RUN_DIR/logs"
echo "[$(date)] Starting TyClaw..." >> "$LOG_FILE"

nohup "$BINARY" \
    --run-dir "$RUN_DIR" \
    --works-dir "$WORKS_DIR" \
    --dingtalk \
    >> "$LOG_FILE" 2>&1 &

echo $! > "$PID_FILE"
echo "TyClaw started (pid=$(cat "$PID_FILE")), log: $LOG_FILE"
