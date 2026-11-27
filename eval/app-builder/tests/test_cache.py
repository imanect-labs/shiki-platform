from __future__ import annotations

import os

import pytest

from abeval.cache import TrialCache, trial_key


def _parts(**over) -> dict:
    parts = {
        "task_id": "basic-summary-file",
        "mode": "a",
        "phrasing": "flow",
        "prompt": "入力の本文を要約して、日付の入った名前のファイルに保存するフローを作ってください。",
        "fixtures": {"folders": [{"ref": "out", "name": "要約"}]},
        "trial": 0,
        "condition": {"effort": "medium", "max_steps": 6},
        "config_sha": "c" * 64,
        "capture_sha": "d" * 64,
        "binary_sha": "e" * 64,
    }
    parts.update(over)
    return parts


@pytest.mark.parametrize(
    "change",
    [
        {"config_sha": "1" * 64},
        {"capture_sha": "2" * 64},
        {"binary_sha": "3" * 64},
        {"phrasing": "app"},
        {"prompt": "別の依頼文"},
        {"fixtures": {"folders": [{"ref": "out", "name": "要約2"}]}},
        {"trial": 1},
        {"condition": {"effort": "high", "max_steps": 6}},
        {"task_id": "basic-branch-amount"},
        {"mode": "b"},
    ],
)
def test_key_changes_with_each_part(change):
    assert trial_key(**_parts(**change)) != trial_key(**_parts())


def test_key_is_stable_and_order_independent():
    a = trial_key(**_parts(condition={"effort": "low", "max_steps": 6}))
    b = trial_key(**_parts(condition={"max_steps": 6, "effort": "low"}))
    assert a == b
    assert len(a) == 64


def test_grader_version_does_not_change_the_key(tmp_path):
    cache = TrialCache(tmp_path)
    key = trial_key(**_parts())
    cache.put(key, {"grader": "v1", "ir": {"name": "x"}})
    # 採点器の版はキーの材料ではない（記録に入れても同じキー）。
    assert trial_key(**_parts()) == key
    cache.put(key, {"grader": "v2", "ir": {"name": "x"}})
    assert cache.get(key)["grader"] == "v2"


def test_key_rejects_missing_or_bad_parts():
    with pytest.raises(TypeError):
        trial_key(**{k: v for k, v in _parts().items() if k != "binary_sha"})
    for bad in ({"trial": -1}, {"trial": True}, {"binary_sha": ""}, {"prompt": None}):
        with pytest.raises(ValueError):
            trial_key(**_parts(**bad))


def test_put_writes_atomically(tmp_path, monkeypatch):
    cache = TrialCache(tmp_path)
    key = trial_key(**_parts())
    assert cache.get(key) is None
    calls = []
    real_replace = os.replace

    def spy(src, dst):
        calls.append((str(src), str(dst)))
        assert str(src).endswith(".tmp")
        assert os.path.exists(src)
        return real_replace(src, dst)

    monkeypatch.setattr(os, "replace", spy)
    path = cache.put(key, {"status": "emit", "text": "日本語"})
    assert calls and calls[0][1] == str(path)
    assert cache.get(key) == {"status": "emit", "text": "日本語"}
    assert not list(cache.dir.glob("*.tmp"))


def test_failed_write_leaves_no_files(tmp_path, monkeypatch):
    cache = TrialCache(tmp_path)
    key = trial_key(**_parts())

    def boom(src, dst):
        raise OSError("disk full")

    monkeypatch.setattr(os, "replace", boom)
    with pytest.raises(OSError):
        cache.put(key, {"x": 1})
    assert cache.get(key) is None
    assert not list(cache.dir.glob(".*.tmp"))


def test_path_rejects_non_keys(tmp_path):
    cache = TrialCache(tmp_path)
    for bad in ("../x", "A" * 64, "abc"):
        with pytest.raises(ValueError):
            cache.path(bad)
