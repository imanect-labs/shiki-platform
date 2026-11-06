"""run_case の照合子（GAP-30 §5.3）。

照合の材料（[`CaseResult`]・`dynamic.py` が組む）:

- `subject`（JSON Pointer で指す）: `{"status", "runs": [{"status", "input", "outputs": {ノード id: 出力}}], "outputs": 最後の
  run の出力}`。出力はノードの最後に成功した step のもの。
- `rows`: fixture のテーブルの ref → 実行の後の行（`{"id", "data"}`）。`rows_before` は実行の前（fixture のまま）。
- `files`: 実行で増えたファイル（`{"folder": ref か None（ルート）, "name", "text"}`）。

照合子（run_case の `expect`）:

- `checks`: `{"op": "equals"|"contains"|"regex"|"in"|"absent", "path": "/outputs/save/name", "value": …}`。
- `rows`: `{"table": ref, "where"?: {列: 値}, "count"?: n, "match"?: [{列: 値}], "absent"?: [{列: 値}], "unchanged"?: true}`。
  `where`・`match`・`absent` は列の部分一致。値には `{"$regex": "…"}`・`{"$absent": true}`（値が無いか null）・
  `{"$fixture": {…}}`（fixture から計算した値）を書ける。
- `files`: `{"folder": ref|null, "name_regex": "…", "count"?: n, "contains"?: [...], "not_contains"?: [...]}`。
- `no_new_rows`: 同じ起動をもう一度流しても、どのテーブルの行も増えない（`rows_rerun` と比べる）。

比べる前に NFKC と数字の正規化（`4`・`4.0`・`4,000`・全角の数字）をかける。

`{"$fixture": {"table": ref, "op": "sum"|"count", "field"?: 列, "where"?: {列: 値}, "range"?: {"field", "from", "to"}}}`
は、fixture の定義の行（`@rel:` は fixture を作った時刻で解く）から計算する。range は両端を含む（文字の大小で比べる。
日付 `YYYY-MM-DD` は文字の順が日付の順）。

`{"$run_date": {}}` は run を始めた日（日本時間の `YYYY-MM-DD`・製品の `$from run /date` と同じ決め方）と一致する
（形だけの正規表現にしない。決め打ちの日付を書いた IR を通さない）。`files[].name_regex` の中の `@run_date` も、
その日付（正規表現に逃がしたもの）に置き換える。run を始めた時刻が記録に無い（古い記録）ときは、run_case を
流し始めた時刻とその 10 分後の日付のどちらかと一致すればよい。
"""

from __future__ import annotations

import datetime as dt
import re
import unicodedata
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any

from abeval.fixtures import resolve_rel

MISSING = object()
_NUMBER = re.compile(r"^[+-]?(?:\d{1,3}(?:,\d{3})+|\d+)(?:\.\d+)?$")


# --- 正規化 ---


def _canon_number(d: Decimal) -> str:
    d = d.normalize()
    if d == 0:
        return "0"
    s = format(d, "f")
    return s


def normalize(value: Any) -> Any:
    """比べる前の正規化（文字は NFKC と前後の空白・数字は桁区切りと末尾の 0 を落とした文字）。"""
    if isinstance(value, bool) or value is None:
        return value
    if isinstance(value, Decimal):
        return _canon_number(value)
    if isinstance(value, int | float):
        try:
            return _canon_number(Decimal(str(value)))
        except InvalidOperation:
            return str(value)
    if isinstance(value, str):
        s = unicodedata.normalize("NFKC", value).strip()
        if _NUMBER.match(s):
            try:
                return _canon_number(Decimal(s.replace(",", "")))
            except InvalidOperation:
                return s
        return s
    if isinstance(value, Mapping):
        return {str(k): normalize(v) for k, v in value.items()}
    if isinstance(value, list | tuple):
        return [normalize(v) for v in value]
    return value


