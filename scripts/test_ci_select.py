"""scripts/ci-select.py の検査（quality ジョブで `python3 -m unittest discover -s scripts -p 'test_*.py'`）。

選択を誤ると必要なテストが黙って回らなくなるので、全量に倒す条件と依存の辿り方をここで固定する。
"""

import importlib.util
import tempfile
import textwrap
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location("ci_select", Path(__file__).with_name("ci-select.py"))
ci = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ci)

REPO = Path(__file__).resolve().parent.parent


def fake_workspace(tmp: Path) -> Path:
    """authz ← storage ← data ← api の小さな workspace（data は dev-dependency でも authz を使う）。"""
    (tmp / "Cargo.toml").write_text(
        textwrap.dedent(
            """
            [workspace]
            members = ["crates/*"]
            [workspace.dependencies]
            shiki-authz = { path = "crates/authz" }
            shiki-storage = { path = "crates/storage" }
            """
        )
    )
    crates = {
        "authz": ("shiki-authz", ""),
        "storage": ("shiki-storage", "shiki-authz.workspace = true\n"),
        "data": ("shiki-data", "shiki-storage = { workspace = true }\n[dev-dependencies]\nshiki-authz.workspace = true\n"),
        "api": ("shiki-api", 'shiki-data = { path = "../data" }\n'),
        "yui": ("shiki-yui", ""),
    }
    for d, (name, deps) in crates.items():
        p = tmp / "crates" / d
        p.mkdir(parents=True)
        (p / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n[dependencies]\n{deps}')
    return tmp


class SelectTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.members = ci.workspace_packages(fake_workspace(Path(self._tmp.name)))

    def tearDown(self):
        self._tmp.cleanup()

    def test_dependents_are_selected_transitively(self):
        sel = ci.select(self.members, ["crates/storage/src/lib.rs"])
        self.assertFalse(sel["full"])
        self.assertEqual(sel["packages"], ["shiki-api", "shiki-data", "shiki-storage"])
        self.assertTrue(sel["rust"] and sel["web"] and sel["compose"])
        self.assertFalse(sel["python"])

    def test_leaf_crate_selects_only_itself(self):
        sel = ci.select(self.members, ["crates/yui/src/lib.rs"])
        self.assertEqual(sel["packages"], ["shiki-yui"])

    def test_nested_path_belongs_to_enclosing_crate(self):
        # 除外クレート（crates/tabular/runner 等）の中身は、それを含むクレートの変更として扱う。
        sel = ci.select(self.members, ["crates/data/runner/src/main.rs"])
        self.assertIn("shiki-data", sel["packages"])

    def test_web_only_skips_rust(self):
        sel = ci.select(self.members, ["web/src/app/page.tsx"])
        self.assertFalse(sel["full"] or sel["rust"] or sel["python"])
        self.assertTrue(sel["web"] and sel["compose"])
        self.assertEqual(sel["packages"], [])

    def test_python_only(self):
        sel = ci.select(self.members, ["ingestion-worker/app/main.py"])
        self.assertEqual((sel["rust"], sel["web"], sel["python"], sel["compose"]), (False, False, True, False))

    def test_full_triggers(self):
        for f in ["Cargo.lock", "migrations/0105_x.sql", ".github/workflows/ci.yml", "deploy/compose/docker-compose.yml",
                  "vendor/secure-exec/src/lib.rs", "rust-toolchain.toml", "scripts/ci-select.py"]:
            with self.subTest(f=f):
                sel = ci.select(self.members, [f])
                self.assertTrue(sel["full"])
                self.assertEqual(sel["nextest_filter"], "all()")
                self.assertEqual(len(sel["packages"]), len(self.members))

    def test_unknown_path_is_full(self):
        for f in ["scripts/gen-api.sh", "deploy/keycloak/realm.json", "crates/README-unowned.txt"]:
            with self.subTest(f=f):
                self.assertTrue(ci.select(self.members, [f])["full"])

    def test_docs_are_ignored(self):
        sel = ci.select(self.members, ["docs/design.md", "crates/data/README.md", ".claude/skills/pr/SKILL.md"])
        self.assertFalse(sel["full"] or sel["rust"] or sel["web"] or sel["python"] or sel["compose"])

    def test_always_run_is_in_the_filter(self):
        f = ci.select(self.members, ["crates/yui/src/lib.rs"])["nextest_filter"]
        self.assertIn("package(shiki-yui)", f)
        self.assertIn("binary_id(shiki-data::policy_threat_it)", f)
        self.assertIn("binary_id(shiki-authz::check)", f)

    def test_full_run_flag(self):
        sel = ci.select(self.members, [], full_run=True)
        self.assertTrue(sel["full"] and sel["rust"] and sel["python"])


class RealWorkspaceTest(unittest.TestCase):
    """実際の workspace で、常に回す脅威テストが実在すること（改名で黙って外れないように）。"""

    def test_always_run_targets_exist(self):
        members = ci.workspace_packages(REPO)
        for pkg, test in ci.ALWAYS_RUN:
            with self.subTest(pkg=pkg, test=test):
                self.assertIn(pkg, members)
                self.assertTrue((REPO / members[pkg]["dir"] / "tests" / f"{test}.rs").exists())

    def test_authz_change_reaches_most_of_the_workspace(self):
        members = ci.workspace_packages(REPO)
        sel = ci.select(members, ["crates/authz/src/lib.rs"])
        self.assertIn("shiki-api", sel["packages"])
        self.assertIn("shiki-data", sel["packages"])


class DiffCoverageTest(unittest.TestCase):
    DIFF = textwrap.dedent(
        """\
        diff --git a/crates/a/src/lib.rs b/crates/a/src/lib.rs
        --- a/crates/a/src/lib.rs
        +++ b/crates/a/src/lib.rs
        @@ -10,0 +11,3 @@ fn x() {
        +a
        +b
        +c
        @@ -20 +23 @@
        +d
        diff --git a/crates/b/src/main.rs b/crates/b/src/main.rs
        deleted file mode 100644
        --- a/crates/b/src/main.rs
        +++ /dev/null
        @@ -1,2 +0,0 @@
        -x
        """
    )

    def test_parse_changed_lines(self):
        self.assertEqual(ci.parse_changed_lines(self.DIFF), {"crates/a/src/lib.rs": {11, 12, 13, 23}})

    def test_diff_coverage_counts_only_executable_lines(self):
        lcov = ci.parse_lcov(
            "SF:/repo/crates/a/src/lib.rs\nDA:11,1\nDA:12,0\nDA:23,4\nend_of_record\n"
            "SF:/repo/crates/c/src/bin/x.rs\nDA:1,0\nend_of_record\n",
            "/repo",
        )
        lines = {"crates/a/src/lib.rs": {11, 12, 13, 23}, "crates/c/src/bin/x.rs": {1}}
        r = ci.diff_coverage(lines, lcov, r"src/bin/")
        self.assertEqual((r["total"], r["covered"]), (3, 2))
        self.assertAlmostEqual(r["percent"], 200 / 3)
        self.assertEqual(r["files"][0]["missed"], [12])

    def test_no_executable_change_passes(self):
        r = ci.diff_coverage({"crates/a/src/lib.rs": {1}}, {}, None)
        self.assertEqual(r["percent"], 100.0)

    def test_format_ranges(self):
        self.assertEqual(ci.format_ranges([1, 2, 3, 7, 9, 10]), "1-3, 7, 9-10")


if __name__ == "__main__":
    unittest.main()
