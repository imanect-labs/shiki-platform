#!/usr/bin/env bash
# verify-main.sh — main（このブランチを入れた後の main）を、1 コマンドで確かめる。
#
#   手元にスタックを確かめ用の DB で立て（.claude/skills/pr/scripts/dev-up.sh の native 起動）、
#   E2E（web/e2e の Playwright）を 1 本ずつ流して、結果を表にまとめる。どのチェックアウトでも動く
#   （その中の dev-up.sh と web/e2e を使う）。
#
# 使い方:
#   bash scripts/verify-main.sh                  既定の抜粋（26 本＋あれば 4 本）を流し、最後にスタックを止める
#   bash scripts/verify-main.sh --keep-up        流し終わってもスタックを止めず、触って確かめる URL とアカウントを出す
#   bash scripts/verify-main.sh --keep-up --no-e2e   起動だけする（E2E は流さない）
#   bash scripts/verify-main.sh --all            web/e2e の spec を全部流す（*.manual.spec.ts を除く。
#                                                このブランチで 185 本。main を入れると 3 本、7p で 1 本増える）
#   bash scripts/verify-main.sh --specs "auth.spec.ts notes.spec.ts"   指定した spec だけ流す
#   bash scripts/verify-main.sh --list           流す spec と選んだ理由を出すだけ（何も起動しない）
#   bash scripts/verify-main.sh --down           --keep-up で残したスタックを止める
#   bash scripts/verify-main.sh --status         スタックが動いているかだけ見る
#
#   ほかの引数:
#     --keep-db           前の回の DB とストアをそのまま使う（作り直さない）
#     --replace           :3000・:8080 で別のスタックが動いていても止めて立て直す（既定は止まって知らせる。
#                         止めたスタックは、終わった後に起動し直さない）
#     --restart-every N   N 本ごとにスタックを起動し直す（既定 6・0 で起動し直さない）
#     --min-free-mb N     空きメモリが N MB を切ったら、次の spec の前に起動し直す（既定 3200）
#     --spec-timeout N    1 本の上限（分・既定 30）
#     --no-prepare        pnpm install と pnpm gen:api を飛ばす
#     --shots             画面を撮る spec の画像を結果の置き場に残す（E2E_SHOTS_DIR）
#     --verbose           Playwright の出力をそのまま画面にも出す
#     --db NAME / --store NAME   確かめ用の DB とストアの名前（shiki_verify・shiki-verify で始まる名前だけ）
#
# かかる時間（2 CPU・メモリ 10GB の手元のマシンで）:
#   既定の抜粋: E2E がおよそ 40〜60 分。スタックの起動・起動し直しは 1 回 1〜2 分。
#   --all: 3〜4 時間。
#   新しいチェックアウトの初回は、ビルドが加わる（shiki-server が 10〜15 分、CSV の集計の隔離ランナーの
#   release ビルドが 30 分以上）。debug のランナーだけがあれば、それを使う（ビルドしない）。
#
# 前提:
#   docker（compose v2）・cargo・pnpm・node・curl・git・gzip。Playwright の Chromium
#   （無ければ: cd web && pnpm exec playwright install chromium）。
#   ポート :3000（web）・:8080（API）・:8081（Keycloak）・:8082（OpenFGA）・:18090/:18091（ゲートウェイ）を使う。
#   ほかのスタックがこれらを使っていたら止まって知らせる（--replace で立て直す）。
#
# ディスクとメモリの目安:
#   ディスク: 新しいチェックアウトなら空き 40GB 以上（target が 20GB〜・隔離ランナーが数 GB・node_modules 1GB・
#             Chromium 0.9GB）。結果の置き場は 1 回あたり数十〜数百 MB（ログは gzip・トレースは落ちたテストだけ）。
#   メモリ: スタックと E2E で 6〜8GB。next dev は流すほど太り（100 本で 3GB 超）、OpenFGA も長く動かすと太る。
#           そのため N 本ごとと、空きメモリが少ないときに起動し直す。始める時点で空きが 4GB を切っていたら知らせる。
#
# DB とストア（開発の DB「shiki」には触らない）:
#   DB shiki_verify・OpenFGA のストア shiki-verify・バケット shiki-verify・Redis の 2 番を使う。
#   毎回、前の回の shiki_verify を DROP して作り直し、同じ名前のストアも消す（サーバーが起動のときに作り直す。
#   前の回のデータは消える）。残したいときは --keep-db。名前に shiki を渡すと断る。
#
# LLM:
#   stub（決まった応答を返す）だけで流す。環境の SHIKI__LLM__* の上書きは消して、SHIKI__LLM__BACKEND=stub にする。
#   SHIKI_CONFIG が無く ~/.config/shiki-dev/llm-stub.toml があれば、それを使う（stub の判定用のモデルが入っており、
#   FEAT-1 の判定の spec が飛ばされずに走る）。どちらも無ければ dev-up.sh の既定に任せる。既定では、カタログは
#   stub のモデル default の 1 つだけになる。E2E は管理の API（/api/org-admin/ai/providers）で種別が stub の
#   プロバイダのモデルを探して使う（web/e2e/helpers.ts。プロバイダは作らない）ので、既定でも動く。判定用の
#   モデルが無いので、判定の spec の一部は飛ばされる。どちらの設定でも、組織の既定のモデルが stub でなければ、
#   E2E の global-setup がテストの前に止める。
#
# 結果:
#   ~/.local/state/shiki-verify/<日時>/ に置く。summary.md（表と要約）・results.tsv・env.txt・logs/・json/・
#   details/・artifacts/（落ちたテストのトレース）・stack/（起動のログ・サーバーと web のログ）。
#   ~/.local/state/shiki-verify/latest が、いちばん新しい回を指す。
#   落ちた spec は最後にもう 1 回だけ流し直し、「流し直すと通った（揺れの疑い）」と「続けて落ちた」に分ける。
#   手元の E2E は next dev で流す（CI は next build）。data-sql.spec.ts は next dev では必ず落ちるので「既知」と出す
#   （docs/workflow/quality-campaign.md §9.4）。
#
# 終わりのコード: 0 = 続けて落ちたものが無い・1 = 続けて落ちた／途中で止まった・2 = 引数の誤り・130 = 中断（Ctrl-C）。
# Ctrl-C で止めると、そこまでの結果をまとめて、スタックを止める（--keep-up なら残す）。
set -uo pipefail

err() { printf '%s\n' "$*" >&2; }
say() { printf '%s\n' "$*"; }
die() { err "$1"; exit "${2:-1}"; }

