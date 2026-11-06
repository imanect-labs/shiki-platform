"""課題の fixture（テーブル・行・フォルダー・CSV）を作り、作ったものだけを片付ける（GAP-30 §4.7 の 1・11）。

課題の `fixtures` の形（ここで読む部分。課題のスキーマの正は後の段の tasks.py）:

```json
{
  "folders": [{"ref": "weekly", "name": "週報", "parent": "<folder の ref>"}],
  "tables":  [{"ref": "daily", "name": "日報", "schema": {"fields": [...]},
               "records": [{"report_date": "@rel:-6d", "visits": 3}]}],
  "csv":     [{"ref": "survey", "name": "アンケート.csv", "folder": "<folder の ref>", "text": "a,b\\n1,2\\n"}]
}
```

- 値の `@rel:<N><d|w|h|m>` は基準の時刻（Asia/Tokyo）からの相対。`d`・`w` は日付（`YYYY-MM-DD`）、`@rel:-6dT09:00` は
  その日の日本時間の時刻（RFC 3339・+09:00）、`h`・`m` は時刻（RFC 3339・+09:00・秒は 0）。
- 値の `@id:<ref>` は、先に作った fixture の id（フォルダー・テーブル・CSV のファイル）。`@record:<table の ref>:<番号>`
  は、そのテーブルの何番目（0 から）の行の id。
- 作った id は順に `FixtureRecord` に残す（途中で失敗しても、そこまでの分を片付けられる）。
- 片付けで消すのは、記録した id と、試行の開始の後に作られた `abeval-` の印付きのもの（ルート直下のノード・
  テーブル・ワークフロー）だけ。本人の全資源を消すことはしない。
- 番兵（alice のルート直下のフォルダー `abeval-sentinel`）が無いサーバーでは、作るのも片付けるのも拒む
  （キャンペーンの DB などで消さないため）。番兵は `python -m abeval.doctor --init-sentinel` でだけ作る。
"""

from __future__ import annotations

import datetime as dt
import re
from collections.abc import Mapping
from dataclasses import asdict, dataclass, field
from typing import Any
from zoneinfo import ZoneInfo

from abeval.shiki import ShikiClient, ShikiError

JST = ZoneInfo("Asia/Tokyo")
SENTINEL_NAME = "abeval-sentinel"
MARK = "abeval-"
_REL = re.compile(r"^@rel:([+-]?\d{1,5})([dwhm])(?:T(\d{2}):(\d{2}))?$")
_ID = re.compile(r"^@id:([A-Za-z0-9_.-]+)$")
_RECORD = re.compile(r"^@record:([A-Za-z0-9_.-]+):(\d+)$")
# 片付けの順（後に作ったものから・行はテーブルごと消えるので、テーブルが記録にあれば消さない）。
KINDS = ("workflow", "thread", "file", "record", "table", "folder")


class FixtureError(RuntimeError):
    """fixture の定義の誤り・作成の失敗（`record` にそこまで作った分が入る）。"""

    def __init__(self, message: str, record: FixtureRecord | None = None):
        super().__init__(message)
        self.record = record


class SentinelMissing(RuntimeError):
    """番兵が無い（評価用のサーバーではない）。"""


def now_utc() -> dt.datetime:
    return dt.datetime.now(dt.UTC)


def _iso(t: dt.datetime) -> str:
    return t.isoformat(timespec="seconds")


