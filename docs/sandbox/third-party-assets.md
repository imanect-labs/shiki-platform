# サンドボックス同梱物の第三者ライセンス・帰属

サンドボックス（wasm ティア・`vendor/secure-exec/`）が guest に提供する実行アセット・コマンド群の帰属。
`cargo deny` は Rust 依存グラフを検査するが、以下の**バイナリ/wheel アセットは検査対象外**のため手動管理する。

## secure-exec フォーク本体

- **secure-exec** — Apache-2.0（`vendor/secure-exec/LICENSE`・Copyright 2025 Rivet Gaming, Inc.）。

## Python ランタイム（code_interpreter）

- **Pyodide 0.28.0**（`asset-manifest.sha256` で pin・非コミット）— Mozilla Public License 2.0。
- **CPython 3.13**（Pyodide 同梱・`python_stdlib.zip`）— Python Software Foundation License。
- **numpy 2.2.5** — BSD-3-Clause。
- **pandas 2.3.3** — BSD-3-Clause。
- matplotlib は同梱しない（可視化は generative UI・design §4.7）。

## ゲストコマンドスイート（`registry/native` から wasm32-wasip1 ビルド）

`registry/software/*` の各パッケージとして提供。主な出所とライセンス:

- **coreutils / findutils / diffutils 等**（uutils 実装）— MIT。
- **grep / sed / gawk**（Rust 再実装または uutils 系）— MIT。
- **git**（clean-room Rust 再実装）— Apache-2.0。
- **curl**（C upstream overlay を wasm32-wasip1 ビルド・`registry/native/c/curl-upstream-overlay`）—
  curl ライセンス（MIT/X 派生）。上流 curl の帰属を継承。
- **wget / jq / ripgrep / fd / tree / gzip / tar / unzip / zip / file / vim** — 各上流ライセンス
  （MIT / GPL 系はビルド時に混入しないもののみ採用。詳細は `registry/software/<name>` の package.json）。

> ⚠️ リリース配布物には Pyodide・各コマンドの LICENSE を同梱すること。GPL 系コマンドを guest に追加する
> 場合は配布形態（動的リンク相当か）を法務確認する。現行の既定 software 集合は MIT/Apache/BSD/curl のみ。

## V8

- **V8**（`v8` crate / rusty_v8・130 系）— BSD-3-Clause（V8）。隔離境界そのもの。CVE 監視は
  [fork-policy.md](./fork-policy.md) 参照。