# --- 既定の抜粋 ---------------------------------------------------------------------------------------------
# ci/scenarios.toml のシナリオごとに、このブランチで変わった所に当たる主な spec を選んだ（[ ] はシナリオ）。
# 大事なもの（human が報告した BUG-1〜5・P0・FEAT-1・D-10 の AI 作成・権限と共有と承認の境界）を先に流す。
# workflows はこのブランチの本題で spec も 77 本あるので 4 本にした。--list はこの行のコメントを理由として出す。
DEFAULT_SPECS=(
  auth.spec.ts                    # [always] ログイン → 保護ページ → ログアウトの最小経路（7p-2 で /login?error=account_unusable の表示も足す予定）
  workflows-editor.spec.ts        # [workflows] human が報告した BUG-1（変数）・BUG-2（DnD）・BUG-4（モデル）・BUG-5（CSV のファイル）と BUG-9・15・31・GAP-09・12（33 件）
  workflows-data-trigger.spec.ts  # [workflows] P0 の GAP-07: テーブルに行が追加されたら起動するきっかけ（BUG-36）
  workflows-enable.spec.ts        # [workflows] P0 の BUG-95（スケジュールの曜日）と、有効化の同意・止める理由（D-196・7o-F1 の直し）
  workflow-decide-editor.spec.ts  # [workflows] FEAT-1 の「AI で判定」をエディタで組み、stub の決定器で出口へ進む（T6・D-7）
  admin-decision-models.spec.ts   # [admin] FEAT-1 の判定用のモデルの管理画面（既定・Auto に選べず、チャットの一覧に出ない）
  ai-workflow-try.spec.ts         # [home・workflows] D-10 の AI 作成の続き: 会話の試運転の承認カードと閲覧のみの境界（T04・D-197・7o-F1）
  chat-builder-ref.spec.ts        # [home] 7p-10 の、会話で作ったテーブル・ビュー・アプリ・ワークフローへの参照カード（builder_ref・D-204）
  approvals.spec.ts               # [approvals] 承認待ちの受信箱と承認を待つブロック（GAP-08・D-92・BUG-119・owner でない編集者の見え方）
  data-state-flow.spec.ts         # [data・approvals] 状態の進め方: ほかの人（bob）の承認で進み、申請した本人は判断できない（GAP-44）
  data-created-by.spec.ts         # [data] 作成者の列で、申請した本人が自分で承認するのを防ぐ（BUG-148・D-122）
  data-share-add-only.spec.ts     # [data] テーブルの「追加だけ」の共有の境界（GAP-49・D-113）
  share-owner-only.spec.ts        # [skills・workflows・apps] 共有の操作は持ち主にだけ出す（BUG-120・BUG-78）
  mini-apps.spec.ts               # [apps] ミニアプリの作成・起動・共有・結果の表示（BUG-17〜20・GAP-09・GAP-37・14 件）
  miniapp-access-check.spec.ts    # [apps] 使う人に足りない権限を、押す前に理由つきで言う（GAP-37・BUG-217）
  miniapp-b1.spec.ts              # [apps] コードアプリの publish → インストール → 隔離（BUG-172）。7p-8 の取り下げもここに足す予定
  drive-crud.spec.ts              # [drive] フォルダ作成〜ゴミ箱からの復元（BUG-125）。7p の完全削除（GAP-108）が触るゴミ箱の回帰
  drive-picker-focus.spec.ts      # [drive・csv・workflows] 「ドライブから選ぶ」を閉じたときのフォーカス（BUG-261・263・BUG-5 のファイルの欄）
  drive-mobile-tabs.spec.ts       # [drive] スマホのドライブの上端のタブ。main の #1185（PC でも上端のタブ）と重なる所
  csv-editor.spec.ts              # [csv] CSV のグリッドと SQL（Shift_JIS の GAP-45・複数 CSV の JOIN #1059・予約語 #1142）
  audit-log.spec.ts               # [admin] 監査ログの閲覧者の境界（alice・charlie・bob）。7p-0・7p-1 で監査の語彙が増える
  notes.spec.ts                   # [notes] ノートの回帰（共同編集・閲覧者は読取専用・分割ビューの fail-closed）
  composer-create.spec.ts         # [office・slides・csv・notes・home] 入力欄の「+」→「作成」で各エディタが開く（Collabora 無しで流せる）
  search-page.spec.ts             # [search] 検索の動線のスモーク（RAG なし）
  sidebar-mobile.spec.ts          # [settings・home・drive] スマホのドロワ。main の #1185（サイドバーをタイル型に）と重なる所
  answer-links.spec.ts            # [reference] 回答のリンクの確認。main の #1143 と、このブランチの外部リンクの扱い（GAP-09）を merge で合わせた所
)
# 入れた後の main にはあるが、この作業ツリーにはまだ無いことがある spec（無ければ飛ばし、そう出す）。
OPTIONAL_SPECS=(
  drive-trash-purge.spec.ts       # [drive・7p-5] ゴミ箱の完全削除（GAP-108b・D-201）。7p で足す予定
  chat-branch.spec.ts             # [home・main #1172] 発話を編集して送り直すと枝ができ、切り替えられる・「新しいチャットに分岐」
  chat-translation-export.spec.ts # [home・notes・main #1162] 会話の書き出しに Web ページの訳文が入る
  pdf-workspace.spec.ts           # [drive・main #1177] PDF の OCR の範囲選択とチャット添付（storage の internal_io.rs を両方で変えた・32 件）
)
# next dev では製品の不具合でなく必ず落ちる spec（docs/workflow/quality-campaign.md §9.4）。続けて落ちても「既知」と出す。
declare -A KNOWN_DEV_FAIL=(
  [data-sql.spec.ts]="next dev（React の StrictMode）では glide の表が描かれず、結果の行の確かめで必ず落ちる。本番ビルドでは通る"
)

# --- 引数 ---------------------------------------------------------------------------------------------------
ACTION=run
MODE=default
SPECS_ARG=""
KEEP_UP=0
NO_E2E=0
KEEP_DB=0
REPLACE=0
RESTART_EVERY=6
MIN_FREE_MB=3200
SPEC_TIMEOUT_MIN=30
PREPARE=1
SHOTS=0
VERBOSE=0
DB_NAME=shiki_verify
STORE=shiki-verify
REDIS_DB=2
LABEL=verify-main

need_val() {
  if [ $# -lt 2 ] || [ -z "$2" ] || [[ "$2" == --* ]]; then die "$1 には値が要ります" 2; fi
}
need_int() {
  [[ "$2" =~ ^[0-9]{1,6}$ ]] || die "$1 は 0 以上の整数です: $2" 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --all)            MODE=all ;;
    --specs)          need_val "$@"; MODE=specs; SPECS_ARG=$2; shift ;;
    --list)           ACTION=list ;;
    --keep-up)        KEEP_UP=1 ;;
    --no-e2e)         NO_E2E=1 ;;
    --keep-db)        KEEP_DB=1 ;;
    --replace)        REPLACE=1 ;;
    --restart-every)  need_val "$@"; need_int "$1" "$2"; RESTART_EVERY=$((10#$2)); shift ;;
    --min-free-mb)    need_val "$@"; need_int "$1" "$2"; MIN_FREE_MB=$((10#$2)); shift ;;
    --spec-timeout)   need_val "$@"; need_int "$1" "$2"; SPEC_TIMEOUT_MIN=$((10#$2)); shift ;;
    --no-prepare)     PREPARE=0 ;;
    --shots)          SHOTS=1 ;;
    --verbose)        VERBOSE=1 ;;
    --db)             need_val "$@"; DB_NAME=$2; shift ;;
    --store)          need_val "$@"; STORE=$2; shift ;;
    --down)           ACTION=down ;;
    --status)         ACTION=status ;;
    -h|--help)        awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *)                die "未知の引数: $1（使い方は --help）" 2 ;;
  esac
  shift
done

