#!/usr/bin/env bash
# dev-up.sh — 変更を目で確認できる状態まで環境を起動する。
#
#   使い方:
#     dev-up.sh [--compose] [--rag] [--sandbox] [--office] [--reset-db]
#     dev-up.sh --down     # 起動したものを止める
#     dev-up.sh --status   # 生存確認だけする
#
#   --reset-db: compose の dev DB 'shiki' を DROP/CREATE してから起動する（破壊操作）。
#               別ブランチの migration が当たって checksum 不一致になった時に使う。
#
#   既定（native モード）:
#     compose で依存だけ起動（postgres/keycloak/openfga/redis/minio）
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
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }
say() { printf '%s\n' "$*"; }

MODE=native
RESET_DB=0
WITH_RAG=0
WITH_SANDBOX=0
WITH_OFFICE=0
ACTION=up

while [ $# -gt 0 ]; do
  case "$1" in
    --compose) MODE=compose ;;
    --reset-db) RESET_DB=1 ;;
    --rag)     WITH_RAG=1 ;;
    --sandbox) WITH_SANDBOX=1 ;;
    --office)  WITH_OFFICE=1 ;;
    --down)    ACTION=down ;;
    --status)  ACTION=status ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) err "未知の引数: $1"; exit 2 ;;
  esac
  shift
done

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

command -v docker >/dev/null 2>&1 || { err "docker が見つかりません。"; exit 2; }
command -v curl >/dev/null 2>&1 || { err "curl が見つかりません。"; exit 2; }

