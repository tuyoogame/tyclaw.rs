#!/bin/bash
cd "$(dirname "$0")"

PID_FILE="workspace/.tyclaw.pid"

if [ ! -f "$PID_FILE" ]; then
    echo "No PID file found, TyClaw is not running"
    exit 0
fi

PID=$(cat "$PID_FILE")
if kill -0 "$PID" 2>/dev/null; then
    kill "$PID"
    echo "Stopped (pid=$PID)"
else
    echo "Not running (stale pid=$PID)"
fi
rm -f "$PID_FILE"
