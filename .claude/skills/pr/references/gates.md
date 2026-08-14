# ローカル品質ゲート（CI と同一）

**正本は `.github/workflows/ci.yml`。** 下の表はそれを引き写したものなので、CI が変われば腐る。
食い違いを見つけたら **ci.yml が正**として扱い、**この表と `local-gates.sh` を同じ PR で直す**（Phase 3-b のドキュメント整合点検は `.claude/skills/*` も対象）。

## ドリフト検出は自動（思い出さなくてよい）

突き合わせは `local-gates.sh` が**毎回強制的に**行う。`ci.yml` のうちローカルで追随すべき部分
（`paths-ignore` / ジョブ ID・名前 / `if` 条件 / ステップの `name`・`run`）を正規化して
`.claude/skills/pr/ci-jobs.snapshot` に記録してあり、差があればゲートが落ちて差分が出る。

```bash
.claude/skills/pr/scripts/ci-snapshot.sh --check    # 差があれば diff を出して exit 1
.claude/skills/pr/scripts/ci-snapshot.sh --update   # 追随を済ませてから更新する
```

**差分が出たときの順序を守る**（逆にすると検出した意味が無い）:

1. 差分を読み、**この gates.md の対応表**と **`local-gates.sh`** を追随させる。
2. そのうえで `ci-snapshot.sh --update` を実行し、スナップショットも同じコミットに含める。

`uses:` / `with:` / `env:` / `timeout-minutes` / `runs-on` は意図的に対象外
（変更頻度の割にローカルコマンドへ写らず、churn だけが増える）。
アクションのバージョン bump ではドリフトにならない。

**なぜ機構にしたか**: 以前はここに突き合わせ用のワンライナーを置いていたが、
「実行しようと思い出す」必要があった。実際に `ci.yml` へステップを足した本人が、
同じセッション内で `local-gates.sh` への追随を忘れている（#455）。

**`docs/**` / `**.md` / `.claude/**` のみの変更では CI が丸ごとスキップされる**（`paths-ignore`）ため、docs のみの PR で「チェックなし」は正常。

## CI ジョブ ↔ ローカルコマンド 対応表

| CI ジョブ | ローカルで回す | 回す条件 |
| --- | --- | --- |
| Rust (fmt/clippy/test) | `cargo fmt --all --check`<br>`cargo clippy --all-targets --all-features -- -D warnings`<br>`cargo nextest run --workspace --all-features` | `crates/` 差分 |
| Build shiki-server | （ローカルでは不要。compose 系ジョブへ release バイナリを artifact 共有するための CI 専用ジョブ） | — |
| Quality gates | `bash scripts/check-file-size.sh`<br>`bash scripts/check-migration-versions.sh`<br>`bash scripts/check-compose-ports.sh`<br>`cargo machete crates` | 常に（`.rs` 追加・依存追加・migration 追加時は必須） |
| cargo-deny | `cargo deny --all-features check` | `Cargo.toml` / `Cargo.lock` 差分 |
| Coverage (gate 80%) | 下記「カバレッジ」節 | `.rs` の**新規追加**時 |
| Web | `pnpm install --frozen-lockfile`<br>`pnpm gen:api`<br>`pnpm lint`<br>`pnpm build` | `web/` 差分 |
| Python | `cd ingestion-worker && uv sync --frozen`<br>`uv run ruff check .`<br>`uv run pytest -m "not slow" -q` | `ingestion-worker/` 差分 |
| Tabular runner | `cargo fmt --manifest-path crates/tabular/runner/Cargo.toml --check`<br>`cargo clippy --manifest-path crates/tabular/runner/Cargo.toml --release -- -D warnings`<br>`cargo build --manifest-path crates/tabular/runner/Cargo.toml --release`<br>→ `SHIKI_TABULAR_RUNNER=crates/tabular/runner/target/release/shiki-tabular-runner cargo test -p shiki-tabular --test runner_adversarial_it` | `crates/tabular/` 差分 |
| compose smoke | `bash deploy/compose/smoke-bff.sh` | 認証・起動経路・compose 定義の変更時 |
| Web E2E | `references/verify.md` 参照 | `web/` 差分 |
| Sandbox gVisor IT | `bash scripts/fetch-native-assets.sh && bash scripts/build-sandbox-rootfs.sh`<br>→ `SANDBOX_GVISOR_IT=1 RUNSC_BIN=deploy/sandbox-assets/bin/runsc GVISOR_ROOTFS=deploy/sandbox-assets/rootfs cargo test -p shiki-sandbox-orchestrator --test gvisor_it -- --test-threads=1` | `crates/sandbox-*` 差分（CI では**非ブロッキング**） |

