# Build
FROM rust:1.72 as builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN rustup target add x86_64-unknown-linux-musl
RUN cargo build --release --target x86_64-unknown-linux-musl

# Runtime
FROM alpine:3.18.3

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/concat_video /usr/local/bin/concat_video

CMD ["concat_video"]
