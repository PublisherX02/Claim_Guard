# One image for the API and the queue worker (they differ only by command). Build from the repository root:
#   docker build -f deploy/Dockerfile -t claimguard:latest .
FROM python:3.12-slim

ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1 PIP_NO_CACHE_DIR=1 PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONPATH=/app/src CLAIMGUARD_DATA_DIR=/data

RUN useradd --system --uid 10001 --home-dir /app claimguard && mkdir -p /data && chown claimguard /data
WORKDIR /app

COPY requirements.txt .
RUN pip install -r requirements.txt

COPY src ./src
COPY scripts ./scripts
COPY rules ./rules
COPY schemas ./schemas
COPY prompts ./prompts
COPY data/dataset_manifest.json ./data/dataset_manifest.json
COPY data/development/claims.jsonl ./data/development/claims.jsonl

USER claimguard
VOLUME ["/data"]
EXPOSE 8443

HEALTHCHECK --interval=15s --timeout=4s --start-period=40s --retries=5 \
    CMD python -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:8443/healthz', timeout=3).status == 200 else 1)"

# The API listens inside the private network only; the proxy in front terminates TLS (see deploy/compose.yml).
CMD ["python", "scripts/serve_access.py", "--host", "0.0.0.0", "--port", "8443", "--queue", "--behind-proxy"]