`scripts/local-gates.sh` は、この表のうち**自動化できるもの**（file-size / fmt / clippy / test / machete / deny / doctest / tabular / web / python）を差分から判定して実行する。
**compose smoke・Web E2E・gVisor IT・カバレッジ実測は自動実行しない**（compose 起動やアセット取得が要るため）。必要な時に上表のコマンドで手動実行する。

### CI との差異（意図的）

- **`cargo build --all` は不要**。CI では廃止済み（`clippy --all-targets` が全ターゲットの型検査を兼ねる）。
- **rust ジョブは compose 非依存** = env ゲートされた結合テストはスキップされ、実質ユニットのみ走る。結合テストは coverage ジョブと compose ジョブが担保する。ローカルで IT も回すなら下記「結合テスト」節の env を与える。
- **doctest は nextest が実行しない**。coverage 側（`cargo llvm-cov --all`）で走るため、doctest を書いたら `cargo test --doc` を別途回す。

## 結合テスト（IT）をローカルで回す

テスト専用コンテナを使う（compose 本体の `:5432` / `:8082` とは別物）:

```bash
export STORAGE_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:55432/shiki   # pg-test
export OPENFGA_TEST_URL=http://localhost:58080                                        # fga-test
export REDIS_TEST_URL=redis://localhost:6379
export STORAGE_TEST_S3_ENDPOINT=http://localhost:9000
export RAG_TEST_QDRANT_URL=http://localhost:6333
```

- migration checksum 不一致（`VersionMismatch(n)`）は別ブランチの残骸。使い捨てなのでリセットしてよい:
  `docker exec pg-test psql -U postgres -c "DROP DATABASE shiki WITH (FORCE);" -c "CREATE DATABASE shiki;"`
- **jobq の `chat_generation` は 1 テストバイナリ内で共有される。** ワーカー能力（`WorkerDeps`）の異なるテストを同一バイナリに同居させない（他テストの run を横取りし、偽陽性/偽陰性になる）。chat 系 IT のイベント待ちがタイムアウトする時はまずこれを疑う。
- **`main.rs` に `mod foo;` で宣言したモジュール**（`api/src/miniapp_triggers.rs` など）はバイナリクレート所属。`cargo test -p shiki-api --lib` では拾えない（0 tests filtered）ので `--bin shiki-server <filter>` で実行する。

## カバレッジ 80% ゲート

CI は **workspace 総計の行カバレッジ**を `cargo llvm-cov --fail-under-lines 80` で判定する（per-crate ではない）。

**最頻の落とし穴**: 新規 API ルートファイル（`crates/api/src/routes/*.rs`）はハンドラを叩く HTTP テストが無いと 0〜30% になり、**総計を 80% 未満へ引きずり落とす**。過去に複数 PR がこれで落ちている。

対策 — `crates/api/tests/<feature>_http.rs` を追加し、`http.rs` の `state_with` / `AllowAll` / `FakeStore` ハーネスを流用する。ただし:

- **実 pool を注入する**（lazy な到達不能 URL ではなく `STORAGE_TEST_DATABASE_URL`）。
- セッション Cookie は `shiki_session=<sid>.<tenant>`。状態変更系は `shiki_csrf=<t>` ＋ `x-csrf-token` の二重送信。
- `node` テーブルへの生 insert は `updated_by` が **NOT NULL**（migration 0045）。未設定は `23502` で決定的に失敗する。

