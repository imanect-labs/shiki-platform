"""説明文と system の取り込み（`python -m abeval capture`・GAP-30 §4.5）。

実験を始めるたびに（fixture を作った後で）取り直す。製品の文面は Python に写さず、製品が Anthropic へ実際に
送った要求から読む。費用は $0（abeval.proxy の `capture_only` の実行で受け、上流へ送らない）。

1. abeval.proxy に `capture_only` の実行を登録する（登録の無い要求は abeval.proxy が拒む）。
2. 新しいスレッドを作って会話 ID（thread_id）をその実行に割り当て、短い発話を model `claude-haiku-5-5`・
   effort ありで送る。
3. abeval.proxy が保存した要求（`<cache>/requests/<run>/`）のうち、tools に emit_workflow を含む最初のものから
   system・tools・max_tokens を `data/captures/<label>/<exp>/{system.txt,tools.json,meta.json}` に書く。
   - system の「今日の日付」の一文（`crates/chat/src/worker/opts.rs` の `today_note` の書式）は固定の日付に
     置き換える（元の日付は meta.json に残す）。
   - tools の説明文に載る fixture のテーブルの id（data のツールの一覧）は置き場所の印に戻し、ほかの UUID は
     `<uuid>` にする（実験ごとに id が変わっても sha が変わらない）。
4. 指紋: llm-eval.toml の sha（鍵の形の項目を除く）・カスタム指示が空か・`/llm/models` の workflow と decision の
   id・fixture の名前・OpenAPI の機能・tools と system の sha。前の取り込みと違う項目を meta.json に残す。

会話の全文は書かない（取り込むのは製品の文面だけ）。
"""

from __future__ import annotations

import copy
import datetime as dt
import json
import os
import re
import time
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from abeval.anthropic_client import DUMMY_CREDENTIAL
from abeval.config import (
    ROOT,
    Config,
    canonical_json,
    find_secret_like,
    server_config_sha,
    sha256_hex,
)
from abeval.proxy_control import ProxyControl, safe_id
from abeval.shiki import ShikiClient, capabilities_of
from abeval.trial import EMIT_TOOL, MODEL

CAPTURES_DIR = ROOT / "data" / "captures"


def captures_dir() -> Path:
    """取り込みの置き場（`ABEVAL_CAPTURES_DIR` で差し替えられる・テストは一時の場所に向ける）。"""
    env = os.environ.get("ABEVAL_CAPTURES_DIR")
    return Path(env).expanduser() if env else CAPTURES_DIR


CAPTURE_PROMPT = "こんにちは。"
WEEKDAYS = "月火水木金土日"
# opts.rs の today_note: "\n\n今日は {Y}年{M}月{D}日（{曜}）です（{tz}）。「今日」「来週」「今月」などはこの日付を基準に解釈してください。"
DATE_NOTE = re.compile(
    r"\n\n今日は (\d{4})年(\d{1,2})月(\d{1,2})日（([月火水木金土日])）です（([^）\n]+)）。"
    r"「今日」「来週」「今月」などはこの日付を基準に解釈してください。"
)
_UUID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
STRIP_TOOL_KEYS = ("defer_loading", "cache_control")


class CaptureError(RuntimeError):
    """取り込めない（要求が届かない・形が違う・設定が違う）。"""


def today_note(day: dt.date, tz: str) -> str:
    """製品の `today_note` と同じ書式の一文（固定の日付で置き換えるときに使う）。"""
    return (
        f"\n\n今日は {day.year}年{day.month}月{day.day}日（{WEEKDAYS[day.weekday()]}）です（{tz}）。"
        "「今日」「来週」「今月」などはこの日付を基準に解釈してください。"
    )


def normalize_system(system: str, fixed: dt.date) -> tuple[str, list[str]]:
    """「今日の日付」の一文を固定の日付に置き換える。(置き換えた system, 元の日付 YYYY-MM-DD の列)。"""
    found: list[str] = []

    def repl(m: re.Match) -> str:
        y, mo, d = int(m.group(1)), int(m.group(2)), int(m.group(3))
        found.append(dt.date(y, mo, d).isoformat())
        return today_note(fixed, m.group(5))

    return DATE_NOTE.sub(repl, system), found


