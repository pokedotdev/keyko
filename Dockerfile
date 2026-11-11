# The litekeys key server, a static binary on an empty image. See deploy/ and the README.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p litekeys-server

FROM scratch
COPY --from=build /src/target/release/litekeys-server /litekeys-server
USER 65534:65534
# Loopback only: in deploy/compose.yaml it shares the Tailscale sidecar's network namespace, so
# only `tailscale serve` reaches it.
ENV LITEKEYS_LISTEN=127.0.0.1:8787
ENTRYPOINT ["/litekeys-server"]
