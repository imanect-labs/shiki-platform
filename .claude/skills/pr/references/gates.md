# ローカル品質ゲート（CI と同一）

**正本は `.github/workflows/ci.yml`。** 下の表はそれを引き写したものなので、CI が変われば腐る。
食い違いを見つけたら **ci.yml が正**として扱い、**この表と `local-gates.sh` を同じ PR で直す**（Phase 3-b のドキュメント整合点検は `.claude/skills/*` も対象）。

その場で突き合わせるには:

```bash
# jobs: 以降に限定する（限定しないと on: の push: を拾って偽陽性になる）
sed -n '/^jobs:/,$p' .github/workflows/ci.yml | grep -E '^  [a-z-]+:$'
```

表に無いジョブがあれば、それがドリフトしている。**各ジョブが実行する `run:` も突き合わせる**
（ジョブ名が同じでもステップが増えていることがある）:

```bash
sed -n '/^  <ジョブ ID>:/,/^  [a-z-]*:$/p' .github/workflows/ci.yml | grep -E '^\s+(- name|run):'
```

PR の CI に `paths-ignore` は無い。`docs/**` / 最上位の `*.md` / `.claude/**` のみの変更では changes ジョブが重量ジョブを
すべて skip し、`changes`・`quality`・`ci-ok` だけが走る（required check の `ci-ok` が pending のまま固着しないため）。
**チェックが 0 件なら CI は起動していない**（コンフリクトか未登録）。

**マージは `ci-ok` が緑になってからマージキューに入れる。** PR は変更から選んだテストだけを回し、マージキュー
（`merge_group`）が docs 以外の変更を含む限り全量を回す。キューで落ちたら、PR では選ばれなかったテストが
壊れている（選択規則の漏れか flaky）ので、PR に戻って直す。

## CI ジョブ ↔ ローカルコマンド 対応表

| CI ジョブ | ローカルで回す | 回す条件 |
| --- | --- | --- |
| Rust (fmt/clippy/test) | `cargo fmt --all --check`<br>`cargo clippy --all-targets --all-features -- -D warnings`<br>`cargo nextest run --workspace --all-features` | `crates/` 差分 |
| Build shiki-server | （ローカルでは不要。compose 系ジョブへ release バイナリを artifact 共有するための CI 専用ジョブ） | — |
| Quality gates | `bash scripts/check-file-size.sh`<br>`bash scripts/check-migration-versions.sh`<br>`bash scripts/check-compose-ports.sh`<br>`python3 -m unittest discover -s scripts -p 'test_*.py'`<br>`python3 scripts/ci-select.py check`<br>`cargo machete crates` | 常に（`.rs` 追加・依存追加・migration 追加・E2E spec／ページ／クレート追加時は必須） |
| cargo-deny | `cargo deny --all-features check` | `Cargo.toml` / `Cargo.lock` 差分 |
| Coverage (gate 80%) | 下記「カバレッジ」節 | `crates/` 差分（CI は Rust の変更があれば回す。`.rs` の新規追加時は特に注意） |
| Web | `pnpm install --frozen-lockfile`<br>`pnpm gen:api`<br>`pnpm lint`<br>`pnpm build` | `web/` 差分 |
| Python | `cd ingestion-worker && uv sync --frozen`<br>`uv run ruff check .`<br>`uv run pytest -m "not slow" -q` | `ingestion-worker/` 差分 |
| Tabular runner | `cargo fmt --manifest-path crates/tabular/runner/Cargo.toml --check`<br>`cargo clippy --manifest-path crates/tabular/runner/Cargo.toml --release -- -D warnings`<br>`cargo build --manifest-path crates/tabular/runner/Cargo.toml --release`<br>→ `SHIKI_TABULAR_RUNNER=crates/tabular/runner/target/release/shiki-tabular-runner cargo test -p shiki-tabular --test runner_adversarial_it` | `crates/tabular/` 差分 |
| compose smoke | `bash deploy/compose/smoke-bff.sh` | 認証・起動経路・compose 定義の変更時 |
| Keycloak User Profile smoke | `bash deploy/compose/smoke-keycloak-user-profile.sh`（compose の Keycloak 相手。`KC=` で URL 指定可） | `deploy/keycloak/` 差分・Keycloak admin REST 経路（`crates/api/src/keycloak_admin.rs`）の変更時 |
| Web E2E | `references/verify.md` 参照 | `web/` 差分（CI は下記「テストの選択」の spec だけを回す） |
| Sandbox gVisor IT | `bash scripts/fetch-native-assets.sh && bash scripts/build-sandbox-rootfs.sh`<br>→ `SANDBOX_GVISOR_IT=1 RUNSC_BIN=deploy/sandbox-assets/bin/runsc GVISOR_ROOTFS=deploy/sandbox-assets/rootfs cargo test -p shiki-sandbox-orchestrator --test gvisor_it -- --test-threads=1` | `crates/sandbox-*` 差分（CI では**非ブロッキング**） |

