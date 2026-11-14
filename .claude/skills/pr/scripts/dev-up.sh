#!/usr/bin/env bash
# dev-up.sh — 変更を目で確認できる状態まで環境を起動する。
#
#   使い方:
#     dev-up.sh [--compose] [--rag|--rag-remote] [--sandbox] [--office] [--reset-db]
#     dev-up.sh --down     # 起動したものを止める
#     dev-up.sh --status   # 生存確認だけする
#
#   --reset-db: compose の dev DB 'shiki' を DROP/CREATE してから起動する（破壊操作）。
#               別ブランチの migration が当たって checksum 不一致になった時に使う。
#
#   --rag-remote: RAG を有効にし、ingestion-worker と S3 は**外部のものを使う**（native のみ）。
#                 worker のイメージは 9.7GB・常駐 RAM 2〜3GB あり、開発機ごとに持つと重い。
#                 S3 も外部にするのは、worker が presigned URL を自分で取りに行くため
#                 （開発機の S3 は外部の worker から見えない）。接続先は環境変数で渡す:
#                   SHIKI_DEV_RAG_WORKER_URL     worker のベース URL
#                   SHIKI_DEV_S3_ENDPOINT        S3 のエンドポイント（サーバ・worker が使う）
#                   SHIKI_DEV_S3_PUBLIC_ENDPOINT ブラウザが presigned URL で叩く先（既定は上と同じ）
#                   SHIKI_DEV_S3_BUCKET          **開発環境ごとに別のバケット**にする（後述）
#                   SHIKI_DEV_S3_ACCESS_KEY / SHIKI_DEV_S3_SECRET_KEY
#                   SHIKI_DEV_RAG_WORKER_TOKEN   worker の共有トークン（worker が WORKER_AUTH_TOKEN を
#                                 設定しているなら必須・#812）。worker 側の SOURCE_URL_ALLOWED_HOSTS には
#                                 SHIKI_DEV_S3_ENDPOINT のホストが入っていること（無いと索引だけ入らない）。
#                                 トークンを渡すなら worker の URL は https かループバックに限る
#                                 （それ以外の平文 HTTP は起動前に拒否する）
#                 バケットを他の開発環境と共有しない。オブジェクトキーは
#                 {tenant_id}/{org}/{sha256} で環境を区別せず、参照カウントも各自の DB にしか
#                 無いため、別環境のテナント削除（{tenant_id}/ 以下の一括削除）が自分の blob を消す。
#                 compose の DB は docker ホストごとに 1 つなので、バケットもホストごとに分ける。
#                   SHIKI_DEV_S3_CORS_ALLOWED_ORIGINS  バケットの CORS を起動時に設定する（例 '[http://localhost:3000]'）。
#                                 未指定ならバケット側の既存の設定に任せる（外部のバケットを勝手に書き換えない）。
#                 DB と保存先は組で使う。--rag（ローカルの RustFS）で使った DB のまま切り替えると、
#                 既存の blob が読めず、同じ内容の再アップロードも重複扱いで実体が作られない。
#                 組が変わる時は止めるので、--reset-db で DB を作り直す。
#
#   既定（native モード）:
#     compose で依存だけ起動（postgres/keycloak/openfga/redis/rustfs）
#     → shiki-server を `cargo run` でホスト :8080
#     → web を `pnpm dev` で :3000
#   Rust を変更した時の反復が速い（docker イメージの再ビルドが要らない）。
#
#   --compose: shiki-server も compose で起動する（CI の web-e2e と同一構成）。
#              Rust を変えたら `docker compose up -d --build shiki-server` の再ビルドが要る。
#
#   web は必ず :3000。SHIKI__AUTH__REDIRECT_URI が :3000 前提のため
#   （:8080 にすると callback 後に backend ルートへ 303 して落ちる）。
#
#   プロセスは detach して起動し、ログのパスを表示して終了する。
#
#   版のサーバー（評価ハーネス eval/app-builder・GAP-30）向けの引数（native のみ。省けば今までどおり）:
#     --server-bin PATH    cargo build を飛ばし、このバイナリ（絶対パス）で起動する
#     --db-name NAME       DB の名前（既定 shiki）。無ければ CREATE DATABASE する。保存先の印の
#                          コメントと --reset-db もこの DB に向ける
#     --store-name NAME    OpenFGA の store の名前（既定 shiki）。無ければサーバーが作る
#     --bucket NAME        S3 のバケット（既定 shiki-blobs）。無ければサーバーが作る（--rag-remote とは併用しない）
#     --redis-db N         セッションの Redis の DB 番号（0〜15。既定は番号を付けない）
#     --port N             API のポート（--api-only と併用。8080 か 10067＝Keycloak が許すリダイレクト先）
#     --gateway-ports A,B  ゲートウェイと B1 のポート（既定 18090,18091）
#     --api-only           web（:3000）・pnpm install・砂箱のビルドをしない。ログインの戻り先は
#                          http://localhost:<port>/auth/callback（API を直接叩く）
#     --run-label L        PID ファイルとログの置き場を L ごとに分ける。止めるときは --down --run-label L。
#                          L か --server-bin を付けると、同じバイナリで動く別のサーバーを pgrep で止めない
#                          （止めるのは L の PID ファイルのものと、自分のポートを握る自分のバイナリだけ）
#   例: dev-up.sh --server-bin ~/.local/share/shiki-eval/bin/shiki-server-v0 --db-name shiki_eval_v0 \
#         --store-name shiki-eval-v0 --bucket shiki-eval-v0 --redis-db 5 --port 10067 \
#         --gateway-ports 18190,18191 --api-only --run-label abeval-v0
#       dev-up.sh --down --run-label abeval-v0
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }
say() { printf '%s\n' "$*"; }
# URL のホスト部（userinfo・ポート・パスを除く。IPv6 は [..] のまま）。
url_host() {
  local a=${1#*://}
  a=${a%%[/?#]*}
  a=${a##*@}
  if [[ "$a" == \[* ]]; then printf '%s' "${a%%]*}]"; else printf '%s' "${a%%:*}"; fi
}
# ループバックのホストか（localhost・127.0.0.0/8・[::1] だけ）。
is_loopback_host() {
  local h=${1,,}
  [[ "$h" == localhost || "$h" == "[::1]" || "$h" =~ ^127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]
}

MODE=native
RESET_DB=0
WITH_RAG=0
# --rag-remote: worker と S3 を外部のものにする（RAG 自体は WITH_RAG で有効にする）。
RAG_REMOTE=0
WITH_SANDBOX=0
WITH_OFFICE=0
ACTION=up
# 版のサーバー向け（GAP-30）。既定は今までの固定の値。
SERVER_BIN=""
DB_NAME=shiki
STORE_NAME=shiki
BUCKET=""
REDIS_DB=""
API_PORT=8080
PORT_SET=0
GATEWAY_PORTS=""
API_ONLY=0
RUN_LABEL=""
EVAL_ARGS=0

# 値を取る引数（値が無い・空なら終了コード 2）。
need_val() {
  if [ $# -lt 2 ] || [ -z "$2" ]; then
    printf '%s には値が要ります\n' "$1" >&2
    exit 2
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --server-bin)    need_val "$@"; SERVER_BIN=$2; EVAL_ARGS=1; shift ;;
    --db-name)       need_val "$@"; DB_NAME=$2; EVAL_ARGS=1; shift ;;
    --store-name)    need_val "$@"; STORE_NAME=$2; EVAL_ARGS=1; shift ;;
    --bucket)        need_val "$@"; BUCKET=$2; EVAL_ARGS=1; shift ;;
    --redis-db)      need_val "$@"; REDIS_DB=$2; EVAL_ARGS=1; shift ;;
    --port)          need_val "$@"; API_PORT=$2; PORT_SET=1; EVAL_ARGS=1; shift ;;
    --gateway-ports) need_val "$@"; GATEWAY_PORTS=$2; EVAL_ARGS=1; shift ;;
    --api-only)      API_ONLY=1; EVAL_ARGS=1 ;;
    --run-label)     need_val "$@"; RUN_LABEL=$2; EVAL_ARGS=1; shift ;;
    --compose) MODE=compose ;;
    --reset-db) RESET_DB=1 ;;
    --rag)     WITH_RAG=1 ;;
    --rag-remote) WITH_RAG=1; RAG_REMOTE=1 ;;
    --sandbox) WITH_SANDBOX=1 ;;
    --office)  WITH_OFFICE=1 ;;
    --down)    ACTION=down ;;
    --status)  ACTION=status ;;
    # 冒頭のコメントの塊を最後まで出す（行数を決め打ちすると説明を足した時に途中で切れる）。
    -h|--help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) err "未知の引数: $1"; exit 2 ;;
  esac
  shift