def resolve_rel(value: Any, now: dt.datetime) -> Any:
    """値の中の `@rel:` を基準の時刻からの相対の日付・時刻に解く（入れ子の dict・list も）。"""
    if now.tzinfo is None:
        raise ValueError("基準の時刻は時差つきで渡します")
    if isinstance(value, Mapping):
        return {k: resolve_rel(v, now) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve_rel(v, now) for v in value]
    if not isinstance(value, str) or not value.startswith("@rel:"):
        return value
    m = _REL.match(value)
    if not m:
        raise FixtureError(f"@rel: の書き方が違います: {value!r}")
    n, unit, hh, mm = int(m.group(1)), m.group(2), m.group(3), m.group(4)
    local = now.astimezone(JST)
    if unit in ("d", "w"):
        day = local.date() + dt.timedelta(days=n * (7 if unit == "w" else 1))
        if hh is None:
            return day.isoformat()
        h, mi = int(hh), int(mm)
        if h > 23 or mi > 59:
            raise FixtureError(f"@rel: の時刻が違います: {value!r}")
        return _iso(dt.datetime(day.year, day.month, day.day, h, mi, tzinfo=JST))
    if hh is not None:
        raise FixtureError(f"@rel: の時刻は d か w にだけ付けます: {value!r}")
    delta = dt.timedelta(hours=n) if unit == "h" else dt.timedelta(minutes=n)
    return _iso((local + delta).replace(second=0, microsecond=0))


def resolve_refs(value: Any, refs: Mapping[str, str], rows: Mapping[str, list[str]]) -> Any:
    """値の中の `@id:<ref>`・`@record:<ref>:<番号>` を、作った fixture の id に置き換える。"""
    if isinstance(value, Mapping):
        return {k: resolve_refs(v, refs, rows) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve_refs(v, refs, rows) for v in value]
    if isinstance(value, str):
        m = _ID.match(value)
        if m:
            if m.group(1) not in refs:
                raise FixtureError(f"まだ作っていない fixture です: {value!r}")
            return refs[m.group(1)]
        m = _RECORD.match(value)
        if m:
            ids = rows.get(m.group(1))
            i = int(m.group(2))
            if ids is None or i >= len(ids):
                raise FixtureError(f"まだ作っていない行です: {value!r}")
            return ids[i]
    return value


@dataclass
class Created:
    kind: str
    id: str
    name: str
    ref: str | None = None
    # 行のテーブル・ファイルやフォルダーの親。
    parent: str | None = None


@dataclass
class FixtureRecord:
    """試行で作ったもの（試行の記録に残す）。`started_at` より後に作られた印付きのものも片付けの対象。"""

    user: str
    started_at: str
    created: list[Created] = field(default_factory=list)
    refs: dict[str, str] = field(default_factory=dict)
    rows: dict[str, list[str]] = field(default_factory=dict)

    def add(self, kind: str, id: str, name: str, ref: str | None = None, parent: str | None = None):
        if kind not in KINDS:
            raise ValueError(f"記録できない種類です: {kind!r}")
        self.created.append(Created(kind, id, name, ref, parent))
        if ref and kind in ("folder", "table", "file"):
            self.refs[ref] = id

    def ids(self, kind: str) -> list[str]:
        return [c.id for c in self.created if c.kind == kind]

    def to_dict(self) -> dict:
        return {
            "user": self.user,
            "started_at": self.started_at,
            "created": [asdict(c) for c in self.created],
            "refs": dict(self.refs),
            "rows": {k: list(v) for k, v in self.rows.items()},
        }

    @classmethod
    def from_dict(cls, raw: Mapping) -> FixtureRecord:
        rec = cls(user=raw["user"], started_at=raw["started_at"])
        for c in raw.get("created", []):
            rec.created.append(Created(**c))
        rec.refs = dict(raw.get("refs", {}))
        rec.rows = {k: list(v) for k, v in raw.get("rows", {}).items()}
        return rec


def new_record(user: str) -> FixtureRecord:
    """試行の記録を始める（開始の時刻はマイクロ秒まで・印付きの片付けの境）。"""
    return FixtureRecord(user=user, started_at=now_utc().isoformat())


def find_sentinel(client: ShikiClient) -> dict | None:
    node = client.node_by_name(SENTINEL_NAME)
    return node if node and node.get("kind") == "folder" else None


def require_sentinel(client: ShikiClient) -> dict:
    node = find_sentinel(client)
    if node is None:
        raise SentinelMissing(
            f"番兵（{client.user} のルート直下のフォルダー「{SENTINEL_NAME}」）がありません。評価用のサーバーでなければ"
            "作らないでください（作るときは python -m abeval.doctor --init-sentinel）"
        )
    return node


