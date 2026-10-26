# voice-pipe image. Usage: see "Run with Docker" in the README.

FROM rust:1-bookworm AS build

# LiveKit's prebuilt WebRTC needs clang 21 or newer; Debian has clang 14.
ARG LLVM_VERSION=23
RUN curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key \
                -o /etc/apt/trusted.gpg.d/apt.llvm.org.asc \
        && echo "deb http://apt.llvm.org/bookworm/ llvm-toolchain-bookworm-${LLVM_VERSION} main" \
                > /etc/apt/sources.list.d/llvm.list \
        && apt-get update \
        && apt-get install -y --no-install-recommends \
                cmake pkg-config clang-${LLVM_VERSION} \
        && rm -rf /var/lib/apt/lists/*
ENV CC=clang-${LLVM_VERSION} CXX=clang++-${LLVM_VERSION}

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
COPY src ./src
RUN cargo build --release --locked \
        && cp target/release/voice-pipe /voice-pipe

# The binary needs only glibc and libgcc.
FROM gcr.io/distroless/cc-debian12
COPY --from=build /voice-pipe /usr/local/bin/voice-pipe
WORKDIR /data
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/voice-pipe"]
CMD ["/data/config.toml"]
