"""scripts/ci_scenarios.py（E2E の担当シナリオ対応表）の検査（quality ジョブで unittest discover）。

対応表の更新を忘れても「テストが黙って漏れる」経路が無いこと（全部に倒れるか、check が落とすか）をここで固定する。
"""

import importlib.util
import subprocess
import sys
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ci_scenarios as cs  # noqa: E402

_spec = importlib.util.spec_from_file_location("ci_select", Path(__file__).with_name("ci-select.py"))
ci = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ci)

REPO = Path(__file__).resolve().parent.parent

TABLE = textwrap.dedent(
    """
    [always]
    e2e = ["auth.spec.ts"]

    [all]
    pages = ["web/src/app/login/page.tsx"]
    paths = ["web/e2e/helpers.ts"]
    crates = ["shiki-api", "shiki-authz"]

    [none]
    crates = ["shiki-bench"]

    [scenario.data]
    pages = ["web/src/app/(auth)/data/**"]
    paths = ["crates/api/src/routes/data*.rs"]
    crates = ["shiki-data"]
    e2e = ["data-grid.spec.ts"]

    [scenario.notes]
    pages = ["web/src/app/(auth)/notes/**"]
    paths = []
    crates = ["shiki-collab"]
    e2e = ["notes.spec.ts"]
    """
)

SOURCES = {
    "web/src/app/layout.tsx": 'import "./globals.css";\n',
    "web/src/app/globals.css": "",
    "web/src/app/login/page.tsx": 'import { Button } from "@/components/ui/button";\n',
    "web/src/app/(auth)/data/page.tsx": 'import { Grid } from "@/components/data/grid";\n',
    "web/src/app/(auth)/notes/[id]/page.tsx": (
        'import {\n  Editor,\n} from "../../../../components/notes/editor";\n'
        'const Lazy = dynamic(() => import("@/components/notes/lazy"));\n'
    ),
    "web/src/app/api/[...path]/route.ts": 'import { proxy } from "@/lib/bff";\n',
    "web/src/components/ui/button.tsx": 'import React from "react";\n',
    "web/src/components/data/grid.tsx": 'import { Button } from "../ui/button";\n',
    "web/src/components/notes/editor.tsx": 'export * from "@/lib/notes-api";\n',
    "web/src/components/notes/lazy.tsx": "",
    "web/src/lib/notes-api.ts": "",
    "web/src/lib/bff.ts": "",
    "web/src/lib/orphan.ts": "",
    "web/e2e/auth.spec.ts": "",
    "web/e2e/data-grid.spec.ts": "",
    "web/e2e/notes.spec.ts": "",
    "web/e2e/perf.manual.spec.ts": "",
    "web/e2e/helpers.ts": "",
    "crates/api/src/routes/data.rs": "",
    "crates/api/src/lib.rs": "",
}

# api → data → authz、api → collab、bench は単独、orphan クレートは対応表に無い。
MEMBERS = {
    "shiki-api": {"dir": "crates/api", "deps": {"shiki-data", "shiki-collab"}},
    "shiki-data": {"dir": "crates/data", "deps": {"shiki-authz"}},
    "shiki-authz": {"dir": "crates/authz", "deps": set()},
    "shiki-collab": {"dir": "crates/collab", "deps": set()},
    "shiki-bench": {"dir": "crates/bench", "deps": set()},
}


def scenarios(table: str = TABLE, sources: dict | None = None) -> cs.Scenarios:
    src = SOURCES if sources is None else sources
    return cs.Scenarios(cs.load_table(table), sorted(src), lambda p: src[p])


def run(sc: cs.Scenarios, files: list[str], lock: set[str] = frozenset(), members: dict = MEMBERS) -> dict:
    return sc.select(files, set(lock), lambda p: ci.owner(members, p), lambda c: ci.reverse_closure(members, {c}))