[ "$SPEC_TIMEOUT_MIN" -gt 0 ] || die "--spec-timeout は 1 分以上です" 2
if [ "$NO_E2E" = 1 ] && [ "$KEEP_UP" = 0 ] && [ "$ACTION" = run ]; then
  die "--no-e2e は --keep-up と一緒に使います（起動だけして残す）。" 2
fi

# 開発の DB「shiki」・ストア「shiki」と、compose のほかの DB（openfga・keycloak など）を、決して作り直さない。
# 名前を確かめ用の接頭辞に限る（dev-up.sh の --reset-db はこの DB を DROP する）。
if [ "$DB_NAME" = shiki ] || [ "$STORE" = shiki ]; then
  die "開発の DB・ストア「shiki」は使いません（作り直すので）。確かめ用の名前（既定 shiki_verify・shiki-verify）にしてください。" 2
fi
[[ "$DB_NAME" =~ ^shiki_verify[a-z0-9_]{0,40}$ ]] || die "--db は shiki_verify で始まる英小文字・数字・_ の名前だけ受けます: $DB_NAME" 2
[[ "$STORE" =~ ^shiki-verify[a-z0-9-]{0,40}$ ]] || die "--store は shiki-verify で始まる英小文字・数字・- の名前だけ受けます: $STORE" 2
[[ "$STORE" == *- ]] && die "--store の最後は英小文字か数字にしてください（バケットの名前にも使う）: $STORE" 2
BUCKET=$STORE

# --- チェックアウト -----------------------------------------------------------------------------------------
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null) || die "git のチェックアウトの中の scripts/verify-main.sh を実行してください。" 2
DEVUP="$ROOT/.claude/skills/pr/scripts/dev-up.sh"
E2E_DIR="$ROOT/web/e2e"
[ -f "$DEVUP" ] || die "dev-up.sh がありません: $DEVUP" 2
[ -d "$E2E_DIR" ] || die "web/e2e がありません: $E2E_DIR" 2
# 確かめ用の DB・ストア・ラベルを渡す引数が要る（無い dev-up.sh は開発の DB「shiki」で起動してしまう）。
for opt in --db-name --store-name --bucket --redis-db --run-label --server-bin --reset-db; do
  grep -q -- "${opt})" "$DEVUP" || die "この dev-up.sh は ${opt} を受けません（古いチェックアウト）。確かめ用の DB を分けられないので止めます。" 2
done

STATE_BASE="$HOME/.local/state/shiki-verify"
# dev-up.sh の PID ファイルとログの置き場（${TMPDIR}/shiki-dev-up-…）を、tmpfs の /tmp でなくディスクに置く
# （サーバーのログは debug で太る）。dev-up.sh には毎回同じ TMPDIR を渡す（--down が同じ置き場を読む）。
DEVUP_TMP="$STATE_BASE/tmp"
RUNDIR="$DEVUP_TMP/shiki-dev-up-$(id -u)-$(printf '%s' "$ROOT" | cksum | cut -d' ' -f1)-$LABEL"

devup() { mkdir -p "$DEVUP_TMP" && (cd "$ROOT" && TMPDIR="$DEVUP_TMP" bash "$DEVUP" "$@"); }
stop_stack() { devup --down --run-label "$LABEL" >>"${STACK_LOG:-/dev/null}" 2>&1 || true; }

case "$ACTION" in
  down)   devup --down --run-label "$LABEL"; exit $? ;;
  status) devup --status --run-label "$LABEL"; exit $? ;;
esac

# --- 流す spec ----------------------------------------------------------------------------------------------
# 理由は DEFAULT_SPECS・OPTIONAL_SPECS の行のコメントから読む（コメントを正にして二重に書かない）。
declare -A REASON=()
while IFS= read -r line; do
  if [[ "$line" =~ ^[[:space:]]+([A-Za-z0-9_.-]+\.spec\.ts)[[:space:]]+#[[:space:]]?(.*)$ ]]; then
    REASON[${BASH_REMATCH[1]}]=${BASH_REMATCH[2]}
  fi
done < "${BASH_SOURCE[0]}"

SPECS=()
MISSING_OPTIONAL=()
declare -A SEEN=()
add_spec() { [ -n "${SEEN[$1]:-}" ] && return 0; SEEN[$1]=1; SPECS+=("$1"); }
valid_name() { [[ "$1" =~ ^[A-Za-z0-9_.-]+\.spec\.ts$ ]]; }

case "$MODE" in
  all)
    [ -f "$E2E_DIR/auth.spec.ts" ] && add_spec auth.spec.ts
    while IFS= read -r f; do add_spec "$f"; done < <(
      find "$E2E_DIR" -maxdepth 1 -type f -name '*.spec.ts' ! -name '*.manual.spec.ts' -printf '%f\n' | LC_ALL=C sort)
    ;;
  specs)
    read -r -a want <<< "$SPECS_ARG"
    [ "${#want[@]}" -gt 0 ] || die "--specs に spec がありません" 2
    for s in "${want[@]}"; do
      s=${s##*/}
      [[ "$s" == *.spec.ts ]] || s="$s.spec.ts"
      valid_name "$s" || die "spec の名前に使えない文字があります: $s" 2
      [ -f "$E2E_DIR/$s" ] || die "web/e2e にありません: $s" 2
      add_spec "$s"
    done
    ;;
  default)
    for s in "${DEFAULT_SPECS[@]}"; do
      [ -f "$E2E_DIR/$s" ] || die "既定の抜粋の spec が web/e2e にありません: $s（改名・削除されたなら DEFAULT_SPECS を直す）" 2
      add_spec "$s"
    done
    for s in "${OPTIONAL_SPECS[@]}"; do
      if [ -f "$E2E_DIR/$s" ]; then add_spec "$s"; else MISSING_OPTIONAL+=("$s"); fi
    done
    ;;
esac
[ "${#SPECS[@]}" -gt 0 ] || die "流す spec がありません" 2

if [ "$ACTION" = list ]; then
  say "流す spec（${#SPECS[@]} 本・この順に流す。チェックアウト: $ROOT）"
  for i in "${!SPECS[@]}"; do
    printf '%3d  %-34s %s\n' "$((i + 1))" "${SPECS[$i]}" "${REASON[${SPECS[$i]}]:-}"
  done
  for s in "${MISSING_OPTIONAL[@]}"; do
    printf '  -  %-34s %s\n' "$s" "（この作業ツリーに無いので飛ばす）${REASON[$s]:-}"
  done
  exit 0
fi

# --- 前提の確かめ -------------------------------------------------------------------------------------------
for c in docker curl git cargo pnpm node gzip; do
  command -v "$c" >/dev/null 2>&1 || die "$c が見つかりません（前提は --help）。" 2
done
docker compose version >/dev/null 2>&1 || die "docker compose（v2）が使えません。" 2

mem_avail_mb() { awk '/^MemAvailable:/ { printf "%d", $2 / 1024; exit }' /proc/meminfo 2>/dev/null || echo 0; }
disk_free_gb() { df -Pk "$ROOT" 2>/dev/null | awk 'NR == 2 { printf "%d", $4 / 1024 / 1024 }'; }

