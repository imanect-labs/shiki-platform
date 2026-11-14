"""試行のキャッシュ（同じ依頼・同じ版・同じ条件なら生成も実行もしない・GAP-30 §6.5）。

キーは次を正規化した JSON の sha256（どれかが変われば自然に再実行になる）:
課題 id・モード・言い方（id と依頼文の本文）・fixture の定義・試行の番号・条件・設定の sha・取り込みの sha・
バイナリの sha。Mode A は採点器の版を入れない（生成だけを置き、採点はキャッシュから何度でもやり直す・`extra` は
送る model）。Mode B は採点した結果を置くので、`extra` に採点器の版と課題の期待の sha、会話と採点を左右する条件
（聞き返しへの答え・承認の方針・1 発話の予算など・`mode_b.key_conditions`）を入れる（直したら流し直す）。

置き場は `<cache>/trials/<key>.json`（`data/cache/` は .gitignore 済み）。書き込みは同じ場所の一時ファイルに
書いて fsync してから rename する（途中で落ちても壊れた記録を残さない）。
"""

from __future__ import annotations

import json
import os
import re
import threading
from pathlib import Path

from abeval.config import canonical_json, sha256_hex

_KEY = re.compile(r"^[0-9a-f]{64}$")


def trial_key(
    *,
    task_id: str,
    mode: str,
    phrasing: str,
    prompt: str,
    fixtures: object,
    trial: int,
    condition: dict,
    config_sha: str,
    capture_sha: str,
    binary_sha: str,
    extra: dict | None = None,
) -> str:
    """試行のキャッシュのキー（`extra` は Mode B の採点器の版と課題の期待の sha など・無ければ入れない）。"""
    if isinstance(trial, bool) or not isinstance(trial, int) or trial < 0:
        raise ValueError(f"trial は 0 以上の整数です: {trial!r}")
    for name, v in (
        ("task_id", task_id),
        ("mode", mode),
        ("phrasing", phrasing),
        ("prompt", prompt),
        ("config_sha", config_sha),
        ("capture_sha", capture_sha),
        ("binary_sha", binary_sha),
    ):
        if not isinstance(v, str) or not v:
            raise ValueError(f"{name} は空でない文字列です: {v!r}")
    material = {
        "task_id": task_id,
        "mode": mode,
        "phrasing": phrasing,
        "prompt": prompt,
        "fixtures": fixtures,
        "trial": trial,
        "condition": condition,
        "config_sha": config_sha,
        "capture_sha": capture_sha,
        "binary_sha": binary_sha,
    }
    if extra is not None:
        material["extra"] = extra
    return sha256_hex(canonical_json(material))


class TrialCache:
    def __init__(self, root: Path):
        self.dir = Path(root) / "trials"

    def path(self, key: str) -> Path:
        if not _KEY.match(key):
            raise ValueError(f"キャッシュのキーではありません: {key!r}")
        return self.dir / f"{key}.json"

    def get(self, key: str) -> dict | None:
        p = self.path(key)
        try:
            return json.loads(p.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return None

    def put(self, key: str, record: dict) -> Path:
        final = self.path(key)
        self.dir.mkdir(parents=True, exist_ok=True)
        tmp = self.dir / f".{key}.{os.getpid()}.{threading.get_ident()}.tmp"
        data = json.dumps(record, ensure_ascii=False, indent=1, sort_keys=True).encode()
        try:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            try:
                view = memoryview(data)
                while view:
                    view = view[os.write(fd, view) :]
                os.fsync(fd)
            finally:
                os.close(fd)
            os.replace(tmp, final)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
        dfd = os.open(self.dir, os.O_RDONLY)
        try:
            os.fsync(dfd)
        finally:
            os.close(dfd)
        return final
