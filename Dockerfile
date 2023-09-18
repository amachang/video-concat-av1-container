# Build
FROM rust:1.72 as builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN apt update && apt install -y libssl-dev
RUN cargo build --release

# Runtime
FROM rust:1.72

COPY --from=builder /app/target/release/concat_video /usr/local/bin/concat_video
RUN mkdir data

ENTRYPOINT ["concat_video"]