if [ "$NO_E2E" = 0 ]; then
  pw_base=${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}
  if [ "$pw_base" != 0 ] && ! compgen -G "$pw_base/chromium*-*" >/dev/null; then
    die "Playwright の Chromium が見つかりません（$pw_base）。入れる: cd web && pnpm install --frozen-lockfile && pnpm exec playwright install chromium" 2
  fi
fi

# 同時に 2 つ流すと、同じポートと同じ DB を取り合う。PID のファイルで止める（fd の flock は、背後で動き続ける
# サーバーに受け継がれてしまうので使わない）。
mkdir -p "$STATE_BASE"
PIDFILE="$STATE_BASE/.running"
if [ -f "$PIDFILE" ]; then
  other=$(cat "$PIDFILE" 2>/dev/null || true)
  if [ -n "$other" ] && kill -0 "$other" 2>/dev/null && grep -q verify-main "/proc/$other/cmdline" 2>/dev/null; then
    die "ほかの verify-main.sh が動いています（PID $other）。終わるのを待つか、止めてから流してください。"
  fi
fi
OWN_PIDFILE=0

# --- 結果の置き場 -------------------------------------------------------------------------------------------
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="$STATE_BASE/$STAMP"
mkdir -p "$OUT/logs" "$OUT/json" "$OUT/details" "$OUT/artifacts" "$OUT/stack"
ln -sfn "$OUT" "$STATE_BASE/latest"
STACK_LOG="$OUT/stack/dev-up.log"
RESULTS="$OUT/results.tsv"
printf 'idx\tspec\tphase\tstatus\tpassed\tfailed\tskipped\tsecs\tlog\n' > "$RESULTS"

# --- LLM は stub だけ ---------------------------------------------------------------------------------------
# figment は環境変数の名前の大文字・小文字を区別しないので、SHIKI__LLM で始まるものを大小を問わず消す。
while IFS= read -r v; do
  [[ "${v^^}" == SHIKI__LLM* ]] && unset "$v"
done < <(compgen -e)
export SHIKI__LLM__BACKEND=stub
unset E2E_ALLOW_REAL_LLM_DEFAULT
STUB_TOML="$HOME/.config/shiki-dev/llm-stub.toml"
if [ -z "${SHIKI_CONFIG:-}" ] && [ -f "$STUB_TOML" ]; then
  SHIKI_CONFIG="$STUB_TOML"
fi
if [ -n "${SHIKI_CONFIG:-}" ]; then
  [ -f "$SHIKI_CONFIG" ] || die "SHIKI_CONFIG のファイルがありません: $SHIKI_CONFIG" 2
  export SHIKI_CONFIG
  if [ "$(basename "$SHIKI_CONFIG")" != llm-stub.toml ]; then
    err "⚠ SHIKI_CONFIG=$SHIKI_CONFIG を使います。組織の既定のモデルが stub でなければ、E2E の global-setup がテストの前に止めます。"
  fi
else
  unset SHIKI_CONFIG
fi

# CSV の集計の隔離ランナー: 呼び出し側の指定が無く、release のランナーが無くて debug のランナーがあれば、debug を使う
# （dev-up.sh は release を初回 30 分以上かけてビルドする）。
RELEASE_RUNNER="$ROOT/crates/tabular/runner/target/release/shiki-tabular-runner"
DEBUG_RUNNER="$ROOT/crates/tabular/runner/target/debug/shiki-tabular-runner"
if [ -z "${SHIKI__TABULAR__RUNNER_PATH:-}" ] && [ ! -x "$RELEASE_RUNNER" ] && [ -x "$DEBUG_RUNNER" ]; then
  export SHIKI__TABULAR__RUNNER_PATH="$DEBUG_RUNNER"
  say "CSV の隔離ランナーは debug のものを使います: $DEBUG_RUNNER"
fi
# 起動し直すときはビルドを飛ばす（--server-bin）。そのとき使うランナー。
RESTART_RUNNER=${SHIKI__TABULAR__RUNNER_PATH:-$RELEASE_RUNNER}
SERVER_EXE="$ROOT/target/debug/shiki-server"

# --- 状態 ---------------------------------------------------------------------------------------------------
STACK_STARTED=0      # dev-up.sh を呼んだ（途中で落ちても止める対象）
STACK_READY=0        # 起動しきった
E2E_STARTED=0
INTERRUPTED=0
ABORT_REASON=""
RESTARTS=0
SINCE_RESTART=0
CURRENT=""
RUN_START=$(date +%s)
FGA_URL=http://localhost:8082
declare -a R1=() R1C=() R1T=() R1TAG=() R2=() R2C=() R2T=() R2TAG=()

fmt_dur() {
  local s=$1
  if [ "$s" -ge 3600 ]; then printf '%d時間%02d分' $((s / 3600)) $((s % 3600 / 60))
  elif [ "$s" -ge 60 ]; then printf '%d分%02d秒' $((s / 60)) $((s % 60))
  else printf '%d秒' "$s"; fi
}

# 作業のための cargo の錠（手元のキャンペーンの決まり: ビルドを 1 本ずつにし、古い成果物の掃除と重ねない）。
# 錠のファイルが無い環境ではそのまま流す。-o で、背後で動き続けるサーバーに錠の fd を渡さない。
CARGO_LOCK=${SHIKI_CARGO_LOCK:-$HOME/.local/state/shiki-campaign/cargo.lock}
with_cargo_lock() {
  if [ -e "$CARGO_LOCK" ] && command -v flock >/dev/null 2>&1; then
    flock -n "$CARGO_LOCK" true 2>/dev/null || say "   （ほかの cargo が錠を持っています。空くまで待ちます: $CARGO_LOCK）"
    flock -o "$CARGO_LOCK" "$@"
  else
    "$@"
  fi
}

save_stack_logs() {  # save_stack_logs <印>: サーバーと web のログを残す（起動し直すと dev-up.sh が空にする）
  local f
  for f in shiki-server web; do
    [ -s "$RUNDIR/$f.log" ] && gzip -c "$RUNDIR/$f.log" > "$OUT/stack/$f-$1.log.gz" 2>/dev/null
  done
  return 0
}

ours_running() {
  local f p
  for f in "$RUNDIR/web.pid" "$RUNDIR/shiki-server.pid"; do
    [ -f "$f" ] || continue
    p=$(cat "$f" 2>/dev/null || true)
    [ -n "$p" ] && kill -0 "$p" 2>/dev/null && return 0
  done
  return 1
}
answers() { curl -s -o /dev/null --max-time 2 "$1" 2>/dev/null; }
# ポートで待ち受けているか（next dev は初めての要求に数秒かかるので、HTTP の応答では見ない）。
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

print_access() {
  say ""
  say "==== 触って確かめる（スタックは動いたまま） ===="
  say "  画面       http://localhost:3000   （/login から入る。前のログインが残っていたら、いったんログアウトする）"
  say "  アカウント（パスワードはどれも password）"
  say "    alice    a-corp の組織の管理者・監査ログも見られる"
  say "    bob      a-corp の一般のメンバー（alice と同じ部署「営業部」）"
  say "    charlie  別の組織 b-corp・管理者でない監査ログの閲覧者"
  say "  Keycloak   http://localhost:8081      API  http://localhost:8080/healthz"
  say "  DB ${DB_NAME}・ストア ${STORE}・バケット ${BUCKET}（開発の DB「shiki」とは別。E2E が作ったものが残っていることがある）"
  say "  LLM        stub（決まった応答を返す）。実モデルが要る確かめは、この台本では立てない（台帳 §9.2 の手順）"
  say "  ログ       $RUNDIR/shiki-server.log"
  say "             $RUNDIR/web.log"
  say "  止める     bash scripts/verify-main.sh --down"
}