RUNDIR="${TMPDIR:-/tmp}/shiki-dev-up-$(id -u)"
mkdir -p "$RUNDIR"
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
  api=$(http_code http://localhost:8080/healthz)
  web=$(http_code http://localhost:3000/)
  kc=$(http_code http://localhost:8081/realms/shiki)
  say "shiki-server (:8080/healthz): $api"
  say "web          (:3000):         $web"
  say "keycloak     (:8081):         $kc"
  [ "$api" = "200" ] && { [ "$web" = "200" ] || [ "$web" = "302" ] || [ "$web" = "307" ]; }
}

# --- 停止 ---
stop_all() {
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
    fuser -k 3000/tcp >/dev/null 2>&1 || true
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

# --- 1. compose 依存サービス ---
DEPS="postgres keycloak openfga redis minio"
[ "$WITH_RAG" = 1 ] && DEPS="$DEPS qdrant ingestion-worker"
[ "$WITH_SANDBOX" = 1 ] && DEPS="$DEPS sandbox-orchestrator"
[ "$WITH_OFFICE" = 1 ] && DEPS="$DEPS collabora"

say "== 1. compose 依存サービスを起動 =="
say "   $DEPS"

# web の依存導入は compose 起動にも cargo ビルドにも依存しないので、先に背後で始めておく
# （直列だと初回の実時間が「compose 待ち ＋ cold build ＋ install」の総和になる）。
WEB_INSTALL_PID=""
if [ ! -d web/node_modules ]; then
  say "   （pnpm install を背後で先行実行します）"
  ( cd web && pnpm install --frozen-lockfile ) > "$RUNDIR/pnpm-install.log" 2>&1 &
  WEB_INSTALL_PID=$!
fi

(
  cd deploy/compose
  [ -f .env ] || cp .env.example .env
  # shellcheck disable=SC2086
  docker compose up -d $DEPS
  for svc in $DEPS; do
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

# --- 1.5 dev DB のリセット（明示 opt-in） ---
# 別ブランチの migration が当たった compose DB は checksum 不一致で起動を止める
# （"migration N was previously applied but has been modified"）。dev DB は使い捨てなので
# 作り直してよいが、破壊操作なので必ず明示フラグでのみ行う。
reset_dev_db() {
  err "⚠️  compose の dev DB 'shiki' を DROP して作り直します（データは失われます）。"
  docker compose -f deploy/compose/docker-compose.yml exec -T postgres \
    psql -U postgres -c "DROP DATABASE IF EXISTS shiki WITH (FORCE);" -c "CREATE DATABASE shiki;"
  say "   [ok] dev DB をリセットしました（SHIKI_DEV_SEED=true が再投入します）"
}
[ "$RESET_DB" = 1 ] && reset_dev_db

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
  say "== 2. shiki-server を cargo run で起動（:8080・native） =="

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
  MINIO_PORT=$(host_port minio 9000 9000)
  QDRANT_PORT=$(host_port qdrant 6333 6333)
  # 任意サービス（フラグを付けた時だけ起動している）。未起動ならフォールバック値のまま。
  WORKER_PORT=$(host_port ingestion-worker 8000 8000)
  SANDBOX_PORT=$(host_port sandbox-orchestrator 50000 50000)
  COLLABORA_PORT=$(host_port collabora 9980 9980)
  # native 専用のゲートウェイポート（compose の publish と衝突させない）。
  GW_PORT=18090
  B1_PORT=18091
  GW_ORIGIN=http://localhost:$GW_PORT
  B1_ORIGIN=http://localhost:$B1_PORT

  # /builtin/slide-editor の配信元。未ビルドだと 404 → スライドエディタが閲覧
  # フォールバックに落ち、変更が反映されない画面をスクショすることになる。
  if [ ! -f web/editor-sandbox/dist/slide-editor.html ]; then
    if [ -d web/node_modules ]; then
      say "   スライドエディタ砂箱をビルドします（/builtin 配信用）…"
      ( cd web && pnpm build:editor-sandbox ) || err "   ⚠️  砂箱のビルドに失敗（/builtin は 404 のまま）"
    else
      say "   （node_modules 未導入のため砂箱ビルドは web 起動後に実施）"
    fi
  fi
  say "   依存ポート: pg=$PG_PORT kc=$KC_PORT fga=$FGA_PORT redis=$REDIS_PORT minio=$MINIO_PORT qdrant=$QDRANT_PORT"

  # コンテナ内部ホスト名をホストの公開ポートへ読み替える。
  # env 名は deploy/compose/docker-compose.yml の shiki-server 節が正（食い違ったら合わせる）。
  cat > "$RUNDIR/run-server.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
cd "$ROOT"
export SHIKI__SERVER__HOST=0.0.0.0
export SHIKI__SERVER__PORT=8080
export SHIKI__DATABASE__URL=postgres://postgres:postgres@localhost:${PG_PORT}/shiki
export SHIKI__AUTH__ISSUER=http://localhost:${KC_PORT}/realms/shiki
export SHIKI__AUTH__INTERNAL_BASE_URL=http://localhost:${KC_PORT}/realms/shiki
export SHIKI__AUTH__AUDIENCE=shiki-api
export SHIKI__AUTH__TENANCY=multi
export SHIKI__AUTH__TENANT_ID=default
export SHIKI_DEV_ALLOW_MULTI_TENANT=true
export SHIKI__AUTH__CLIENT_ID=shiki-web
export SHIKI__AUTH__CLIENT_SECRET=shiki-web-dev-secret
export SHIKI__AUTH__PROVISIONER_CLIENT_ID=shiki-provisioner
export SHIKI__AUTH__PROVISIONER_CLIENT_SECRET=shiki-provisioner-dev-secret
# web と同一オリジンの callback。:8080 にすると callback 後に backend ルートへ 303 して落ちる。
export SHIKI__AUTH__REDIRECT_URI=http://localhost:3000/auth/callback
export SHIKI__AUTH__POST_LOGOUT_REDIRECT_URI=http://localhost:3000/
export SHIKI__SESSION__REDIS_URL=redis://localhost:${REDIS_PORT}
export SHIKI__SESSION__SECURE=false
export SHIKI__AUTHZ__BASE_URL=http://localhost:${FGA_PORT}
export SHIKI__AUTHZ__STORE_NAME=shiki
export SHIKI__STORAGE__BACKEND=minio
export SHIKI__STORAGE__S3__INTERNAL_ENDPOINT=http://localhost:${MINIO_PORT}
export SHIKI__STORAGE__S3__PUBLIC_ENDPOINT=http://localhost:${MINIO_PORT}
export SHIKI__STORAGE__S3__BUCKET=shiki-blobs
export SHIKI__STORAGE__S3__ACCESS_KEY=minioadmin
export SHIKI__STORAGE__S3__SECRET_KEY=minioadmin
export SHIKI__GATEWAY__ENABLED=true
# compose の ingestion-worker が 127.0.0.1:8090 を publish するため、native の第2/第3
# リスナは 8090/8091 を避ける（--rag 併用時に EADDRINUSE で起動できなくなる）。
# web 側には NEXT_PUBLIC_GATEWAY_ORIGIN / NEXT_PUBLIC_B1_ORIGIN で同じ値を渡す。
export SHIKI__GATEWAY__PORT=${GW_PORT}
export SHIKI__GATEWAY__B1_PORT=${B1_PORT}
export SHIKI__GATEWAY__PUBLIC_ORIGIN=http://localhost:${GW_PORT}
export SHIKI__GATEWAY__WEB_ORIGIN=http://localhost:3000
# /builtin/* の配信元（スライドエディタ砂箱）。未設定だと 404 になり、スライドエディタが
# 閲覧フォールバックに落ちるため、変更が反映されない画面をスクショしてしまう。
export SHIKI__GATEWAY__BUILTIN_DIR=${ROOT}/web/editor-sandbox/dist
export SHIKI__CHAT__ENABLED=true
export SHIKI__WORKFLOW__ENABLED=true
export SHIKI__LLM__BACKEND=stub
export SHIKI__WEBSEARCH__BACKEND=stub
export SHIKI__RAG__ENABLED=$([ "$WITH_RAG" = 1 ] && echo true || echo false)
export SHIKI__RAG__QDRANT_URL=http://localhost:${QDRANT_PORT}
export SHIKI__RAG__WORKER_BASE_URL=http://localhost:${WORKER_PORT}
export SHIKI__RAG__INDEX_DATA_DIR="$RUNDIR/index"
export SHIKI__CHAT__SANDBOX_ENDPOINT=http://localhost:${SANDBOX_PORT}
export SHIKI__OFFICE__ENABLED=$([ "$WITH_OFFICE" = 1 ] && echo true || echo false)
export SHIKI__OFFICE__COLLABORA_BASE_URL=http://localhost:${COLLABORA_PORT}
export SHIKI__OFFICE__WEB_ORIGIN=http://localhost:3000
# dev シードが無いとログイン後に何も無い。
export SHIKI_DEV_SEED=true
export RUST_LOG=info,shiki_api=debug,authz=debug
exec "$ROOT/target/debug/shiki-server"
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
  if command -v fuser >/dev/null 2>&1 && fuser 8080/tcp >/dev/null 2>&1; then
    holder=$(fuser 8080/tcp 2>/dev/null | tr -d ' ')
    holder_exe=$(readlink -f "/proc/${holder%% *}/exe" 2>/dev/null || echo "?")
    case "$holder_exe" in
      "$ROOT/target/debug/shiki-server") : ;;   # 自分のもの。下で停止する。
      *) err "既に :8080 を使用中のプロセスがあります（$holder_exe）。停止してから再実行してください。"
         exit 1 ;;
    esac
  fi

  # 既存の native サーバが残っていると、新しいプロセスは bind に失敗するのに終了せず、
  # chat/workflow ワーカーだけが二重に走って同じジョブキューを取り合う。さらに古い方が
  # /healthz に応答するため「起動成功」と誤判定される。先に確実に止める。
  OLD_PIDS=$(pgrep -f "^$ROOT/target/debug/shiki-server$" 2>/dev/null || true)
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
  say "   ビルド中（cold なら数分〜十数分かかります）…"
  if ! cargo build -p shiki-api --bin shiki-server; then
    err "shiki-server のビルドに失敗しました。"
    exit 1
  fi

  setsid nohup "$RUNDIR/run-server.sh" > "$SERVER_LOG" 2>&1 < /dev/null &
  echo $! > "$SERVER_PID"
  disown || true
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
    curl -fsS --max-time 2 http://localhost:8080/healthz >/dev/null 2>&1 && { ok=1; break; }
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
    err "shiki-server が :8080 で応答しません。tail -50 $SERVER_LOG"
    tail -30 "$SERVER_LOG" >&2
    exit 1
  fi
  say "   [ok] shiki-server (native)"
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
if [ ! -f web/src/generated/api.d.ts ]; then
  say "   生成型が無いので pnpm gen:api します（codegen が正・手書き型を作らない）…"
  ( cd web && pnpm gen:api ) || { err "pnpm gen:api に失敗しました。"; exit 1; }
fi
# node_modules が無くて step 2 でスキップした場合はここで砂箱を建てる。
# ServeDir はリクエスト時にディスクを読むので、サーバの再起動は要らない。
if [ "$MODE" != compose ] && [ ! -f web/editor-sandbox/dist/slide-editor.html ]; then
  say "   スライドエディタ砂箱をビルドします（/builtin 配信用）…"
  ( cd web && pnpm build:editor-sandbox ) || err "   ⚠️  砂箱のビルドに失敗（/builtin は 404 のまま）"
fi

# 稼働中の next dev と同じ worktree で build すると .next を壊すため、
# 既存の :3000 を先に落としてから起動する。
command -v fuser >/dev/null 2>&1 && { fuser -k 3000/tcp >/dev/null 2>&1 || true; sleep 1; }

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
say "  RAG=$([ "$WITH_RAG" = 1 ] && echo on || echo off)  sandbox=$([ "$WITH_SANDBOX" = 1 ] && echo on || echo off)  office=$([ "$WITH_OFFICE" = 1 ] && echo on || echo off)  （必要なら --rag / --sandbox / --office）"
say ""
say "  E2E:  cd web && E2E_BASE_URL=http://localhost:3000 pnpm exec playwright test e2e/<x>.spec.ts"
say "  停止:  $0 --down"
say "================================"
