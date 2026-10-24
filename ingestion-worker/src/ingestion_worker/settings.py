"""環境変数からの設定（pydantic-settings）。"""

from functools import lru_cache

from pydantic_settings import BaseSettings


class Settings(BaseSettings):
    """worker の設定。compose では environment で注入する。"""

    # 埋め込みモデル（日本語特化・CPU 可の小型モデルが既定）。
    # ここを変えたら Rust 側 SHIKI__RAG__EMBEDDING_MODEL_VERSION も揃えること
    # （不一致はインジェスト時に version 突合ガードで拒否される）。
    embed_model: str = "cl-nagoya/ruri-v3-30m"
    # 日本語 cross-encoder reranker（CPU 可）。
    # ここを変えたら Rust 側の関連度の閾値 SHIKI__RAG__RELEVANCE_HIGH / _LOW を実クエリで
    # 測り直し、SHIKI__RAG__RELEVANCE_MODEL も揃えること（スコアのスケールはモデルごとに違う・
    # 食い違いはサーバのログに警告が出る・#424）。
    rerank_model: str = "hotchpotch/japanese-reranker-cross-encoder-xsmall-v1"
    # /parse がダウンロードする blob の上限（bytes）。Rust 側の max_parse_bytes と対にする。
    max_download_bytes: int = 50 * 1024 * 1024
    # 埋め込みのバッチサイズ（CPU 前提の控えめな既定）。
    embed_batch_size: int = 16
    # /edit が受け付ける文書バイトの上限（デコード後）。Rust 側 office と対にする。
    max_edit_bytes: int = 20 * 1024 * 1024
    # 同時に走らせる Docling 解析の上限。Docling は同期でキャンセルできないため、
    # 呼び出し側が諦めても解析スレッドは走り続ける。上限を超える要求は待たせずに
    # 503 で返し、取り残しがスレッドプールと CPU を食い潰さないようにする（#405）。
    max_concurrent_parses: int = 2
    # 起動時にモデルを読み込んで空打ちしておく（バックグラウンド）。しないと、起動後や
    # 久しぶりの最初の検索が reranker の読み込み（torch の import を含めて十数秒）を待つ。
    warmup_models: bool = True
    # shiki-server と共有するトークン（`Authorization: Bearer`・#812）。/healthz 以外の
    # 全エンドポイントで要求する。server 側 SHIKI__RAG__WORKER_AUTH_TOKEN と同じ値にする。
    # 空は後方互換で無認証のまま受け付ける（起動時に警告する）。
    worker_auth_token: str = ""
    # /parse の source_url で取りに行ってよいホスト（カンマ区切り。`host` か `host:port`）。
    # presigned URL の発行元（server の SHIKI__STORAGE__S3__INTERNAL_ENDPOINT 等）だけを書く。
    # 空なら URL 経路を全て拒否する（fail-closed・#812）。content_base64 経路は影響しない。
    source_url_allowed_hosts: str = ""


@lru_cache
def get_settings() -> Settings:
    return Settings()