def text_of(value: Any) -> str:
    """部分一致に使う文字（NFKC）。"""
    if isinstance(value, str):
        return unicodedata.normalize("NFKC", value)
    return unicodedata.normalize("NFKC", str(normalize(value)))


def resolve_pointer(doc: Any, path: str) -> Any:
    """JSON Pointer（RFC 6901）で指す値（無ければ MISSING）。"""
    if path in ("", "/"):
        return doc
    if not path.startswith("/"):
        return MISSING
    cur = doc
    for raw in path[1:].split("/"):
        t = raw.replace("~1", "/").replace("~0", "~")
        if isinstance(cur, Mapping) and t in cur:
            cur = cur[t]
        elif isinstance(cur, list) and t.isdigit() and int(t) < len(cur):
            cur = cur[int(t)]
        else:
            return MISSING
    return cur


# --- fixture から計算した期待値 ---


def _fixture_rows(fixtures: Mapping, ref: str, now: dt.datetime) -> list[Mapping]:
    for t in fixtures.get("tables") or []:
        if t.get("ref") == ref:
            return [resolve_rel(r, now) for r in t.get("records") or []]
    raise KeyError(f"fixture にテーブル {ref!r} がありません")


def fixture_value(spec: Mapping, fixtures: Mapping, now: dt.datetime) -> Any:
    rows = _fixture_rows(fixtures, spec["table"], now)
    where = resolve_rel(spec.get("where") or {}, now)
    rows = [r for r in rows if all(normalize(r.get(k)) == normalize(v) for k, v in where.items())]
    rng = spec.get("range")
    if rng:
        lo, hi = resolve_rel(rng["from"], now), resolve_rel(rng["to"], now)
        rows = [
            r for r in rows if r.get(rng["field"]) is not None and lo <= str(r[rng["field"]]) <= hi
        ]
    op = spec.get("op", "count")
    if op == "count":
        return len(rows)
    if op == "sum":
        return sum(Decimal(str(r[spec["field"]])) for r in rows if r.get(spec["field"]) is not None)
    raise ValueError(f"$fixture の op が違います: {op!r}")


RUN_DATE_TOKEN = "@run_date"


def expand_expected(
    value: Any, fixtures: Mapping, now: dt.datetime, run_dates: Sequence[str] = ()
) -> Any:
    """期待値の中の `$fixture` を計算した値に、`$run_date` を run を始めた日の候補（`$one_of`）にする。"""
    if isinstance(value, Mapping):
        if set(value) == {"$fixture"}:
            return fixture_value(value["$fixture"], fixtures, now)
        if set(value) == {"$run_date"}:
            return {"$one_of": list(run_dates)}
        out = {k: expand_expected(v, fixtures, now, run_dates) for k, v in value.items()}
        if isinstance(out.get("name_regex"), str) and RUN_DATE_TOKEN in out["name_regex"]:
            alt = "|".join(re.escape(d) for d in run_dates) or "(?!)"
            out["name_regex"] = out["name_regex"].replace(RUN_DATE_TOKEN, f"(?:{alt})")
        return out
    if isinstance(value, list):
        return [expand_expected(v, fixtures, now, run_dates) for v in value]
    return value


JST = dt.timezone(dt.timedelta(hours=9))


def run_start_dates(started: Sequence[str | None], fallback: str | None) -> list[str]:
    """run を始めた日（日本時間）の候補。始めた時刻が無ければ、流し始めた時刻とその 10 分後の日。"""
    out: list[str] = []

    def add(t: dt.datetime) -> None:
        d = t.astimezone(JST).date().isoformat()
        if d not in out:
            out.append(d)

    for s in started:
        if s:
            add(dt.datetime.fromisoformat(s.replace("Z", "+00:00")))
    if not out and fallback:
        t = dt.datetime.fromisoformat(fallback.replace("Z", "+00:00"))
        add(t)
        add(t + dt.timedelta(minutes=10))
    return out


# --- 値の比べ方 ---


