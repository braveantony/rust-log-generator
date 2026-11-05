# Build stage: run the unit and end-to-end tests, then compile a static musl
# binary.
# The builder is pinned by digest so that every build uses the same compiler;
# a tag such as `1-alpine` moves with each new Rust or Alpine release. The
# digest is that of the multi-platform image index. To update, pick a newer
# tag and take its digest from
#   skopeo inspect --raw docker://docker.io/library/rust:<tag> | sha256sum
FROM docker.io/library/rust:1.99.1-alpine3.24@sha256:f432d4b8663fbc50b29e33846bf6a1fb9f17f142023598899d641cddf57c6b22 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
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