# --- スタック -----------------------------------------------------------------------------------------------
fga_port() {
  local hp
  hp=$(cd "$ROOT/deploy/compose" && docker compose port openfga 8080 2>/dev/null) || hp=""
  if [ -n "$hp" ]; then printf '%s' "${hp##*:}"; else printf '8082'; fi
}

read -r -d '' FGA_LIST_JS <<'JS' || true
let s = "";
process.stdin.on("data", (d) => (s += d)).on("end", () => {
  let b;
  try {
    b = JSON.parse(s);
  } catch {
    console.log("BAD");
    return;
  }
  for (const st of b.stores || []) if (st.name === process.argv[1]) console.log("ID " + st.id);
  console.log("TOKEN " + (b.continuation_token || ""));
});
JS

# 前の回のストアを消す（同じ名前のストアに前の DB のタプルが残っていると、新しい DB のログインの同期が台帳を
# 書かずに落ちることがある・BUG-270）。OpenFGA は消したストアを一覧に出さず、サーバーが起動のときに作り直す。
reset_fga_store() {
  local url body ids=() token="" line n=0 page=()
  url="http://localhost:$(fga_port)"
  if ! answers "$url/healthz"; then
    say "   OpenFGA を起動します"
    ( cd "$ROOT/deploy/compose" && { [ -f .env ] || cp .env.example .env; } && docker compose up -d openfga ) >>"$STACK_LOG" 2>&1 \
      || { err "OpenFGA を起動できません（$STACK_LOG）"; return 1; }
    url="http://localhost:$(fga_port)"
    for _ in $(seq 1 60); do answers "$url/healthz" && break; sleep 2; done
    answers "$url/healthz" || { err "OpenFGA が応答しません: $url"; return 1; }
  fi
  while :; do
    page=(--data page_size=100)
    [ -n "$token" ] && page+=(--data-urlencode "continuation_token=$token")
    body=$(curl -fsS --max-time 15 -G "$url/stores" "${page[@]}") \
      || { err "OpenFGA のストアの一覧を読めません: $url"; return 1; }
    token=""
    while IFS= read -r line; do
      case "$line" in
        "ID "*) ids+=("${line#ID }") ;;
        "TOKEN "*) token=${line#TOKEN } ;;
        BAD) err "OpenFGA のストアの一覧が読めない形でした: $url/stores"; return 1 ;;
      esac
    done < <(printf '%s' "$body" | node -e "$FGA_LIST_JS" "$STORE")
    [ -n "$token" ] || break
    n=$((n + 1)); [ "$n" -lt 1000 ] || break
  done
  for id in "${ids[@]}"; do
    curl -fsS --max-time 15 -X DELETE "$url/stores/$id" >/dev/null || { err "OpenFGA のストア $id を消せません"; return 1; }
  done
  if [ "${#ids[@]}" -gt 0 ]; then say "   前の回の OpenFGA のストア '$STORE' を消しました（${#ids[@]} 個）"; fi
  return 0
}

start_stack() {  # start_stack first|restart
  local args=(--db-name "$DB_NAME" --store-name "$STORE" --bucket "$BUCKET" --redis-db "$REDIS_DB" --run-label "$LABEL") rc
  STACK_STARTED=1
  STACK_READY=0
  if [ "$1" = first ]; then
    [ "$KEEP_DB" = 1 ] || args+=(--reset-db)
    say "== スタックを起動します（dev-up.sh・DB ${DB_NAME}・ストア ${STORE}） =="
    with_cargo_lock env TMPDIR="$DEVUP_TMP" bash -c 'cd "$1" && shift && exec bash "$@"' _ "$ROOT" "$DEVUP" "${args[@]}" 2>&1 | tee -a "$STACK_LOG"
    rc=${PIPESTATUS[0]}
  else
    # 起動し直しはビルドを飛ばす（同じバイナリで起動し直す・作業ツリーのほかの変更を途中で拾わない）。
    if [ -x "$SERVER_EXE" ] && [ -x "$RESTART_RUNNER" ] && [[ "$SERVER_EXE" =~ ^[A-Za-z0-9._/+-]+$ ]]; then
      args+=(--server-bin "$SERVER_EXE")
      SHIKI__TABULAR__RUNNER_PATH="$RESTART_RUNNER" devup "${args[@]}" >>"$STACK_LOG" 2>&1
    else
      with_cargo_lock env TMPDIR="$DEVUP_TMP" bash -c 'cd "$1" && shift && exec bash "$@"' _ "$ROOT" "$DEVUP" "${args[@]}" >>"$STACK_LOG" 2>&1
    fi
    rc=$?
  fi
  if [ "$rc" != 0 ]; then
    err "スタックを起動できませんでした（ログ: $STACK_LOG）"
    tail -15 "$STACK_LOG" >&2
    return 1
  fi
  FGA_URL="http://localhost:$(fga_port)"
  STACK_READY=1
  return 0
}

restart_stack() {  # restart_stack <理由>
  RESTARTS=$((RESTARTS + 1))
  say "   == スタックを起動し直します（$1） =="
  save_stack_logs "$RESTARTS"
  stop_stack
  local t0=$SECONDS
  start_stack restart || return 1
  SINCE_RESTART=0
  say "   == 起動し直しました（$(fmt_dur $((SECONDS - t0)))・空きメモリ $(mem_avail_mb)MB） =="
}

maybe_restart() {
  local why="" avail
  if [ "$RESTART_EVERY" -gt 0 ] && [ "$SINCE_RESTART" -ge "$RESTART_EVERY" ]; then
    why="${RESTART_EVERY} 本ごと"
  elif [ "$SINCE_RESTART" -gt 0 ]; then
    avail=$(mem_avail_mb)
    [ "$avail" -lt "$MIN_FREE_MB" ] && why="空きメモリが ${avail}MB で ${MIN_FREE_MB}MB を切った"
  fi
  [ -n "$why" ] || return 0
  restart_stack "$why"
}

warn_openfga_memory() {
  local id mem name
  id=$(cd "$ROOT/deploy/compose" && docker compose ps -q openfga 2>/dev/null) || return 0
  [ -n "$id" ] || return 0
  mem=$(docker stats --no-stream --format '{{.MemUsage}}' "$id" 2>/dev/null | awk '{ print $1 }')
  case "$mem" in
    *GiB)
      if awk -v g="${mem%GiB}" 'BEGIN { exit !(g >= 2.0) }'; then
        name=$(docker inspect -f '{{.Name}}' "$id" 2>/dev/null | tr -d /)
        err "⚠ OpenFGA が ${mem} 使っています（長く動かすと太る・BUG-165）。長く流す前に戻すなら: docker restart ${name}"
        err "  （データは Postgres にあるので消えない。同じ OpenFGA を使うほかのセッションは数秒つながらなくなる）"
      fi
      ;;
  esac
  return 0
}

