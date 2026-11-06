"""abeval.capture（日付の置き換え・id の印・指紋・取り込みの全体を偽の shiki と capture_only の中継で）。

上流（偽の上流）には一度も届かないこと（capture_only は $0）も確かめる。
"""

from __future__ import annotations

import datetime as dt
import json
import re

import pytest

from abeval import capture as cap_mod
from abeval import fixtures
from abeval.capture import (
    DATE_NOTE,
    Capture,
    CaptureError,
    changed_items,
    emit_tool,
    fingerprint,
    fingerprint_sha,
    from_request,
    llm_config_state,
    normalize_system,
    pick_request,
    scrub_ids,
    today_note,
)
from abeval.config import ROOT, load_config
from abeval.ledger import Ledger
from abeval.shiki import ShikiClient
from fake_chat import SYSTEM, TODAY_NOTE, ChatFake
from fake_upstream import FakeUpstream
from inproc import running_proxy

OPTS_RS = ROOT.parent.parent / "crates" / "chat" / "src" / "worker" / "opts.rs"
FIXED = dt.date(2026, 11, 9)


def product_format() -> str:
    """製品の today_note の format! の文字列（opts.rs から読む・書式のずれを見張る）。"""
    src = OPTS_RS.read_text(encoding="utf-8")
    body = src[src.index("pub(crate) fn today_note") :]
    lit = re.search(r'format!\(\s*"((?:[^"\\]|\\.)*)"', body).group(1)
    return lit.replace("\\n", "\n")


def render_product(day: dt.date, tz: str) -> str:
    fmt = product_format().replace("{weekday}", "{w}").replace("{tz}", "{z}")
    fmt = fmt.replace("{}", "{y}", 1).replace("{}", "{m}", 1).replace("{}", "{d}", 1)
    return fmt.format(y=day.year, m=day.month, d=day.day, w="月火水木金土日"[day.weekday()], z=tz)


@pytest.mark.parametrize("day", [dt.date(2026, 11, 6), dt.date(2026, 1, 31), dt.date(2027, 12, 5)])
def test_date_note_matches_the_product_format(day):
    rendered = render_product(day, "Asia/Tokyo")
    assert DATE_NOTE.fullmatch(rendered), rendered
    assert today_note(day, "Asia/Tokyo") == rendered


def test_normalize_replaces_the_date_and_keeps_the_original():
    system = "基本の指示。" + render_product(dt.date(2026, 11, 6), "Asia/Tokyo")
    out, found = normalize_system(system, FIXED)
    assert found == ["2026-11-06"]
    assert out == "基本の指示。" + render_product(FIXED, "Asia/Tokyo")
    # 置き換えた後は同じ（取り込みの日が違っても sha が同じ）。
    assert normalize_system(out, FIXED)[0] == out
    assert normalize_system("日付の無い system", FIXED) == ("日付の無い system", [])


def test_scrub_ids_marks_fixture_ids_and_hides_other_uuids():
    fid, other = "11111111-2222-3333-4444-555555555555", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    v, n = scrub_ids({"d": f"- 日報 / {fid}\n- 他 / {other}", "l": [fid]}, {fid: "@table:daily"})
    assert v == {"d": "- 日報 / @table:daily\n- 他 / <uuid>", "l": ["@table:daily"]}
    assert n == 1


def request(fid: str, day: dt.date) -> dict:
    return {
        "model": "claude-haiku-5-5",
        "max_tokens": 8192,
        "system": SYSTEM + render_product(day, "Asia/Tokyo"),
        "tools": [
            {"name": "tool_search", "description": "x", "input_schema": {}},
            {
                "name": "emit_workflow",
                "description": "e",
                "input_schema": {},
                "defer_loading": True,
            },
            {"name": "data_query", "description": f"一覧:\n- 日報 / {fid}\n", "input_schema": {}},
        ],
        "messages": [{"role": "user", "content": "こんにちは。"}],
    }


def test_capture_sha_does_not_depend_on_ids_or_the_day():
    a, _ = from_request(
        request("11111111-2222-3333-4444-555555555555", dt.date(2026, 11, 6)),
        fixed=FIXED,
        ids={"11111111-2222-3333-4444-555555555555": "@table:daily"},
    )
    b, _ = from_request(
        request("99999999-2222-3333-4444-555555555555", dt.date(2026, 11, 20)),
        fixed=FIXED,
        ids={"99999999-2222-3333-4444-555555555555": "@table:daily"},
    )
    assert a.sha == b.sha
    assert a.meta["original_dates"] == ["2026-11-06"] and b.meta["original_dates"] == ["2026-11-20"]
    tool = a.emit_tool()
    assert "defer_loading" not in tool and tool["name"] == "emit_workflow"


def test_pick_request_skips_requests_without_emit_workflow():
    title = {"model": "x", "max_tokens": 64, "system": "題名", "messages": []}
    chat = request("11111111-2222-3333-4444-555555555555", FIXED)
    assert pick_request([title, chat]) is chat
    with pytest.raises(CaptureError):
        pick_request([title])
    with pytest.raises(CaptureError):
        emit_tool([{"name": "tool_search"}])


FP = dict(
    llm_config_sha="c" * 64,
    instructions="empty",
    workflow_models=["claude-haiku-5-5", "default"],
    decision_models=[],
    fixtures={"tables": ["日報"], "folders": [], "csv": []},
    capabilities={"node_ports": True, "trigger": True},
    tools_sha="t" * 64,
    system_sha="s" * 64,
)


