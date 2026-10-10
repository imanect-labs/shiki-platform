"""OpenAI 互換の chat completions クライアント（データ生成と E2E 用）。

既定は opencode Go。`x-opencode-session` が無いと 400 になり、Cloudflare は既定の
User-Agent を弾くため、両方を明示する。
"""

from __future__ import annotations

import json
import os
import re
import time
import uuid
from pathlib import Path

import httpx

BASE_URL = os.environ.get("TSEVAL_LLM_BASE_URL", "https://opencode.ai/zen/go/v1")
MODEL = os.environ.get("TSEVAL_LLM_MODEL", "qwen3.6-plus")
_KEY_FILE = Path(os.environ.get("TSEVAL_LLM_KEY_FILE", "~/.config/opencode/api-key")).expanduser()
_SESSION = f"tseval-{uuid.uuid4()}"


def _headers() -> dict[str, str]:
    return {
        "authorization": f"Bearer {_KEY_FILE.read_text().strip()}",
        "x-opencode-session": _SESSION,
        "user-agent": "curl/8.5.0",
    }


def chat(
    messages: list[dict],
    *,
    tools: list[dict] | None = None,
    max_tokens: int = 16384,
    temperature: float = 0.7,
    thinking: bool = False,
    retries: int = 4,
) -> dict:
    """1 回の生成（非ストリーム）。返り値は choices[0].message と usage。

    `thinking=False` は推論トークンを止める（データ生成は推論なしで足り、数倍速い）。
    """
    body: dict = {
        "model": MODEL,
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "enable_thinking": thinking,
    }
    if tools:
        body["tools"] = tools
    last: Exception | None = None
    for attempt in range(retries):
        try:
            r = httpx.post(
                f"{BASE_URL}/chat/completions", json=body, headers=_headers(), timeout=600
            )
            if r.status_code >= 500 or r.status_code == 429:
                raise httpx.HTTPStatusError(r.text[:300], request=r.request, response=r)
            r.raise_for_status()
            data = r.json()
            return {"message": data["choices"][0]["message"], "usage": data.get("usage", {})}
        except (httpx.HTTPError, KeyError) as e:
            last = e
            time.sleep(2 ** (attempt + 1))
    raise RuntimeError(f"LLM 呼び出しに失敗: {last}")


_FENCE = re.compile(r"```(?:json)?\s*(.*?)```", re.S)


def chat_json(prompt: str, *, temperature: float = 0.7) -> object:
    """JSON だけを返させる（コードフェンスは剥がす）。壊れていたら 1 回だけ作り直させる。"""
    messages = [{"role": "user", "content": prompt}]
    for _ in range(2):
        text = chat(messages, temperature=temperature)["message"].get("content") or ""
        m = _FENCE.search(text)
        raw = m.group(1) if m else text
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            messages += [
                {"role": "assistant", "content": text},
                {
                    "role": "user",
                    "content": "JSON として解釈できませんでした。JSON だけを返してください。",
                },
            ]
    raise RuntimeError("JSON を得られませんでした")
