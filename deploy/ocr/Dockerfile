# The document reader: turns a PDF page or a picture into text (PDFium for pages, Tesseract and an optional vision model for pictures).
# Build from the repository root:   docker build -f deploy/ocr/Dockerfile -t claimguard-reader:latest .
# It runs on a private network with no route out, read-only, as an unprivileged user, with memory and process limits (see compose.yml): rasterizing a hostile
# PDF is the riskiest thing the system does with an uploaded file, and it happens here, not in the server.
FROM python:3.12-slim@sha256:f77ac9e44ae96ef2c90b8053ea08c31f8be030f824196b0ae4db6d462c84e51f
# eng is enough for English claims; ara adds Arabic. More languages: add tesseract-ocr-<code> and list them in DOCUMENT_READER_LANGS (eng+ara).
RUN apt-get update \
 && apt-get install -y --no-install-recommends tesseract-ocr tesseract-ocr-eng tesseract-ocr-ara \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY deploy/ocr/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir --require-virtualenv=false -r /app/requirements.txt
COPY deploy/ocr/service.py /app/service.py
ENV OCR_HOST=0.0.0.0 OCR_PORT=8090 PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 TESSDATA_PREFIX=/usr/share/tesseract-ocr/5/tessdata
USER 65532:65532
EXPOSE 8090
HEALTHCHECK --interval=20s --timeout=5s --start-period=10s --retries=3 CMD ["python", "-c", "import os,sys,urllib.request as u; r=u.Request('http://127.0.0.1:8090/health', headers={'Authorization': 'Bearer ' + os.environ['OCR_TOKEN']}); sys.exit(0 if u.urlopen(r, timeout=4).status == 200 else 1)"]
CMD ["python", "/app/service.py"]