def equals(actual: Any, expected: Any) -> bool:
    if isinstance(expected, Mapping) and set(expected) == {"$one_of"}:
        return actual is not MISSING and any(
            normalize(actual) == normalize(w) for w in expected["$one_of"]
        )
    if isinstance(expected, Mapping) and set(expected) == {"$regex"}:
        return (
            isinstance(actual, str | int | float)
            and re.search(expected["$regex"], text_of(actual)) is not None
        )
    if isinstance(expected, Mapping) and set(expected) == {"$absent"}:
        return (actual is MISSING or actual is None) == bool(expected["$absent"])
    if actual is MISSING:
        return False
    return normalize(actual) == normalize(expected)


def contains(actual: Any, expected: Any) -> bool:
    if isinstance(actual, str):
        return text_of(expected) in text_of(actual)
    if isinstance(actual, list):
        return any(
            equals(a, expected) or (isinstance(a, Mapping) and subset(a, expected)) for a in actual
        )
    if isinstance(actual, Mapping) and isinstance(expected, Mapping):
        return subset(actual, expected)
    return False


def subset(actual: Mapping, expected: Mapping) -> bool:
    """`expected` の各列が `actual` の同じ列と一致する（列の部分一致）。"""
    if not isinstance(actual, Mapping) or not isinstance(expected, Mapping):
        return False
    return all(equals(actual.get(k, MISSING), v) for k, v in expected.items())


@dataclass
class CheckResult:
    kind: str
    ok: bool
    detail: str

    def to_dict(self) -> dict:
        return {"kind": self.kind, "ok": self.ok, "detail": self.detail}


def check_pointer(spec: Mapping, subject: Any) -> CheckResult:
    op, path = spec["op"], spec["path"]
    actual = resolve_pointer(subject, path)
    want = spec.get("value")
    if op == "absent":
        ok = actual is MISSING or actual is None
    elif op == "equals":
        ok = equals(actual, want)
    elif op == "contains":
        ok = actual is not MISSING and contains(actual, want)
    elif op == "regex":
        ok = (
            actual is not MISSING
            and isinstance(actual, str | int | float)
            and re.search(str(want), text_of(actual)) is not None
        )
    elif op == "in":
        ok = actual is not MISSING and any(equals(actual, w) for w in want)
    else:
        raise ValueError(op)
    shown = "（無い）" if actual is MISSING else repr(actual)[:200]
    return CheckResult(f"check:{op}", ok, f"{path} = {shown}（期待 {op} {want!r}）")


def _data(rows: Sequence[Mapping]) -> list[Mapping]:
    return [r.get("data") or {} for r in rows]


def check_rows(
    spec: Mapping, rows: Mapping[str, list], before: Mapping[str, list]
) -> list[CheckResult]:
    ref = spec["table"]
    all_rows = rows.get(ref)
    if all_rows is None:
        return [CheckResult("rows", False, f"{ref}: 行を読めていません")]
    data = _data(all_rows)
    where = spec.get("where") or {}
    picked = [d for d in data if subset(d, where)]
    out: list[CheckResult] = []
    if "count" in spec:
        ok = len(picked) == int(normalize(spec["count"]))
        out.append(
            CheckResult(
                "rows:count", ok, f"{ref} {where}: {len(picked)} 行（期待 {spec['count']}）"
            )
        )
    for want in spec.get("match") or []:
        ok = any(subset(d, want) for d in picked)
        out.append(
            CheckResult(
                "rows:match",
                ok,
                f"{ref}: {want} に合う行{'がある' if ok else 'が無い'}（{_short(picked)}）",
            )
        )
    for bad in spec.get("absent") or []:
        hit = [d for d in picked if subset(d, bad)]
        out.append(CheckResult("rows:absent", not hit, f"{ref}: {bad} に合う行が {len(hit)} 行"))
    if spec.get("unchanged"):
        b = {r["id"]: normalize(r.get("data") or {}) for r in before.get(ref) or []}
        a = {r["id"]: normalize(r.get("data") or {}) for r in all_rows}
        out.append(
            CheckResult(
                "rows:unchanged", a == b, f"{ref}: 実行の前と{'同じ' if a == b else '違う'}"
            )
        )
    return out


