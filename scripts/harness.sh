#!/usr/bin/env bash
# Run the two-peer side-channel harness on the host (no game, no Steam). The workspace default
# target is windows-gnu (for the DLL), which can't execute on macOS, so we override to the host
# triple. unseamless-core/harness have no game deps, so they run natively.
#   scripts/harness.sh [scenario]   # handshake | version-mismatch | config-sync |
#                                   # session-action | log-forward | all (default)
set -euo pipefail
HOST="$(rustc -vV | sed -n 's/^host: //p')"
exec cargo run -q -p harness --target "$HOST" -- "$@"
