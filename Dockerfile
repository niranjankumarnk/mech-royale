# Mech Royale bot (mechfight/1). Built on linux/amd64 with the network on; runs with no network,
# a read-only root, 1 CPU, 512 MB and the map at /arena.
# Base images are pinned by tag and digest; crate versions are pinned by Cargo.lock (--locked).
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin bot

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
COPY --from=build /src/target/release/bot /bot/bot
ENTRYPOINT ["/bot/bot"]
