# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
FROM rust:slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS build
WORKDIR /build/server
COPY server/Cargo.toml server/Cargo.lock ./
COPY server/build.rs ./
COPY server/protocol ./protocol
COPY server/vendor ./vendor
COPY server/src ./src
COPY server/resources ./resources
ARG BUILD_REVISION=development
ENV SUB2OPS_BUILD_REVISION=$BUILD_REVISION
ENV RUSTFLAGS="--remap-path-prefix=/build=/src --remap-path-prefix=/usr/local/cargo=/cargo"
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/server/target \
    cargo build --release --locked \
    && install -m 0755 target/release/sub2ops-server /usr/local/bin/sub2ops-server

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && apt-get clean \
    && find /var/lib/apt/lists -mindepth 1 -delete \
    && groupadd --gid 10001 companion \
    && useradd --system --uid 10001 --gid 10001 companion \
    && install -d -o 10001 -g 10001 -m 0700 /data
COPY --from=build /usr/local/bin/sub2ops-server /usr/local/bin/sub2ops-server
COPY server/resources/modeltrace/LICENSE.txt /usr/share/doc/sub2ops/ModelTrace-LICENSE
COPY server/resources/lm-detector/LICENSE.txt /usr/share/doc/sub2ops/LM-Detector-LICENSE
ARG BUILD_REVISION=development
LABEL org.opencontainers.image.title="Sub2Ops Companion" \
      org.opencontainers.image.revision=$BUILD_REVISION \
      org.opencontainers.image.version="0.1.109"
ENV APP_PORT=18081
USER 10001:10001
EXPOSE 18081
ENTRYPOINT ["/usr/local/bin/sub2ops-server"]
CMD ["serve"]
