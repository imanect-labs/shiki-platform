"""版の振る舞いの指紋（GAP-30 §4.1 の 5・BUG-246）。

serve が版のサーバーを起動した直後に、決まった IR の組（`PROBES`・版で結果が変わるもの）を
`POST /workflows/validate` に送り、返った誤りと警告のコードを版の記録の `behavior_check` に残す（`record`）。
`run`・`capture` の前に同じ組を送り直し、記録と同じことを確かめる（`verify`・違えば流さない）。費用は 0
（検証だけ・保存も LLM の呼び出しもしない）。

組は、V0（81a9899b）と HEAD の保存時検証のコードを読んで選んだ（どれも V0 では誤りなしで通る）:

| 名前 | 中身 | V0（81a9899b） | V1（71e9b79a） | HEAD |
| --- | --- | --- | --- | --- |
| `port_next` | 出口が `out` だけの llm.invoke から `from_port: "next"` の線 | 通る（V2 は出口を見ない・`ir/validate/v2_graph.rs`） | `ir.unknown_port` | `ir.unknown_port`（`v2_graph.rs`・`ports.rs`） |
| `template_var` | vars に無い `{missing}` を差し込む `$template` | 通る（V5 は `$template` の形だけを見る・`v5_dataflow.rs`） | `ir.bad_template` | `ir.bad_template`（`v5_dataflow.rs` の `check_template`） |
| `unknown_folder` | どこにも無い決まった id のフォルダーを一覧する storage.list | 通る（V4 は secret と skill だけ・`refs.rs`） | 通る（同じ） | `ir.unknown_folder`（GAP-35・`exists.rs`・API が保存者の見え方で引く） |

llm.invoke の model は `default`（V0 は、カタログにモデルがあれば model を省くと `ir.missing_model` にする。評価の
設定と stub の設定の両方に `default` がある）。名前は `abeval-probe-*`（保存しないので残らない）。

指紋は「その版のバイナリが返したもの」を記録して比べる（V0 と HEAD の期待は上の表で、HEAD の分は
`tests/test_behavior.py` の lint_bin と server のテストが確かめる）。組を変えたら `PROBES_SHA` が変わるので、
記録し直す（`serve --label L --record-behavior`）まで流さない。
"""

from __future__ import annotations

import datetime as dt
from collections.abc import Mapping
from pathlib import Path

from abeval.config import canonical_json, sha256_hex
from abeval.versions import VersionError, read_record, update_record

# どの DB にも無い決まったフォルダーの id（UUID の形・GAP-35 の実在の照合に掛ける）。
PROBE_FOLDER = "5f0c1d2e-3b4a-4c6d-8e9f-a0b1c2d3e4f5"


def _llm(node_id: str, prompt: object) -> dict:
    return {"id": node_id, "type": "llm.invoke", "params": {"model": "default", "prompt": prompt}}


PROBES: dict[str, dict] = {
    "port_next": {
        "ir_version": 1,
        "name": "abeval-probe-port-next",
        "triggers": [{"kind": "interactive"}],
        "nodes": [_llm("a", "abeval probe a"), _llm("b", "abeval probe b")],
        "edges": [{"from": "a", "from_port": "next", "to": "b"}],
    },
    "template_var": {
        "ir_version": 1,
        "name": "abeval-probe-template-var",
        "triggers": [{"kind": "interactive"}],
        "nodes": [_llm("a", {"$template": "abeval probe {missing}", "vars": {}})],
        "edges": [],
    },
    "unknown_folder": {
        "ir_version": 1,
        "name": "abeval-probe-unknown-folder",
        "triggers": [{"kind": "interactive"}],
        "nodes": [{"id": "a", "type": "storage.list", "params": {"folder": PROBE_FOLDER}}],
        "edges": [],
    },
}
PROBES_SHA = sha256_hex(canonical_json(PROBES))
RECORD_KEY = "behavior_check"


def _codes(items: object) -> list[str]:
    if not isinstance(items, list):
        return []
    return sorted(str(e.get("code")) for e in items if isinstance(e, Mapping))


def fingerprint(client) -> dict[str, dict[str, list[str]]]:
    """組を検証に送り、名前ごとの誤りと警告のコード（並べたもの）。"""
    out = {}
    for name, ir in PROBES.items():
        res = client.validate(ir)
        out[name] = {"errors": _codes(res.get("errors")), "warnings": _codes(res.get("warnings"))}
    return out


def record(label: str, client, *, versions_dir: Path | None = None) -> dict:
    """版の記録に指紋を残す（記録の無い版は VersionError）。残したものを返す。"""
    read_record(label, versions_dir)
    entry = {
        "probes_sha": PROBES_SHA,
        "results": fingerprint(client),
        "recorded_at": dt.datetime.now(dt.UTC).isoformat(timespec="seconds"),
    }
    update_record(label, {RECORD_KEY: entry}, versions_dir)
    return entry


def differences(want: Mapping, got: Mapping) -> list[str]:
    out = []
    for name in sorted(set(want) | set(got)):
        if want.get(name) != got.get(name):
            out.append(f"{name}: 記録 {want.get(name)}・いま {got.get(name)}")
    return out


def verify(label: str, client, *, versions_dir: Path | None = None) -> str | None:
    """記録した指紋と、いまのサーバーの指紋が同じか（違えば・記録が無ければ理由）。"""
    try:
        rec = read_record(label, versions_dir)
    except VersionError as e:
        return str(e)
    saved = rec.extra.get(RECORD_KEY)
    redo = f"python -m abeval serve --label {label}（起動し直す）か --record-behavior（起動したまま記録する）"
    if not isinstance(saved, Mapping) or not isinstance(saved.get("results"), Mapping):
        return f"版の記録に振る舞いの指紋がありません。{redo} で残してから流す"
    if saved.get("probes_sha") != PROBES_SHA:
        return f"版の記録の指紋は別の組で取ったものです。{redo} で取り直してから流す"
    got = fingerprint(client)
    diff = differences(saved["results"], got)
    if diff:
        return (
            "版のサーバーの振る舞いが、記録した指紋と違います（別の版のサーバーか、設定が違う）: "
            + "; ".join(diff)[:600]
        )
    return None
