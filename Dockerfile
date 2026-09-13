# Multi-stage build for the api-gateway deployment image (Northflank).
#
# Deliberately simple: one COPY of the source, one release build. There is no
# cargo-chef / dummy-source dependency-caching trick here on purpose -- those
# are faster on rebuild but fragile, and the whole workspace compiles in a few
# minutes anyway. Optimize only if build time actually becomes a problem.

FROM rust:1.98-bookworm AS builder

WORKDIR /app

# Build tooling. We use rustls everywhere (no OpenSSL), but pkg-config is
# needed by the `ring` crate that rustls pulls in.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY . .

# Only the two binaries we actually deploy. sqlx uses the runtime query API
# (not the `query!` macros), so no DATABASE_URL is needed at build time.
# `sqlx::migrate!` does embed migrations at compile time, which is why the
# whole source tree is copied above rather than just the binary crates.
RUN cargo build --release --locked --bin api-gateway --bin xtask

# ---------------------------------------------------------------------------

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/api-gateway /usr/local/bin/api-gateway
COPY --from=builder /app/target/release/xtask        /usr/local/bin/xtask
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# 0.0.0.0 matters: the local default is 127.0.0.1, which would make the
# container unreachable from outside.
ENV BIND_ADDR=0.0.0.0:8080 \
    RUST_LOG=info \
    RUN_MIGRATIONS=true

EXPOSE 8080

# /healthz is liveness only (it does not touch the database), so a slow or
# unreachable DB won't flap the container.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["api-gateway"]