`scripts/local-gates.sh` は、この表のうち**自動化できるもの**（file-size / fmt / clippy / test / machete / deny / doctest / tabular / web / python）を差分から判定して実行する。
**compose smoke・Web E2E・gVisor IT・カバレッジ実測は自動実行しない**（compose 起動やアセット取得が要るため）。必要な時に上表のコマンドで手動実行する。

### CI との差異（意図的）

- **`cargo build --all` は不要**。CI では廃止済み（`clippy --all-targets` が全ターゲットの型検査を兼ねる）。
- **rust ジョブは compose 非依存** = env ゲートされた結合テストはスキップされ、実質ユニットのみ走る。結合テストは coverage ジョブと compose ジョブが担保する。ローカルで IT も回すなら下記「結合テスト」節の env を与える。
- **doctest は nextest が実行しない**。coverage 側（`cargo llvm-cov --all`）で走るため、doctest を書いたら `cargo test --doc` を別途回す。

## テストの選択と対応表（CI が回すものを決める表）

PR の CI は変更ファイルから回すジョブ・テストを選ぶ（`scripts/ci-select.py`・#921/#922。結果は Job Summary の「テストの選択」）。
Rust はクレートの依存グラフから自動で決まる。手で保つ表は 2 つで、**載せ忘れは quality ジョブの
`python3 scripts/ci-select.py check` が落とす**（エラー文言がどこに何を足すかを示す）:

- **`ci/scenarios.toml`（E2E の担当シナリオ対応表）**: E2E spec を足した → その spec が開くページのシナリオの `e2e` へ。
  ページ（`page.tsx`）を足した → そのページを開く spec のシナリオの `pages` へ。クレートを足した → どこからも依存されない
  なら `crates`・`[all]`・`[none]` のどれかへ。部品・lib は import から自動で辿るので載せない。
- **`scripts/ci-select.py` の `ALWAYS_RUN`**: 名前が脅威テストらしい結合テスト（`threat`・`tenant`・`authz`・`scope` 等）を
  足した → 毎回回すなら `ALWAYS_RUN`、回さなくてよいなら理由を付けて `ALWAYS_RUN_EXEMPT` へ。

対応表で決まらない変更は E2E を全部回す（漏れずに遅くなるだけ。PR に「E2E の担当シナリオ対応表に無い」警告が出たら
載せる）。main とマージキューは常に全量で、PR では選ばれないテストがそこで落ちると selection-gap ジョブが Issue を起票する
（選択規則の漏れか flaky）。再試行で通った flaky（CI は緑）は、main の flaky-report ジョブが `flaky: <テスト>` の
Issue に記録する。自分の変更に関係するテストの flaky Issue があれば、ついでに直すか原因を書き足す。

## 結合テスト（IT）をローカルで回す

### S3 は RustFS を使う

ローカル・CI とも MinIO から **RustFS** に移行済み。`autonomous_it` と
`workspace_containment_it` も汎用の `S3ObjectStore` を使うため、MinIO 固有の依存はない。
`minioadmin` は開発用の既定資格情報、設定の `minio` は後方互換 alias、テストの
`http://minio:9000` はモック URL。これらを実サービスの指定と解釈しない。

Compose は 9000/9001 を全インターフェースに公開するため、既定の資格情報のまま起動しない。
既存環境では `.env` に設定した強固な `RUSTFS_ACCESS_KEY` / `RUSTFS_SECRET_KEY` を
同名の環境変数に設定してから実行する（既定値 `minioadmin` は使わない）。
以下は未設定なら新規テスト環境用の資格情報を生成する。生成値は表示せず、このシェルでテストを実行する。

```bash
# リポジトリルートで実行。既存の .env は上書きしない。
export RUSTFS_ACCESS_KEY="${RUSTFS_ACCESS_KEY:-$(openssl rand -hex 16)}"
export RUSTFS_SECRET_KEY="${RUSTFS_SECRET_KEY:-$(openssl rand -hex 32)}"
export STORAGE_TEST_S3_ACCESS_KEY="$RUSTFS_ACCESS_KEY"
export STORAGE_TEST_S3_SECRET_KEY="$RUSTFS_SECRET_KEY"
docker compose -f deploy/compose/docker-compose.yml up -d --wait rustfs
docker compose -f deploy/compose/docker-compose.yml ps rustfs
curl --fail --show-error http://localhost:9000/health/ready
```

`STORAGE_TEST_S3_ENDPOINT` は RustFS の S3 API（既定 `http://localhost:9000`）を指す。
9001 はコンソール用。ポートを変更した場合は `docker compose ... port rustfs 9000`
でホスト側ポートを確認する。資格情報を変更している場合は
`STORAGE_TEST_S3_ACCESS_KEY` / `STORAGE_TEST_S3_SECRET_KEY` も RustFS と揃える。
秘密値をログや報告に出さない。`/health/ready` は準備状態の確認で、認証の成功までは保証しない。

