#!/usr/bin/env bash
# Run the platform-independent core's tests on the macOS host. The workspace's default target
# is windows-gnu (for the DLL), which can't execute on macOS, so we override to the host triple.
# unseamless-core has no game/OS deps, so it compiles and runs natively here.
set -euo pipefail
HOST="$(rustc -vV | sed -n 's/^host: //p')"
exec cargo test -p unseamless-core --target "$HOST" "$@"
