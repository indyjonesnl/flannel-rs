FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release -p flanneld

FROM debian:bookworm-slim
# iptables ships both iptables-legacy and iptables-nft binaries on bookworm;
# flanneld picks the one matching kube-proxy's active backend at runtime.
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 ca-certificates iptables \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/flanneld /usr/local/bin/flanneld
ENTRYPOINT ["/usr/local/bin/flanneld"]
