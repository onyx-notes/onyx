# Build the Onyx sync server as a small self-contained image.
FROM rust:1.90-slim AS build
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p onyx-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home onyx
COPY --from=build /src/target/release/onyx-server /usr/local/bin/onyx-server
USER onyx
VOLUME ["/data"]
EXPOSE 7677
ENTRYPOINT ["onyx-server", "--data-dir", "/data", "--listen", "0.0.0.0:7677"]
