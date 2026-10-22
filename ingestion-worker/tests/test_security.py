"""内部サービスとしての防壁のテスト（#812）: 共有トークンと /parse の許可ホスト。

どれも「届けば誰でも使える代理人」にしないための負のテスト。
"""

import base64

import httpx
import pytest
from conftest import TEST_WORKER_TOKEN, FakeRegistry
from fastapi import HTTPException
from fastapi.testclient import TestClient

from ingestion_worker import parse as parse_mod
from ingestion_worker.main import create_app
from ingestion_worker.security import parse_allowed_hosts, source_url_rejection
from ingestion_worker.settings import get_settings

_EMBED = {"tenant_id": "a-corp", "input_type": "query", "texts": ["q"]}
_PARSE_INLINE = {
    "tenant_id": "a-corp",
    "content_base64": base64.b64encode(b"hello").decode(),
    "content_type": "text/plain",
    "file_name": "a.txt",
}
_RERANK = {"tenant_id": "a-corp", "query": "q", "passages": [{"id": "p", "text": "t"}]}
_EDIT = {
    "tenant_id": "a-corp",
    "content_type": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "file_name": "d.docx",
    "data_base64": base64.b64encode(b"x").decode(),
    "ops": [{"op": "append_markdown", "markdown": "a"}],
}
_ENDPOINTS = [("/embed", _EMBED), ("/parse", _PARSE_INLINE), ("/rerank", _RERANK), ("/edit", _EDIT)]


# --- 共有トークン ---


@pytest.mark.parametrize(("path", "body"), _ENDPOINTS)
@pytest.mark.parametrize(
    "authorization",
    [None, "Bearer wrong-token", f"Basic {TEST_WORKER_TOKEN}", TEST_WORKER_TOKEN, "Bearer "],
)
def test_every_endpoint_rejects_missing_or_wrong_token(
    client: TestClient, path: str, body: dict, authorization: str | None
) -> None:
    headers = {"Authorization": authorization} if authorization is not None else {}
    # client 既定の正しいヘッダを上書き（None は削除）して送る。
    raw = TestClient(client.app)
    resp = raw.post(path, json=body, headers=headers)
    assert resp.status_code == 401, resp.text
    assert resp.headers["WWW-Authenticate"] == "Bearer"


def test_correct_token_is_accepted_case_insensitive_scheme(client: TestClient) -> None:
    raw = TestClient(client.app)
    resp = raw.post("/embed", json=_EMBED, headers={"Authorization": f"bearer {TEST_WORKER_TOKEN}"})
    assert resp.status_code == 200


def test_healthz_stays_open_without_token(client: TestClient) -> None:
    # compose の healthcheck・dev-up の疎通確認はトークンを持たない。
    assert TestClient(client.app).get("/healthz").status_code == 200


def test_unset_token_keeps_backward_compatible_open_access(
    monkeypatch: pytest.MonkeyPatch, worker_env: None
) -> None:
    from ingestion_worker import model_registry

    monkeypatch.setenv("WORKER_AUTH_TOKEN", "")
    get_settings.cache_clear()
    original = model_registry.get_registry()
    model_registry.set_registry_for_tests(FakeRegistry())
    try:
        resp = TestClient(create_app()).post("/embed", json=_EMBED)
    finally:
        model_registry.set_registry_for_tests(original)
    assert resp.status_code == 200


# --- source_url の許可ホスト ---


def _fail_download(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    """拒否されるべき URL で _download が呼ばれたら記録する（呼ばれないことを確かめる）。"""
    called: list[str] = []

    async def fake_download(url: str) -> bytes:
        called.append(url)
        return b"leaked"

    monkeypatch.setattr(parse_mod, "_download", fake_download)
    return called


@pytest.mark.parametrize(
    "url",
    [
        # 内部ホスト（Qdrant・Redis・自分自身）。
        "http://qdrant:6333/collections",
        "http://redis:6379/",
        "http://ingestion-worker:8000/healthz",
        # 許可ホストでもポートが違う。
        "http://minio:6333/blob",
        # IP リテラル（10 進・IPv6・ループバック・クラウドメタデータ）。
        "http://127.0.0.1:9000/blob",
        "http://2130706433:9000/blob",
        "http://[::1]:9000/blob",
        "http://169.254.169.254/latest/meta-data/",
        # userinfo で許可ホストを装う。
        "http://minio:9000@qdrant:6333/collections",
        "http://user:pw@minio:9000/blob",
        # 許可ホストの接尾・接頭を名乗る別ホスト。
        "http://minio:9000.evil.example/blob",
        "http://evil-minio:9000/blob",
        "http://minio.:9000/blob",
        # スキーム。
        "file:///etc/passwd",
        "ftp://minio:9000/blob",
        # 解釈できない。
        "http://[::1/blob",
    ],
)
def test_parse_rejects_source_url_outside_allowed_hosts(
    client: TestClient, monkeypatch: pytest.MonkeyPatch, url: str
) -> None:
    called = _fail_download(monkeypatch)
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": url,
            "content_type": "text/plain",
            "file_name": "a.txt",
        },
    )
    assert resp.status_code == 422, resp.text
    assert resp.json()["detail"]["error"] == "source_url_not_allowed"
    assert called == [], "拒否すべき URL を取りに行っている"


