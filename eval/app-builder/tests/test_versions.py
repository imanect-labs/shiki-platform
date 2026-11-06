"""abeval.versions（版の記録と、版のサーバーの dev-up.sh の引数）。"""

from __future__ import annotations

import json
import os

import pytest

from abeval import versions
from abeval.versions import VersionError

SHA = "a" * 64
COMMIT = "0123456789abcdef0123456789abcdef01234567"


def _record(label="v0", **over):
    raw = {
        "label": label,
        "commit": COMMIT,
        "tree_clean": True,
        "binary_sha256": SHA,
        "built_at": "2026-11-06T00:00:00Z",
        "rustc": "rustc 1.99.0",
    }
    raw.update(over)
    return raw


@pytest.mark.parametrize("label", ["v0", "vn", "v0-retry", "a", "a1", "x" * 21])
def test_good_labels(label):
    assert versions.check_label(label) == label


@pytest.mark.parametrize("label", ["", "V0", "0v", "v_0", "v0-", "-v0", "x" * 22, "v0/x", None, 3])
def test_bad_labels(label):
    with pytest.raises(VersionError):
        versions.check_label(label)


def test_names_are_derived_from_the_label():
    assert versions.db_name("v0-retry") == "shiki_eval_v0_retry"
    assert versions.store_name("v0-retry") == "shiki-eval-v0-retry"
    assert versions.bucket_name("v0-retry") == "shiki-eval-v0-retry"
    assert versions.run_label("v0") == "abeval-v0"
    n = versions.redis_db("v0")
    assert n == versions.redis_db("v0")  # 決まった番号
    labels = [f"v{i}" for i in range(50)]
    assert all(4 <= versions.redis_db(x) <= 15 for x in labels)
    assert len({versions.redis_db(x) for x in labels}) > 1


def test_record_roundtrip_and_update(tmp_path):
    rec = versions.parse_record(_record())
    path = versions.write_record(rec, tmp_path)
    assert path == tmp_path / "v0.json"
    assert json.loads(path.read_text()) == _record()
    back = versions.read_record("v0", tmp_path)
    assert back == rec
    new = versions.update_record("v0", {"capabilities": {"node_ports": True}}, tmp_path)
    assert new.extra == {"capabilities": {"node_ports": True}}
    raw = json.loads(path.read_text())
    assert raw["capabilities"] == {"node_ports": True} and raw["commit"] == COMMIT
    with pytest.raises(VersionError, match="書き換えません"):
        versions.update_record("v0", {"binary_sha256": "b" * 64}, tmp_path)


@pytest.mark.parametrize(
    "over",
    [
        {"commit": "81a9899b"},
        {"binary_sha256": "xyz"},
        {"tree_clean": "yes"},
        {"rustc": ""},
        {"label": "V0"},
    ],
)
def test_bad_records_are_refused(over):
    with pytest.raises(VersionError):
        versions.parse_record(_record(**over))


def test_missing_and_mismatched_records(tmp_path):
    with pytest.raises(VersionError, match="build-version.sh"):
        versions.read_record("v0", tmp_path)
    (tmp_path / "v0.json").write_text(json.dumps(_record(label="v1")))
    with pytest.raises(VersionError, match="食い違います"):
        versions.read_record("v0", tmp_path)
    (tmp_path / "v0.json").write_text("{")
    with pytest.raises(VersionError, match="JSON"):
        versions.read_record("v0", tmp_path)


@pytest.fixture
def installed(tmp_path):
    """版の記録・退避したバイナリ・LLM の設定（権限 600）。"""
    home = tmp_path / "home"
    exe = home / "bin" / "shiki-server-v0"
    exe.parent.mkdir(parents=True)
    exe.write_bytes(b"fake server\n")
    exe.chmod(0o755)
    vdir = tmp_path / "versions"
    versions.write_record(
        versions.parse_record(_record(binary_sha256=versions.file_sha256(exe))), vdir
    )
    cfg = tmp_path / "llm-eval.toml"
    cfg.write_text("[llm]\n")
    cfg.chmod(0o600)
    return {"home": home, "exe": exe, "versions_dir": vdir, "llm_config": cfg}


