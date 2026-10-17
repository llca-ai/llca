# OSS Scanner build environment for llca (https://github.com/llca-ai/llca).
# The scanner builds this with the repository as the build context. Network access is
# available only during this build, so everything the tests need is installed here.
# llca has no third-party crates: SQLite is the system library (libsqlite3), via FFI.

FROM rust:1-bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=never \
    CARGO_PROFILE_RELEASE_DEBUG=true \
    RUSTFLAGS="-C force-frame-pointers=yes"

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libsqlite3-dev sqlite3 git python3 \
    && rm -rf /var/lib/apt/lists/*

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Build the binary as the maintainers build it (release, with debug info).
RUN cargo build --release --locked

# Pre-build the test binaries so tests can run offline, then run them once.
# A failing test does not abort the build.
RUN cargo test --release --locked --no-run \
    && (cargo test --release --locked || echo "WARNING: cargo test reported failures")