class SelectTest(unittest.TestCase):
    def test_component_reaches_only_its_pages(self):
        r = run(scenarios(), ["web/src/components/data/grid.tsx"])
        self.assertFalse(r["all"])
        self.assertEqual(r["specs"], ["auth.spec.ts", "data-grid.spec.ts"])

    def test_imports_are_followed_through_relative_multiline_reexport_and_dynamic(self):
        self.assertEqual(run(scenarios(), ["web/src/lib/notes-api.ts"])["specs"], ["auth.spec.ts", "notes.spec.ts"])
        self.assertEqual(run(scenarios(), ["web/src/components/notes/lazy.tsx"])["specs"],
                         ["auth.spec.ts", "notes.spec.ts"])

    def test_shared_component_reaching_the_login_page_runs_everything(self):
        r = run(scenarios(), ["web/src/components/ui/button.tsx"])
        self.assertTrue(r["all"])
        self.assertEqual(r["unmapped"], [])

    def test_layout_reaches_every_page_below_it(self):
        r = run(scenarios(), ["web/src/app/globals.css"])
        self.assertTrue(r["all"])  # ルートの layout → login も含む全ページ

    def test_file_no_page_imports_runs_everything_and_is_reported(self):
        r = run(scenarios(), ["web/src/lib/orphan.ts"])
        self.assertTrue(r["all"])
        self.assertEqual(r["unmapped"], ["web/src/lib/orphan.ts"])

    def test_api_route_handler_runs_everything(self):
        r = run(scenarios(), ["web/src/lib/bff.ts"])
        self.assertTrue(r["all"])
        self.assertEqual(r["unmapped"], [])

    def test_page_missing_from_the_table_runs_everything_and_is_reported(self):
        src = dict(SOURCES)
        src["web/src/app/(auth)/new/page.tsx"] = 'import { Grid } from "@/components/data/grid";\n'
        r = run(scenarios(sources=src), ["web/src/components/data/grid.tsx"])
        self.assertTrue(r["all"])
        self.assertEqual(r["unmapped"], ["web/src/app/(auth)/new/page.tsx"])

    def test_spec_change_runs_that_spec(self):
        self.assertEqual(run(scenarios(), ["web/e2e/notes.spec.ts"])["specs"], ["auth.spec.ts", "notes.spec.ts"])
        self.assertEqual(run(scenarios(), ["web/e2e/perf.manual.spec.ts"])["specs"], ["auth.spec.ts"])

    def test_e2e_helpers_and_web_config_run_everything(self):
        self.assertTrue(run(scenarios(), ["web/e2e/helpers.ts"])["all"])
        self.assertTrue(run(scenarios(), ["web/package.json"])["all"])

    def test_table_change_runs_everything(self):
        self.assertTrue(run(scenarios(), [cs.TABLE])["all"])

    def test_crate_selects_scenarios_through_its_dependents(self):
        self.assertEqual(run(scenarios(), ["crates/data/src/lib.rs"])["specs"], ["auth.spec.ts", "data-grid.spec.ts"])
        self.assertEqual(run(scenarios(), [], lock={"shiki-collab"})["specs"], ["auth.spec.ts", "notes.spec.ts"])

    def test_all_and_none_crates(self):
        self.assertTrue(run(scenarios(), ["crates/authz/src/lib.rs"])["all"])
        r = run(scenarios(), ["crates/bench/src/lib.rs"])
        self.assertEqual((r["all"], r["specs"]), (False, ["auth.spec.ts"]))

    def test_crate_missing_from_the_table_runs_everything_and_is_reported(self):
        members = dict(MEMBERS, **{"shiki-new": {"dir": "crates/new", "deps": set()}})
        r = run(scenarios(), ["crates/new/src/lib.rs"], members=members)
        self.assertTrue(r["all"])
        self.assertEqual(r["unmapped"], ["shiki-new"])

    def test_paths_override_the_crate_rule(self):
        r = run(scenarios(), ["crates/api/src/routes/data.rs"])
        self.assertEqual((r["all"], r["specs"]), (False, ["auth.spec.ts", "data-grid.spec.ts"]))
        self.assertTrue(run(scenarios(), ["crates/api/src/lib.rs"])["all"])  # api は [all]

    def test_paths_outside_web_and_crates_do_not_touch_e2e(self):
        self.assertEqual(run(scenarios(), ["ingestion-worker/app.py"])["specs"], ["auth.spec.ts"])

    def test_glob(self):
        self.assertTrue(cs.glob_regex("web/src/app/(auth)/data/**").match("web/src/app/(auth)/data/[id]/page.tsx"))
        self.assertFalse(cs.glob_regex("crates/api/src/routes/*.rs").match("crates/api/src/routes/workflows/x.rs"))
        self.assertTrue(cs.glob_regex("crates/api/src/routes/data*.rs").match("crates/api/src/routes/data_fsm.rs"))