done

# --- 版のサーバー向けの引数を確かめる（何かを起動・変更する前に止める） ---
bad_arg() { err "$1"; exit 2; }
[[ "$DB_NAME" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || bad_arg "--db-name は英小文字・数字・_ の 63 文字までです: $DB_NAME"
[[ "$STORE_NAME" =~ ^[a-z][a-z0-9_-]{0,62}$ ]] || bad_arg "--store-name は英小文字で始まる英小文字・数字・_・- の 63 文字までです: $STORE_NAME"
if [ -n "$BUCKET" ]; then
  [[ "$BUCKET" =~ ^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$ ]] || bad_arg "--bucket は英小文字・数字・- の 3〜63 文字です（S3 のバケット名）: $BUCKET"
fi
if [ -n "$REDIS_DB" ]; then
  { [[ "$REDIS_DB" =~ ^[0-9]{1,2}$ ]] && [ "$((10#$REDIS_DB))" -le 15 ]; } || bad_arg "--redis-db は 0〜15 です: $REDIS_DB"
  REDIS_DB=$((10#$REDIS_DB))
fi
if [ "$PORT_SET" = 1 ]; then
  [ "$API_ONLY" = 1 ] || bad_arg "--port は --api-only と併用してください（web は :8080 の API に繋ぐ）。"
  case "$API_PORT" in
    8080|10067) : ;;
    *) bad_arg "--port は 8080 か 10067 です（Keycloak が許すリダイレクト先）: $API_PORT" ;;
  esac
fi
if [ -n "$GATEWAY_PORTS" ]; then
  [[ "$GATEWAY_PORTS" =~ ^([0-9]{4,5}),([0-9]{4,5})$ ]] || bad_arg "--gateway-ports は A,B（ポート 2 つ）です: $GATEWAY_PORTS"
  for p in "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"; do
    { [ "$p" -ge 1024 ] && [ "$p" -le 65535 ]; } || bad_arg "--gateway-ports のポートは 1024〜65535 です: $p"
  done
  [ "${BASH_REMATCH[1]}" != "${BASH_REMATCH[2]}" ] || bad_arg "--gateway-ports の 2 つは別のポートにしてください: $GATEWAY_PORTS"
fi
if [ -n "$RUN_LABEL" ]; then
  [[ "$RUN_LABEL" =~ ^[a-z][a-z0-9-]{0,40}$ ]] || bad_arg "--run-label は英小文字で始まる英小文字・数字・- の 41 文字までです: $RUN_LABEL"
fi
if [ -n "$SERVER_BIN" ]; then
  [[ "$SERVER_BIN" == /* ]] || bad_arg "--server-bin は絶対パスです: $SERVER_BIN"
  [ -f "$SERVER_BIN" ] && [ -x "$SERVER_BIN" ] || bad_arg "--server-bin が実行できるファイルではありません: $SERVER_BIN"
  SERVER_BIN=$(readlink -f "$SERVER_BIN")
  [[ "$SERVER_BIN" =~ ^[A-Za-z0-9._/+-]+$ ]] || bad_arg "--server-bin のパスに使えない文字があります: $SERVER_BIN"
fi
if [ "$EVAL_ARGS" = 1 ] && [ "$MODE" = compose ]; then
  bad_arg "--server-bin・--db-name・--store-name・--bucket・--redis-db・--port・--gateway-ports・--api-only・--run-label は native 専用です（--compose とは併用しない）。"
fi
if [ -n "$BUCKET" ] && [ "$RAG_REMOTE" = 1 ]; then
  bad_arg "--bucket は --rag-remote と併用しません（外部のバケットは SHIKI_DEV_S3_BUCKET で渡す）。"
fi

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { err "git リポジトリ内で実行してください。"; exit 2; }
cd "$ROOT"

# Collabora はコンテナから shiki-server の WOPI を取りに来る。native では server が
# ホスト側にいて、compose に extra_hosts(host-gateway) も無いため到達できない。
# 中途半端に起動して「なぜか動かない」を作らず、ここで止める。
if [ "$WITH_OFFICE" = 1 ] && [ "$MODE" != compose ]; then
  err "--office は --compose と併用してください（native では Collabora がホストの shiki-server に到達できません）。"
  err "  例: $0 --compose --office"
  exit 2
fi

# compose の shiki-server は worker・S3 の接続先が docker-compose.yml に固定されており、
# 外部の接続先を渡す経路が無い。起動はできても索引が入らない状態を作らず、ここで止める。
if [ "$RAG_REMOTE" = 1 ] && [ "$MODE" = compose ]; then
  err "--rag-remote は native 専用です（compose の shiki-server には外部の worker・S3 を渡せません）。"
  err "  compose で RAG を使うなら: $0 --compose --rag"
  exit 2
fi

command -v docker >/dev/null 2>&1 || { err "docker が見つかりません。"; exit 2; }
command -v curl >/dev/null 2>&1 || { err "curl が見つかりません。"; exit 2; }

# worktree ごとに分ける。UID だけで共有すると、別 worktree の --down が
# こちらの PID ファイルを読んで無関係なプロセスを止めてしまう。
# --run-label を付けたときは、さらにラベルごとに分ける（版のサーバーを同じ worktree から並べて起動するため）。
RUNDIR="${TMPDIR:-/tmp}/shiki-dev-up-$(id -u)-$(printf '%s' "$ROOT" | cksum | cut -d' ' -f1)${RUN_LABEL:+-$RUN_LABEL}"
mkdir -p "$RUNDIR"
# 起動するサーバーのバイナリ（--server-bin が無ければ cargo build したもの）。
SERVER_EXE=${SERVER_BIN:-$ROOT/target/debug/shiki-server}
# --run-label のサーバーのポートとバイナリ（--down が読む）。
SERVER_ENV="$RUNDIR/server.env"
SERVER_LOG="$RUNDIR/shiki-server.log"
WEB_LOG="$RUNDIR/web.log"
SERVER_PID="$RUNDIR/shiki-server.pid"
WEB_PID="$RUNDIR/web.pid"

# --- 生存確認 ---
# HTTP ステータスだけを返す（到達不能なら NG）。-f を付けないのは 4xx も
# 「応答している」として区別したいため。
http_code() {
  local c
  c=$(curl -sS --max-time 3 -o /dev/null -w '%{http_code}' "$1" 2>/dev/null) || { echo NG; return; }
  case "$c" in 000|"") echo NG ;; *) echo "$c" ;; esac
}

check_status() {
  local api web kc
  api=$(http_code "http://localhost:${API_PORT}/healthz")
  if [ "$API_ONLY" = 1 ]; then
    kc=$(http_code http://localhost:8081/realms/shiki)
    say "shiki-server (:${API_PORT}/healthz): $api"
    say "keycloak     (:8081):         $kc"
    [ "$api" = "200" ]
    return
  fi
  web=$(http_code http://localhost:3000/)
  kc=$(http_code http://localhost:8081/realms/shiki)
  say "shiki-server (:8080/healthz): $api"
  say "web          (:3000):         $web"
  say "keycloak     (:8081):         $kc"
  [ "$api" = "200" ] && { [ "$web" = "200" ] || [ "$web" = "302" ] || [ "$web" = "307" ]; }
}

# :PORT のリスナが「自分のもの」か判定して止める。所有者を見ずに fuser -k すると、
# 別 worktree の next dev や無関係なローカルサービスを突然停止してしまう。
# 期待する実行ファイル名（部分一致）を渡す。
kill_port_if_ours() {  # kill_port_if_ours <port> <exe に含まれるべき文字列>
  command -v fuser >/dev/null 2>&1 || return 0
  local pids exe matched=0
  pids=$(fuser "$1/tcp" 2>/dev/null | tr -s ' ' '\n' | grep -E '^[0-9]+$' || true)
  [ -n "$pids" ] || return 0
  for pid in $pids; do
    exe=$(readlink -f "/proc/$pid/exe" 2>/dev/null || echo "")
    # next dev は node なので、実行ファイルではなく cwd で worktree を判定する。
    cwd=$(readlink -f "/proc/$pid/cwd" 2>/dev/null || echo "")
    case "$exe$cwd" in
      *"$2"*) kill -TERM "$pid" 2>/dev/null || true; matched=1 ;;
    esac
  done
  if [ "$matched" = 0 ]; then
    err ":$1 を別のプロセスが使用しています（この worktree のものではありません）。停止してから再実行してください。"
    fuser -v "$1/tcp" >&2 2>/dev/null || true
    return 1
  fi
  return 0
}

# 砂箱の成果物が無い、またはソース/ビルドスクリプトより古いなら再ビルドが要る。
sandbox_needs_build() {
  local out=web/editor-sandbox/dist/slide-editor.html
  [ -f "$out" ] || return 0
  [ -n "$(find web/editor-sandbox/src web/editor-sandbox/build.mjs -newer "$out" 2>/dev/null | head -1)" ]
}

# --- ラベルのサーバーの停止 ---
# PID ファイルのものと、「自分のポートを握り、exe が自分のバイナリのプロセス」だけを止める。
# pgrep -f で同じパスのバイナリを探して止めると、同じバイナリで動く別のサーバー（stub のスタックなど）まで止める。
# （--run-label・--server-bin のときだけ使う。引数を省いた起動の経路は pgrep のまま。）
own_server_pids() {  # own_server_pids <port> <exe>: 自分のポートを握る自分のバイナリの PID
  local pid pids=""
  for pid in $(fuser "$1/tcp" 2>/dev/null | tr -s ' ' '\n' | grep -E '^[0-9]+$' || true); do
    [ "$(readlink -f "/proc/$pid/exe" 2>/dev/null || echo "?")" = "$2" ] && pids="$pids $pid"
  done
  printf '%s' "$pids"
}

stop_own_server() {  # stop_own_server <port> <exe>（止められなければ 1）
  local port=$1 exe=$2 pid pids
  if [ -f "$SERVER_PID" ]; then
    pid=$(cat "$SERVER_PID" 2>/dev/null || true)
    if [ -n "$pid" ]; then
      kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    fi
    rm -f "$SERVER_PID"
  fi
  command -v fuser >/dev/null 2>&1 || return 0
  for _ in $(seq 1 20); do
    pids=$(own_server_pids "$port" "$exe")
    [ -n "$pids" ] || return 0
    # shellcheck disable=SC2086
    kill -TERM $pids 2>/dev/null || true
    sleep 1
  done
  # TERM で 20 秒たっても止まらなければ KILL する（残ったまま新しいプロセスを起こすと bind に失敗し、古い方が
  # /healthz に答えるので起動できたと誤る）。それでも空かなければ止める。
  pids=$(own_server_pids "$port" "$exe")
  [ -n "$pids" ] || return 0
  err "   :${port} の自分のサーバーが TERM で止まらないので KILL します:${pids}"
  # shellcheck disable=SC2086
  kill -KILL $pids 2>/dev/null || true
  sleep 1
  pids=$(own_server_pids "$port" "$exe")
  if [ -n "$pids" ]; then
    err "   :${port} の自分のサーバーを止められません:${pids}"
    return 1
  fi
  return 0
}

stop_labeled() {
  local port="" exe=""
  if [ -f "$SERVER_ENV" ]; then
    port=$(sed -n 's/^port=//p' "$SERVER_ENV")
    exe=$(sed -n 's/^exe=//p' "$SERVER_ENV")
  fi
  if [ -f "$WEB_PID" ]; then
    pid=$(cat "$WEB_PID" 2>/dev/null || true)
    [ -n "$pid" ] && { kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true; }
    rm -f "$WEB_PID"
  fi
  if [ -n "$port" ] && [ -n "$exe" ]; then
    stop_own_server "$port" "$exe"
  elif [ -f "$SERVER_PID" ]; then
    pid=$(cat "$SERVER_PID" 2>/dev/null || true)
    [ -n "$pid" ] && { kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true; }
    rm -f "$SERVER_PID"
  fi
  rm -f "$SERVER_ENV"
  say "停止しました（--run-label $RUN_LABEL・compose の依存サービスは残しています）。"
}

# --- 停止 ---
stop_all() {
  if [ -n "$RUN_LABEL" ]; then
    stop_labeled
    return 0
  fi
  # native 起動時のみ SERVER_PID を書いている（compose 起動なら :8080 は docker-proxy）。
  local had_native=0
  [ -f "$SERVER_PID" ] && had_native=1
  for f in "$WEB_PID" "$SERVER_PID"; do
    if [ -f "$f" ]; then
      pid=$(cat "$f" 2>/dev/null || true)
      if [ -n "$pid" ]; then
        # プロセスグループごと止める（setsid で起動しているため）。
        kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
      fi
      rm -f "$f"
    fi
  done
  # setsid 経由の $! はプロセスグループ ID と一致しないことがあるため、ポート指定でも落とす。
  # pkill -f は自分のコマンド行にもマッチして自爆する（exit 144 の連鎖）ので使わない。
  if command -v fuser >/dev/null 2>&1; then
    kill_port_if_ours 3000 "$ROOT" || true
    # :8080 は「今そこにいるのが自分の native バイナリの時だけ」落とす。
    # PID ファイルの有無で判断すると、native がクラッシュして PID ファイルだけ残った後に
    # compose を起動した場合に、compose の :8080 を publish している docker-proxy を
    # SIGKILL してしまう（コンテナは生きたままホストの :8080 だけ不通になる）。
    if [ "$had_native" = 1 ]; then
      holder=$(fuser 8080/tcp 2>/dev/null | tr -d ' ' || true)
      if [ -n "$holder" ]; then
        holder_exe=$(readlink -f "/proc/${holder%% *}/exe" 2>/dev/null || echo "?")
        [ "$holder_exe" = "$ROOT/target/debug/shiki-server" ] && { fuser -k 8080/tcp >/dev/null 2>&1 || true; }
      fi
    fi
  fi
  say "停止しました（compose の依存サービスは残しています。落とすなら deploy/compose で docker compose down）。"
}

case "$ACTION" in
  status) check_status && say "=> 検証可能" || { say "=> 未起動 or 異常"; exit 1; }; exit 0 ;;
  down)   stop_all; exit 0 ;;
esac

# --- 0. 外部の worker・S3 を使うなら、先に接続先と疎通を確かめる ---
# **黙ってローカルへフォールバックしない。** 届かないまま進むと、worker が presigned URL を
# 取りに来られず「アップロードは通るのに索引だけ入らない」という分かりにくい壊れ方をする。
if [ "$RAG_REMOTE" = 1 ]; then
  say "== 0. 外部の ingestion-worker と S3 を確認 =="
  missing=""
  for v in SHIKI_DEV_RAG_WORKER_URL SHIKI_DEV_S3_ENDPOINT SHIKI_DEV_S3_BUCKET SHIKI_DEV_S3_ACCESS_KEY SHIKI_DEV_S3_SECRET_KEY; do
    [ -n "${!v:-}" ] || missing="$missing $v"
  done
  if [ -n "$missing" ]; then
    err "   --rag-remote には次の環境変数が要ります:$missing"
    err "   （worker と S3 をローカルに立てるなら --rag）"
    exit 2
  fi
  # server はベース URL の末尾の / を除いて連結するので、確認側も合わせる。
  SHIKI_DEV_RAG_WORKER_URL=${SHIKI_DEV_RAG_WORKER_URL%/}
  # 共有トークンは Bearer で毎リクエスト送られる。ループバック以外へ平文 HTTP で送ると経路上で
  # 盗聴され得るので拒否する。TLS の無い worker は SSH トンネル等で localhost へ引き込んで使う。
  # 判定は URL のホスト部で行う（`http://localhost.example.com` のような接頭辞一致を通さない）。
  if [ -n "${SHIKI_DEV_RAG_WORKER_TOKEN:-}" ] && [[ "${SHIKI_DEV_RAG_WORKER_URL,,}" == http://* ]] \
    && ! is_loopback_host "$(url_host "$SHIKI_DEV_RAG_WORKER_URL")"; then
    err "   worker のトークンを平文 HTTP（$SHIKI_DEV_RAG_WORKER_URL）では送りません。"
    err "   https の URL にするか、SSH トンネル等で localhost へ引き込んでください"
    err "   （例: ssh -N -L 8000:localhost:8000 <worker のホスト> → SHIKI_DEV_RAG_WORKER_URL=http://localhost:8000）"
    exit 2
  fi
  if ! worker_health=$(curl -fsS -m 5 "$SHIKI_DEV_RAG_WORKER_URL/healthz" 2>/dev/null); then
    err "   ingestion-worker に届きません: $SHIKI_DEV_RAG_WORKER_URL"
    exit 1
  fi
  # 埋め込みの版は server と worker で一致していないと、/embed の応答が版の突合で毎回拒否される
  # （起動は成功して索引だけが入らない）。worker が名乗るモデルを server の期待値にする。
  REMOTE_EMBED_MODEL=$(printf '%s' "$worker_health" | grep -o '"embed":{"id":"[^"]*"' | sed 's/.*"id":"//; s/"$//' || true)
  if [ -z "$REMOTE_EMBED_MODEL" ]; then
    err "   ingestion-worker の /healthz から埋め込みモデルを読めません: $worker_health"
    exit 1
  fi
  # S3 は無署名リクエストに 403 などを返す。応答コードが返れば届いている。
  # 公開側（ブラウザが presigned URL で叩く先）を別に指定したなら、そちらも確かめる。
  # server は presigned URL を手元で作るだけなので、届かなくても起動は通り、最初のアップロードで初めて落ちる。
  for ep in "$SHIKI_DEV_S3_ENDPOINT" ${SHIKI_DEV_S3_PUBLIC_ENDPOINT:+"$SHIKI_DEV_S3_PUBLIC_ENDPOINT"}; do
    if ! curl -s -m 5 -o /dev/null -w '%{http_code}' "$ep" 2>/dev/null | grep -qE '^[2-4]'; then
      err "   S3 に届きません: $ep"
      exit 1
    fi
  done
  say "   [ok] worker=$SHIKI_DEV_RAG_WORKER_URL  embed=$REMOTE_EMBED_MODEL"
  say "        s3=$SHIKI_DEV_S3_ENDPOINT  bucket=$SHIKI_DEV_S3_BUCKET"
  # 資格情報は export されて入ってくる。このままだと pnpm install・compose・cargo・next dev まで
  # 引き継ぐので、非 export の変数へ退避して元を消す（shiki-server の起動時にだけ渡す）。
  REMOTE_S3_AK=$SHIKI_DEV_S3_ACCESS_KEY
  REMOTE_S3_SK=$SHIKI_DEV_S3_SECRET_KEY
  REMOTE_WORKER_TOKEN=${SHIKI_DEV_RAG_WORKER_TOKEN:-}
  unset SHIKI_DEV_S3_ACCESS_KEY SHIKI_DEV_S3_SECRET_KEY SHIKI_DEV_RAG_WORKER_TOKEN
fi

# --- 1. compose 依存サービス ---
DEPS="postgres keycloak openfga redis"
# 外部の S3 を使うなら RustFS は立てない。
[ "$RAG_REMOTE" = 1 ] || DEPS="$DEPS rustfs"
# Qdrant は常にローカル（小さく、ベクトルは手元で使い捨てたい）。
[ "$WITH_RAG" = 1 ] && DEPS="$DEPS qdrant"
[ "$WITH_RAG" = 1 ] && [ "$RAG_REMOTE" = 0 ] && DEPS="$DEPS ingestion-worker"
[ "$WITH_SANDBOX" = 1 ] && DEPS="$DEPS sandbox-orchestrator"
[ "$WITH_OFFICE" = 1 ] && DEPS="$DEPS collabora"

say "== 1. compose 依存サービスを起動 =="
say "   $DEPS"

# web の依存導入は compose 起動にも cargo ビルドにも依存しないので、先に背後で始めておく
# （直列だと初回の実時間が「compose 待ち ＋ cold build ＋ install」の総和になる）。
WEB_INSTALL_PID=""
if [ "$API_ONLY" = 0 ] && [ ! -d web/node_modules ]; then
  say "   （pnpm install を背後で先行実行します）"
  ( cd web && pnpm install --frozen-lockfile ) > "$RUNDIR/pnpm-install.log" 2>&1 &
  WEB_INSTALL_PID=$!
fi

compose_up() {  # compose_up <service...>: 起動して healthy になるまで待つ
  (
    cd deploy/compose
    [ -f .env ] || cp .env.example .env
    docker compose up -d "$@"
    for svc in "$@"; do
      ok=""
      for _ in $(seq 1 60); do
        s=$(docker compose ps "$svc" --format '{{.Health}}' 2>/dev/null || true)
        # healthcheck 未定義のサービスは Health が空になる。起動していれば良しとする。
        if [ "$s" = "healthy" ] || { [ -z "$s" ] && [ -n "$(docker compose ps -q "$svc" 2>/dev/null)" ]; }; then
          ok=1; break
        fi
        sleep 5
      done
      [ -n "$ok" ] || { err "$svc が healthy になりませんでした（docker compose logs $svc）"; exit 1; }
      say "   [ok] $svc"
    done
  )
}
# 先に Postgres だけ立て、DB のリセットと保存先の組の確認を済ませてから残りを立てる
# （確認で止まる時に、ローカルの worker など重いものを起動したまま残さない）。
compose_up postgres

# --- 1.5 dev DB のリセット（明示 opt-in） ---
# 別ブランチの migration が当たった compose DB は checksum 不一致で起動を止める
# （"migration N was previously applied but has been modified"）。dev DB は使い捨てなので
# 作り直してよいが、破壊操作なので必ず明示フラグでのみ行う。
# compose が解釈した POSTGRES_USER（.env の引用符やシェルの上書きを反映済み）を、起動中のコンテナから読む。
pg_user() {
  local u
  u=$(docker compose -f deploy/compose/docker-compose.yml exec -T postgres printenv POSTGRES_USER 2>/dev/null | tr -d '\r\n') || u=""
  printf '%s' "${u:-postgres}"
}
reset_dev_db() {
  err "⚠️  compose の dev DB '$DB_NAME' を DROP して作り直します（データは失われます）。"
  docker compose -f deploy/compose/docker-compose.yml exec -T postgres \
    psql -U "$(pg_user)" -c "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE);" -c "CREATE DATABASE $DB_NAME;"
  say "   [ok] dev DB をリセットしました（SHIKI_DEV_SEED=true が再投入します）"
}
[ "$RESET_DB" = 1 ] && reset_dev_db
# --db-name の DB が無ければ作る（migration と dev seed はサーバーが流す）。既定の 'shiki' は compose が作る。
ensure_db() {
  local out
  if ! out=$(docker compose -f deploy/compose/docker-compose.yml exec -T postgres \
    psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" 2>&1); then
    err "DB の有無を確かめられません: $out"
    exit 1
  fi
  if [ "${out%%$'\n'*}" != 1 ]; then
    if ! out=$(docker compose -f deploy/compose/docker-compose.yml exec -T postgres \
      psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d postgres -c "CREATE DATABASE $DB_NAME;" 2>&1); then
      err "DB '$DB_NAME' を作れません: $out"
      exit 1
    fi
    say "   [ok] DB '$DB_NAME' を作りました（migration と dev seed はサーバーが流します）"
  fi
}
if [ "$DB_NAME" != shiki ]; then
  ensure_db
fi

# --- 1.6 DB と保存先の組を確かめる ---
# blob の行は保存先を記録しないので、DB を残したまま保存先だけ変えると既存の blob が読めず、
# 同じ内容を上げ直しても重複扱いで実体が作られない。どの保存先と組で使った DB かを
# DB のコメントに残し、食い違えば止める（DB を作り直すとコメントも消える）。
STORE_TAG=$([ "$RAG_REMOTE" = 1 ] && printf 'shiki-dev-store=%s/%s' "$SHIKI_DEV_S3_ENDPOINT" "$SHIKI_DEV_S3_BUCKET" || printf 'shiki-dev-store=local%s' "${BUCKET:+/$BUCKET}")
dev_psql() {  # dev_psql <sql>  → 1 行目の値だけ返す
  # **接続や SQL の失敗を握り潰さない。** 空として扱うと、印も blob も無い DB と見なして
  # 別の保存先へ切り替えを通してしまう。
  local out
  if ! out=$(docker compose -f deploy/compose/docker-compose.yml exec -T postgres \
    psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d "$DB_NAME" -tAc "$1" 2>&1); then
    err "dev DB への問い合わせに失敗しました: $out"
    exit 1
  fi
  printf '%s' "${out%%$'\n'*}"
}
cur_tag=$(dev_psql "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = '$DB_NAME'")
if [ -z "$cur_tag" ]; then
  # 印の無い DB は、この仕組みより前に作られたもの（＝ローカルの RustFS と組）か、まだ空のもの。
  # blob 表が無い（migration 前の）DB で FROM blob を書くとエラーになるので、先に有無を見る。
  if [ "$(dev_psql "SELECT to_regclass('public.blob') IS NOT NULL")" = t ] \
    && [ "$(dev_psql "SELECT EXISTS (SELECT 1 FROM blob)")" = t ]; then
    cur_tag="shiki-dev-store=local"
  fi
fi
if [ -n "$cur_tag" ] && [ "$cur_tag" != "$STORE_TAG" ]; then
  err "❌ この dev DB は別の保存先と組で使われています。"
  err "   DB の保存先: ${cur_tag#shiki-dev-store=}"
  err "   今回の保存先: ${STORE_TAG#shiki-dev-store=}"
  err "   切り替えるなら DB を作り直してください: $0 --reset-db ...（元に戻すなら前と同じ引数で起動）"
  exit 1
fi

# native ＋ ローカルの RAG では、worker をホストのネットワークで動かす（native のサーバーが出す presigned URL の
# localhost:9000 に届くように・BUG-300）。compose 版と --rag-remote は今までどおり。
if [ "$MODE" = native ] && [ "$WITH_RAG" = 1 ] && [ "$RAG_REMOTE" = 0 ]; then
  export COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.yml}:docker-compose.native-rag.yml"
fi
# shellcheck disable=SC2086
compose_up $DEPS

# 印の付け替えと不要なサービスの停止。**起動前の検査をすべて通した後に呼ぶ。**
# 先にやると、後の検査（:8080 の取り合いなど）で止まった時に今の環境を壊してしまう。
apply_store_switch() {
  dev_psql "COMMENT ON DATABASE $DB_NAME IS '${STORE_TAG//\'/\'\'}'" >/dev/null
  # --rag から切り替えた時、前に立てたローカルの worker（常駐 2〜3GB）と RustFS は
  # DEPS から外しても動き続ける（restart: unless-stopped）。外部を使うなら止める。
  if [ "$RAG_REMOTE" = 1 ]; then
    for svc in ingestion-worker rustfs; do
      if [ -n "$(docker compose -f deploy/compose/docker-compose.yml ps -q "$svc" 2>/dev/null)" ]; then
        docker compose -f deploy/compose/docker-compose.yml stop "$svc" >/dev/null
        say "   [stop] $svc（外部のものを使うため）"
      fi
    done
  fi
}

# --- 2. shiki-server ---
# web に渡すゲートウェイ/B1 オリジン。compose は publish 済みの 8090/8091、
# native は衝突回避のため後段で上書きする。
GW_ORIGIN=http://localhost:8090
B1_ORIGIN=http://localhost:8091

if [ "$MODE" = compose ]; then
  say "== 2. shiki-server を compose で起動（:8080） =="
  # DB を作り直したら、コンテナも作り直さないと migration とシードが再実行されない
  # （イメージも env も同一だと compose はコンテナを再生成せず「Running」のまま。
  #   healthcheck は /healthz＝DB 非依存なので healthy と出るが、実際は空 DB で全 API が 500）。
  RECREATE=""
  [ "$RESET_DB" = 1 ] && RECREATE="--force-recreate"
  apply_store_switch
  (
    cd deploy/compose
    SHIKI__AUTH__REDIRECT_URI=http://localhost:3000/auth/callback \
    SHIKI__AUTH__POST_LOGOUT_REDIRECT_URI=http://localhost:3000/ \
    SHIKI__RAG__ENABLED="$([ "$WITH_RAG" = 1 ] && echo true || echo false)" \
    SHIKI__OFFICE__ENABLED="$([ "$WITH_OFFICE" = 1 ] && echo true || echo false)" \
      docker compose up -d --build $RECREATE shiki-server
    for _ in $(seq 1 60); do
      [ "$(docker compose ps shiki-server --format '{{.Health}}' 2>/dev/null)" = "healthy" ] && exit 0
      sleep 5
    done
    err "shiki-server が healthy になりませんでした"; exit 1
  )
  say "   [ok] shiki-server (compose)"
else
  say "== 2. shiki-server を cargo run で起動（:${API_PORT}・native） =="

  # 依存の接続先は compose から導出する（ポート番号をこのスクリプトに焼き込むと、
  # compose 側の変更で静かに壊れる）。取れなければ compose の既定値へフォールバックする。
  host_port() {  # host_port <service> <container_port> <fallback>
    local hp
    hp=$( cd deploy/compose && docker compose port "$1" "$2" 2>/dev/null ) || hp=""
    if [ -n "$hp" ]; then printf '%s' "${hp##*:}"; else printf '%s' "$3"; fi
  }
  PG_PORT=$(host_port postgres 5432 5432)
  KC_PORT=$(host_port keycloak 8080 8081)
  FGA_PORT=$(host_port openfga 8080 8082)
  REDIS_PORT=$(host_port redis 6379 6379)
  RUSTFS_PORT=$(host_port rustfs 9000 9000)
  QDRANT_PORT=$(host_port qdrant 6333 6333)
  # 資格情報も compose から導出する。minioadmin で焼き込むと、.env で
  # RUSTFS_ACCESS_KEY（または旧 MINIO_ROOT_USER）を変えている環境で native 版
  # サーバだけ S3 認証に失敗する。**起動済みコンテナの env が、compose の
  # フォールバック連鎖（RUSTFS_* → MINIO_ROOT_* → 既定）を通った唯一の正解。**
  rustfs_env() {  # rustfs_env <VAR> <fallback>
    local v
    v=$( cd deploy/compose && docker compose exec -T rustfs printenv "$1" 2>/dev/null | tr -d '\r\n' ) || v=""
    if [ -n "$v" ]; then printf '%s' "$v"; else printf '%s' "$2"; fi
  }
  RUSTFS_AK=$(rustfs_env RUSTFS_ACCESS_KEY minioadmin)
  RUSTFS_SK=$(rustfs_env RUSTFS_SECRET_KEY minioadmin)
  # Qdrant の API キー・worker の共有トークンも起動済みコンテナから読む（#812）。
  # 未起動、またはキー導入前に作られたコンテナ（無認証）なら空のまま＝無認証で繋ぐ。
  svc_env() {  # svc_env <service> <VAR>
    ( cd deploy/compose && docker compose exec -T "$1" printenv "$2" 2>/dev/null | tr -d '\r\n' ) || true
  }
  QDRANT_KEY=""
  [ "$WITH_RAG" = 1 ] && QDRANT_KEY=$(svc_env qdrant QDRANT__SERVICE__API_KEY)
  # 任意サービス（フラグを付けた時だけ起動している）。未起動ならフォールバック値のまま。
  WORKER_PORT=$(host_port ingestion-worker 8000 8000)
  SANDBOX_PORT=$(host_port sandbox-orchestrator 50000 50000)
  COLLABORA_PORT=$(host_port collabora 9980 9980)
  # native 専用のゲートウェイポート。8090/8091 は compose の shiki-server が publish する
  # ため、別 worktree で compose 版が動いていると衝突する。native は常に 18090/18091 に逃がす。
  GW_PORT=18090
  B1_PORT=18091
  # 版のサーバーは --gateway-ports で分ける（runner はループバックで委譲するので、同じだと別のサーバーへ届く）。
  if [ -n "$GATEWAY_PORTS" ]; then
    GW_PORT=${GATEWAY_PORTS%%,*}
    B1_PORT=${GATEWAY_PORTS##*,}
  fi
  GW_ORIGIN=http://localhost:$GW_PORT
  B1_ORIGIN=http://localhost:$B1_PORT

  # /builtin/slide-editor の配信元。未ビルドだと 404 → スライドエディタが閲覧
  # フォールバックに落ち、変更が反映されない画面をスクショすることになる。
  # 成果物がソースより古ければ作り直す。存在チェックだけだと、砂箱のソースを変更しても
  # 古い成果物が /builtin から配信され続け、変更前の UI をスクショして成功扱いにしてしまう。
  if [ "$API_ONLY" = 0 ] && sandbox_needs_build; then
    if [ -d web/node_modules ]; then
      say "   スライドエディタ砂箱をビルドします（/builtin 配信用）…"
      ( cd web && pnpm build:editor-sandbox ) || err "   ⚠️  砂箱のビルドに失敗（/builtin は 404 のまま）"
    else
      say "   （node_modules 未導入のため砂箱ビルドは web 起動後に実施）"
    fi
  fi
  say "   依存ポート: pg=$PG_PORT kc=$KC_PORT fga=$FGA_PORT redis=$REDIS_PORT rustfs=$RUSTFS_PORT qdrant=$QDRANT_PORT"

  # S3 と worker の実効値。heredoc は unquoted で中の分岐が生成時に評価されるため、外で決める。
  if [ "$RAG_REMOTE" = 1 ]; then
    EFF_S3_INTERNAL=$SHIKI_DEV_S3_ENDPOINT
    # ブラウザが presigned URL を直接叩くので、ブラウザから届く先でなければならない。
    EFF_S3_PUBLIC=${SHIKI_DEV_S3_PUBLIC_ENDPOINT:-$SHIKI_DEV_S3_ENDPOINT}
    EFF_S3_BUCKET=$SHIKI_DEV_S3_BUCKET
    EFF_S3_AK=$REMOTE_S3_AK
    EFF_S3_SK=$REMOTE_S3_SK
    EFF_WORKER_URL=$SHIKI_DEV_RAG_WORKER_URL
    EFF_WORKER_TOKEN=$REMOTE_WORKER_TOKEN
    EMBED_LINE="export SHIKI__RAG__EMBEDDING_MODEL_VERSION=$(printf '%q' "$REMOTE_EMBED_MODEL")"
    # 外部のバケットの CORS は、指定された時だけ設定する（既存の設定を勝手に書き換えない）。
    EFF_CORS=${SHIKI_DEV_S3_CORS_ALLOWED_ORIGINS:-}
  else
    EFF_S3_INTERNAL=http://localhost:${RUSTFS_PORT}
    EFF_S3_PUBLIC=http://localhost:${RUSTFS_PORT}
    EFF_S3_BUCKET=${BUCKET:-shiki-blobs}
    EFF_S3_AK=$RUSTFS_AK
    EFF_S3_SK=$RUSTFS_SK
    EFF_WORKER_URL=http://localhost:${WORKER_PORT}
    EFF_WORKER_TOKEN=""
    [ "$WITH_RAG" = 1 ] && EFF_WORKER_TOKEN=$(svc_env ingestion-worker WORKER_AUTH_TOKEN)
    EFF_CORS='[*]'
    EMBED_LINE="# 埋め込みの版は server の既定値（ローカルの worker と同じ）"
  fi
  # CSV の集計の隔離ランナー（csv.* のブロックと CSV の画面の SQL が使う・BUG-22）。呼び出し側の env にあればそれを使う
  # （版のサーバーの評価は版のランナーを渡す）。無ければ CI の tabular-runner ジョブと同じ release ビルドを下でビルドして使う。
  TABULAR_RUNNER_FROM_ENV=${SHIKI__TABULAR__RUNNER_PATH:+1}
  TABULAR_RUNNER=${SHIKI__TABULAR__RUNNER_PATH:-$ROOT/crates/tabular/runner/target/release/shiki-tabular-runner}
  # ログインの戻り先（web と同じオリジン）。--api-only では API を直接叩くので API のオリジンにする。
  LOGIN_ORIGIN=http://localhost:3000
  [ "$API_ONLY" = 1 ] && LOGIN_ORIGIN=http://localhost:${API_PORT}
  REDIS_SUFFIX=${REDIS_DB:+/$REDIS_DB}
  # 生成するスクリプトへ値を埋め込むので、シェルの構文としてクォートする（& などを含んでも壊れない）。
  Q_TABULAR_RUNNER=$(printf '%q' "$TABULAR_RUNNER")
  Q_S3_INTERNAL=$(printf '%q' "$EFF_S3_INTERNAL")
  Q_S3_PUBLIC=$(printf '%q' "$EFF_S3_PUBLIC")
  Q_S3_BUCKET=$(printf '%q' "$EFF_S3_BUCKET")
  Q_WORKER_URL=$(printf '%q' "$EFF_WORKER_URL")
  if [ -n "$EFF_CORS" ]; then
    CORS_LINE="export SHIKI__STORAGE__S3__CORS_ALLOWED_ORIGINS=$(printf '%q' "$EFF_CORS")"
  else
    # 呼び出し元の環境に残っていると引き継いでしまうので、明示的に消す。
    CORS_LINE="unset SHIKI__STORAGE__S3__CORS_ALLOWED_ORIGINS  # バケット側の既存の設定に任せる（SHIKI_DEV_S3_CORS_ALLOWED_ORIGINS で設定できる）"
  fi

  # コンテナ内部ホスト名をホストの公開ポートへ読み替える。
  # env 名は deploy/compose/docker-compose.yml の shiki-server 節が正（食い違ったら合わせる）。
  cat > "$RUNDIR/run-server.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
cd "$ROOT"
export SHIKI__SERVER__HOST=0.0.0.0
export SHIKI__SERVER__PORT=${API_PORT}
export SHIKI__DATABASE__URL=postgres://postgres:postgres@localhost:${PG_PORT}/${DB_NAME}
export SHIKI__AUTH__ISSUER=http://localhost:${KC_PORT}/realms/shiki
export SHIKI__AUTH__INTERNAL_BASE_URL=http://localhost:${KC_PORT}/realms/shiki
export SHIKI__AUTH__AUDIENCE=shiki-api
export SHIKI__AUTH__TENANCY=multi
export SHIKI__AUTH__TENANT_ID=default
export SHIKI__AUTH__CLIENT_ID=shiki-web
export SHIKI__AUTH__CLIENT_SECRET=shiki-web-dev-secret
export SHIKI__AUTH__PROVISIONER_CLIENT_ID=shiki-provisioner
export SHIKI__AUTH__PROVISIONER_CLIENT_SECRET=shiki-provisioner-dev-secret
# web と同一オリジンの callback。:8080 にすると callback 後に backend ルートへ 303 して落ちる。
export SHIKI__AUTH__REDIRECT_URI=${LOGIN_ORIGIN}/auth/callback
export SHIKI__AUTH__POST_LOGOUT_REDIRECT_URI=${LOGIN_ORIGIN}/
export SHIKI__SESSION__REDIS_URL=redis://localhost:${REDIS_PORT}${REDIS_SUFFIX}
export SHIKI__SESSION__SECURE=false
export SHIKI__AUTHZ__BASE_URL=http://localhost:${FGA_PORT}
export SHIKI__AUTHZ__STORE_NAME=${STORE_NAME}
export SHIKI__STORAGE__BACKEND=s3
export SHIKI__STORAGE__S3__INTERNAL_ENDPOINT=${Q_S3_INTERNAL}
export SHIKI__STORAGE__S3__PUBLIC_ENDPOINT=${Q_S3_PUBLIC}
export SHIKI__STORAGE__S3__BUCKET=${Q_S3_BUCKET}
# ブラウザ直 PUT/GET のバケット CORS。設定しないと put_bucket_cors が呼ばれず
# Drive のアップロードが CORS で失敗する（MinIO 時代はサーバ側 env で代替されていた）。
# figment は env 値をブラケット記法で配列として読む（裸のカンマ区切りは不可）。
${CORS_LINE}
# **資格情報はこのファイルに書かない。** 起動時に dev-up.sh がプロセスの環境で渡す
# （外部の S3 の資格情報が /tmp に平文で残るのを避ける。値をシェルとして再解析もしない）。
# **この heredoc は unquoted なので、コメント行まで生成時に展開される。**
# コマンド置換や位置パラメータをここに書くと（コメントの中であっても）
# dev-up.sh 自身のシェルで評価され、set -u のもと未設定変数で死ぬ。
# 実行時に評価したいものはドル記号をバックスラッシュでエスケープするか、heredoc の外で解決して値を渡すこと。
export SHIKI__STORAGE__S3__ACCESS_KEY="\${SHIKI_DEV_RUN_S3_ACCESS_KEY:?dev-up.sh から起動してください}"
export SHIKI__STORAGE__S3__SECRET_KEY="\${SHIKI_DEV_RUN_S3_SECRET_KEY:?dev-up.sh から起動してください}"
export SHIKI__GATEWAY__ENABLED=true
# native の第2/第3リスナは compose の shiki-server（8090/8091 を publish）と衝突しない
# 18090/18091 を使う。web 側には NEXT_PUBLIC_GATEWAY_ORIGIN / NEXT_PUBLIC_B1_ORIGIN で
# 同じ値を渡す。
export SHIKI__GATEWAY__PORT=${GW_PORT}
export SHIKI__GATEWAY__B1_PORT=${B1_PORT}
export SHIKI__GATEWAY__PUBLIC_ORIGIN=http://localhost:${GW_PORT}
export SHIKI__GATEWAY__WEB_ORIGIN=http://localhost:3000
# /builtin/* の配信元（スライドエディタ砂箱）。未設定だと 404 になり、スライドエディタが
# 閲覧フォールバックに落ちるため、変更が反映されない画面をスクショしてしまう。
export SHIKI__GATEWAY__BUILTIN_DIR=${ROOT}/web/editor-sandbox/dist
export SHIKI__CHAT__ENABLED=true
# 回答の ui ブロック（Yui）の図（#530）。compose の既定と揃える。
export SHIKI__CHAT__YUI=true
export SHIKI__WORKFLOW__ENABLED=true
# data の一覧の続きの位置の鍵（無いと起動が止まる）は開発用の乱数鍵にする（compose の shiki-server と同じ既定）。
# CSV の集計は DuckDB の隔離ランナーで動かす（無いと csv.* と CSV の画面が失敗する）。ランナーは workspace の外の
# クレートなので dev-up.sh が個別にビルドし、絶対パスで渡す（既定の bare 名は PATH で探し、native では見つからない）。
# どちらも呼び出し側の env にあればそれを使う（BUG-22）。
export SHIKI__DATA__EPHEMERAL_CURSOR_KEY=${SHIKI__DATA__EPHEMERAL_CURSOR_KEY:-true}
export SHIKI__TABULAR__RUNNER_PATH=${Q_TABULAR_RUNNER}
# 既定は stub（E2E の前提）。実 LLM で確かめるときは呼び出し側で BACKEND 等を渡す
# （シェルに残った値を拾うことがあるので、起動後のまとめに LLM の種類を出して警告する）。
export SHIKI__LLM__BACKEND=${SHIKI__LLM__BACKEND:-stub}
export SHIKI__WEBSEARCH__BACKEND=stub
export SHIKI__RAG__ENABLED=$([ "$WITH_RAG" = 1 ] && echo true || echo false)
export SHIKI__RAG__QDRANT_URL=http://localhost:${QDRANT_PORT}
export SHIKI__RAG__WORKER_BASE_URL=${Q_WORKER_URL}
# Qdrant の API キーと worker の共有トークン（#812）も資格情報と同じくプロセスの環境で渡す。
export SHIKI__RAG__QDRANT_API_KEY="\${SHIKI_DEV_RUN_QDRANT_API_KEY:-}"
export SHIKI__RAG__WORKER_AUTH_TOKEN="\${SHIKI_DEV_RUN_WORKER_TOKEN:-}"
${EMBED_LINE}
export SHIKI__RAG__INDEX_DATA_DIR="$RUNDIR/index"
export SHIKI__CHAT__SANDBOX_ENDPOINT=http://localhost:${SANDBOX_PORT}
export SHIKI__OFFICE__ENABLED=$([ "$WITH_OFFICE" = 1 ] && echo true || echo false)
export SHIKI__OFFICE__COLLABORA_BASE_URL=http://localhost:${COLLABORA_PORT}
export SHIKI__OFFICE__WEB_ORIGIN=http://localhost:3000
# dev シードが無いとログイン後に何も無い。
export SHIKI_DEV_SEED=true
export RUST_LOG=info,shiki_api=debug,authz=debug
exec "${SERVER_EXE}"
EOF
  chmod +x "$RUNDIR/run-server.sh"
  mkdir -p "$RUNDIR/index"

  # compose の shiki-server が :8080 を握っていると native は bind できない。
  # fuser で殺すと docker-proxy を落とすことになるので、明示的に止めてもらう。
  if [ -n "$(docker compose -f deploy/compose/docker-compose.yml ps -q shiki-server 2>/dev/null)" ]; then
    err "compose の shiki-server が起動中です（:8080 が埋まっています）。"
    err "  native で使う: (cd deploy/compose && docker compose stop shiki-server)"
    err "  compose のまま使う: $0 --compose"
    exit 1
  fi

  # 別 worktree / 別クローンのサーバが :8080 を握っている場合、下の pgrep（このリポジトリの
  # パスにアンカー）では検出できない。bind 失敗を「別プロセスの 200」で成功と誤認しないよう、
  # 自分のものでないリスナがいたら先に止めてもらう。
  if command -v fuser >/dev/null 2>&1 && fuser "${API_PORT}/tcp" >/dev/null 2>&1; then
    holder=$(fuser "${API_PORT}/tcp" 2>/dev/null | tr -d ' ')
    holder_exe=$(readlink -f "/proc/${holder%% *}/exe" 2>/dev/null || echo "?")
    case "$holder_exe" in
      "$SERVER_EXE") : ;;   # 自分のもの。下で停止する。
      *) err "既に :${API_PORT} を使用中のプロセスがあります（$holder_exe）。停止してから再実行してください。"
         exit 1 ;;
    esac
  fi

  # 既存の native サーバが残っていると、新しいプロセスは bind に失敗するのに終了せず、
  # chat/workflow ワーカーだけが二重に走って同じジョブキューを取り合う。さらに古い方が
  # /healthz に応答するため「起動成功」と誤判定される。先に確実に止める。
  # --run-label・--server-bin のときは pgrep を使わない（同じバイナリで動く別のサーバーまで止めるため）。
  # 止めるのは、自分の PID ファイルのものと、自分のポートを握る自分のバイナリだけ。
  OLD_PIDS=""
  if [ -n "$RUN_LABEL" ] || [ -n "$SERVER_BIN" ]; then
    stop_own_server "$API_PORT" "$SERVER_EXE"
  else
    OLD_PIDS=$(pgrep -f "^$ROOT/target/debug/shiki-server$" 2>/dev/null || true)
  fi
  if [ -n "$OLD_PIDS" ]; then
    say "   既存の shiki-server を停止します: $(printf '%s' "$OLD_PIDS" | tr '\n' ' ')"
    # shellcheck disable=SC2086
    kill -TERM $OLD_PIDS 2>/dev/null || true
    for _ in $(seq 1 20); do
      pgrep -f "^$ROOT/target/debug/shiki-server$" >/dev/null 2>&1 || break
      sleep 1
    done
    # shellcheck disable=SC2086
    pgrep -f "^$ROOT/target/debug/shiki-server$" >/dev/null 2>&1 && kill -KILL $OLD_PIDS 2>/dev/null || true
  fi

  # ビルドは前景で回す。cargo run に含めると「ビルド中」と「起動失敗」が
  # ヘルスチェックのタイムアウトとして区別できなくなる（cold build は 10 分超える）。
  if [ -n "$SERVER_BIN" ]; then
    say "   ビルドを飛ばします（--server-bin $SERVER_BIN）"
  else
    say "   ビルド中（cold なら数分〜十数分かかります）…"
    if ! cargo build -p shiki-api --bin shiki-server; then
      err "shiki-server のビルドに失敗しました。"
      exit 1
    fi
  fi

  # CSV の隔離ランナー（CI の tabular-runner ジョブと同じ release ビルド）。呼び出し側がランナーもサーバーも渡さない
  # ときだけ、毎回 cargo に任せる（変更が無ければすぐ終わり、crates/tabular の変更はランナーにも入る）。DuckDB を同梱
  # するため、初回だけ数分かかる。渡されたもの（版のサーバーの評価など・--server-bin）は作業ツリーからビルドし直さない。
  if [ -z "$TABULAR_RUNNER_FROM_ENV" ] && [ -z "$SERVER_BIN" ]; then
    if [ ! -x "$TABULAR_RUNNER" ]; then
      say "   CSV ランナー（shiki-tabular-runner）をビルド中（初回は数分）…"
    fi
    if ! cargo build --manifest-path "$ROOT/crates/tabular/runner/Cargo.toml" --release; then
      err "shiki-tabular-runner のビルドに失敗しました（CSV の表示・クエリは使えません）。"
      exit 1
    fi
  elif [ ! -x "$TABULAR_RUNNER" ]; then
    err "   ⚠️ CSV の集計の隔離ランナーがありません: $TABULAR_RUNNER"
    err "      このままでは csv.* のブロックと CSV の画面の SQL が失敗します。SHIKI__TABULAR__RUNNER_PATH で渡すか、"
    err "      次でビルドしてください: cargo build --locked --release --manifest-path crates/tabular/runner/Cargo.toml"
  fi

  apply_store_switch
  SHIKI_DEV_RUN_S3_ACCESS_KEY="$EFF_S3_AK" SHIKI_DEV_RUN_S3_SECRET_KEY="$EFF_S3_SK" \
    SHIKI_DEV_RUN_QDRANT_API_KEY="$QDRANT_KEY" SHIKI_DEV_RUN_WORKER_TOKEN="$EFF_WORKER_TOKEN" \
    setsid nohup "$RUNDIR/run-server.sh" > "$SERVER_LOG" 2>&1 < /dev/null &
  echo $! > "$SERVER_PID"
  disown || true
  if [ -n "$RUN_LABEL" ]; then
    printf 'port=%s\nexe=%s\n' "$API_PORT" "$SERVER_EXE" > "$SERVER_ENV"
  fi
  say "   起動中… ログ: $SERVER_LOG"
  ok=""
  for _ in $(seq 1 40); do
    # /healthz は依存に触れず常に 200 を返すため、「応答した = 自分が起動した」ではない。
    # 別 worktree の古いサーバが :8080 を握っていると、こちらは bind 失敗で死んでいるのに
    # 相手の 200 を見て成功と誤判定する。起動した PID の生存を必ず併せて確認する。
    if ! kill -0 "$(cat "$SERVER_PID")" 2>/dev/null; then
      err "shiki-server が終了しました。tail -50 $SERVER_LOG"
      grep -iE "bind|address already in use|アドレス" "$SERVER_LOG" | tail -5 >&2 || true
      tail -20 "$SERVER_LOG" >&2
      exit 1
    fi
    curl -fsS --max-time 2 "http://localhost:${API_PORT}/healthz" >/dev/null 2>&1 && { ok=1; break; }
    sleep 3
  done
  if [ -z "$ok" ]; then
    # よくある失敗は「別ブランチの migration が当たった DB」。原因を特定して手当てを示す。
    if grep -q "previously applied but has been modified\|VersionMismatch" "$SERVER_LOG" 2>/dev/null; then
      err ""
      err "❌ migration の checksum 不一致で起動できません（別ブランチの migration が当たった DB です）。"
      err "   dev DB は使い捨てなので作り直して構いません:"
      err "     $0 --reset-db"
      err "   （SHIKI_DEV_SEED=true が起動時にシードを再投入します）"
      err ""
      grep -A3 "Caused by" "$SERVER_LOG" | head -8 >&2
      exit 1
    fi
    err "shiki-server が :${API_PORT} で応答しません。tail -50 $SERVER_LOG"
    tail -30 "$SERVER_LOG" >&2
    exit 1
  fi
  say "   [ok] shiki-server (native)"
fi

# --- --api-only: web を起動しない（API を直接叩く・版のサーバー） ---
if [ "$API_ONLY" = 1 ]; then
  say ""
  say "================================"
  say "  api        http://localhost:${API_PORT}   （/healthz・web は起動していません）"
  say "  keycloak   http://localhost:8081   （ログインの戻り先: ${LOGIN_ORIGIN}/auth/callback）"
  say "  DB=${DB_NAME}  store=${STORE_NAME}  bucket=${EFF_S3_BUCKET}  redis=${REDIS_DB:-（番号なし）}  gateway=${GW_PORT},${B1_PORT}"
  say "  ログ       $SERVER_LOG"
  LLM_BACKEND=${SHIKI__LLM__BACKEND:-stub}
  say "  LLM=${LLM_BACKEND}$([ "$LLM_BACKEND" = stub ] || echo "  ⚠️ stub ではない（実プロバイダへ課金される）")  SHIKI_CONFIG=${SHIKI_CONFIG:-（無し）}"
  say "  停止:  $0 --down${RUN_LABEL:+ --run-label $RUN_LABEL}"
  say "================================"
  exit 0
fi

# --- 3. web (:3000) ---
say "== 3. web を :3000 で起動 =="

# 新しい worktree では node_modules も生成型も無い（web/src/generated/ は .gitignore 済み）。
# どちらが欠けても `next dev` はコンパイルできないので、先に用意する。
if [ -n "$WEB_INSTALL_PID" ]; then
  say "   先行実行した pnpm install の完了を待ちます…"
  wait "$WEB_INSTALL_PID" || { err "pnpm install に失敗しました。tail -30 $RUNDIR/pnpm-install.log"; tail -30 "$RUNDIR/pnpm-install.log" >&2; exit 1; }
fi
if [ ! -d web/node_modules ]; then
  say "   node_modules が無いので pnpm install します（数分）…"
  ( cd web && pnpm install --frozen-lockfile ) || { err "pnpm install に失敗しました。"; exit 1; }
fi
# 生成物を足したら、ここにも足す（既存の checkout は api.d.ts だけあって新しい生成物が無いことがある）。
if [ ! -f web/src/generated/api.d.ts ] || [ ! -f web/src/generated/audit-actions.ts ]; then
  say "   生成型が無いので pnpm gen:api します（codegen が正・手書き型を作らない）…"
  ( cd web && pnpm gen:api ) || { err "pnpm gen:api に失敗しました。"; exit 1; }
fi
# node_modules が無くて step 2 でスキップした場合はここで砂箱を建てる。
# ServeDir はリクエスト時にディスクを読むので、サーバの再起動は要らない。
if [ "$MODE" != compose ] && sandbox_needs_build; then
  say "   スライドエディタ砂箱をビルドします（/builtin 配信用）…"
  ( cd web && pnpm build:editor-sandbox ) || err "   ⚠️  砂箱のビルドに失敗（/builtin は 404 のまま）"
fi

# 稼働中の next dev と同じ worktree で build すると .next を壊すため、
# 既存の :3000 を先に落としてから起動する。
kill_port_if_ours 3000 "$ROOT" || exit 1
sleep 1

cat > "$RUNDIR/run-web.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
cd "$ROOT/web"
export BACKEND_ORIGIN=http://localhost:8080
export NEXT_PUBLIC_GATEWAY_ORIGIN=${GW_ORIGIN}
export NEXT_PUBLIC_B1_ORIGIN=${B1_ORIGIN}
exec pnpm exec next dev -p 3000
EOF
chmod +x "$RUNDIR/run-web.sh"
setsid nohup "$RUNDIR/run-web.sh" > "$WEB_LOG" 2>&1 < /dev/null &
echo $! > "$WEB_PID"
disown || true

ok=""
for _ in $(seq 1 60); do
  code=$(curl -fsS --max-time 3 -o /dev/null -w '%{http_code}' http://localhost:3000/ 2>/dev/null || true)
  # ルートは未ログインだと /login へリダイレクトするため 2xx/3xx を生存とみなす。
  case "$code" in 200|302|307) ok=1; break ;; esac
  kill -0 "$(cat "$WEB_PID")" 2>/dev/null || { err "web が終了しました。tail -50 $WEB_LOG"; tail -30 "$WEB_LOG" >&2; exit 1; }
  sleep 3
done
[ -n "$ok" ] || { err "web が :3000 で応答しません。tail -50 $WEB_LOG"; exit 1; }
say "   [ok] web"

say ""
say "================================"
say "  web        http://localhost:3000   （ログイン: alice / password）"
say "  api        http://localhost:8080   （/healthz）"
say "  keycloak   http://localhost:8081"
say "  ログ       $SERVER_LOG"
say "             $WEB_LOG"
say ""
# サーバの環境ファイルと同じ既定（stub）で表示する（このシェルでは未設定のことがある）。
LLM_BACKEND=${SHIKI__LLM__BACKEND:-stub}
say "  LLM=${LLM_BACKEND}$([ "$LLM_BACKEND" = stub ] || echo "  ⚠️ stub ではない（E2E は stub が前提・実プロバイダへ課金される）")"
say "  RAG=$([ "$WITH_RAG" = 1 ] && { [ "$RAG_REMOTE" = 1 ] && echo "on(worker と S3 は外部)" || echo on; } || echo off)  sandbox=$([ "$WITH_SANDBOX" = 1 ] && echo on || echo off)  office=$([ "$WITH_OFFICE" = 1 ] && echo on || echo off)  （必要なら --rag / --rag-remote / --sandbox / --office）"
say ""
say "  E2E:  cd web && E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/<x>.spec.ts"
say "  停止:  $0 --down"
say "================================"