# --- E2E ----------------------------------------------------------------------------------------------------
# Playwright の JSON の報告から、件数・落ちたテスト・飛ばした理由・前提の誤りを TSV で出す。
read -r -d '' PW_PARSE_JS <<'JS' || true
const fs = require("fs");
const clean = (s) => String(s ?? "").replace(/\u001b\[[0-9;]*[A-Za-z]/g, "").replace(/[\t\r]/g, " ");
const first = (s) => clean(s).split("\n").map((x) => x.trim()).find((x) => x) || "";
let r;
try {
  r = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
} catch {
  console.log("NOJSON");
  process.exit(0);
}
const st = r.stats || {};
console.log(["STATS", st.expected | 0, st.unexpected | 0, st.skipped | 0, st.flaky | 0].join("\t"));
for (const e of r.errors || []) console.log("ERR\t" + first(e.message || e.value || JSON.stringify(e)).slice(0, 300));
const walk = (s) => {
  for (const sp of s.specs || [])
    for (const t of sp.tests || []) {
      const title = clean(sp.title).replace(/\n/g, " ");
      if (t.status === "unexpected" || t.status === "flaky") {
        const res = (t.results || []).slice(-1)[0] || {};
        const msg = first((res.error && res.error.message) || ((res.errors || [])[0] || {}).message);
        console.log(["FAIL", `${sp.file}:${sp.line}`, title, msg.slice(0, 300)].join("\t"));
        for (const a of res.attachments || []) if (a.name === "trace" && a.path) console.log("TRACE\t" + a.path);
      } else if (t.status === "skipped") {
        const a = (t.annotations || []).find((x) => x.type === "skip" || x.type === "fixme");
        console.log(["SKIP", title, clean((a && a.description) || "")].join("\t"));
      }
    }
  for (const c of s.suites || []) walk(c);
};
for (const s of r.suites || []) walk(s);
JS

E2E_ENV=(
  E2E_BASE_URL=http://localhost:3000
  E2E_OPENFGA_STORE="$STORE"
  # native の B1 のゲートウェイは 18091（compose の 8091 と衝突させない・dev-up.sh）。
  E2E_B1_ORIGIN=http://localhost:18091
)
[ "$SHOTS" = 1 ] && E2E_ENV+=(E2E_SHOTS_DIR="$OUT/shots")

# run_one <first|rerun> <i>: 1 本流して R1／R2 に結果を入れる。
run_one() {
  local phase=$1 i=$2 spec n tag base log json art det rc t0 secs ok ng sk st label total pattern shown=0
  spec=${SPECS[$i]}
  n=$(printf '%02d' "$((i + 1))")
  base=${spec%.spec.ts}
  tag=$n-$base
  [ "$phase" = rerun ] && tag=rerun-$tag
  log="$OUT/logs/$tag.log"
  json="$OUT/json/$tag.json"
  art="$OUT/artifacts/$tag"
  det="$OUT/details/$tag.tsv"
  # Playwright の引数はファイルのパスへの正規表現（CI の ci-select.py と同じ形: . を逃がして末尾に $）。
  pattern="e2e/${spec//./\\.}\$"
  CURRENT=$spec
  t0=$(date +%s)
  local cmd=(env -u CI -u E2E_ALLOW_REAL_LLM_DEFAULT "${E2E_ENV[@]}" E2E_OPENFGA_URL="$FGA_URL"
    PLAYWRIGHT_JSON_OUTPUT_FILE="$json"
    pnpm exec playwright test "$pattern" --workers=1 --retries=0 --reporter=list,json
    --output="$art" --trace=retain-on-failure --global-timeout="$((SPEC_TIMEOUT_MIN * 60000))")
  if [ "$VERBOSE" = 1 ]; then
    (cd "$ROOT/web" && "${cmd[@]}") 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
  else
    (cd "$ROOT/web" && "${cmd[@]}") >"$log" 2>&1
    rc=$?
  fi
  secs=$(($(date +%s) - t0))
  SINCE_RESTART=$((SINCE_RESTART + 1))
  node -e "$PW_PARSE_JS" "$json" >"$det" 2>/dev/null || echo NOJSON >"$det"
  ok=0 ng=0 sk=0
  IFS=$'\t' read -r _ ok ng sk _ < <(grep -m1 $'^STATS\t' "$det") || true
  ok=${ok:-0} ng=${ng:-0} sk=${sk:-0}
  if [ "$rc" = 0 ]; then
    if [ "${ok:-0}" = 0 ] && [ "${sk:-0}" != 0 ]; then st=skipall; else st=pass; fi
  else
    st=fail
  fi
  if [ "$phase" = first ]; then R1[$i]=$st; R1C[$i]="$ok $ng $sk"; R1T[$i]=$secs; R1TAG[$i]=$tag
  else R2[$i]=$st; R2C[$i]="$ok $ng $sk"; R2T[$i]=$secs; R2TAG[$i]=$tag; fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$((i + 1))" "$spec" "$phase" "$st" "$ok" "$ng" "$sk" "$secs" "$log" >> "$RESULTS"
  total=$((ok + ng + sk))
  case "$st" in
    pass)    label="通った ${ok}/${total}"; [ "$sk" != 0 ] && label="$label（${sk} 件は飛ばした）" ;;
    skipall)
      label=$(awk -F'\t' '$1 == "SKIP" && $3 != "" { print $3; exit }' "$det")
      label="全部飛ばした${label:+（$label）}"
      ;;
    fail)    label="落ちた（通った ${ok}・落ちた ${ng}・飛ばした ${sk}・終わりのコード ${rc}）" ;;
  esac
  say "        → ${label}（$(fmt_dur "$secs")）"
  if [ "$st" = fail ]; then
    while IFS=$'\t' read -r kind a b c; do
      case "$kind" in
        FAIL) say "          ✗ $a $b"; [ -n "$c" ] && say "            $c"; shown=$((shown + 1)) ;;
        ERR)  say "          ✗ $a"; shown=$((shown + 1)) ;;
        NOJSON) say "          ✗ 報告（JSON）がありません。ログを見る: $log"; shown=$((shown + 1)) ;;
      esac
      [ "$shown" -ge 3 ] && break
    done < "$det"
    say "          ログ: $log"
  fi
  CURRENT=""
}

final_label() {  # final_label <i>
  local i=$1 sk
  case "${R1[$i]:-}" in
    pass)    read -r _ _ sk <<< "${R1C[$i]}"; if [ "$sk" != 0 ]; then echo "通った（一部飛ばした）"; else echo "通った"; fi ;;
    skipall) echo "全部飛ばした" ;;
    fail)
      case "${R2[$i]:-}" in
        pass|skipall) echo "流し直すと通った（揺れの疑い）" ;;
        fail) if [ -n "${KNOWN_DEV_FAIL[${SPECS[$i]}]:-}" ]; then echo "既知（next dev では落ちる）"; else echo "続けて落ちた"; fi ;;
        *) echo "落ちた（流し直していない）" ;;
      esac
      ;;
    *) if [ "$CURRENT" = "${SPECS[$i]}" ]; then echo "中断した"; else echo "流していない"; fi ;;
  esac
}

