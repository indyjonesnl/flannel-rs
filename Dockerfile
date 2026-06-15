FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release -p flanneld

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/flanneld /usr/local/bin/flanneld
ENTRYPOINT ["/usr/local/bin/flanneld"]
