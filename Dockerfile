# ── Build ────────────────────────────────────────────────────────────────────
FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
COPY data ./data
RUN cargo build --release --locked

# ── abgx360 (GPL; built from source and run only as a separate program) ─────────
FROM debian:trixie-slim AS abgx
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates build-essential autoconf automake libcurl4-openssl-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
ARG ABGX_REPO=https://github.com/WB2024/X360Forge
ARG ABGX_REF=fdb0aeac1e81b51051ec130eef617675b9d5475c
RUN git clone "$ABGX_REPO" /x && cd /x && git checkout "$ABGX_REF" \
    && cd x_tool/abgx360-src && ./configure && make -j"$(nproc)" \
    && mkdir -p /out && install -m755 abgx360 /out/abgx360

# ── Runtime ──────────────────────────────────────────────────────────────────
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates util-linux fdisk dosfstools exfatprogs e2fsprogs ntfs-3g zstd partclone libcurl4t64 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/rustybox /usr/local/bin/rustybox
COPY --from=abgx /out/abgx360 /usr/local/bin/abgx360

ENV RUSTYBOX_BIND=0.0.0.0:8080 \
    RUSTYBOX_TLS_BIND=0.0.0.0:8443 \
    RUSTYBOX_CONFIG_DIR=/config

VOLUME ["/config"]
EXPOSE 8080 8443
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["rustybox", "healthcheck"]
ENTRYPOINT ["rustybox"]
CMD ["serve"]
