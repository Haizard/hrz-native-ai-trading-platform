#!/bin/sh
# Hostless start command.
#
# Hostless may run `start_command` through /bin/bash in a build environment
# rather than through the Dockerfile ENTRYPOINT. This script works in both:
#   * Docker image: /usr/local/bin/api-gateway
#   * Railpack/auto Rust build: ./target/release/api-gateway
# and it applies the same PORT -> BIND_ADDR contract as docker-entrypoint.sh.

set -eu

if [ -z "${BIND_ADDR:-}" ]; then
    BIND_ADDR="0.0.0.0:${PORT:-8080}"
fi
export BIND_ADDR PORT="${PORT:-8080}"

if command -v api-gateway >/dev/null 2>&1; then
    exec api-gateway
fi

for candidate in \
    ./target/release/api-gateway \
    /app/target/release/api-gateway \
    /app/api-gateway \
    /usr/local/bin/api-gateway
do
    if [ -x "$candidate" ]; then
        exec "$candidate"
    fi
done

# Last resort for build layouts that put the binary somewhere unexpected.
found=$(find . -maxdepth 5 -type f -name api-gateway -perm -111 2>/dev/null | head -n 1 || true)
if [ -n "$found" ]; then
    exec "$found"
fi

echo "start.sh: api-gateway binary not found" >&2
echo "start.sh: looked in PATH, target/release, /app and /usr/local/bin" >&2
exit 127
