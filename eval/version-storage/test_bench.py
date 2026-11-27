"""Validate the measuring instrument, not application behavior."""

import os
import random
import shutil
import tempfile
import unittest

from bench import METHODS, FileObjects, Store, pack, unpack
from corpus import archive, office_entries, serialized_history


class BenchmarkTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.xdelta = os.environ.get("XDELTA3", shutil.which("xdelta3"))

    def methods(self):
        return [m for m in METHODS if not m.startswith("xdelta") or self.xdelta]

    def store(self, method):
        return Store(method, self.xdelta, self.tmp.name)

    def test_roundtrip_binary_empty_duplicates_and_undo(self):
        rng = random.Random(97)
        base = rng.randbytes(150_000)
        versions = [b"", b"a", base, base[:70_000] + b"insert" + base[70_000:], base, b"", base]
        for method in self.methods():
            with self.subTest(method=method):
                store = self.store(method)
                for i, v in enumerate(versions):
                    store.add(v, i)
                count = len(store.objects)
                for key, v in zip(store.references, versions):
                    self.assertEqual(store.get(key), v)
                self.assertEqual(count, len(store.objects))
                self.assertEqual(store.references[2], store.references[4])

    def test_chains_bounded_even_across_duplicate_checkpoints(self):
        for method in self.methods():
            if method.startswith(("full", "cdc", "fixed")):
                continue
            with self.subTest(method=method):
                store = self.store(method)
                versions = [b"abcxyz" * 4000 + b"original"]
                for i in range(1, 70):
                    versions.append(versions[3] if i % 8 == 0 else b"abcxyz" * 4000 + str(i).encode())
                for i, v in enumerate(versions):
                    store.add(v, i)
                cap = 1 if method.startswith("zanchor") else (7 if method.endswith("8") else 31)
                self.assertLessEqual(max(store.depths.values()), cap)
                for i in (0, 3, 8, 32, 64, 69):
                    self.assertEqual(store.get(store.references[i]), versions[i])

    def test_adaptive_compression_and_checksum(self):
        rng = random.Random(97)
        for v in (b"", rng.randbytes(65_536), b"x" * 65_536):
            p = pack(v)
            self.assertLessEqual(len(p), len(v) + 1)
            self.assertEqual(unpack(p), v)
        p = pack(b"x" * 65_536)
        with self.assertRaises(Exception):
            unpack(p[:-1] + bytes([p[-1] ^ 1]))

    def test_cdc_reuses_chunks_after_shift(self):
        base = random.Random(97).randbytes(1_048_576)
        counts = {}
        for method in ("fixed64-zstd", "cdc16-zstd"):
            store = self.store(method)
            store.add(base, 0)
            before = sum(map(len, store.objects.values()))
            store.add(b"insert" + base, 1)
            counts[method] = sum(map(len, store.objects.values())) - before
            self.assertEqual(store.get(store.references[1]), b"insert" + base)
        self.assertLess(counts["cdc16-zstd"], counts["fixed64-zstd"] // 2)

    def test_ooxml_library_outputs_can_be_reopened(self):
        import io
        from docx import Document
        from openpyxl import load_workbook
        from pptx import Presentation
        for kind, reader in (("docx", Document), ("xlsx", load_workbook), ("pptx", Presentation)):
            entries = office_entries(kind, 4096, 73)
            for edit in ("local", "add_delete", "global"):
                for v in serialized_history(kind, entries, edit, 73, 4):
                    self.assertIsNotNone(reader(io.BytesIO(v)))

    def test_archive_repack_changes_bytes_but_not_contents(self):
        import io
        import zipfile
        entries = {"a": b"abc" * 100, "b": b"xyz" * 100}
        a, b = archive(entries), archive(entries, 1, True)
        self.assertNotEqual(a, b)
        with zipfile.ZipFile(io.BytesIO(b)) as z:
            self.assertEqual({n: z.read(n) for n in z.namelist()}, entries)

    def test_generated_core_properties_ignore_wall_clock(self):
        a = archive({"docProps/core.xml": b"<modified>2026-11-12T00:00:01Z</modified>"})
        b = archive({"docProps/core.xml": b"<modified>2026-11-12T00:00:59Z</modified>"})
        self.assertEqual(a, b)

    def test_local_file_backend_uses_same_representations(self):
        from pathlib import Path
        versions = [b"x" * 80_000, b"x" * 40_000 + b"changed" + b"x" * 40_000]
        for method in ("full-cas", "cdc16-zstd", "zanchor8", "zdict8"):
            with self.subTest(method=method):
                memory, disk = self.store(method), self.store(method)
                disk.objects = FileObjects(Path(self.tmp.name) / "objects")
                for i, v in enumerate(versions):
                    memory.add(v, i)
                    disk.add(v, i)
                self.assertEqual(sum(map(len, memory.objects.values())), sum(map(len, disk.objects.values())))
                for key, v in zip(disk.references, versions):
                    self.assertEqual(disk.get(key), v)


if __name__ == "__main__":
    unittest.main()