class CheckTest(unittest.TestCase):
    def closure(self, members):
        return lambda c: ci.reverse_closure(members, {c})

    def errors(self, table: str = TABLE, sources: dict | None = None, members: dict = MEMBERS) -> list[str]:
        return scenarios(table, sources).check(members, self.closure(members))

    def test_consistent_table_passes(self):
        self.assertEqual(self.errors(), [])

    def test_new_spec_not_in_the_table_fails(self):
        src = dict(SOURCES, **{"web/e2e/new-feature.spec.ts": ""})
        [e] = self.errors(sources=src)
        self.assertIn("web/e2e/new-feature.spec.ts", e)

    def test_new_page_not_in_the_table_fails(self):
        src = dict(SOURCES, **{"web/src/app/(auth)/new/page.tsx": ""})
        [e] = self.errors(sources=src)
        self.assertIn("web/src/app/(auth)/new/page.tsx", e)

    def test_new_crate_that_reaches_no_scenario_fails(self):
        members = dict(MEMBERS, **{"shiki-new": {"dir": "crates/new", "deps": set()}})
        [e] = self.errors(members=members)
        self.assertIn("shiki-new", e)

    def test_new_crate_used_by_a_scenario_crate_needs_no_entry(self):
        members = dict(MEMBERS, **{"shiki-new": {"dir": "crates/new", "deps": set()}})
        members["shiki-data"] = {"dir": "crates/data", "deps": {"shiki-authz", "shiki-new"}}
        self.assertEqual(self.errors(members=members), [])

    def test_renamed_or_deleted_things_fail(self):
        src = {k: v for k, v in SOURCES.items() if k not in ("web/e2e/notes.spec.ts", "crates/api/src/routes/data.rs")}
        src["web/src/app/(auth)/notes/[id]/page.tsx"] = ""
        errors = "\n".join(self.errors(sources=src))
        self.assertIn("notes.spec.ts", errors)
        self.assertIn("crates/api/src/routes/data*.rs", errors)
        members = {k: v for k, v in MEMBERS.items() if k != "shiki-bench"}
        self.assertTrue(any("shiki-bench" in e for e in self.errors(members=members)))

    def test_manual_spec_and_overlapping_crates_fail(self):
        table = TABLE.replace('e2e = ["notes.spec.ts"]', 'e2e = ["notes.spec.ts", "perf.manual.spec.ts"]')
        self.assertTrue(any("手動" in e for e in self.errors(table)))
        table = TABLE.replace('crates = ["shiki-collab"]', 'crates = ["shiki-collab", "shiki-authz"]')
        self.assertTrue(any("shiki-authz" in e for e in self.errors(table)))

    def test_unknown_keys_are_rejected(self):
        with self.assertRaises(ValueError):
            cs.load_table(TABLE + '\n[scenario.x]\npage = ["typo"]\ne2e = ["auth.spec.ts"]\n')


class RepoTest(unittest.TestCase):
    """このリポジトリの対応表そのもの（quality ジョブの check と同じ）。"""

    def test_repository_table_is_consistent(self):
        tracked = subprocess.run(["git", "ls-files"], cwd=REPO, capture_output=True, text=True, check=True).stdout
        sc = cs.Scenarios.from_repo(REPO, [f for f in tracked.splitlines() if f])
        members = ci.workspace_packages(REPO)
        self.assertEqual(sc.check(members, lambda c: ci.reverse_closure(members, {c})), [])


if __name__ == "__main__":
    unittest.main()
