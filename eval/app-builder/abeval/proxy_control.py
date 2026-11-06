"""abeval.proxy の制御の口を叩くクライアント（実行の登録・会話 ID の割り当て・active・累計）。

合言葉は abeval.proxy が起動のときに `<home>/proxy.token`（権限 600）へ書いたもの。上流（手元の中継）へは
繋がない（制御の口は abeval.proxy の中で終わる）。
"""

from __future__ import annotations

import re
from pathlib import Path

import httpx

from abeval.proxy import CONTROL_HEADER, TOKEN_FILE

_WORD = re.compile(r"[^A-Za-z0-9._:-]+")


class ProxyControlError(RuntimeError):
    """制御の口が期待しない応答を返した。"""


def safe_id(text: str, limit: int = 128) -> str:
    """abeval.proxy の id に使える形（英数字と . _ : - の 1〜128 文字）にする。"""
    out = _WORD.sub("-", text).strip("-")[:limit]
    if not out:
        raise ValueError(f"id にできません: {text!r}")
    return out


def read_token(home: Path) -> str:
    path = Path(home) / TOKEN_FILE
    try:
        return path.read_text().strip()
    except OSError as e:
        raise ProxyControlError(f"合言葉がありません: {path}（abeval.proxy を起動する）") from e


class ProxyControl:
    def __init__(
        self,
        url: str,
        token: str,
        *,
        transport: httpx.BaseTransport | None = None,
        timeout: float = 10.0,
    ):
        self.url = url.rstrip("/")
        self._http = httpx.Client(
            transport=transport, timeout=timeout, trust_env=False, headers={CONTROL_HEADER: token}
        )

    def close(self) -> None:
        self._http.close()

    def __enter__(self) -> ProxyControl:
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def _send(self, method: str, path: str, body: object = None, ok=(200, 201, 204)) -> dict:
        r = self._http.request(method, self.url + path, json=body)
        if r.status_code not in ok:
            raise ProxyControlError(f"{method} {path} → {r.status_code}: {r.text[:300]}")
        return r.json() if r.content else {}

    def register_run(self, run_id: str, exp: str, cap_usd: float, *, capture_only=False) -> dict:
        return self._send(
            "POST",
            "/abeval/runs",
            {"run_id": run_id, "exp": exp, "cap_usd": cap_usd, "capture_only": capture_only},
        )

    def remove_run(self, run_id: str) -> None:
        self._send("DELETE", f"/abeval/runs/{run_id}", ok=(204, 404))

    def register_session(self, session: str, trial: str, run_id: str) -> dict:
        return self._send(
            "POST", "/abeval/sessions", {"session": session, "trial": trial, "run_id": run_id}
        )

    def set_active(self, run_id: str | None, trial: str | None = None) -> dict:
        return self._send("PUT", "/abeval/active", {"run_id": run_id, "trial": trial})

    def spend(self) -> dict:
        return self._send("GET", "/abeval/spend")
