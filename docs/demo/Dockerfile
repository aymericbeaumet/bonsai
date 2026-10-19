# Recording environment for demo.gif: VHS plus git, zsh, and a bonsai built
# from this checkout. Built by record.sh with the repository as context.

FROM rust:1.99.1-slim-trixie@sha256:4df2ae8cfdfebdfc13a65da7f253b73d68983dc0339f2719bc17a279c913b13d AS build
WORKDIR /src
COPY . .
# The recording does not need the fully optimized release profile.
ENV CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/bonsai /usr/local/bin/bonsai

FROM ghcr.io/charmbracelet/vhs:v0.12.1@sha256:ea49a6a1c529be83153e88321892b5585964418f0b9055e8c1e0d732194234a3
RUN apt-get update \
    && apt-get install --yes --no-install-recommends git zsh \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/bonsai /usr/local/bin/bonsai
