# Build stage: run the unit tests, then compile a static musl binary.
FROM docker.io/library/rust:1-alpine AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo test --locked \
    && cargo build --release --locked \
    && mkdir /logs

# Runtime stage: nothing but the binary and an empty log directory.
FROM scratch
COPY --from=builder /src/target/release/rust-log-generator /rust-log-generator
# /logs is the program's default LOG_DIR. It belongs to the unprivileged user
# the program runs as, and a new named volume mounted here inherits that
# ownership. The user is numeric so Kubernetes can verify runAsNonRoot.
COPY --from=builder --chown=65532:65532 /logs /logs
USER 65532:65532
VOLUME /logs
ENTRYPOINT ["/rust-log-generator"]
