ARG RUST_ALPINE_DIGEST=sha256:a41f7740f8b45d45795624eec13a8b42263cc700f19f7e4e86e04d3dda08a479

FROM --platform=$TARGETPLATFORM rust:1.96-alpine@${RUST_ALPINE_DIGEST} AS builder

RUN apk add --no-cache ca-certificates binutils musl-dev

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --bin helium-service && \
    mkdir -p /out && \
    cp target/release/helium-service /out/helium-service && \
    strip /out/helium-service

RUN set -eux; \
    mkdir -p /rootfs/etc /rootfs/etc/ssl/certs /rootfs/usr/local/bin /rootfs/app /rootfs/tmp/helium-dictionaries; \
    echo "helium:x:1000:1000:helium:/app:/sbin/nologin" > /rootfs/etc/passwd; \
    echo "helium:x:1000:" > /rootfs/etc/group; \
    chmod 1777 /rootfs/tmp; \
    chown 1000:1000 /rootfs/tmp/helium-dictionaries

FROM scratch

COPY --from=builder /rootfs/ /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /out/helium-service /usr/local/bin/helium-service

USER 1000:1000
WORKDIR /app

EXPOSE 8000

HEALTHCHECK --start-period=3s --start-interval=5s --interval=5m --timeout=10s \
    CMD ["helium-service", "healthcheck"]

ENTRYPOINT ["helium-service"]
