"""E2E の担当シナリオ対応表（ci/scenarios.toml）で、変更から回す Playwright spec を選び、対応表を検査する（#922）。

ci-select.py から使う（標準ライブラリのみ）。対応表は手書きなので、更新を忘れても「テストが黙って漏れる」経路を
作らないことを原則にする:

- 対応表で決まらない変更は E2E を全部回す（遅くなるだけで漏れない）。どの変更で全部に倒れたかは
  unmapped として Job Summary と警告に出す。
- web/src の部品・lib がどのページに届くかは import を辿って自動で決める（手書きしない）。
  クレートは依存グラフ（ci-select.py の reverse_closure）で決める。
- spec・ページ・クレートを足して対応表に載せ忘れたら、check() が quality ジョブを落とす。
"""

from __future__ import annotations

import posixpath
import re
import tomllib
from pathlib import Path
from typing import Callable

TABLE = "ci/scenarios.toml"
WEB_SRC = "web/src/"
APP_DIR = "web/src/app/"
E2E_DIR = "web/e2e/"

# Next.js App Router が読み込む特別なファイル。page はそのページだけ、それ以外は配下の全ページに効く。
ENTRY_STEMS = {"page", "layout", "template", "loading", "error", "not-found", "global-error", "default"}
# API ルート（BFF のプロキシ等）。全ページが通るので、ここに届く変更は E2E を全部回す。
ROUTE_STEM = "route"
CODE_EXT = (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs")
RESOLVE_SUFFIXES = ("", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json", ".css",
                    "/index.ts", "/index.tsx", "/index.js", "/index.jsx", "/index.mjs")

# spec 名は Playwright の引数（正規表現）と GITHUB_OUTPUT に流すので、想定外の文字は通さない。
SPEC_NAME = re.compile(r"^[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*\.spec\.ts$")
MANUAL_SUFFIX = ".manual.spec.ts"

# from "x" / import "x" / import("x") / require("x")。文字列やコメント中の誤検出は辺が増えるだけ（選び過ぎる側）。
IMPORT = re.compile(r"""(?:\bfrom\s*|\bimport\s*\(?\s*|\brequire\s*\(\s*)["']([^"'\n]+)["']""")

SCENARIO_KEYS = {"pages", "paths", "crates", "e2e"}


def glob_regex(pattern: str) -> re.Pattern[str]:
    """`*` は `/` を跨がず、`**` は跨ぐ glob。"""
    out = []
    i = 0
    while i < len(pattern):
        if pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


def matches(path: str, patterns: list[str]) -> bool:
    return any(glob_regex(p).match(path) for p in patterns)


def _str_list(value, where: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise ValueError(f"{TABLE}: {where} は文字列の配列にする")
    return value


def load_table(text: str) -> dict:
    """対応表を読み、形を検める。{always, all, none, scenarios: {名前: {pages, paths, crates, e2e}}}。"""
    data = tomllib.loads(text)
    unknown = set(data) - {"always", "all", "none", "scenario"}
    if unknown:
        raise ValueError(f"{TABLE}: 知らないキー {sorted(unknown)}")
    always = data.get("always", {})
    every = data.get("all", {})
    none = data.get("none", {})
    table = {
        "always": _str_list(always.get("e2e", []), "[always].e2e"),
        "all": {k: _str_list(every.get(k, []), f"[all].{k}") for k in ("pages", "paths", "crates")},
        "none": _str_list(none.get("crates", []), "[none].crates"),
        "scenarios": {},
    }
    for name, body in data.get("scenario", {}).items():
        extra = set(body) - SCENARIO_KEYS
        if extra:
            raise ValueError(f"{TABLE}: [scenario.{name}] に知らないキー {sorted(extra)}")
        table["scenarios"][name] = {k: _str_list(body.get(k, []), f"[scenario.{name}].{k}") for k in SCENARIO_KEYS}
    return table


def resolve_import(src: str, spec: str, files: set[str]) -> str | None:
    if spec.startswith("@/"):
        base = WEB_SRC + spec[2:]
    elif spec.startswith("."):
        base = posixpath.normpath(posixpath.join(posixpath.dirname(src), spec))
    else:
        return None  # パッケージ
    for suffix in RESOLVE_SUFFIXES:
        if base + suffix in files:
            return base + suffix
    return None


def import_graph(files: set[str], read: Callable[[str], str]) -> dict[str, set[str]]:
    """web/src のファイル → それが import する web/src のファイル。"""
    graph: dict[str, set[str]] = {}
    for f in files:
        if not f.startswith(WEB_SRC) or not f.endswith(CODE_EXT):
            continue
        deps = set()
        for spec in IMPORT.findall(read(f)):
            target = resolve_import(f, spec, files)
            if target and target != f:
                deps.add(target)
        graph[f] = deps
    return graph


def importers(graph: dict[str, set[str]], target: str) -> set[str]:
    """target と、それを（推移的に）import するファイル。"""
    reverse: dict[str, set[str]] = {}
    for src, deps in graph.items():
        for d in deps:
            reverse.setdefault(d, set()).add(src)
    seen = {target}
    stack = [target]
    while stack:
        for src in reverse.get(stack.pop(), ()):
            if src not in seen:
                seen.add(src)
                stack.append(src)
    return seen


def _stem(path: str) -> str:
    return posixpath.basename(path).split(".")[0]


def is_page(path: str) -> bool:
    return path.startswith(APP_DIR) and path.endswith(CODE_EXT) and _stem(path) == "page"


def is_entry(path: str) -> bool:
    return path.startswith(APP_DIR) and path.endswith(CODE_EXT) and _stem(path) in ENTRY_STEMS


def is_route_handler(path: str) -> bool:
    return path.startswith(APP_DIR) and path.endswith(CODE_EXT) and _stem(path) == ROUTE_STEM


def pages_of_entry(entry: str, pages: list[str]) -> set[str]:
    if _stem(entry) == "page":
        return {entry}
    prefix = posixpath.dirname(entry) + "/"
    return {p for p in pages if p.startswith(prefix)}


def spec_files(tracked: list[str]) -> list[str]:
    """web/e2e 直下の spec 名（手動計測の *.manual.spec.ts を含む）。"""
    return sorted(f[len(E2E_DIR):] for f in tracked
                  if f.startswith(E2E_DIR) and "/" not in f[len(E2E_DIR):] and f.endswith(".spec.ts"))


class Scenarios:
    """対応表 ＋ 追跡ファイル ＋ import グラフ。select() と check() の両方が使う。"""

    def __init__(self, table: dict, tracked: list[str], read: Callable[[str], str]):
        self.table = table
        self.tracked = tracked
        self.files = set(tracked)
        self.read = read
        self.pages = sorted(f for f in tracked if is_page(f))
        self._graph: dict[str, set[str]] | None = None

    @classmethod
    def from_repo(cls, root: Path, tracked: list[str]) -> "Scenarios":
        table = load_table((root / TABLE).read_text(encoding="utf-8"))
        return cls(table, tracked, lambda p: (root / p).read_text(encoding="utf-8", errors="replace"))

    @property
    def graph(self) -> dict[str, set[str]]:
        if self._graph is None:
            self._graph = import_graph(self.files, self.read)
        return self._graph

    def scenarios_of_page(self, page: str) -> set[str]:
        return {n for n, s in self.table["scenarios"].items() if matches(page, s["pages"])}

    def crate_state(self, crate: str, closure: set[str]) -> tuple[str, set[str]]:
        """("none" | "all" | "scenarios" | "unmapped", シナリオ)。closure は crate とそれに依存するクレート。"""
        if crate in self.table["none"]:
            return "none", set()
        if crate in self.table["all"]["crates"]:
            return "all", set()
        hit = {n for n, s in self.table["scenarios"].items() if set(s["crates"]) & closure}
        return ("scenarios", hit) if hit else ("unmapped", set())

    def select(self, files: list[str], lock_crates: set[str], owner: Callable[[str], str | None],
               closure: Callable[[str], set[str]]) -> dict:
        """files は選択に使う変更ファイル（無視・全量の判定は呼び出し側）。lock_crates は Cargo.lock の差分で
        変わった依存を使うクレート。owner はパス → クレート名、closure はクレート → それ自身と依存元。

        戻り値: {"all", "specs"（all のときは空）, "reasons", "unmapped"（全部に倒した対応表の穴）}。
        """
        chosen: set[str] = set()
        reasons: list[str] = []
        unmapped: list[str] = []
        by_crate: dict[str, str] = {}
        every = False

        def go_all(reason: str, hole: str | None = None) -> None:
            nonlocal every
            every = True
            reasons.append(reason)
            if hole:
                unmapped.append(hole)

        def add(names: set[str], reason: str) -> None:
            for n in sorted(names):
                chosen.update(self.table["scenarios"][n]["e2e"])
            reasons.append(f"{reason} → {', '.join(sorted(names))}")

        for f in files:
            by_path = {n for n, s in self.table["scenarios"].items() if matches(f, s["paths"])}
            if f == TABLE:
                go_all(f"E2E 全部（対応表の変更）: {f}")
            elif by_path:
                add(by_path, f"対応表の paths: {f}")
            elif matches(f, self.table["all"]["paths"]):
                go_all(f"E2E 全部（[all].paths）: {f}")
            elif f.startswith(E2E_DIR):
                name = f[len(E2E_DIR):]
                if "/" not in name and SPEC_NAME.match(name):
                    if not name.endswith(MANUAL_SUFFIX):
                        chosen.add(name)
                        reasons.append(f"spec の変更: {name}")
                else:
                    go_all(f"E2E 全部（spec 以外の E2E の変更）: {f}")
            elif f.startswith(WEB_SRC):
                self._select_web(f, add, go_all)
            elif f.startswith("web/"):
                go_all(f"E2E 全部（web の設定・依存）: {f}")
            elif f.startswith("crates/"):
                crate = owner(f)
                if crate is None:
                    go_all(f"E2E 全部（クレートを判定できない）: {f}", f)
                else:
                    by_crate.setdefault(crate, f)
        for crate in sorted(lock_crates):
            by_crate.setdefault(crate, f"Cargo.lock（{crate}）")
        # 同じクレートの変更は 1 回だけ判定する（理由の行が並ばないように）。
        for crate, label in sorted(by_crate.items()):
            self._select_crate(crate, closure(crate), add, go_all, label)
        specs = [] if every else sorted(chosen | set(self.table["always"]))
        return {"all": every, "specs": specs, "reasons": reasons, "unmapped": sorted(set(unmapped))}

    def _select_web(self, f: str, add, go_all) -> None:
        if f not in self.files and not is_entry(f):
            # 削除されたファイル。import していた側も同じ PR で変わっている（変えなければビルドが落ちる）ので、
            # そちらで判定する。削除されたページは下の is_entry の経路でそのページのシナリオになる。
            return
        reached = importers(self.graph, f)
        if any(is_route_handler(r) for r in reached):
            go_all(f"E2E 全部（API ルートに届く）: {f}")
            return
        shared = sorted(r for r in reached if matches(r, self.table["all"]["paths"]))
        if shared:
            go_all(f"E2E 全部（[all].paths の {shared[0]} に届く）: {f}")
            return
        pages: set[str] = set()
        for r in reached:
            if is_entry(r):
                pages |= pages_of_entry(r, self.pages)
        if not pages:
            go_all(f"E2E 全部（どのページからも import されない）: {f}", f)
            return
        names: set[str] = set()
        for page in sorted(pages):
            if matches(page, self.table["all"]["pages"]):
                go_all(f"E2E 全部（[all].pages: {page}）: {f}")
                return
            hit = self.scenarios_of_page(page)
            if not hit:
                go_all(f"E2E 全部（ページが対応表に無い: {page}）: {f}", page)
                return
            names |= hit
        add(names, f"{f}（ページ {len(pages)} 件）")

    def _select_crate(self, crate: str, closure: set[str], add, go_all, label: str) -> None:
        state, names = self.crate_state(crate, closure)
        if state == "all":
            go_all(f"E2E 全部（[all].crates: {crate}）: {label}")
        elif state == "unmapped":
            go_all(f"E2E 全部（クレートが対応表に無い: {crate}）: {label}", crate)
        elif state == "scenarios":
            add(names, f"{label}（{crate}）")

    def check(self, members: dict[str, dict], closure: Callable[[str], set[str]]) -> list[str]:
        """対応表の更新忘れ・空振りを見つける（quality ジョブで落とす）。エラー文言に直し方を書く。"""
        errors: list[str] = []
        sc = self.table["scenarios"]
        specs = spec_files(self.tracked)
        listed = set(self.table["always"]) | {e for s in sc.values() for e in s["e2e"]}

        for name in sorted(listed):
            if not SPEC_NAME.match(name):
                errors.append(f"spec 名が不正: {name!r}（web/e2e 直下のファイル名を書く）")
            elif name not in specs:
                errors.append(f"対応表の spec が無い: {name}（改名・削除したら {TABLE} からも消す）")
            elif name.endswith(MANUAL_SUFFIX):
                errors.append(f"手動計測の spec は対応表に載せない: {name}")
        for name in specs:
            if not name.endswith(MANUAL_SUFFIX) and name not in listed:
                errors.append(
                    f"spec がどのシナリオにも無い: {E2E_DIR}{name}（その spec が開くページのシナリオの e2e に足す・{TABLE}）"
                )

        for where, patterns in [("[all].pages", self.table["all"]["pages"]), ("[all].paths", self.table["all"]["paths"])] + [
            (f"[scenario.{n}].{k}", s[k]) for n, s in sc.items() for k in ("pages", "paths")
        ]:
            for p in patterns:
                if where.endswith(".pages") and not p.startswith(APP_DIR):
                    errors.append(f"{where} の {p!r} は {APP_DIR} 配下のページを指す")
                elif not any(glob_regex(p).match(f) for f in self.tracked):
                    errors.append(f"{where} の {p!r} に当たるファイルが無い（改名・削除したら {TABLE} も直す）")

        for page in self.pages:
            if not matches(page, self.table["all"]["pages"]) and not self.scenarios_of_page(page):
                errors.append(
                    f"ページがどのシナリオにも無い: {page}（そのページを開く spec のシナリオの pages に足す・{TABLE}）"
                )

        declared: dict[str, list[str]] = {}
        for c in self.table["all"]["crates"]:
            declared.setdefault(c, []).append("[all]")
        for c in self.table["none"]:
            declared.setdefault(c, []).append("[none]")
        for n, s in sc.items():
            for c in s["crates"]:
                declared.setdefault(c, []).append(f"[scenario.{n}]")
        for c, places in sorted(declared.items()):
            if c not in members:
                errors.append(f"対応表のクレートが無い: {c}（改名・削除したら {TABLE} からも消す）")
            elif ("[all]" in places or "[none]" in places) and len(places) > 1:
                errors.append(f"クレート {c} が {', '.join(places)} に重なっている（[all]・[none] は他と併記しない）")
        for c in sorted(self.table["none"]):
            if c in members:
                reaches = sorted(closure(c) - set(self.table["none"]))
                if reaches:
                    errors.append(
                        f"[none] のクレート {c} に依存するクレートがある: {', '.join(reaches)}（サーバーに載るなら "
                        f"[none] から外し、scenario の crates か [all] に移す・{TABLE}）"
                    )
        for c in sorted(members):
            state, _ = self.crate_state(c, closure(c))
            if state == "unmapped":
                errors.append(
                    f"クレートがどのシナリオにも届かない: {c}（E2E に効くなら scenario の crates か [all]、"
                    f"compose の shiki-server に載らないなら [none] に足す・{TABLE}）"
                )

        for n, s in sc.items():
            if not s["e2e"]:
                errors.append(f"[scenario.{n}] に e2e が無い")
        return errors