def test_serve_plan(installed):
    base = {"PATH": "/usr/bin", "SHIKI__CHAT__MAX_STEPS": "99", "SHIKI_CONFIG": "/x", "HOME": "/h"}
    plan = versions.serve_plan(
        "v0",
        home=installed["home"],
        llm_config=installed["llm_config"],
        versions_dir=installed["versions_dir"],
        max_steps=12,
        dev_up=versions.DEV_UP,
        base_env=base,
    )
    n = versions.redis_db("v0")
    assert plan.argv == [
        "bash",
        str(versions.DEV_UP),
        "--server-bin",
        str(installed["exe"].resolve()),
        "--db-name",
        "shiki_eval_v0",
        "--store-name",
        "shiki-eval-v0",
        "--bucket",
        "shiki-eval-v0",
        "--redis-db",
        str(n),
        "--port",
        "10067",
        "--gateway-ports",
        "18190,18191",
        "--api-only",
        "--run-label",
        "abeval-v0",
    ]
    # 呼び出し側の SHIKI__* は渡さず、条件だけをここで決める。
    assert plan.env == {
        "PATH": "/usr/bin",
        "HOME": "/h",
        "SHIKI_CONFIG": str(installed["llm_config"]),
        "SHIKI__LLM__BACKEND": "stub",
        "SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS": "150000",
        "SHIKI__CHAT__MAX_STEPS": "12",
    }
    assert plan.dropped == ("SHIKI_CONFIG", "SHIKI__CHAT__MAX_STEPS")
    assert "SHIKI__CHAT__MAX_STEPS=12" in plan.show()


def test_serve_plan_defaults_leave_max_steps_to_the_product(installed):
    plan = versions.serve_plan(
        "v0",
        home=installed["home"],
        llm_config=installed["llm_config"],
        versions_dir=installed["versions_dir"],
        base_env={},
    )
    assert "SHIKI__CHAT__MAX_STEPS" not in plan.env
    assert plan.env["SHIKI__CHAT__BUDGET_MAX_COST_USD_MICROS"] == "150000"


def test_serve_refuses_a_changed_binary(installed):
    installed["exe"].write_bytes(b"rebuilt\n")
    with pytest.raises(VersionError, match="sha が記録と違います"):
        versions.serve_plan(
            "v0",
            home=installed["home"],
            llm_config=installed["llm_config"],
            versions_dir=installed["versions_dir"],
            base_env={},
        )


def test_serve_refuses_a_missing_binary(installed):
    installed["exe"].unlink()
    with pytest.raises(VersionError, match="バイナリがありません"):
        versions.serve_plan(
            "v0",
            home=installed["home"],
            llm_config=installed["llm_config"],
            versions_dir=installed["versions_dir"],
            base_env={},
        )


def test_serve_refuses_a_dirty_build(installed):
    raw = json.loads((installed["versions_dir"] / "v0.json").read_text())
    raw["tree_clean"] = False
    (installed["versions_dir"] / "v0.json").write_text(json.dumps(raw))
    with pytest.raises(VersionError, match="tree_clean"):
        versions.serve_plan(
            "v0",
            home=installed["home"],
            llm_config=installed["llm_config"],
            versions_dir=installed["versions_dir"],
            base_env={},
        )


def test_serve_refuses_a_loose_or_missing_llm_config(installed):
    kw = {"home": installed["home"], "versions_dir": installed["versions_dir"], "base_env": {}}
    installed["llm_config"].chmod(0o644)
    with pytest.raises(VersionError, match="権限"):
        versions.serve_plan("v0", llm_config=installed["llm_config"], **kw)
    with pytest.raises(VersionError, match="ありません"):
        versions.serve_plan("v0", llm_config=installed["llm_config"].with_name("nope.toml"), **kw)


@pytest.mark.parametrize("over", [{"max_steps": 0}, {"budget_micros": 0}, {"redis": 16}])
def test_serve_refuses_bad_conditions(installed, over):
    with pytest.raises(VersionError):
        versions.serve_plan(
            "v0",
            home=installed["home"],
            llm_config=installed["llm_config"],
            versions_dir=installed["versions_dir"],
            base_env={},
            **over,
        )


def test_down_plan():
    plan = versions.down_plan("v0", base_env={"PATH": "/usr/bin", "SHIKI__X": "1"})
    assert plan.argv == ["bash", str(versions.DEV_UP), "--down", "--run-label", "abeval-v0"]
    assert plan.env == {"PATH": "/usr/bin"}


def test_cli_dry_run(installed, monkeypatch, capsys):
    monkeypatch.setenv("ABEVAL_HOME", str(installed["home"]))
    monkeypatch.setattr(versions, "VERSIONS_DIR", installed["versions_dir"])
    monkeypatch.setattr(
        versions,
        "serve_plan",
        lambda label, **kw: versions.ServePlan(["bash", "dev-up.sh", "--api-only"], {}),
    )
    assert versions.main(["--label", "v0", "--dry-run"]) == 0
    assert "dev-up.sh --api-only" in capsys.readouterr().err
    assert versions.main(["--label", "V0", "--dry-run"]) == 2


def test_cli_refuses_without_a_record(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("ABEVAL_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(versions, "VERSIONS_DIR", tmp_path / "versions")
    real = versions.serve_plan
    monkeypatch.setattr(
        versions,
        "serve_plan",
        lambda label, **kw: real(label, versions_dir=tmp_path / "versions", **kw),
    )
    assert versions.main(["--label", "v0", "--dry-run"]) == 2
    assert "版の記録がありません" in capsys.readouterr().err


def test_dev_up_path_exists():
    assert versions.DEV_UP.is_file()
    assert os.access(versions.DEV_UP, os.R_OK)
