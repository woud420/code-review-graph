FROM rust:1.100.0-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY rust/Cargo.toml rust/LICENSE rust/README.md ./rust/
COPY rust/assets ./rust/assets
COPY rust/src ./rust/src
RUN cargo build --release --locked --bin code-review-graph --bin crg-daemon

FROM debian:bookworm-slim
ENV HOME=/home/crg \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0=safe.directory \
    GIT_CONFIG_VALUE_0=/workspace

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 crg \
    && useradd --uid 10001 --gid crg --create-home crg \
    && mkdir /workspace \
    && chown crg:crg /workspace

COPY --from=builder /build/target/release/code-review-graph /build/target/release/crg-daemon /usr/local/bin/
USER crg:crg
WORKDIR /workspace
ENTRYPOINT ["code-review-graph"]
CMD ["--help"]
