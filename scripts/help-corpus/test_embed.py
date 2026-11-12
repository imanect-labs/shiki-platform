"""配布コーパスの改ざん拒否と worker 互換の document 入力を検証する。"""

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "help_embed", Path(__file__).with_name("embed.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CorpusBuildTest(unittest.TestCase):
    def fixture(self, root):
        source = root / "source"
        source.mkdir()
        chunk = {
            "chunk_id": "chunk",
            "text": "手順",
            "status": "reviewed",
            "corpus_version": "v1",
        }
        chunk["content_sha256"] = hashlib.sha256(chunk["text"].encode()).hexdigest()
        data = (json.dumps(chunk) + "\n").encode()
        manifest = {
            "schema_version": 1,
            "corpus_id": "shiki-help",
            "visibility": "public",
            "asset_count": 0,
            "chunk_count": 1,
            "corpus_version": "v1",
            "chunks_bytes": len(data),
            "chunks_sha256": hashlib.sha256(data).hexdigest(),
        }
        (source / "manifest.json").write_text(json.dumps(manifest))
        (source / "chunks.jsonl").write_bytes(data)
        return source

    def test_document_inference_and_generation_are_bound_to_original_bytes(self):
        seen = []
        model = SimpleNamespace(encode=lambda texts: seen.extend(texts) or [[1.0, 0.0]])
        modules = {
            "ingestion_worker.model_registry": SimpleNamespace(
                get_registry=lambda: SimpleNamespace(embedding=lambda: model)
            ),
            "ingestion_worker.settings": SimpleNamespace(
                get_settings=lambda: SimpleNamespace(
                    embed_model="cl-nagoya/ruri-v3-30m"
                )
            ),
        }
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.dict(sys.modules, modules),
        ):
            root = Path(directory)
            source = self.fixture(root)
            output = root / "output"
            module.prepare(source, output)
            self.assertEqual(seen, ["検索文書: 手順"])
            self.assertEqual(
                (source / "manifest.json").read_bytes(),
                (output / "manifest.json").read_bytes(),
            )
            manifest = json.loads((output / "vectors.manifest.json").read_text())
            self.assertEqual(manifest["model_version"], "cl-nagoya/ruri-v3-30m")
            self.assertEqual(
                manifest["vectors_sha256"],
                hashlib.sha256((output / "vectors.jsonl").read_bytes()).hexdigest(),
            )
            (source / "chunks.jsonl").write_text("tampered")
            with self.assertRaises(ValueError):
                module.prepare(source, root / "bad")
            self.assertFalse((root / "bad").exists())


if __name__ == "__main__":
    unittest.main()