def init_sentinel(client: ShikiClient) -> tuple[dict, bool]:
    """番兵を作る（あれば作らない）。(ノード, 作ったか)。"""
    node = find_sentinel(client)
    if node is not None:
        return node, False
    return client.create_folder(SENTINEL_NAME), True


def _spec_list(spec: Mapping, key: str) -> list[Mapping]:
    items = spec.get(key) or []
    if not isinstance(items, list) or not all(isinstance(x, Mapping) for x in items):
        raise FixtureError(f"fixtures.{key} はオブジェクトの配列です")
    return items


def _name(item: Mapping, where: str) -> str:
    name = item.get("name")
    if not isinstance(name, str) or not name.strip():
        raise FixtureError(f"{where} の name がありません")
    return name


def create_fixtures(
    client: ShikiClient,
    spec: Mapping | None,
    *,
    now: dt.datetime | None = None,
    record: FixtureRecord | None = None,
) -> FixtureRecord:
    """fixture を作る（フォルダー → テーブル → 行 → CSV の順）。失敗したら FixtureError（record つき）。"""
    spec = spec or {}
    now = now or now_utc()
    rec = record or new_record(client.user)
    require_sentinel(client)
    try:
        for i, f in enumerate(_spec_list(spec, "folders")):
            name = _name(f, f"folders[{i}]")
            parent = rec.refs.get(f["parent"]) if f.get("parent") else None
            if f.get("parent") and parent is None:
                raise FixtureError(f"folders[{i}] の parent がまだありません: {f['parent']!r}")
            node = client.create_folder(name, parent)
            rec.add("folder", node["id"], name, f.get("ref"), parent)
        for i, t in enumerate(_spec_list(spec, "tables")):
            name = _name(t, f"tables[{i}]")
            schema = resolve_refs(t.get("schema") or {"fields": []}, rec.refs, rec.rows)
            table = client.create_table(name, schema)
            rec.add("table", table["id"], name, t.get("ref"))
            _insert_rows(client, rec, t, table["id"], now)
        for i, c in enumerate(_spec_list(spec, "csv")):
            name = _name(c, f"csv[{i}]")
            text = c.get("text")
            if not isinstance(text, str):
                raise FixtureError(f"csv[{i}] の text がありません")
            folder = rec.refs.get(c["folder"]) if c.get("folder") else None
            if c.get("folder") and folder is None:
                raise FixtureError(f"csv[{i}] の folder がまだありません: {c['folder']!r}")
            saved = client.tabular_save(name, text, folder)
            rec.add("file", saved["node_id"], saved.get("name", name), c.get("ref"), folder)
    except FixtureError as e:
        e.record = rec
        raise
    except (ShikiError, KeyError, TypeError) as e:
        raise FixtureError(f"fixture を作れません: {e}", rec) from e
    return rec


def _insert_rows(
    client: ShikiClient, rec: FixtureRecord, table_spec: Mapping, table_id: str, now: dt.datetime
) -> None:
    rows = table_spec.get("records") or []
    if not isinstance(rows, list) or not all(isinstance(r, Mapping) for r in rows):
        raise FixtureError("tables[].records はオブジェクトの配列です")
    ref = table_spec.get("ref") or table_id
    ids = rec.rows.setdefault(ref, [])
    for row in rows:
        data = resolve_refs(resolve_rel(row, now), rec.refs, rec.rows)
        created = client.create_record(table_id, data)
        ids.append(created["id"])


def reload_rows(
    client: ShikiClient, rec: FixtureRecord, spec: Mapping, *, now: dt.datetime | None = None
) -> None:
    """fixture のテーブルの行を消して入れ直す（run_case の前・記録にあるテーブルだけ）。"""
    now = now or now_utc()
    require_sentinel(client)
    for t in _spec_list(spec, "tables"):
        ref = t.get("ref")
        table_id = rec.refs.get(ref) if ref else None
        if table_id is None or table_id not in rec.ids("table"):
            raise FixtureError(f"記録に無いテーブルの行は入れ直しません: {ref!r}")
        for row in client.records(table_id):
            client.delete_record(table_id, row["id"], row["rev"], missing_ok=True)
        rec.rows[ref] = []
        _insert_rows(client, rec, t, table_id, now)