失敗時は接続先、Compose の状態・ログ、ready、S3 の認証/バケット操作の順に調べる。
DB の env だけを設定すると、上記2テストは S3 の既定値 `localhost:9000` に接続するため、
RustFS 未起動でもテストは始まり、バケット準備で失敗する。
「MinIO が無い」「環境に S3 互換ストレージが無い」と推測で終了せず、起動して再実行する。
Docker が使えない等で起動できない場合は、実行したコマンド・具体的なエラーと未検証のテストを報告する。
変更前の main でも同じ失敗であることは、依存サービスの確認を省く根拠にはしない。

### 接続先を設定して実行する

DB / OpenFGA はテスト専用コンテナを使う（compose 本体の `:5432` / `:8082` とは別物）:

```bash
export STORAGE_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:55432/shiki   # pg-test
export OPENFGA_TEST_URL=http://localhost:58080                                        # fga-test
export REDIS_TEST_URL=redis://localhost:6379
export STORAGE_TEST_S3_ENDPOINT=http://localhost:9000
export RAG_TEST_QDRANT_URL=http://localhost:6333
export RAG_TEST_QDRANT_API_KEY=shiki-qdrant-dev-key  # compose の Qdrant は API キー付き（#812）
# tabular の IT（tabular_http_it / workflow_nodes_it の csv.*）は隔離ランナーが要る（無いと黙ってスキップ）
export SHIKI_TABULAR_RUNNER=$PWD/crates/tabular/runner/target/release/shiki-tabular-runner
```

- **`SHIKI_REQUIRE_IT=1` を付けて回す。** 接続先が未設定の結合テストはスキップして pass に数えられるので、
  env を 1 つ渡し忘れても緑に見える。これを付けると未設定が失敗になる（`crates/testenv`・CI の coverage ジョブと同じ）。
  CI が用意しない依存（実 Keycloak・実 LLM 等）は ci.yml の `SHIKI_REQUIRE_IT_EXCEPT` と同じ値を `SHIKI_REQUIRE_IT_EXCEPT` に渡す。
  新しい結合テストの接続先は `shiki_testenv::var("…")` で読む（`std::env::var` 直読みは quality ジョブの it-env gate が落とす）。
- tabular の IT はアップロードした blob を content-addressing で共有する。**別バケットで同じ DB を使い回すと**
  「同じ内容は既にある」扱いで実体が上がらず `ObjectStore(NotFound)` の 500 になる。再実行で落ちたら DB を作り直す。

- migration checksum 不一致（`VersionMismatch(n)`）は別ブランチの残骸。使い捨てなのでリセットしてよい:
  `docker exec pg-test psql -U postgres -c "DROP DATABASE shiki WITH (FORCE);" -c "CREATE DATABASE shiki;"`
- **jobq の `chat_generation` は 1 テストバイナリ内で共有される。** ワーカー能力（`WorkerDeps`）の異なるテストを同一バイナリに同居させない（他テストの run を横取りし、偽陽性/偽陰性になる）。chat 系 IT のイベント待ちがタイムアウトする時はまずこれを疑う。
- **`main.rs` に `mod foo;` で宣言したモジュール**（`api/src/miniapp_triggers.rs` など）はバイナリクレート所属。`cargo test -p shiki-api --lib` では拾えない（0 tests filtered）ので `--bin shiki-server <filter>` で実行する。

## カバレッジ 80% ゲート

CI は 2 段で判定する（#921）。

- **PR**: changes ジョブ（`scripts/ci-select.py`）が選んだテスト（変更したクレート＋依存するクレート＋常に回す脅威テスト）だけを計測し、**変更した行**の 80% 以上で判定する（`scripts/ci-select.py diff-coverage`）。未達の行はカバレッジコメントと Job Summary に出る。ローカルでは CI と同じ除外（ci.yml の coverage ジョブの `COV_IGNORE`）を付けて `cargo llvm-cov report --lcov --output-path lcov.info --ignore-filename-regex "$COV_IGNORE"` の後に `python3 scripts/ci-select.py diff-coverage --lcov lcov.info --ignore "$COV_IGNORE" --base "$(git merge-base origin/main HEAD)" --head HEAD`（base は merge-base。`origin/main` を直接渡すと main 側の変更が混ざる）。ファイルの移動・改名は検出して動かしただけの行は数えないが、1000 行ゲートのための**分割**は移した行が新規行として数えられる。元が 80% 未満なら分割と同じ PR でテストを足すか、分割だけの PR にして理由を書く。
- **main**: 全テストを計測し、**workspace 総計の行カバレッジ**を `cargo llvm-cov report --fail-under-lines 80 --ignore-filename-regex "$COV_IGNORE"` で判定する（per-crate ではない）。PR の変更行が通っても、テストの無い新規ファイルが総計を下げると main で落ちる。

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
