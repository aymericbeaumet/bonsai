# Both build stages use the same locked tools as the contributor tasks.
FROM ghcr.io/jdx/mise:2026.11.12@sha256:991e2a550518ec5cae6f25adbc862ec0253ff714ee6e5e6bf328058a7324a041 AS mise-bin
FROM buildpack-deps:trixie@sha256:188a9dfa5e813c917be00c3f474bc75488b62914305e13fa21dff1cfef2eb354 AS mise
COPY --from=mise-bin /usr/local/bin/mise /usr/local/bin/mise
ENV MISE_AUTO_INSTALL=false MISE_TASK_RUN_AUTO_INSTALL=false
WORKDIR /src
COPY .mise.toml mise.lock ./
RUN mise trust .mise.toml

FROM mise AS build
RUN mise install --locked rust
COPY . .
# A recording needs the checkout's CLI but can skip release optimizations.
ENV CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/target \
    mise run release && cp target/release/bonsai /usr/local/bin/bonsai

FROM mise AS shells
ENV MISE_DATA_DIR=/opt/demo
RUN mise install --locked conda:git conda:zsh \
    && mkdir /opt/demo/tools \
    && ln -s "$(mise where conda:git)" /opt/demo/tools/git \
    && ln -s "$(mise where conda:zsh)" /opt/demo/tools/zsh

FROM ghcr.io/charmbracelet/vhs:v0.12.1@sha256:ea49a6a1c529be83153e88321892b5585964418f0b9055e8c1e0d732194234a3
COPY --from=shells /opt/demo/installs /opt/demo/installs
COPY --from=shells /opt/demo/tools /opt/demo/tools
ENV PATH="/opt/demo/tools/git/bin:/opt/demo/tools/zsh/bin:${PATH}"
COPY --from=build /usr/local/bin/bonsai /usr/local/bin/bonsai
