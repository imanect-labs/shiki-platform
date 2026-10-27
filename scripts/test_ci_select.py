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
        for f in ["Cargo.toml", "crates/data/Cargo.toml", "migrations/0105_x.sql", ".github/workflows/ci.yml", "deploy/compose/docker-compose.yml",
                  "vendor/secure-exec/src/lib.rs", "rust-toolchain.toml", "scripts/ci-select.py"]:
            with self.subTest(f=f):
                sel = ci.select(self.members, [f])
                self.assertTrue(sel["full"])
                self.assertEqual(sel["nextest_filter"], "all()")
                self.assertEqual(len(sel["packages"]), len(self.members))

    LOCK_BASE = textwrap.dedent(
        """
        [[package]]
        name = "itoa"
        version = "1.0.0"
        source = "registry+https://github.com/rust-lang/crates.io-index"
        checksum = "aaa"

        [[package]]
        name = "serde_json"
        version = "1.0.0"
        source = "registry+https://github.com/rust-lang/crates.io-index"
        checksum = "bbb"
        dependencies = ["itoa"]

        [[package]]
        name = "shiki-storage"
        version = "0.0.0"
        dependencies = ["serde_json", "shiki-authz"]

        [[package]]
        name = "shiki-yui"
        version = "0.0.0"

        [[package]]
        name = "shiki-authz"
        version = "0.0.0"
        """
    )

    def test_cargo_lock_selects_only_users_of_changed_dependencies(self):
        # itoa だけ上がった: itoa → serde_json → shiki-storage と、その依存元（data・api）を選ぶ。
        head = self.LOCK_BASE.replace('version = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "aaa"',
                                      'version = "1.0.1"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "ccc"')
        self.assertNotEqual(head, self.LOCK_BASE)
        sel = ci.select(self.members, ["Cargo.lock"], lock=(self.LOCK_BASE, head))
        self.assertFalse(sel["full"])
        self.assertEqual(sel["packages"], ["shiki-api", "shiki-data", "shiki-storage"])

    def test_cargo_lock_dependency_list_change_is_detected(self):
        # shiki-yui の依存だけが増えた（版は 0.0.0 のまま）: yui とその依存元を選ぶ。
        head = self.LOCK_BASE.replace('name = "shiki-yui"\nversion = "0.0.0"\n',
                                      'name = "shiki-yui"\nversion = "0.0.0"\ndependencies = ["itoa"]\n')
        self.assertNotEqual(head, self.LOCK_BASE)
        sel = ci.select(self.members, ["Cargo.lock"], lock=(self.LOCK_BASE, head))
        self.assertEqual(sel["packages"], ["shiki-yui"])

    def test_cargo_lock_unreadable_or_missing_is_full(self):
        self.assertTrue(ci.select(self.members, ["Cargo.lock"])["full"])
        self.assertTrue(ci.select(self.members, ["Cargo.lock"], lock=(self.LOCK_BASE, "not = [toml"))["full"])

    def test_cargo_lock_without_dependency_change_selects_nothing(self):
        sel = ci.select(self.members, ["Cargo.lock"], lock=(self.LOCK_BASE, self.LOCK_BASE + "\n"))
        self.assertEqual((sel["full"], sel["rust"], sel["packages"]), (False, False, []))

    def test_unknown_path_is_full(self):
        for f in ["scripts/gen-api.sh", "deploy/keycloak/realm.json", "crates/README-unowned.txt"]:
            with self.subTest(f=f):
                self.assertTrue(ci.select(self.members, [f])["full"])

    def test_docs_are_ignored(self):
        sel = ci.select(self.members, ["docs/design.md", "README.md", ".claude/skills/pr/SKILL.md"])
        self.assertFalse(sel["full"] or sel["rust"] or sel["web"] or sel["python"] or sel["compose"])

    def test_markdown_inside_a_crate_is_that_crates_change(self):
        # include_str! でコンパイルに入る .md がある（crates/chat/src/worker/yui_guide.md）。
        sel = ci.select(self.members, ["crates/data/src/guide.md"])
        self.assertIn("shiki-data", sel["packages"])

    def test_unexpected_package_name_is_rejected(self):
        bad = Path(self._tmp.name) / "crates" / "bad"
        bad.mkdir()
        (bad / "Cargo.toml").write_text('[package]\nname = "x\\ny"\n')
        with self.assertRaises(ValueError):
            ci.workspace_packages(Path(self._tmp.name))

    def test_always_run_is_in_the_filter(self):
        f = ci.select(self.members, ["crates/yui/src/lib.rs"])["nextest_filter"]
        self.assertIn("package(=shiki-yui)", f)
        self.assertIn("binary_id(=shiki-data::policy_threat_it)", f)
        self.assertIn("binary_id(=shiki-authz::check)", f)

    def test_always_run_tests_are_one_invocation(self):
        tests = ci.select(self.members, ["crates/yui/src/lib.rs"])["always_run_tests"].split()
        self.assertEqual(tests[0::2], ["--test"] * len(ci.ALWAYS_RUN))
        self.assertEqual(len(set(tests[1::2])), len(ci.ALWAYS_RUN), "テスト名は workspace 内で一意であること")

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

    def test_path_with_space_has_trailing_tab_removed(self):
        diff = "+++ b/crates/a/src/my file.rs\t\n@@ -0,0 +1 @@\n+x\n"
        self.assertEqual(ci.parse_changed_lines(diff), {"crates/a/src/my file.rs": {1}})

    def test_unmatched_sources_detects_path_mismatch(self):
        members = {"shiki-a": {"dir": "crates/a", "deps": set()}, "shiki-b": {"dir": "crates/b", "deps": set()}}
        lines = {"crates/a/src/lib.rs": {1}, "crates/a/tests/it.rs": {1}}
        # lcov のパスが別の root のまま（正規化に失敗）: 変更行が 1 行も数えられず 100% に倒れるのを検出する。
        lcov = ci.parse_lcov("SF:/elsewhere/crates/a/src/lib.rs\nDA:1,0\nend_of_record\n", "/repo")
        self.assertEqual(ci.unmatched_sources(lines, lcov, None, members), ["crates/a/src/lib.rs"])
        ok = ci.parse_lcov("SF:/repo/crates/a/src/lib.rs\nDA:1,0\nend_of_record\n", "/repo")
        self.assertEqual(ci.unmatched_sources(lines, ok, None, members), [])
        # 除外パターンに当たるファイルだけの変更・src 以外だけの変更は対象外。
        self.assertEqual(ci.unmatched_sources({"crates/a/src/main.rs": {1}}, {}, r"src/main\.rs", members), [])
        self.assertEqual(ci.unmatched_sources({"crates/a/tests/it.rs": {1}}, {}, None, members), [])
        # 計測されていないクレート（選択漏れ）のファイルは検出する。
        self.assertEqual(ci.unmatched_sources({"crates/b/src/lib.rs": {1}}, ok, None, members), ["crates/b/src/lib.rs"])

    def test_file_without_executable_lines_is_not_a_mismatch(self):
        # 文字列定数だけのファイルは計測しても lcov に現れない。同じクレートの他のファイルがあれば正常。
        members = {"shiki-a": {"dir": "crates/a", "deps": set()}}
        lcov = ci.parse_lcov("SF:/repo/crates/a/src/lib.rs\nDA:1,1\nend_of_record\n", "/repo")
        self.assertEqual(ci.unmatched_sources({"crates/a/src/prompts.rs": {3, 4}}, lcov, None, members), [])

    def test_md_code_escapes_table_breakers(self):
        self.assertEqual(ci.md_code("a|b`c.rs"), "`a\\|b'c.rs`")

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
