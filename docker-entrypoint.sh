#!/bin/sh
# Container entrypoint.
#
# Applies pending migrations, then execs the real command (default: the
# api-gateway).
#
# Running migrations here rather than as a separate deploy step is safe even
# with several replicas: sqlx takes a Postgres advisory lock, so concurrent
# migrators serialize instead of racing. Set RUN_MIGRATIONS=false if you'd
# rather run `xtask migrate` as a dedicated Northflank job.

set -eu

if [ "${RUN_MIGRATIONS:-true}" = "true" ]; then
    if [ -z "${DATABASE_URL:-}" ]; then
        echo "docker-entrypoint: RUN_MIGRATIONS=true but DATABASE_URL is not set." >&2
        echo "docker-entrypoint: set DATABASE_URL, or set RUN_MIGRATIONS=false to skip." >&2
        exit 1
    fi

    echo "docker-entrypoint: applying migrations"
    xtask migrate
fi

echo "docker-entrypoint: starting $*"
exec "$@"
