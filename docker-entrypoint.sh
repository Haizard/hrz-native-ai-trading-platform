#!/bin/sh
# Container entrypoint.
#
# Applies pending migrations, then execs the real command (default: the
# api-gateway).
#
# Running migrations here rather than as a separate deploy step is safe even
# with several replicas: sqlx takes a Postgres advisory lock, so concurrent
# migrators serialize instead of racing. Set RUN_MIGRATIONS=false if you'd
# rather run `xtask migrate` as a dedicated deployment job.
#
# Two launch contracts matter on managed hosts:
#   * a web service must listen on $PORT and on 0.0.0.0; hosts that do not set
#     BIND_ADDR for us still get the right address from PORT.
#   * a host that overrides CMD with an empty list would otherwise reach
#     `exec "$@"` with no arguments, which exits 0 -- the exact "crashed on
#     start, exit code 0" signature. Default to the gateway when no command
#     was supplied.

set -eu

# PORT is the host's contract. BIND_ADDR remains the gateway's own spelling,
# and an explicit operator override still wins.
if [ -z "${BIND_ADDR:-}" ]; then
    BIND_ADDR="0.0.0.0:${PORT:-8080}"
fi
export BIND_ADDR PORT="${PORT:-8080}"

# No command means run the server, never exit successfully.
if [ "$#" -eq 0 ]; then
    set -- api-gateway
fi

if [ "${RUN_MIGRATIONS:-true}" = "true" ]; then
    if [ -z "${DATABASE_URL:-}" ]; then
        echo "docker-entrypoint: RUN_MIGRATIONS=true but DATABASE_URL is not set." >&2
        echo "docker-entrypoint: set DATABASE_URL, or set RUN_MIGRATIONS=false to skip." >&2
        exit 1
    fi

    echo "docker-entrypoint: applying migrations"
    xtask migrate
fi

echo "docker-entrypoint: starting $* on ${BIND_ADDR}"
exec "$@"
