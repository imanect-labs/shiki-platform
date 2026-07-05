"""shiki-platform ingestion worker。

Docling によるパース（日本語 OCR）・Ruri 埋め込み・reranker を HTTP で提供する
ステートレスなサービス。shiki-server 側は DocumentParser / EmbeddingProvider /
Reranker トレイトの HTTP 実装からこれを呼ぶ（トレイト裏なので TEI 等へ差し替え可能）。
"""
