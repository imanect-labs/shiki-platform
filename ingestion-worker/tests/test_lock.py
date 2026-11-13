"""uv.lock が CPU 版の torch を固定していることの検査。

torch は docling・sentence-transformers の間接依存で、uv の [tool.uv.sources] は直接依存にしか
効かない。pyproject で直接依存から外すと、pytorch-cpu の指定が黙って無視され、Linux では PyPI の
CUDA 版（nvidia-* 一式・triton）が入る。イメージが数 GB 膨らみ、プレビューのノードのディスクを
圧迫して pod が退避された（2026-11-13）。ロックファイルの段階で気づけるようにする。
"""

import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CPU_INDEX = "https://download.pytorch.org/whl/cpu"


def _packages() -> list[dict]:
    with (ROOT / "uv.lock").open("rb") as f:
        return tomllib.load(f)["package"]


def test_torch_comes_only_from_the_cpu_index() -> None:
    for name in ("torch", "torchvision"):
        entries = [p for p in _packages() if p["name"] == name]
        assert entries, f"{name} がロックに無い"
        for p in entries:
            assert p["source"].get("registry") == CPU_INDEX, (name, p["version"], p["source"])


def test_no_gpu_runtime_is_locked() -> None:
    gpu = sorted(
        p["name"]
        for p in _packages()
        if p["name"].startswith(("nvidia-", "cuda-")) or p["name"] == "triton"
    )
    assert gpu == []
