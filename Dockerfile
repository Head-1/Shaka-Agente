# Build reproducível do binário Shaka.
FROM rust:1.98-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --bin shaka

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.title="Shaka" \
      org.opencontainers.image.description="Agente Rust governado, auditável e deny-by-default" \
      org.opencontainers.image.source="https://github.com/Head-1/Shaka-Agente"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 shaka
WORKDIR /app
COPY --from=builder /app/target/release/shaka /usr/local/bin/shaka
RUN chmod 0555 /usr/local/bin/shaka \
    && mkdir -p /app/data \
    && chown -R shaka:shaka /app \
    && chmod 0750 /app/data
USER 10001:10001
ENV SHAKA_DATABASE=/app/data/shaka.db
ENV SHAKA_SKILLS_FILE=/app/data/skills.json
ENV SHAKA_AUDIT_REQUIRED=true
ENV SHAKA_API_BIND=0.0.0.0:8080
EXPOSE 8080
STOPSIGNAL SIGTERM
VOLUME ["/app/data"]
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 CMD ["/usr/local/bin/shaka", "doctor"]
ENTRYPOINT ["/usr/local/bin/shaka"]
CMD ["serve"]
