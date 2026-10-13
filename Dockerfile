ARG RUST_VERSION=1.96.0
ARG TARGET_PLATFORM=linux/arm/v7
FROM --platform=${TARGET_PLATFORM} rust:${RUST_VERSION}-slim-bookworm AS builder

WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential cmake pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked \
    && install -m 755 target/release/neu-login /usr/local/bin/neu-login

FROM --platform=${TARGET_PLATFORM} debian:bookworm-slim AS runtime

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl libgcc-s1 tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 neu-login \
    && useradd --uid 10001 --gid 10001 --home-dir /data --no-create-home --shell /usr/sbin/nologin neu-login \
    && mkdir -p /data /backgrounds \
    && chown 10001:10001 /data /backgrounds \
    && chmod 700 /data

COPY --from=builder /usr/local/bin/neu-login /usr/local/bin/neu-login

ENV NEU_LISTEN=0.0.0.0:11099 \
    NEU_DATA_DIR=/data \
    TZ=Asia/Shanghai \
    RUST_LOG=neu_login=info

WORKDIR /data
USER 10001:10001
VOLUME ["/data"]
EXPOSE 11099/tcp
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error --noproxy '*' "http://127.0.0.1:${NEU_LISTEN##*:}/healthz" || exit 1
ENTRYPOINT ["/usr/local/bin/neu-login"]
