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
    rerank_model: str = "hotchpotch/japanese-reranker-cross-encoder-xsmall-v1"
    # /parse がダウンロードする blob の上限（bytes）。Rust 側の max_parse_bytes と対にする。
    max_download_bytes: int = 50 * 1024 * 1024
    # 埋め込みのバッチサイズ（CPU 前提の控えめな既定）。
    embed_batch_size: int = 16
    # /edit が受け付ける文書バイトの上限（デコード後）。Rust 側 office と対にする。
    max_edit_bytes: int = 20 * 1024 * 1024


@lru_cache
def get_settings() -> Settings:
    return Settings()
