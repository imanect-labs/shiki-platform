"""scripts/check-tenant-sql.py の単体テスト（quality ジョブの `python3 -m unittest discover -s scripts` で回る）。"""
import importlib.util
import textwrap
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location("check_tenant_sql", Path(__file__).with_name("check-tenant-sql.py"))
lint = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(lint)

TABLES = {"node", "job_queue"}


class TenantTablesTest(unittest.TestCase):
    def test_tables_with_tenant_columns_are_collected(self):
        migrations = [
            "CREATE TABLE node (id uuid, tenant_id text NOT NULL, name text);",
            "create table if not exists data_record (tenant_int int);",
            "CREATE TABLE plain (id int);",
            "CREATE TABLE p (tenant_id text) PARTITION BY HASH (tenant_id);",
            "ALTER TABLE plain2 ADD COLUMN tenant_id text;",
        ]
        # data_* は crates/data の S3 が見るので対象外。
        self.assertEqual(lint.tenant_tables(migrations), {"node", "p", "plain2"})


class LiteralsTest(unittest.TestCase):
    def test_literals_skip_comments_chars_and_lifetimes(self):
        src = textwrap.dedent(
            '''\
            // "in a comment"
            fn f<'a>(c: char) -> &'a str {
                let _ = '"';
                /* "block" */
                let s = "SELECT 1";
                let r = r#"multi
            line"#;
                "tail"
            }
            '''
        )
        self.assertEqual(lint.string_literals(src), [(5, "SELECT 1"), (6, "multi\nline"), (8, "tail")])


class ViolationsTest(unittest.TestCase):
    def check(self, src):
        return lint.violations("x.rs", textwrap.dedent(src), TABLES)

    def test_missing_tenant_condition_is_reported(self):
        self.assertEqual(self.check('let q = "SELECT * FROM node WHERE id = $1";\n'), ["x.rs:1: node"])

    def test_tenant_condition_passes(self):
        self.assertEqual(self.check('let q = "SELECT * FROM node WHERE tenant_id = $1 AND id = $2";\n'), [])

    def test_unrelated_tables_and_non_sql_pass(self):
        self.assertEqual(self.check('let q = "SELECT * FROM other";\nlet m = "a node from somewhere";\n'), [])

    def test_allow_annotation_needs_a_reason(self):
        ok = '// tenant-sql: allow(全テナント横断の GC)\nlet q = "DELETE FROM job_queue WHERE done";\n'
        self.assertEqual(self.check(ok), [])
        empty = '// tenant-sql: allow()\nlet q = "DELETE FROM job_queue WHERE done";\n'
        self.assertEqual(self.check(empty), ["x.rs:2: job_queue"])

    def test_allow_annotation_must_be_near(self):
        far = '// tenant-sql: allow(遠い)\n\n\n\n\nlet q = "UPDATE node SET name = $1";\n'
        self.assertEqual(self.check(far), ["x.rs:6: node"])

    def test_test_modules_are_skipped(self):
        src = 'fn a() {}\n#[cfg(test)]\nmod tests {\n    const Q: &str = "SELECT * FROM node";\n}\n'
        self.assertEqual(self.check(src), [])

    def test_test_paths(self):
        for p in ["crates/a/tests/x.rs", "crates/a/src/tests.rs", "crates/a/src/b_tests.rs", "crates/a/src/x/tests_y.rs"]:
            self.assertTrue(lint.is_test_path(p), p)
        self.assertFalse(lint.is_test_path("crates/a/src/lib.rs"))


if __name__ == "__main__":
    unittest.main()
