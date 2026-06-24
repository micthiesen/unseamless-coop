#!/usr/bin/env bash
# Two-process TCP harness: a host + client run the real Peer/Session logic over a localhost socket
# (real serialization, real cross-process concurrency). This is the host half of the planned
# layer-3 debug bridge. The workspace default target is windows-gnu (for the DLL), which can't
# execute on macOS, so we override to the host triple.
#   scripts/harness-tcp.sh [port]      # default port 47620
set -euo pipefail
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
PORT="${1:-47620}"

# Build once so both ends start without a compile race.
cargo build -q -p harness --target "$HOST_TRIPLE"
BIN="target/$HOST_TRIPLE/debug/harness"

"$BIN" tcp-listen "$PORT" &
HOST_PID=$!
trap 'kill "$HOST_PID" 2>/dev/null || true' EXIT

"$BIN" tcp-connect "$PORT"
wait "$HOST_PID"
