# The embedding sidecar for "Ask the rulebook" (docs/52): BAAI/bge-m3 on CPU (docs/55), about 2.3 GB of weights, baked in at a pinned revision. The earlier MiniLM image (120 MB) needs EMBEDDER_THRESHOLD=0.415.
# Build from the repository root:   docker build -f deploy/embed/Dockerfile -t claimguard-embed:latest .
# It runs on the private network with no route out and HF_HUB_OFFLINE=1, read-only, as an unprivileged user (see compose.yml).
FROM python:3.12-slim@sha256:f77ac9e44ae96ef2c90b8053ea08c31f8be030f824196b0ae4db6d462c84e51f
WORKDIR /app
COPY deploy/embed/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir --index-url https://download.pytorch.org/whl/cpu torch==2.14.1 \
 && pip install --no-cache-dir -r /app/requirements.txt
# The model is fetched at build time at this exact revision; nothing is fetched when the container runs. The weights ship only as a pickle file (no safetensors at this revision),
# so the build refuses any file whose SHA-256 differs from the one checked when the model was chosen, and transformers loads it with weights_only=True (no code runs on load) (docs/57).
ENV HF_HOME=/opt/hf
RUN python -c "from huggingface_hub import snapshot_download as d; d('BAAI/bge-m3', revision='5617a9f61b028005a4858fdac845db406aefb181', allow_patterns=['*.json', '*.txt', '*.model', 'pytorch_model.bin', 'tokenizer*', '1_Pooling/*', 'modules.json'])" \
 && echo "b5e0ce3470abf5ef3831aa1bd5553b486803e83251590ab7ff35a117cf6aad38  $(find /opt/hf -name pytorch_model.bin | head -1)" | sha256sum -c - \
 && chmod -R a+rX /opt/hf
COPY deploy/embed/service.py /app/service.py
ENV HF_HUB_OFFLINE=1 EMBED_HOST=0.0.0.0 EMBED_PORT=8091 PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
USER 65532:65532
EXPOSE 8091
HEALTHCHECK --interval=20s --timeout=5s --start-period=30s --retries=3 CMD ["python", "-c", "import os,sys,urllib.request as u; r=u.Request('http://127.0.0.1:8091/health', headers={'Authorization': 'Bearer ' + os.environ['EMBED_TOKEN']}); sys.exit(0 if u.urlopen(r, timeout=4).status == 200 else 1)"]
CMD ["python", "/app/service.py"]