def test_parse_accepts_allowed_host_case_insensitively(
    client: TestClient, monkeypatch: pytest.MonkeyPatch
) -> None:
    called = _fail_download(monkeypatch)
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": "http://MINIO:9000/shiki-blobs/a?X-Amz-Signature=abc",
            "content_type": "text/plain",
            "file_name": "a.txt",
        },
    )
    assert resp.status_code == 200, resp.text
    assert len(called) == 1


def test_parse_rejects_all_urls_when_allowlist_is_unset(
    client: TestClient, monkeypatch: pytest.MonkeyPatch
) -> None:
    # fail-closed: 設定漏れで任意 URL を取りに行く状態に戻らない。
    monkeypatch.setenv("SOURCE_URL_ALLOWED_HOSTS", "")
    get_settings.cache_clear()
    called = _fail_download(monkeypatch)
    resp = client.post(
        "/parse",
        json={
            "tenant_id": "a-corp",
            "source_url": "http://minio:9000/blob",
            "content_type": "text/plain",
            "file_name": "a.txt",
        },
    )
    assert resp.status_code == 422
    assert resp.json()["detail"]["error"] == "source_url_not_allowed"
    assert called == []
    # インライン経路は許可ホストと無関係に使える。
    assert client.post("/parse", json=_PARSE_INLINE).status_code == 200


@pytest.mark.parametrize("status", [301, 302, 303, 307, 308])
async def test_download_does_not_follow_redirects(status: int) -> None:
    """許可ホストが 3xx で内部ホストへ向けても追従しない。"""
    requested: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requested.append(str(request.url))
        return httpx.Response(status, headers={"Location": "http://qdrant:6333/collections"})

    with pytest.raises(HTTPException) as excinfo:
        await parse_mod._download("http://minio:9000/blob", transport=httpx.MockTransport(handler))
    assert excinfo.value.detail["error"] == "source_redirect_not_allowed"
    assert requested == ["http://minio:9000/blob"]


async def test_download_reads_body_from_allowed_host() -> None:
    def handler(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"body")

    data = await parse_mod._download(
        "http://minio:9000/blob", transport=httpx.MockTransport(handler)
    )
    assert data == b"body"


def test_allowed_hosts_parsing() -> None:
    assert parse_allowed_hosts(" rustfs:9000 , Storage.googleapis.com,[fd00::1]:9000,") == [
        ("rustfs", 9000),
        ("storage.googleapis.com", None),
        ("fd00::1", 9000),
    ]
    for bad in [
        "rustfs:abc",
        "rustfs:0",
        "rustfs:70000",
        "http://rustfs",
        "u@rustfs",
        "[::1]x",
        # 黙って「どの URL にも一致しない」「全ポート許可」に化けていた書き損じ
        "fd00::1:9000",
        "rustfs:9000:1",
        "[::1]:",
        "rustfs:",
        "[::1",
    ]:
        with pytest.raises(ValueError):
            parse_allowed_hosts(bad)


def test_invalid_idna_host_is_rejected_not_500() -> None:
    # `.host` の IDNA デコードで落ちる A-label は 500 ではなく拒否理由になる。
    reason = source_url_rejection("http://xn--rustfs-:9000/obj", "rustfs:9000")
    assert reason is not None
    assert "解釈できません" in reason


def test_port_less_entry_allows_any_port_and_scheme_default_ports() -> None:
    allowed = "storage.googleapis.com,rustfs:443"
    assert source_url_rejection("https://storage.googleapis.com/b/o", allowed) is None
    assert source_url_rejection("http://storage.googleapis.com:8080/b/o", allowed) is None
    # 既定ポート（https=443）は明示されていなくても照合できる。
    assert source_url_rejection("https://rustfs/b/o", allowed) is None
    assert source_url_rejection("http://rustfs/b/o", allowed) is not None
