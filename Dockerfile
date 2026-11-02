# Runtime image for ferl-server. The executable is staged by
# scripts/build-image.sh from the verified release archive; this file does not
# compile Rust code. The base is the linux/amd64 manifest of distroless
# cc-debian12:nonroot (Debian 12 glibc, CA certificates, UID 65532).
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:777e96cf322c46bc32aca926c263624c4dc8d7cf37e2fa65ba2c7e697318ebbb

ARG FERL_VERSION
ARG FERL_REVISION

LABEL org.opencontainers.image.source="https://github.com/ai-slop-code/ferl" \
      org.opencontainers.image.version="${FERL_VERSION}" \
      org.opencontainers.image.revision="${FERL_REVISION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.title="ferl"

COPY bin/ferl-server /usr/local/bin/ferl-server

ENV FERL_LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080

USER 65532:65532
ENTRYPOINT ["/usr/local/bin/ferl-server"]
CMD ["serve"]