@dataclass
class CleanupReport:
    deleted: list[tuple[str, str, str]] = field(default_factory=list)
    missing: list[tuple[str, str]] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return not self.errors


def _parse_time(value: object) -> dt.datetime | None:
    if not isinstance(value, str):
        return None
    try:
        t = dt.datetime.fromisoformat(value)
    except ValueError:
        return None
    return t if t.tzinfo else t.replace(tzinfo=dt.UTC)


def _delete(client: ShikiClient, kind: str, id: str, parent: str | None) -> bool:
    if kind == "workflow":
        return client.delete_artifact(id, missing_ok=True)
    if kind == "thread":
        return client.delete_thread(id, missing_ok=True)
    if kind == "file":
        return client.delete_file(id, missing_ok=True)
    if kind == "table":
        return client.delete_table(id, missing_ok=True)
    if kind == "folder":
        return client.delete_folder(id, missing_ok=True)
    if kind == "record":
        if parent is None:
            raise ValueError("行の片付けにはテーブルの id が要ります")
        try:
            rev = client.get_record(parent, id)["rev"]
        except ShikiError as e:
            if e.status == 404:
                return False
            raise
        return client.delete_record(parent, id, rev, missing_ok=True)
    raise ValueError(kind)


def cleanup(client: ShikiClient, rec: FixtureRecord, *, sweep: bool = True) -> CleanupReport:
    """記録した id（新しい順）と、開始の後に作られた印付きのもの（`sweep`）を消す。番兵が無ければ拒む。"""
    sentinel = require_sentinel(client)
    report = CleanupReport()
    tables = set(rec.ids("table"))
    done: set[str] = set()
    for c in reversed(rec.created):
        if c.kind == "record" and c.parent in tables:
            continue  # テーブルごと消える
        try:
            if _delete(client, c.kind, c.id, c.parent):
                report.deleted.append((c.kind, c.id, c.name))
            else:
                report.missing.append((c.kind, c.id))
            done.add(c.id)
        except (ShikiError, ValueError) as e:
            report.errors.append(f"{c.kind} {c.id}: {e}")
    if sweep:
        _sweep(client, rec, sentinel["id"], done, report)
    return report


def _sweep(
    client: ShikiClient, rec: FixtureRecord, sentinel_id: str, done: set[str], report: CleanupReport
) -> None:
    since = _parse_time(rec.started_at)
    if since is None:
        report.errors.append(
            f"started_at が読めないので印付きの片付けをしません: {rec.started_at!r}"
        )
        return

    def marked(name: object, when: object, id: object) -> bool:
        t = _parse_time(when)
        return (
            isinstance(name, str)
            and name.startswith(MARK)
            and name != SENTINEL_NAME
            and id != sentinel_id
            and id not in done
            and t is not None
            and t >= since
        )

    targets: list[tuple[str, str, str]] = []
    try:
        for w in client.workflows():
            if marked(w.get("name"), w.get("updated_at"), w.get("id")):
                targets.append(("workflow", w["id"], w["name"]))
        for t in client.tables(q=MARK):
            if marked(t.get("name"), t.get("created_at"), t.get("id")):
                targets.append(("table", t["id"], t["name"]))
        for n in client.nodes():
            if marked(n.get("name"), n.get("created_at"), n.get("id")):
                targets.append(
                    ("folder" if n.get("kind") == "folder" else "file", n["id"], n["name"])
                )
    except ShikiError as e:
        report.errors.append(f"印付きのものを一覧できません: {e}")
        return
    for kind, id, name in targets:
        try:
            if _delete(client, kind, id, None):
                report.deleted.append((kind, id, name))
        except ShikiError as e:
            report.errors.append(f"{kind} {id}: {e}")
