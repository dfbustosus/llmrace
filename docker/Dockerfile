# llmrace in a container: no Rust toolchain, works anywhere Docker does.
#
# ubuntu 24.04 matches the release builder exactly — glibc stays aligned.
#
# Build (CI does this on release tags; locally pass the version):
#   docker build --build-arg VERSION=1.3.1 --build-arg TARGET=x86_64-unknown-linux-gnu \
#     -t ghcr.io/dfbustosus/llmrace .
#
# Run:
#   docker run --rm -e OPENAI_API_KEY ghcr.io/dfbustosus/llmrace openai -n 5

FROM ubuntu:24.04 AS fetch
ARG VERSION=1.3.0
ARG TARGET=x86_64-unknown-linux-gnu
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL \
      "https://github.com/dfbustosus/llmrace/releases/download/v${VERSION}/llmrace-v${VERSION}-${TARGET}.tar.gz" \
      -o /tmp/llmrace.tar.gz \
 && tar xzf /tmp/llmrace.tar.gz -C /tmp \
 && install -m 755 "/tmp/llmrace-v${VERSION}-${TARGET}/bin/llmrace" /usr/local/bin/llmrace \
 && llmrace --version

FROM ubuntu:24.04
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=fetch /usr/local/bin/llmrace /usr/local/bin/llmrace
# reports land in /report — mount a volume to keep them
WORKDIR /report
ENTRYPOINT ["llmrace"]
