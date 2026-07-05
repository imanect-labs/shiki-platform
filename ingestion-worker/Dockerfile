# shiki ingestion-worker — Docling パース（日本語 OCR）・Ruri 埋め込み・reranker。
#
# 軽量化の方針:
#   * torch/torchvision は CPU ホイール（pyproject の pytorch-cpu index 固定）。
#   * ML モデル（埋め込み/reranker/Docling レイアウト）はイメージに焼かず、実行時に
#     HF_HOME（compose の hf-cache volume）へダウンロード・キャッシュする。
#     エアギャップ環境では volume を事前配布して同じ構造で動かせる。
#   * OCR は Tesseract CLI（jpn/jpn_vert）。distro パッケージで日本語モデルごと入る。

FROM python:3.12-slim

# tesseract: スキャン PDF の日本語 OCR。libgl/libglib: docling(opencv) の実行時依存。
RUN apt-get update && apt-get install -y --no-install-recommends \
        tesseract-ocr tesseract-ocr-jpn tesseract-ocr-jpn-vert \
        libgl1 libglib2.0-0 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=ghcr.io/astral-sh/uv:0.5 /uv /usr/local/bin/uv

WORKDIR /app
ENV UV_LINK_MODE=copy \
    UV_COMPILE_BYTECODE=1 \
    HF_HOME=/models

# 依存だけ先に解決してレイヤキャッシュを効かせる。
COPY pyproject.toml uv.lock ./
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-install-project

COPY src ./src
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev

EXPOSE 8000
CMD ["uv", "run", "--no-sync", "uvicorn", "ingestion_worker.main:app", "--host", "0.0.0.0", "--port", "8000"]
