"""日付の決め打ちを見つける（BUG-247）。製品の規則ではなく、課題の期待（実行の日に決まる値）から見る検査。

- **日付の文字**（`date_literals`）: IR のノードの設定（params）の文字（リテラル・`$template` の文・script の中身）に
  ある日付の形（`2026-11-07`・`2026/11/07`・`2026.11.07`・`2026年11月7日`・`11月7日`・`20261107`）。全角の数字は
  NFKC でそろえてから見る。月が 1〜12・日が 1〜31 のものだけを日付とみなす（UUID の一部や金額を拾わない）。
  実行の日に決まる値を持つ課題（`Expect.date_dependent`・run_case が `$run_date`・`@run_date`・`scheduled_at` を
  使う課題か、課題の印 `date_dependent`）では、どれも S2s に数える（`grade.static.date_literal_findings`）。
  依頼の中の日付は課題に無い（「今日」「先週」「実行した日」で頼む）ので、決め打ちとみなす。
- **予定の週**（`scheduled_window`）: 課題の run_case の `scheduled_at`（日本時間）の週（月曜はじまり）。Mode B の
  モデルが見る「今日」は測った日になる（製品の `today_note` は run の作成時刻から作る）ので、測る日がこの週に
  入ると、「今日」「先週」を見えている今日から決め打ちした IR も `scheduled_at` の run_case を通り得る。Mode B は、
  その日には結果が予定の日時で決まる課題（`Task.scheduled_dependent`）を流さない（`mode_b.date_window_reason`）。
  予定の週の前の週は入れない: その週の「今日」から「次の月曜の実行」を見越して先週の範囲を決め打ちする書き方は、
  Mode A の取り込みの今日（`[capture] fixed_date`・2026-11-04）にも同じに当たり、Mode B だけを止めると二つの
  モードの扱いがずれる（文字の形なら `date_literal` が両方で捕まえる）。

文字の形にならない決め打ち（script の中で `new Date(2026, 10, 7)` のように組み立てるなど）は見つけられない（README の
限界）。年の無い斜線の形（`11/9`・`11/2〜11/8`）は見ない（script の割り算や分数を日付と取り違えるため）。script の
注釈や llm.invoke の例示の中の日付も、実行に効かなくても数える（文の意味は読まない・README の限界）。
"""

from __future__ import annotations

import datetime as dt
import re
import unicodedata
from collections.abc import Iterator, Mapping
from typing import Any
from zoneinfo import ZoneInfo

JST = ZoneInfo("Asia/Tokyo")

# 年は 19xx・20xx に限る（版の番号 `1.2.3` や大きな数を日付にしない）。
_YMD = re.compile(r"(?<!\d)((?:19|20)\d{2})\s*[-/.]\s*(\d{1,2})\s*[-/.]\s*(\d{1,2})(?!\d)")
_YMD_JA = re.compile(r"(?<!\d)((?:19|20)\d{2})\s*年\s*(\d{1,2})\s*月\s*(\d{1,2})\s*日")
# 年の後ろも見る（「令和8年11月9日」「26年11月9日」の月日。西暦の年月日は先に `_YMD_JA` が取り、重なりは除く）。
_MD_JA = re.compile(r"(?<!\d)(\d{1,2})\s*月\s*(\d{1,2})\s*日")
_COMPACT = re.compile(r"(?<!\d)(20\d{2})(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])(?!\d)")


def _plausible(month: str, day: str) -> bool:
    return 1 <= int(month) <= 12 and 1 <= int(day) <= 31


def date_literals(text: str) -> list[str]:
    """文字の中の日付の形（見つかった順・重なる部分は先に見つけた形だけ）。"""
    s = unicodedata.normalize("NFKC", text)
    spans: list[tuple[int, int, str]] = []

    def add(m: re.Match, month: str, day: str) -> None:
        if not _plausible(month, day):
            return
        if any(a < m.end() and m.start() < b for a, b, _ in spans):
            return
        spans.append((m.start(), m.end(), m.group(0)))

    for m in _YMD.finditer(s):
        add(m, m.group(2), m.group(3))
    for m in _YMD_JA.finditer(s):
        add(m, m.group(2), m.group(3))
    for m in _MD_JA.finditer(s):
        add(m, m.group(1), m.group(2))
    for m in _COMPACT.finditer(s):
        add(m, m.group(2), m.group(3))
    return [t for _, _, t in sorted(spans)]


def _strings(value: Any, at: str) -> Iterator[tuple[str, str]]:
    """値の中の文字（(場所, 文字)）。参照（`$from`）の source と path は日付にならないので見ない。"""
    if isinstance(value, Mapping):
        if isinstance(value.get("$from"), str):
            if "default" in value:
                yield from _strings(value["default"], f"{at}/default")
            return
        for k, v in value.items():
            yield from _strings(v, f"{at}/{k}")
    elif isinstance(value, list):
        for i, v in enumerate(value):
            yield from _strings(v, f"{at}/{i}")
    elif isinstance(value, str):
        yield at, value


def ir_date_literals(ir: Mapping) -> list[tuple[str, str, str]]:
    """IR のノードの設定の中の日付の文字（(ノード id, 場所, 日付の文字)）。"""
    out = []
    for n in ir.get("nodes") or []:
        if not isinstance(n, Mapping):
            continue
        for at, text in _strings(n.get("params"), "/params"):
            for hit in date_literals(text):
                out.append((str(n.get("id")), at, hit))
    return out


def _jst_date(value: str) -> dt.date:
    t = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if t.tzinfo is None:
        t = t.replace(tzinfo=JST)
    return t.astimezone(JST).date()


def week_of(day: dt.date) -> tuple[dt.date, dt.date]:
    """その日を含む週（月曜〜日曜）。"""
    start = day - dt.timedelta(days=day.weekday())
    return start, start + dt.timedelta(days=6)


def scheduled_window(scheduled: list[str]) -> tuple[dt.date, dt.date] | None:
    """予定の週（日本時間・月曜〜日曜。予定が週をまたげば両端を含む範囲。`scheduled_at` が無ければ None）。"""
    if not scheduled:
        return None
    weeks = [week_of(_jst_date(s)) for s in scheduled]
    return min(w[0] for w in weeks), max(w[1] for w in weeks)


def in_window(now: dt.datetime, window: tuple[dt.date, dt.date] | None) -> bool:
    """測る時刻（日本時間の日付）が範囲に入るか。"""
    if window is None:
        return False
    day = now.astimezone(JST).date()
    return window[0] <= day <= window[1]
