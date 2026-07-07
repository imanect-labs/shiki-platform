# shiki script ゲスト wasm の出所と再現ビルド

`crates/script-runtime/assets/shiki_qjs_guest.wasm` は **リポジトリにコミットされたバイナリ**
（in-repo vendor）である。`vendor/secure-exec/` と同じ統治モデルに従い、品質ゲート
（500 行/ファイル・カバレッジ・clippy）から除外し、出所と再現手順を本書に固定する。

## なぜバイナリを vendor するのか

ゲストは QuickJS（QuickJS-ng・MIT）を wasm 上で駆動する [javy](https://github.com/bytecodealliance/javy)
（Apache-2.0・Bytecode Alliance）を用い、`wasm32-wasip1` へビルドする。このビルドには
**wasi-sdk（clang + sysroot）と libclang（bindgen）** が要り、113MB 超の wasi-sdk を
ダウンロード・展開する。これを CI の毎回のジョブで行うのは非現実的（時間・供給元固定の
両面）なため、**pinned なツールチェーンで一度だけ再現ビルドし、成果物をコミットする**。

## 再現ビルド

```bash
bash scripts/build-qjs-guest.sh
```

- 実行環境: `rust:1.96-bookworm` Docker イメージ（`scripts/build-qjs-guest.sh` の `RUST_IMAGE` で固定）。
- ターゲット: `wasm32-wasip1`。
- 依存の版: `crates/script-runtime/guest/Cargo.toml`（`javy = "8"`）。javy が内部で
  wasi-sdk / QuickJS-ng を取得する（rquickjs-sys が版を固定）。
- 出力: `crates/script-runtime/assets/shiki_qjs_guest.wasm`（約 1MB）。

ビルド中間成果物（wasi-sdk 展開物等）はコンテナ内 `/tmp` に置き、ソースツリーへは残さない
（`crates/script-runtime/guest/.gitignore`）。

## ライセンス

- QuickJS-ng: MIT
- javy / rquickjs: Apache-2.0（一部 MIT/BSD）

いずれも `deny.toml` の `allow` に含まれる permissive ライセンス。ホスト側依存
（wasmtime = Apache-2.0 WITH LLVM-exception 等）は通常の依存グラフとして `cargo deny` が検査する。

## ABI 契約（ホスト ⇄ ゲスト）

`crates/script-runtime/guest/src/lib.rs` と `crates/script-runtime/src/engine.rs` が合意する:

- export `alloc(len) -> ptr` / `dealloc(ptr, len)`: 線形メモリの受け渡し。
- export `exec(js_ptr, js_len, input_ptr, input_len) -> u64`: 実行。戻り値は
  `(result_ptr << 32) | result_len`（結果エンベロープ JSON）。
- import `shiki.hostcall(req_ptr, req_len) -> u64`: 同期能力呼び出し（深さ 1）。応答は
  ホストがゲスト `alloc` で確保した領域へ書き、`(resp_ptr << 32) | resp_len` を返す。

WASI は最小サブセット（`random_get` / `clock_time_get` / `environ_*` / `fd_write`(stdio) のみ・
`crates/script-runtime/src/wasi_stub.rs`）だけを与える。`path_open` / `sock_*` は提供せず、
ゲストは**ファイル/ネットワークへ到達できない**（受け入れ条件「外界不達」の担保）。

## 更新手順

1. `crates/script-runtime/guest/` のソースまたは `javy` 版を更新する。
2. `bash scripts/build-qjs-guest.sh` で再ビルドする。
3. 生成された `.wasm` をコミットし、PR に本書の該当箇所（版・ハッシュ）を更新する。