@pytest.mark.parametrize(
    "field,value",
    [
        ("llm_config_sha", "d" * 64),
        ("instructions", "set"),
        ("workflow_models", ["default"]),
        ("decision_models", ["decide-stub"]),
        ("fixtures", {"tables": ["週報"], "folders": [], "csv": []}),
        ("capabilities", {"node_ports": False, "trigger": True}),
        ("tools_sha", "u" * 64),
        ("system_sha", "z" * 64),
    ],
)
def test_each_fingerprint_item_changes_the_fingerprint(field, value):
    base = fingerprint(**FP)
    other = fingerprint(**{**FP, field: value})
    assert fingerprint_sha(base) != fingerprint_sha(other)
    want = {"instructions": "custom_instructions"}.get(field, field)
    assert changed_items(base, other) == [want]
    assert changed_items(None, other) == []


def test_llm_config_state_checks_the_key_is_the_dummy_without_showing_it(tmp_path):
    p = tmp_path / "llm-eval.toml"
    p.write_text('[llm]\n[[llm.providers]]\nid = "a"\napi_key = "abeval-dummy"\n', encoding="utf-8")
    st = llm_config_state(p)
    assert st["dummy_only"] and st["key_like_items"] == ["llm.providers[0].api_key"]
    p.write_text(
        '[llm]\n[[llm.providers]]\nid = "a"\napi_key = "not-the-dummy"\n', encoding="utf-8"
    )
    st2 = llm_config_state(p)
    assert not st2["dummy_only"]
    assert "not-the-dummy" not in json.dumps(st2)
    assert st["sha"] == st2["sha"]  # 鍵の形の項目は sha に入れない


def test_capture_roundtrip_and_tamper_detection(tmp_path):
    c, _ = from_request(request("11111111-2222-3333-4444-555555555555", FIXED), fixed=FIXED, ids={})
    c.label, c.exp = "v0", "e0"
    d = c.write(tmp_path)
    back = Capture.read("v0", "e0", tmp_path)
    assert back.sha == c.sha and back.tools == c.tools
    (d / "system.txt").write_text("書き換え", encoding="utf-8")
    with pytest.raises(CaptureError):
        Capture.read("v0", "e0", tmp_path)


@pytest.fixture()
def stack():
    """偽の shiki（製品の要求のまねを中継へ送る）・capture_only を受ける中継・偽の上流。"""
    with FakeUpstream() as up, running_proxy(up.url) as proxy:
        fake = ChatFake(llm_url=proxy.url)
        client = ShikiClient("http://localhost:8080", "alice", transport=fake.transport())
        fixtures.init_sentinel(client)
        yield fake, client, proxy, up
        client.close()


TASK_FIX = {
    "tables": [
        {"ref": "daily", "name": "日報", "schema": {"fields": [{"key": "d", "type": "text"}]}}
    ]
}


def test_capture_end_to_end_costs_nothing_and_reaches_no_upstream(stack, tmp_path):
    fake, client, proxy, up = stack
    cfg = load_config()
    rec = fixtures.create_fixtures(client, TASK_FIX)
    llm = tmp_path / "llm-eval.toml"
    llm.write_text(
        '[[llm.providers]]\nid = "anthropic"\napi_key = "abeval-dummy"\n', encoding="utf-8"
    )
    with proxy.control() as ctrl:
        c = cap_mod.capture(
            client,
            ctrl,
            cfg,
            label="v0",
            exp="e0",
            llm_config=llm,
            fixture_ids={rec.refs["daily"]: "@table:daily"},
            fixture_names={"tables": ["日報"]},
            root=tmp_path / "captures",
        )
        assert ctrl.spend()["runs"] == {}  # 取り込みの実行は登録を外す
    assert up.received == []
    assert fake.llm_replies == [200, 200]
    assert c.system == SYSTEM + today_note(cfg.capture_date, "Asia/Tokyo")
    assert cfg.capture_date == dt.date(2026, 11, 4)  # eval.toml の [capture] fixed_date
    assert c.meta["original_dates"] == ["2026-11-06"]
    assert c.meta["fingerprint"]["custom_instructions"] == "empty"
    assert c.meta["fingerprint"]["fixtures"]["tables"] == ["日報"]
    data = next(t for t in c.tools if t["name"] == "data_query")
    assert "@table:daily" in data["description"] and rec.refs["daily"] not in data["description"]
    files = sorted(p.name for p in (tmp_path / "captures" / "v0" / "e0").iterdir())
    assert files == ["meta.json", "system.txt", "tools.json"]
    entries = Ledger(cfg.paths.ledger).entries()
    assert entries and all(e["captured"] and e["cost_micros"] == 0 for e in entries)
    assert fake.threads == {}  # スレッドは消した
    fixtures.cleanup(client, rec)
    assert TODAY_NOTE not in (tmp_path / "captures" / "v0" / "e0" / "system.txt").read_text()


def test_second_capture_reports_what_changed(stack, tmp_path):
    fake, client, proxy, _ = stack
    cfg = load_config()
    with proxy.control() as ctrl:
        cap_mod.capture(client, ctrl, cfg, label="v0", exp="e0", llm_config=None, root=tmp_path)
        fake.custom_instructions = {"about": "経理の担当", "style": "", "tone": "default"}
        c2 = cap_mod.capture(
            client, ctrl, cfg, label="v0", exp="e1", llm_config=None, root=tmp_path
        )
    assert c2.meta["changed_from_previous"] == ["custom_instructions"]


def test_capture_refuses_a_real_looking_key(stack, tmp_path):
    _, client, proxy, up = stack
    llm = tmp_path / "llm-eval.toml"
    llm.write_text('[[llm.providers]]\nid = "anthropic"\napi_key = "zzz"\n', encoding="utf-8")
    with proxy.control() as ctrl, pytest.raises(CaptureError):
        cap_mod.capture(
            client, ctrl, load_config(), label="v0", exp="e0", llm_config=llm, root=tmp_path
        )
    assert up.received == []