counts_cell() {  # counts_cell "<ok> <ng> <sk>"
  local ok ng sk
  read -r ok ng sk <<< "$1"
  printf '%s / %s / %s' "$ok" "$ng" "$sk"
}

write_summary() {
  local f="$OUT/summary.md" i label c_pass=0 c_flaky=0 c_fail=0 c_known=0 c_skip=0 c_int=0 c_rest=0 rerun spec det kind a b c reason
  local -a persist=() flaky=() skipall=()
  for i in "${!SPECS[@]}"; do
    label=$(final_label "$i")
    case "$label" in
      通った*) c_pass=$((c_pass + 1)) ;;
      流し直すと通った*) c_flaky=$((c_flaky + 1)); flaky+=("$i") ;;
      続けて落ちた|落ちた*) c_fail=$((c_fail + 1)); persist+=("$i") ;;
      既知*) c_known=$((c_known + 1)) ;;
      全部飛ばした) c_skip=$((c_skip + 1)); skipall+=("$i") ;;
      中断した) c_int=$((c_int + 1)) ;;
      *) c_rest=$((c_rest + 1)) ;;
    esac
  done
  {
    echo "# main の確かめ（$(date '+%Y-%m-%d %H:%M')）"
    echo
    echo "- チェックアウト: \`$ROOT\`（ブランチ ${BRANCH:-?}・${HEAD_SHORT:-?}${DIRTY_NOTE:-}・origin/main を含む: ${CONTAINS_MAIN:-?}）"
    echo "- スタック: native（dev-up.sh）・DB \`$DB_NAME\`・ストア \`$STORE\`・バケット \`$BUCKET\`・LLM stub（SHIKI_CONFIG=${SHIKI_CONFIG:-無し}）"
    echo "- 流した時間: $(fmt_dur $(($(date +%s) - RUN_START)))・起動し直し ${RESTARTS} 回（${RESTART_EVERY} 本ごと・空きメモリ ${MIN_FREE_MB}MB 未満で）"
    echo "- 置き場: \`$OUT\`"
    [ -n "$ABORT_REASON" ] && echo "- **途中で止めた:** $ABORT_REASON"
    [ "$INTERRUPTED" = 1 ] && echo "- **途中で中断した（Ctrl-C など）。**"
    echo
    echo "## 要約"
    echo
    echo "${#SPECS[@]} 本のうち、通った ${c_pass}・流し直すと通った（揺れの疑い） ${c_flaky}・続けて落ちた ${c_fail}・既知 ${c_known}・全部飛ばした ${c_skip}・流していない ${c_rest}$([ "$c_int" != 0 ] && echo "・中断した ${c_int}")。"
    if [ "${#MISSING_OPTIONAL[@]}" -gt 0 ]; then
      echo
      echo "この作業ツリーに無いので飛ばした spec: ${MISSING_OPTIONAL[*]}"
    fi
    echo
    echo "| # | spec | 結果 | 通った / 落ちた / 飛ばした | 時間 | 流し直し |"
    echo "| ---: | --- | --- | --- | ---: | --- |"
    for i in "${!SPECS[@]}"; do
      rerun=""
      if [ -n "${R2[$i]:-}" ]; then
        case "${R2[$i]}" in pass|skipall) rerun="通った" ;; *) rerun="落ちた" ;; esac
        rerun="$rerun $(counts_cell "${R2C[$i]}")（$(fmt_dur "${R2T[$i]}")）"
      fi
      printf '| %d | %s | %s | %s | %s | %s |\n' "$((i + 1))" "${SPECS[$i]}" "$(final_label "$i")" \
        "$([ -n "${R1C[$i]:-}" ] && counts_cell "${R1C[$i]}")" "$([ -n "${R1T[$i]:-}" ] && fmt_dur "${R1T[$i]}")" "$rerun"
    done
    if [ "${#persist[@]}" -gt 0 ]; then
      echo
      echo "## 続けて落ちた"
      for i in "${persist[@]}"; do
        spec=${SPECS[$i]}
        det="$OUT/details/${R2TAG[$i]:-${R1TAG[$i]}}.tsv"
        echo
        echo "### ${spec}"
        echo
        while IFS=$'\t' read -r kind a b c; do
          case "$kind" in
            FAIL) echo "- ✗ \`$a\` $b"; [ -n "$c" ] && echo "  - $c" ;;
            ERR) echo "- ✗ $a" ;;
            TRACE) echo "  - トレース: \`cd web && pnpm exec playwright show-trace $a\`" ;;
            NOJSON) echo "- 報告（JSON）がありません" ;;
          esac
        done < "$det"
        echo "- ログ: \`$OUT/logs/${R2TAG[$i]:-${R1TAG[$i]}}.log\`"
      done
    fi
    if [ "${#flaky[@]}" -gt 0 ]; then
      echo
      echo "## 流し直すと通った（揺れの疑い）"
      echo
      for i in "${flaky[@]}"; do
        echo "- ${SPECS[$i]}（1 回目のログ: \`$OUT/logs/${R1TAG[$i]}.log\`）"
        while IFS=$'\t' read -r kind a b c; do
          [ "$kind" = FAIL ] && echo "  - 1 回目に落ちたテスト: \`$a\` $b"
        done < "$OUT/details/${R1TAG[$i]}.tsv"
      done
    fi
    if [ "${#skipall[@]}" -gt 0 ]; then
      echo
      echo "## 全部飛ばした（この環境では確かめていない）"
      echo
      for i in "${skipall[@]}"; do
        reason=$(awk -F'\t' '$1 == "SKIP" && $3 != "" { print $3; exit }' "$OUT/details/${R1TAG[$i]}.tsv")
        echo "- ${SPECS[$i]}${reason:+: $reason}"
      done
    fi
    for i in "${!SPECS[@]}"; do
      if [ "$(final_label "$i")" = "既知（next dev では落ちる）" ]; then
        echo
        echo "- 既知: ${SPECS[$i]} は ${KNOWN_DEV_FAIL[${SPECS[$i]}]}。"
      fi
    done
  } > "$f"
  say ""
  cat "$f"
  say ""
  say "まとめ: $f"
}

# --- 終わりと中断 -------------------------------------------------------------------------------------------
on_exit() {
  local rc=$?
  trap - EXIT
  if [ "$STACK_STARTED" = 1 ] && { [ "$KEEP_UP" = 0 ] || [ "$STACK_READY" = 0 ]; }; then
    save_stack_logs final
    say "== スタックを止めます =="
    stop_stack
  fi
  [ "$OWN_PIDFILE" = 1 ] && rm -f "$PIDFILE"
  exit "$rc"
}
on_int() {
  trap - INT TERM
  INTERRUPTED=1
  say ""
  say "== 中断しました（Ctrl-C など） =="
  [ "$E2E_STARTED" = 1 ] && write_summary
  if [ "$KEEP_UP" = 1 ] && [ "$STACK_READY" = 1 ]; then print_access; fi
  exit 130
}
trap on_exit EXIT
trap on_int INT TERM
echo $$ > "$PIDFILE"
OWN_PIDFILE=1