def scrub_ids(value: Any, ids: Mapping[str, str]) -> tuple[Any, int]:
    """文字列の中の fixture の id を印に、ほかの UUID を `<uuid>` に置き換える。(複製, ほかの UUID の数)。"""
    other = 0

    def text(s: str) -> str:
        nonlocal other
        for real, mark in ids.items():
            s = s.replace(real, mark)

        def anon(m: re.Match) -> str:
            nonlocal other
            other += 1
            return "<uuid>"

        return _UUID.sub(anon, s)

    def walk(v: Any) -> Any:
        if isinstance(v, Mapping):
            return {k: walk(x) for k, x in v.items()}
        if isinstance(v, list):
            return [walk(x) for x in v]
        if isinstance(v, str):
            return text(v)
        return v

    return walk(value), other


def emit_tool(tools: list[dict]) -> dict:
    """取り込んだ tools から emit_workflow を取り出す（defer_loading と cache_control を外す）。"""
    for t in tools:
        if isinstance(t, Mapping) and t.get("name") == EMIT_TOOL:
            return {k: copy.deepcopy(v) for k, v in t.items() if k not in STRIP_TOOL_KEYS}
    raise CaptureError(f"取り込んだ tools に {EMIT_TOOL} がありません")


@dataclass
class Capture:
    label: str
    exp: str
    system: str
    tools: list[dict]
    max_tokens: int
    meta: dict = field(default_factory=dict)

    @property
    def tools_sha(self) -> str:
        return sha256_hex(canonical_json(self.tools))

    @property
    def system_sha(self) -> str:
        return sha256_hex(self.system)

    @property
    def sha(self) -> str:
        """取り込みの sha（system・tools・max_tokens）。V1 の採点し直しはこれが同じことを条件にする。"""
        return sha256_hex(
            canonical_json(
                {"system": self.system, "tools": self.tools, "max_tokens": self.max_tokens}
            )
        )

    @property
    def tool_names(self) -> list[str]:
        return [str(t.get("name")) for t in self.tools if isinstance(t, Mapping)]

    def emit_tool(self) -> dict:
        return emit_tool(self.tools)

    def write(self, root: Path | None = None, denylist: Path | None = None) -> Path:
        """`data/captures/<label>/<exp>/` に書く（コミットするので、書く前に鍵の形と拒否リストの語を検査する）。"""
        from abeval.gen_checks import load_denylist
        from abeval.report import check_publishable

        meta = {**self.meta, "label": self.label, "exp": self.exp, "max_tokens": self.max_tokens}
        meta["sha"] = {"capture": self.sha, "system": self.system_sha, "tools": self.tools_sha}
        words, _ = load_denylist(denylist)
        problems = check_publishable(
            {"system": self.system, "tools": self.tools, "meta": meta}, words, forbidden_keys=False
        )
        if problems:
            raise CaptureError(
                "取り込みに入れてはいけないものがあるので書きません（製品の system のカスタム指示など）:\n- "
                + "\n- ".join(problems[:20])
            )
        d = Path(root or captures_dir()) / self.label / self.exp
        d.mkdir(parents=True, exist_ok=True)
        (d / "system.txt").write_text(self.system, encoding="utf-8")
        (d / "tools.json").write_text(
            json.dumps(self.tools, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
        )
        (d / "meta.json").write_text(
            json.dumps(meta, ensure_ascii=False, indent=1, sort_keys=True) + "\n", encoding="utf-8"
        )
        return d

    @classmethod
    def read(cls, label: str, exp: str, root: Path | None = None) -> Capture:
        d = Path(root or captures_dir()) / label / exp
        try:
            meta = json.loads((d / "meta.json").read_text(encoding="utf-8"))
            cap = cls(
                label=label,
                exp=exp,
                system=(d / "system.txt").read_text(encoding="utf-8"),
                tools=json.loads((d / "tools.json").read_text(encoding="utf-8")),
                max_tokens=int(meta["max_tokens"]),
                meta=meta,
            )
        except (OSError, ValueError, KeyError) as e:
            raise CaptureError(f"取り込みを読めません: {d}: {e}") from e
        want = (meta.get("sha") or {}).get("capture")
        if want and want != cap.sha:
            raise CaptureError(f"{d}: 取り込みの sha が meta.json と違います（手で書き換えた？）")
        return cap


def from_request(body: Mapping, *, fixed: dt.date, ids: Mapping[str, str]) -> tuple[Capture, dict]:
    """abeval.proxy が保存した要求から取り込みを作る。(取り込み（label・exp は空）, meta の材料)。"""
    system = body.get("system")
    if not isinstance(system, str):
        raise CaptureError(f"system が文字列ではありません: {type(system).__name__}")
    tools = body.get("tools")
    if not isinstance(tools, list) or not tools:
        raise CaptureError("tools がありません")
    max_tokens = body.get("max_tokens")
    if isinstance(max_tokens, bool) or not isinstance(max_tokens, int) or max_tokens <= 0:
        raise CaptureError(f"max_tokens が正の整数ではありません: {max_tokens!r}")
    norm, dates = normalize_system(system, fixed)
    norm, other_sys = scrub_ids(norm, ids)
    tools2, other_tools = scrub_ids(tools, ids)
    emit_tool(tools2)  # emit_workflow が無ければ止める
    meta = {
        "fixed_date": fixed.isoformat(),
        "original_dates": dates,
        "date_note_found": bool(dates),
        "other_uuids": other_sys + other_tools,
        "model": body.get("model"),
        "thinking": body.get("thinking"),
        "output_config": body.get("output_config"),
    }
    return Capture("", "", norm, tools2, max_tokens, meta), meta


def saved_requests(requests_dir: Path) -> list[dict]:
    """abeval.proxy が保存した要求（送った順）。"""
    out = []
    for p in sorted(Path(requests_dir).glob("*.json")):
        if p.name.endswith(".meta.json"):
            continue
        try:
            body = json.loads(p.read_text(encoding="utf-8"))
        except ValueError:
            continue
        if isinstance(body, dict):
            out.append(body)
    return out


def pick_request(bodies: list[dict]) -> dict:
    """tools に emit_workflow を含む最初の要求（題名の生成などツールの無い要求は飛ばす）。"""
    for b in bodies:
        names = [t.get("name") for t in b.get("tools") or [] if isinstance(t, Mapping)]
        if EMIT_TOOL in names:
            return b
    raise CaptureError(
        f"保存した要求に {EMIT_TOOL} を含むものがありません（{len(bodies)} 件）。版の機能か fixture を確かめる"
    )


# --- 指紋 ---


def instructions_state(ci: Mapping | None) -> str:
    """カスタム指示の状態（absent = 口が無い版・empty = 空・set = 何か入っている）。"""
    if ci is None:
        return "absent"
    empty = (
        str(ci.get("about", "")).strip() == ""
        and str(ci.get("style", "")).strip() == ""
        and ci.get("tone", "default") == "default"
    )
    return "empty" if empty else "set"


def _leaf(raw: Any, at: str) -> Any:
    for part in re.findall(r"[^.\[\]]+", at):
        raw = raw[int(part)] if isinstance(raw, list) else raw.get(part)
    return raw


def llm_config_state(path: Path) -> dict:
    """llm-eval.toml の sha（鍵の形の項目を除く）・鍵の形の項目の場所・その値がどれもダミーか（値は出さない）。"""
    path = Path(path).expanduser()
    with path.open("rb") as f:
        raw = tomllib.load(f)
    items = find_secret_like(raw)
    dummy_only = all(_leaf(raw, at) == DUMMY_CREDENTIAL for at in items)
    return {"sha": server_config_sha(path), "key_like_items": items, "dummy_only": dummy_only}


def fingerprint(
    *,
    llm_config_sha: str | None,
    instructions: str,
    workflow_models: list[str],
    decision_models: list[str],
    fixtures: Mapping[str, list[str]],
    capabilities: Mapping[str, Any],
    tools_sha: str,
    system_sha: str,
) -> dict:
    return {
        "llm_config_sha": llm_config_sha,
        "custom_instructions": instructions,
        "workflow_models": sorted(workflow_models),
        "decision_models": sorted(decision_models),
        "fixtures": {k: sorted(v) for k, v in fixtures.items()},
        "capabilities": dict(sorted(capabilities.items())),
        "tools_sha": tools_sha,
        "system_sha": system_sha,
    }


def fingerprint_sha(fp: Mapping) -> str:
    return sha256_hex(canonical_json(fp))


def changed_items(prev: Mapping | None, cur: Mapping) -> list[str]:
    if not prev:
        return []
    return sorted(k for k in set(prev) | set(cur) if prev.get(k) != cur.get(k))


def previous_fingerprint(label: str, exp: str, root: Path | None = None) -> dict | None:
    """同じ版の、いちばん新しい前の取り込みの指紋（無ければ None）。"""
    best: tuple[str, dict] | None = None
    for meta_path in (Path(root or captures_dir()) / label).glob("*/meta.json"):
        if meta_path.parent.name == exp:
            continue
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except ValueError:
            continue
        at = str(meta.get("captured_at", ""))
        if isinstance(meta.get("fingerprint"), dict) and (best is None or at > best[0]):
            best = (at, meta["fingerprint"])
    return best[1] if best else None


def _model_ids(client: ShikiClient, **q) -> list[str]:
    return [str(m.get("id")) for m in client.llm_models(**q).get("models", []) if m.get("id")]


def capture(
    client: ShikiClient,
    ctrl: ProxyControl,
    cfg: Config,
    *,
    label: str,
    exp: str,
    llm_config: Path | None,
    fixture_ids: Mapping[str, str] | None = None,
    fixture_names: Mapping[str, list[str]] | None = None,
    effort: str = "medium",
    root: Path | None = None,
    wait_secs: float = 120.0,
) -> Capture:
    """版のサーバーから取り込んで書く（fixture は呼び出し側が作っておく・`fixture_ids` は id → 印）。"""
    cfg_state = None
    if llm_config is not None:
        cfg_state = llm_config_state(llm_config)
        if not cfg_state["dummy_only"]:
            raise CaptureError(
                f"{llm_config} の鍵の形の項目がダミーではありません（値は出さない）。鍵は手元の中継だけが持つ"
            )
    run_id = safe_id(f"capture-{label}-{exp}-{int(time.time())}")
    ctrl.register_run(run_id, safe_id(exp), 0.01, capture_only=True)
    thread_id = None
    try:
        thread_id = client.create_thread()["id"]
        ctrl.register_session(thread_id, "capture", run_id)
        # 会話 ID のヘッダーを載せない設定（session_header が無い）でも受けられるよう、active にもする。
        ctrl.set_active(run_id, "capture")
        client.post_message(thread_id, CAPTURE_PROMPT, model=MODEL, effort=effort)
        client.wait_chat_idle(thread_id, timeout=wait_secs)
        body = pick_request(saved_requests(cfg.paths.cache / "requests" / run_id))
    finally:
        ctrl.remove_run(run_id)  # active と会話 ID の割り当ても外れる
        if thread_id:
            client.delete_thread(thread_id, missing_ok=True)
    cap, _ = from_request(body, fixed=cfg.capture_date, ids=dict(fixture_ids or {}))
    cap.label, cap.exp = label, exp
    caps = capabilities_of(client.openapi(), cap.tool_names)
    fp = fingerprint(
        llm_config_sha=cfg_state["sha"] if cfg_state else None,
        instructions=instructions_state(client.custom_instructions()),
        workflow_models=_model_ids(client, purpose="workflow"),
        decision_models=_model_ids(client, purpose="workflow", modality="decision"),
        fixtures=fixture_names or {},
        capabilities=caps,
        tools_sha=cap.tools_sha,
        system_sha=cap.system_sha,
    )
    cap.meta.update(
        {
            "captured_at": dt.datetime.now(dt.UTC).isoformat(timespec="seconds"),
            "tool_names": cap.tool_names,
            "fingerprint": fp,
            "fingerprint_sha": fingerprint_sha(fp),
            "changed_from_previous": changed_items(previous_fingerprint(label, exp, root), fp),
            "llm_config_key_like_items": cfg_state["key_like_items"] if cfg_state else None,
        }
    )
    cap.write(root)
    return cap
