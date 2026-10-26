# syntax=docker/dockerfile:1.7
# Targets: server (default) and cli. Build with `make docker-build`.
ARG RUST_IMAGE=rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY . .
# --locked and a fixed path prefix keep the binaries independent of the build host
ENV RUSTFLAGS="--remap-path-prefix=/src=/pyn --remap-path-prefix=/usr/local/cargo=/cargo"
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p pyn-server -p pyn-cli \
    && install -D target/release/pyn-server /out/pyn-server \
    && install -D target/release/pyn /out/pyn

FROM ${RUNTIME_IMAGE} AS runtime-base
ARG SOURCE_DATE_EPOCH=0
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 pyn \
    && useradd --system --uid 10001 --gid pyn --no-create-home --shell /usr/sbin/nologin pyn

FROM runtime-base AS cli
COPY --from=build /out/pyn /usr/local/bin/pyn
USER pyn
ENV PYN_CONFIG_DIR=/tmp/pyn-config
ENTRYPOINT ["pyn"]

FROM runtime-base AS server
ARG VERSION=dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="pyn-server" \
      org.opencontainers.image.source="https://github.com/avalon-initiative/pyn" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"
COPY --from=build /out/pyn-server /usr/local/bin/pyn-server
# /data is the only writable path besides /tmp; mount a volume there
RUN mkdir /data && chown pyn:pyn /data
VOLUME /data
ENV PYN_ADDR=0.0.0.0:7878 PYN_DATA_DIR=/data
EXPOSE 7878
USER pyn
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:7878/healthz || exit 1
ENTRYPOINT ["pyn-server"]