# --- 流す ---------------------------------------------------------------------------------------------------
BRANCH=$(git -C "$ROOT" branch --show-current 2>/dev/null || true)
HEAD_SHORT=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || true)
dirty=$(git -C "$ROOT" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
DIRTY_NOTE=""
[ "${dirty:-0}" != 0 ] && DIRTY_NOTE="・未コミットの変更 ${dirty} ファイル"
if ! git -C "$ROOT" rev-parse --verify -q origin/main >/dev/null; then CONTAINS_MAIN="（origin/main が無い）"
elif git -C "$ROOT" merge-base --is-ancestor origin/main HEAD 2>/dev/null; then CONTAINS_MAIN="はい"
else CONTAINS_MAIN="いいえ"; fi
{
  echo "checkout=$ROOT"
  echo "branch=$BRANCH"
  echo "head=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null)"
  echo "dirty_files=$dirty"
  echo "contains_origin_main=$CONTAINS_MAIN"
  echo "db=$DB_NAME store=$STORE bucket=$BUCKET redis_db=$REDIS_DB run_label=$LABEL keep_db=$KEEP_DB"
  echo "shiki_config=${SHIKI_CONFIG:-}"
  echo "tabular_runner=${SHIKI__TABULAR__RUNNER_PATH:-（dev-up.sh の既定: release をビルド）}"
  echo "mode=$MODE specs=${#SPECS[@]} restart_every=$RESTART_EVERY min_free_mb=$MIN_FREE_MB spec_timeout_min=$SPEC_TIMEOUT_MIN"
  echo "started=$(date -Is)"
} > "$OUT/env.txt"

say "== main の確かめ =="
say "   チェックアウト $ROOT（${BRANCH:-?}・${HEAD_SHORT:-?}${DIRTY_NOTE}・origin/main を含む: $CONTAINS_MAIN）"
say "   spec ${#SPECS[@]} 本$([ "$NO_E2E" = 1 ] && echo "（--no-e2e なので流さない）")・結果の置き場 $OUT"
[ "${#MISSING_OPTIONAL[@]}" -gt 0 ] && say "   この作業ツリーに無いので飛ばす: ${MISSING_OPTIONAL[*]}"
say "   LLM stub（SHIKI_CONFIG=${SHIKI_CONFIG:-無し}）"
avail=$(mem_avail_mb)
[ "$avail" -lt 4096 ] && err "⚠ 空きメモリが ${avail}MB です。ビルドや E2E の途中でメモリが足りなくなることがあります。"
free_gb=$(disk_free_gb)
[ -n "$free_gb" ] && [ "$free_gb" -lt 15 ] && err "⚠ ディスクの空きが ${free_gb}GB です（新しいチェックアウトのビルドには 40GB ほど要る）。"

# ほかのスタックが :3000・:8080 を使っていれば、黙って止めない（開発の DB「shiki」のスタックかもしれない）。
if { port_open 8080 || port_open 3000; } && ! ours_running; then
  if [ "$REPLACE" = 0 ]; then
    die "いま :3000 か :8080 で別のスタックが動いています（開発の DB「shiki」のものかもしれません）。
止めてよければ --replace を付けて流し直してください（dev-up.sh が止めます。終わった後に起動し直しはしません）。
別の作業ツリーのスタックなら、そちらで dev-up.sh --down してから流してください。"
  fi
  say "   :3000・:8080 のスタックを止めて立て直します（--replace）"
fi

# 前の回に --keep-up で残したものを止める（node_modules を入れ直す前に next dev を、DB を作り直す前に
# つないでいるサーバーを外す）。
stop_stack

if [ "$PREPARE" = 1 ]; then
  # 生成物（web/src/generated）と node_modules は、前のチェックアウトのままのことがある（dev-up.sh は無いときしか作らない）。
  say "== web の依存と生成物をそろえます（pnpm install・pnpm gen:api。cargo を使うので数分かかることがある） =="
  say "   ログ: $OUT/stack/prepare.log"
  (cd "$ROOT/web" && pnpm install --frozen-lockfile) >>"$OUT/stack/prepare.log" 2>&1 \
    || die "pnpm install に失敗しました（$OUT/stack/prepare.log）"
  with_cargo_lock bash -c 'cd "$1/web" && exec pnpm gen:api' _ "$ROOT" >>"$OUT/stack/prepare.log" 2>&1 \
    || die "pnpm gen:api に失敗しました（$OUT/stack/prepare.log）"
fi

if [ "$KEEP_DB" = 0 ]; then
  say "== 前の回の DB ${DB_NAME} とストア ${STORE} を作り直します（開発の DB「shiki」には触らない） =="
  reset_fga_store || exit 1
fi
start_stack first || exit 1
warn_openfga_memory

if [ "$NO_E2E" = 1 ]; then
  print_access
  exit 0
fi

E2E_STARTED=1
N=${#SPECS[@]}
say ""
say "== E2E を 1 本ずつ流します（${N} 本・${RESTART_EVERY} 本ごとと空きメモリが ${MIN_FREE_MB}MB を切ったときに起動し直す） =="
for i in "${!SPECS[@]}"; do
  if ! maybe_restart; then
    ABORT_REASON="スタックを起動し直せなかったので、$((i + 1)) 本目から先を流していない（$STACK_LOG）"
    break
  fi
  say "   [$((i + 1))/$N] ${SPECS[$i]}"
  run_one first "$i"
  # 最小経路が通らなければ、残りは前提の誤り（ログイン・組織の既定のモデル）で全部落ちる。すぐに 1 回だけ流し直して決める。
  if [ "$i" = 0 ] && [ "${SPECS[0]}" = auth.spec.ts ] && [ "${R1[0]}" = fail ]; then
    say "   auth.spec.ts が落ちたので、すぐに 1 回だけ流し直します"
    run_one rerun 0
    if [ "${R2[0]}" = fail ]; then
      ABORT_REASON="ログインの最小経路（auth.spec.ts）が 2 回続けて落ちたので、残りを流していない（前提の誤りは上の ✗ と $OUT/logs を見る）"
      break
    fi
  fi
done

if [ -z "$ABORT_REASON" ]; then
  RERUN=()
  for i in "${!SPECS[@]}"; do
    [ "${R1[$i]:-}" = fail ] && [ -z "${R2[$i]:-}" ] && RERUN+=("$i")
  done
  if [ "${#RERUN[@]}" -gt 0 ]; then
    say ""
    say "== 落ちた ${#RERUN[@]} 本を、もう 1 回だけ流し直します =="
    k=0
    for i in "${RERUN[@]}"; do
      k=$((k + 1))
      if ! maybe_restart; then
        ABORT_REASON="スタックを起動し直せなかったので、流し直しを ${k} 本目から先は流していない（$STACK_LOG）"
        break
      fi
      say "   [流し直し ${k}/${#RERUN[@]}] ${SPECS[$i]}"
      run_one rerun "$i"
    done
  fi
fi

find "$OUT/artifacts" -mindepth 1 -type d -empty -delete 2>/dev/null
save_stack_logs final
echo "finished=$(date -Is)" >> "$OUT/env.txt"
write_summary
[ "$KEEP_UP" = 1 ] && print_access

rc=0
[ -n "$ABORT_REASON" ] && rc=1
for i in "${!SPECS[@]}"; do
  case "$(final_label "$i")" in 続けて落ちた|落ちた*) rc=1 ;; esac
done
exit "$rc"