ローカル実測（CI と同条件にするなら compose 本体 `:5432` / `:8082` / `:6379` / `:9000` / `:6333` を起動）:

```bash
# compose の shiki DB は checksum 不一致になりがちなので使い捨て DB を切る。
# **先に CREATE すること**（env を設定すると IT のスキップ分岐に入らず、接続失敗で panic して
# DB を使う結合テストが全滅する）。
docker compose -f deploy/compose/docker-compose.yml exec -T postgres \
  psql -U postgres -c "DROP DATABASE IF EXISTS shiki_cov WITH (FORCE);" -c "CREATE DATABASE shiki_cov;"

STORAGE_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/shiki_cov \
  cargo llvm-cov --all --no-report
cargo llvm-cov report --summary-only --ignore-filename-regex '<ci.yml と同じ regex>'
```

除外 regex は ci.yml の値をそのままコピーする（ズレると判定が食い違う）。

## 既知の罠

- **base には remote-tracking ref（`origin/main`）を使う。ローカルの `main` はしばしば古い。**
  worktree では特に放置されがちで、`git diff main...HEAD` が「このブランチで追加した」ファイルを
  何千件も誤検出する（実測: ローカル `main` 基準 2427 件 / `origin/main` 基準 1 件）。
  これはゲート選択・カバレッジ注意喚起・Phase 4 の独立レビューに渡す diff の全てを狂わせる。
  精度が要るときは先に `git fetch origin` する。
- **`cmd | tail` はパイプ終端の exit code を返す** = 失敗が exit 0 に化ける。
  `cargo` だけでなく **`gh pr checks --watch | tail` でも起きる**（CI が赤なのに「緑」と読む。実際に踏んだ）。
  合否を見たいコマンドはパイプにつながない。`cmd > log 2>&1; rc=$?` で受けてから log を読む。
- **`check-file-size.sh` は git-tracked ファイルのみ数える**（`*.rs` 対象。上限は同スクリプトの `MAX_LINES`・ここに数値を書かない）。新規ファイルは `git add` 前だとローカル検査をすり抜け、CI で落ちる。
- **migration 番号は並行 PR と衝突する。** 着手時に `gh pr list --json number -q '.[].number' | xargs -I{} gh pr diff {} --name-only | grep migrations` 相当で番号を予約する。
- **`vendor/` は品質ゲート除外**（所有フォーク・行数上限/カバレッジ/clippy 対象外）。`cargo machete` も `crates` のみ対象。
- **ディスク逼迫で linker が Bus error / No space / exit 144 になる。** 順に `rm -rf target/debug/incremental` → `docker builder prune -f`（20GB 級）→ 不要 worktree の `target/` 削除 → `cargo clean`。
- **ts-rs が `TS_RS_LARGE_INT` に対応していない**（バージョンは `Cargo.toml` を見る）。`i64` を TS の `number` にしたいフィールドは `#[ts(type = "number")]`、`Option<i64>` は `#[ts(type = "number | null")]` を付ける。bump したら対応状況を確認して本項を消す。
- **手書き型を作らない。** 型は Rust → OpenAPI → TS（`pnpm gen:api`）、SSE は ts-rs/typeshare。`web/` に手で型を足したら codegen 側を直す。
- **ゲートを回すと追跡済みの生成物が書き換わる。`git add -A` を無条件に使わない。**
  ts-rs のバインディング出力（`crates/*/bindings/*.ts`）はテスト実行時に生成されるため、
  `cargo nextest run --workspace --all-features`（＝ CI と同じコマンド）を回すと
  リポジトリにコミット済みのファイルが書き換わる。`--all-features` が ts-rs の追加 feature を
  有効にし、`import ... from "./X"` が `"./X.js"` になるため。
  この状態で `git add -A` すると**無関係な生成物が数十件コミットに混入する**（実際に 82 件混入した）。
  CI も同じコマンドを使う＝ CI 側でも同じ差分が出るだけなので、**CI はこの混入を検出できない**。
  → ゲート実行後は必ず `git status` / `git diff --stat` を見てから、**パスを明示して add する**。