def _short(rows: list, n: int = 400) -> str:
    return repr(rows)[:n]


def check_files(spec: Mapping, files: Sequence[Mapping]) -> list[CheckResult]:
    folder = spec.get("folder")
    pat = re.compile(spec["name_regex"])
    picked = [
        f
        for f in files
        if f.get("folder") == folder
        and pat.search(unicodedata.normalize("NFKC", f.get("name", "")))
    ]
    names = [f.get("name") for f in files]
    out = []
    if "count" in spec:
        ok = len(picked) == int(spec["count"])
        out.append(
            CheckResult(
                "files:count",
                ok,
                f"{folder}/{spec['name_regex']}: {len(picked)} 件（期待 {spec['count']}・増えたもの {names}）",
            )
        )
    else:
        out.append(
            CheckResult(
                "files:exists",
                bool(picked),
                f"{folder}/{spec['name_regex']}: {len(picked)} 件（増えたもの {names}）",
            )
        )
    for s in spec.get("contains") or []:
        ok = any(text_of(s) in text_of(f.get("text") or "") for f in picked)
        out.append(
            CheckResult(
                "files:contains",
                ok,
                f"{folder}/{spec['name_regex']} に {s!r} が{'ある' if ok else '無い'}",
            )
        )
    for s in spec.get("not_contains") or []:
        hit = any(text_of(s) in text_of(f.get("text") or "") for f in picked)
        out.append(
            CheckResult(
                "files:not_contains",
                not hit,
                f"{folder}/{spec['name_regex']} に {s!r} が{'ある' if hit else '無い'}",
            )
        )
    return out


def check_no_new_rows(after: Mapping[str, list], rerun: Mapping[str, list] | None) -> CheckResult:
    if rerun is None:
        return CheckResult("no_new_rows", False, "再実行の結果がありません")
    grown = {
        k: (len(after.get(k) or []), len(v))
        for k, v in rerun.items()
        if len(v) > len(after.get(k) or [])
    }
    return CheckResult("no_new_rows", not grown, f"再実行で増えた行（前, 後）: {grown}")


@dataclass
class CaseResult:
    """1 つの run_case の結果（照合の材料）。"""

    statuses: list[str]
    subject: Mapping[str, Any]
    rows: Mapping[str, list]
    rows_before: Mapping[str, list]
    files: list[Mapping]
    rows_rerun: Mapping[str, list] | None = None
    notes: list[str] = field(default_factory=list)
    # run を始めた日（日本時間の YYYY-MM-DD）の候補（`$run_date` と name_regex の `@run_date`）。
    run_dates: list[str] = field(default_factory=list)


def status_result(expected: str, statuses: Sequence[str]) -> CheckResult:
    if expected == "any":
        return CheckResult("status", True, f"状態は見ない（{list(statuses)}）")
    if expected == "succeeded":
        ok = bool(statuses) and all(s == "succeeded" for s in statuses)
    else:
        ok = bool(statuses) and statuses[-1] == expected
    return CheckResult("status", ok, f"状態 {list(statuses)}（期待 {expected}）")


def evaluate(
    expect: Mapping, result: CaseResult, fixtures: Mapping, now: dt.datetime
) -> list[CheckResult]:
    """run_case の期待を全部照合する（S4 は全部が ok）。"""
    exp = expand_expected(dict(expect), fixtures, now, result.run_dates)
    out = [status_result(exp.get("status", "succeeded"), result.statuses)]
    for c in exp.get("checks") or []:
        out.append(check_pointer(c, result.subject))
    for r in exp.get("rows") or []:
        out += check_rows(r, result.rows, result.rows_before)
    for f in exp.get("files") or []:
        out += check_files(f, result.files)
    if exp.get("no_new_rows"):
        out.append(check_no_new_rows(result.rows, result.rows_rerun))
    return out
