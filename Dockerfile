# Build
FROM rust:1.72 as builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN apt update && apt install -y libssl-dev
RUN cargo install ab-av1
RUN cargo build --release

# Runtime
FROM rust:1.72

COPY --from=builder /usr/local/cargo/bin/ab-av1 /usr/local/bin/ab-av1
COPY --from=builder /app/target/release/concat_video /usr/local/bin/concat_video
COPY --from=mwader/static-ffmpeg:6.0 /ffmpeg /usr/local/bin/
COPY --from=mwader/static-ffmpeg:6.0 /ffprobe /usr/local/bin/

# if needs more quality, edit here like:
# MIX_CRF=20 ENOUGH_VMAF=98
ENV MIN_CRF 40
ENV ENOUGH_VMAF 80

RUN mkdir data output

ENTRYPOINT ["concat_video"]

