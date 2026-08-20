# Build reproducível do binário Shaka.
FROM rust:1.97-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --bin shaka

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 shaka
WORKDIR /app
COPY --from=builder /app/target/release/shaka /usr/local/bin/shaka
RUN mkdir -p /app/data && chown -R shaka:shaka /app
USER shaka
ENV SHAKA_DATABASE=/app/data/shaka.db
ENV SHAKA_SKILLS_FILE=/app/data/skills.json
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 CMD ["/usr/local/bin/shaka", "doctor"]
ENTRYPOINT ["/usr/local/bin/shaka"]
