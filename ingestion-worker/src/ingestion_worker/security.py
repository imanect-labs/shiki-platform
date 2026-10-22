"""内部サービスとしての防壁（#812）: 呼び出し元の認証と、/parse の取得先の限定。

cell なら「ネットワーク内は信頼」で済んだが、プール（全テナント共有）では内部
コンポーネントが 1 つ侵害されるだけで全テナントに届く。worker は次の 2 つで
「届く者なら誰でも使える代理人」にならないようにする。

- **共有トークン**: shiki-server と同じ `WORKER_AUTH_TOKEN` を `Authorization: Bearer`
  で要求する（/healthz を除く全エンドポイント）。未設定は後方互換で無認証のまま
  受け付け、起動時に警告する。
- **source_url の許可ホスト**: /parse が自分で取りに行く URL を、presigned URL の
  発行元（オブジェクトストア）のホストに限る。未設定なら URL 経路を**全て拒否**する
  （fail-closed。インライン経路 `content_base64` は影響しない）。リダイレクトは
  追従しない（許可ホストから許可外へ逃がされるのを防ぐ）。
"""

from __future__ import annotations

import hmac
import logging
from typing import Annotated

import httpx
from fastapi import Header, HTTPException

from .settings import get_settings

logger = logging.getLogger(__name__)

_DEFAULT_PORTS = {"http": 80, "https": 443}


def require_worker_token(authorization: Annotated[str | None, Header()] = None) -> None:
    """FastAPI 依存: 共有トークンを検証する（未設定なら素通し＝後方互換）。"""
    expected = get_settings().worker_auth_token
    if not expected:
        return
    scheme, _, presented = (authorization or "").partition(" ")
    # 長さや内容で早期に抜けない（定数時間比較）。scheme は RFC 7235 で大文字小文字を区別しない。
    ok = hmac.compare_digest(presented.strip().encode(), expected.encode())
    if scheme.lower() != "bearer" or not ok:
        raise HTTPException(
            status_code=401,
            detail="unauthorized",
            headers={"WWW-Authenticate": "Bearer"},
        )


def parse_allowed_hosts(raw: str) -> list[tuple[str, int | None]]:
    """`host` / `host:port` / `[v6]:port` のカンマ区切りを (host, port) に分解する。

    port を省いた項目はそのホストの全ポートを許す。不正な項目は ValueError（起動時に落とす）。
    """
    entries: list[tuple[str, int | None]] = []
    for item in raw.split(","):
        entry = item.strip().lower()
        if not entry:
            continue
        if entry.startswith("["):
            host, _, rest = entry[1:].partition("]")
            if rest and not rest.startswith(":"):
                raise ValueError(f"許可ホストの書式が不正です: {item!r}")
            port_text = rest[1:]
        elif entry.count(":") == 1:
            host, port_text = entry.split(":")
        else:
            host, port_text = entry, ""
        if not host or "/" in host or "@" in host:
            raise ValueError(f"許可ホストの書式が不正です: {item!r}")
        port = None
        if port_text:
            if not port_text.isdigit() or not 0 < int(port_text) < 65536:
                raise ValueError(f"許可ホストのポートが不正です: {item!r}")
            port = int(port_text)
        entries.append((host, port))
    return entries


def source_url_rejection(url: str, allowed_hosts: str) -> str | None:
    """`source_url` を取りに行ってよいか判定する。拒否なら理由、許可なら None。

    判定は httpx の URL 解釈で行う（取得に使う解釈器と同じ。解釈の食い違いで
    `http://rustfs:9000@qdrant:6333/` のような URL がすり抜けるのを防ぐ）。
    IP リテラル（`127.0.0.1`・`[::1]`・`2130706433` 等）も、許可リストに同じ表記で
    無い限り拒否される。
    """
    allowed = parse_allowed_hosts(allowed_hosts)
    if not allowed:
        return "source_url の許可ホストが未設定です（SOURCE_URL_ALLOWED_HOSTS）"
    try:
        parsed = httpx.URL(url)
    except (httpx.InvalidURL, TypeError, ValueError) as exc:
        return f"source_url を解釈できません: {exc}"
    if parsed.scheme not in _DEFAULT_PORTS:
        return f"source_url のスキームは http/https に限ります: {parsed.scheme!r}"
    if parsed.userinfo:
        return "資格情報を含む source_url は受け付けません"
    host = parsed.host.lower()
    if not host:
        return "source_url にホストがありません"
    port = parsed.port or _DEFAULT_PORTS[parsed.scheme]
    for allowed_host, allowed_port in allowed:
        if host == allowed_host and allowed_port in (None, port):
            return None
    return f"source_url のホスト {host}:{port} は許可されていません"


def log_security_posture() -> None:
    """起動時に、後方互換・fail-closed で動いている設定を警告として残す。"""
    settings = get_settings()
    if not settings.worker_auth_token:
        logger.warning(
            "WORKER_AUTH_TOKEN が未設定: 認証なしで要求を受け付けます"
            "（shiki-server の SHIKI__RAG__WORKER_AUTH_TOKEN と揃えて設定すること・#812）"
        )
    # 書式の誤りは起動時に落とす（最初の /parse まで気づかない壊れ方を避ける）。
    if not parse_allowed_hosts(settings.source_url_allowed_hosts):
        logger.warning(
            "SOURCE_URL_ALLOWED_HOSTS が未設定: /parse の source_url を全て拒否します"
            "（presigned URL の発行元ホストを設定すること・#812）"
        )
