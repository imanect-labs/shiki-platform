#!/usr/bin/env bash
# フルプール前提の outbox → job_queue 経路の実測（#818・docs/events/streaming-platform-evaluation.md 附録）。
#
# 何を測るか:
#   scan   … テナント数 T × outbox 総行数 N を変え、本番と同一形状のクエリ（claim / claim_undelivered /
#            mark_delivered / GC / SSE ライブテール / テナント消去）を EXPLAIN (ANALYZE, BUFFERS) で 5 回測り中央値を出す。
#   jobs   … 1 テナントの一括取込（オンボーディング）で job_queue に B 件積まれたときの jobq claim と HOL 位置。
#   emit   … 書込側（outbox INSERT）の上限 TPS と 1 イベントあたりの WAL 量。
#   mixed  … 書込 RATE ev/s を流しながら relay・台帳コンシューマ 2 本・GC を同時に回し、
#            追従できるか（残滞留）と書込→消費のレイテンシ分布を測る。
#            CONSUMERS=db   … コンシューマを待ち無しで回す（Postgres 側の上限を見る）
#            CONSUMERS=code … 本番コードの定数どおりの周期で回す（relay 500ms×64・workflow 5s×64・
#                             miniapp-functions 5s×50・GC 20×1000/60s）
#
# 前提: 専用 DB に migrations/ を全適用済み（本スクリプトは表を TRUNCATE するので共有 DB に向けないこと）。
#   docker exec shiki-postgres-1 psql -U postgres -c 'create database shiki_818'
#   for f in migrations/*.sql; do psql -q -v ON_ERROR_STOP=1 "$PGURL" -f "$f"; done
#
# 使い方:
#   PGURL=postgres://postgres:postgres@localhost:5432/shiki_818 \
#     bash docs/events/bench/outbox-pool-bench.sh {scan|jobs|emit|mixed RATE SECS|all} > results.txt
#
# 絶対値は機材と同居負荷に依存する。**N・T・RATE に対する増え方**を見るための測定である。
set -euo pipefail

PGURL="${PGURL:-postgres://postgres:postgres@localhost:5432/shiki_818}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPS="${REPS:-5}"
NODES="${NODES:-200000}"
WORK="$(mktemp -d)"
# 途中で失敗しても採れた EXPLAIN は出してから片付ける。
print_plans() {
  if [[ -s "$WORK/plans" ]]; then
    printf '\n## EXPLAIN (ANALYZE, BUFFERS)（代表ケース）\n'
    cat "$WORK/plans"
  fi
  rm -rf "$WORK"
}
trap print_plans EXIT

psql_q() { psql -X -q -v ON_ERROR_STOP=1 "$PGURL" "$@"; }
psql_t() { psql -X -q -At -v ON_ERROR_STOP=1 "$PGURL" "$@"; }

# 安全柵: 共有 DB（shiki 等）を誤って TRUNCATE しない。
case "$PGURL" in
  */shiki_*) ;;
  *) echo "PGURL は専用 DB（.../shiki_<n>）を指すこと: $PGURL" >&2; exit 2 ;;
esac

hdr() { printf '\n## %s\n' "$*"; }

# --------------------------------------------------------------------------
# データ生成
# --------------------------------------------------------------------------

# node を NODES 件（T テナントに均等割り）作る。outbox の LEFT JOIN node 相手を本番に近づけるため。
prep_nodes() {
  local t="$1"
  psql_q <<SQL
TRUNCATE node CASCADE;
DROP TABLE IF EXISTS bench_node;
CREATE TABLE bench_node (k int PRIMARY KEY, id uuid NOT NULL, org text NOT NULL, tenant_id text NOT NULL);
INSERT INTO bench_node
SELECT k, gen_random_uuid(), 'org-' || (k % $t), 'tenant-' || (k % $t)
FROM generate_series(0, $NODES - 1) k;
INSERT INTO node (id, org, tenant_id, kind, name, created_by, updated_by)
SELECT id, org, tenant_id, 'folder', 'f' || k, 'bench', 'bench' FROM bench_node;
INSERT INTO outbox_consumer (name) VALUES ('workflow'), ('miniapp-functions') ON CONFLICT DO NOTHING;
VACUUM ANALYZE node, bench_node;
SQL
}

# outbox に N 行。テール TAIL 行だけ未処理・未配送、残りは processed_at 済み＋2 台帳配送済み
# （＝GC が止まって溜まった状態。N を GC 窓の大きさと読めば GC が回っている定常状態）。
# 1 割は data サービス由来（payload.event_type 付き＝SSE ライブテールの対象）。
load_outbox() {
  local n="$1" tail="$2"
  psql_q <<SQL
TRUNCATE storage_event_outbox, outbox_delivery, job_queue, job_queue_dead RESTART IDENTITY CASCADE;
INSERT INTO storage_event_outbox (org, tenant_id, node_id, version, op, actor, trace_id, payload, processed_at)
SELECT b.org, b.tenant_id, b.id, 1, 'update', 'user-' || (g % 97), md5(g::text),
       CASE WHEN g % 10 = 0
            THEN jsonb_build_object('event_type', 'data.record.transitioned', 'table_id', md5(g::text))
            ELSE jsonb_build_object('kind', 'file', 'blob_sha256', md5(g::text) || md5((g + 1)::text), 'size', 12345)
       END,
       now()
FROM generate_series(1, $n) g JOIN bench_node b ON b.k = g % $NODES
ORDER BY g;
UPDATE storage_event_outbox SET processed_at = NULL WHERE id > $n - $tail;
INSERT INTO outbox_delivery (consumer, event_id, tenant_id)
SELECT c.name, o.id, o.tenant_id
FROM storage_event_outbox o CROSS JOIN outbox_consumer c
WHERE o.id <= $n - $tail;
VACUUM ANALYZE storage_event_outbox, outbox_delivery, job_queue;
SQL
}

# --------------------------------------------------------------------------
# 計測ヘルパ: BEGIN; EXPLAIN (ANALYZE, BUFFERS) ...; ROLLBACK を REPS 回、Execution Time の中央値。
# PLAN=1 なら最後の計画も出力する。
# --------------------------------------------------------------------------
measure() {
  local label="$1" sql="$2" times=() plan="" i
  for ((i = 0; i < REPS; i++)); do
    plan="$(psql_t <<SQL
BEGIN;
EXPLAIN (ANALYZE, BUFFERS) $sql;
ROLLBACK;
SQL
)"
    times+=("$(grep -oP 'Execution Time: \K[0-9.]+' <<<"$plan")")
  done
  local med
  med="$(printf '%s\n' "${times[@]}" | sort -g | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')"
  local bufs
  bufs="$(grep -m1 -oP 'Buffers: \K.*' <<<"$plan" || true)"
  printf '| %s | %s ms | %s |\n' "$label" "$med" "$bufs"
  if [[ "${PLAN:-0}" == 1 ]]; then
    printf '\n### %s — %s\n\n```\n%s\n```\n' "${BCASE:-}" "$label" "$plan" >>"$WORK/plans"
  fi
}

CLAIM_RELAY="SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id,
  o.payload, o.created_at, coalesce(n.system, false) AS system
  FROM storage_event_outbox o LEFT JOIN node n ON n.id = o.node_id
  WHERE o.processed_at IS NULL ORDER BY o.id FOR UPDATE OF o SKIP LOCKED LIMIT 64"
CLAIM_LEDGER="SELECT o.id, o.org, o.tenant_id, o.node_id, o.version, o.op, o.actor, o.trace_id,
  o.payload, o.created_at, coalesce(n.system, false) AS system
  FROM storage_event_outbox o LEFT JOIN node n ON n.id = o.node_id
  WHERE NOT EXISTS (SELECT 1 FROM outbox_delivery d WHERE d.consumer = 'workflow' AND d.event_id = o.id)
  ORDER BY o.id FOR UPDATE OF o SKIP LOCKED LIMIT 64"
GC_BATCH="DELETE FROM storage_event_outbox WHERE id IN (
  SELECT o.id FROM storage_event_outbox o
  WHERE o.processed_at IS NOT NULL
    AND (SELECT count(*) FROM outbox_delivery d
         WHERE d.event_id = o.id AND d.consumer = ANY(ARRAY['miniapp-functions','workflow'])) >= 2
  ORDER BY o.id LIMIT 1000 FOR UPDATE SKIP LOCKED)"

scan_one() {
  local n="$1" t="$2"
  load_outbox "$n" 64
  local maxid active
  maxid="$(psql_t -c 'SELECT max(id) FROM storage_event_outbox')"
  # 「活動中」の代表テナント（T=1 なら tenant-0 しかいない）。
  active="tenant-$((t > 1 ? 1 : 0))"
  BCASE="scan N=$n T=$t"
  hdr "$BCASE"
  echo "| query | median | buffers (last run) |"
  echo "|---|---:|---|"
  measure "claim（RAG relay・processed_at 経路）" "$CLAIM_RELAY"
  measure "claim_undelivered（workflow・追いついている台帳）" "$CLAIM_LEDGER"
  measure "mark_delivered 64 件" "INSERT INTO outbox_delivery (consumer, event_id, tenant_id)
    SELECT 'workflow', o.id, o.tenant_id FROM storage_event_outbox o
    WHERE o.id = ANY(ARRAY(SELECT generate_series($maxid - 63, $maxid)::bigint))
    ON CONFLICT (consumer, event_id) DO NOTHING"
  measure "GC 1 バッチ（1000 行 DELETE）" "$GC_BATCH"
  measure "latest_event_id（活動中テナント）" "SELECT COALESCE(MAX(id), 0) FROM storage_event_outbox WHERE tenant_id = '$active'"
  measure "latest_event_id（イベントの無いテナント）" "SELECT COALESCE(MAX(id), 0) FROM storage_event_outbox WHERE tenant_id = 'tenant-none'"
  measure "peek_app_events_after（SSE 1 poll・直近 1000 行窓）" "SELECT o.id, o.payload, coalesce(n.system, false)
    FROM storage_event_outbox o LEFT JOIN node n ON n.id = o.node_id
    WHERE o.tenant_id = '$active' AND o.id > $maxid - 1000 AND o.payload ? 'event_type'
    ORDER BY o.id LIMIT 500"
  measure "テナント消去（outbox 1 テナント分 DELETE・台帳は CASCADE）" "DELETE FROM storage_event_outbox WHERE tenant_id = '$active'"
  psql_t -F ' | ' <<'SQL' | sed 's/^/| /; s/$/ |/'
SELECT 'size: outbox heap / idx / delivery heap / delivery idx',
       pg_size_pretty(pg_relation_size('storage_event_outbox')) || ' / ' ||
       pg_size_pretty(pg_indexes_size('storage_event_outbox')) || ' / ' ||
       pg_size_pretty(pg_relation_size('outbox_delivery')) || ' / ' ||
       pg_size_pretty(pg_indexes_size('outbox_delivery')),
       round(pg_total_relation_size('storage_event_outbox')::numeric / greatest(count(*), 1)) || ' B/行 (outbox 全体)'
FROM storage_event_outbox;
SQL
}

cmd_scan() {
  local t n
  for t in 1 100 1000; do
    prep_nodes "$t"
    for n in 10000 1000000; do
      PLAN=$([[ $t == 1000 ]] && echo 1 || echo 0) scan_one "$n" "$t"
    done
  done
  # T=1000 で行数の伸びを細かく見る（10 万）。300 万行は docker 既定の /dev/shm（64MB）では
  # 並列クエリの共有メモリが確保できず ERROR になるため、既定では回さない（EXTRA_N="100000 3000000" で追加可）。
  for n in ${EXTRA_N:-100000}; do
    PLAN=0 scan_one "$n" 1000
  done
  truncate_all
}

# --------------------------------------------------------------------------
# jobs: 1 テナント（whale）の一括取込で rag_ingest に B 件積まれた状態の jobq claim。
# --------------------------------------------------------------------------
cmd_jobs() {
  local b
  for b in 1000 100000 1000000; do
    psql_q <<SQL
TRUNCATE job_queue, job_queue_dead RESTART IDENTITY;
-- 先頭 99% を whale（tenant-0）の一括取込、続く 1% を他テナントの通常書込とする（FIFO で後ろに並ぶ）。
-- 同じ job_queue 表を使う chat_generation（チャット生成ジョブ）を 1000 件、その後ろに積む。
INSERT INTO job_queue (queue, tenant_id, payload, max_attempts)
SELECT 'rag_ingest', CASE WHEN g <= $b * 0.99 THEN 'tenant-0' ELSE 'tenant-' || (1 + g % 999) END,
       jsonb_build_object('node_id', gen_random_uuid(), 'version', 1, 'op', 'create'), 5
FROM generate_series(1, $b) g;
INSERT INTO job_queue (queue, tenant_id, payload, max_attempts)
SELECT 'chat_generation', 'tenant-' || (g % 1000), '{}'::jsonb, 5 FROM generate_series(1, 1000) g;
VACUUM ANALYZE job_queue;
SQL
    BCASE="jobs B=$b"
    hdr "jobs B=$b（rag_ingest・99% が tenant-0）"
    echo "| query | median | buffers (last run) |"
    echo "|---|---:|---|"
    PLAN=$([[ $b == 1000000 ]] && echo 1 || echo 0) measure "jobq::claim（rag_ingest・limit 2）" "UPDATE job_queue SET visible_at = now() + 300 * interval '1 second', attempts = attempts + 1
      WHERE id IN (SELECT id FROM job_queue WHERE queue = 'rag_ingest' AND visible_at <= now()
                   ORDER BY id LIMIT 2 FOR UPDATE SKIP LOCKED)
      RETURNING id, queue, tenant_id, payload, attempts, max_attempts, trace_id, enqueued_at"
    PLAN=$([[ $b == 1000000 ]] && echo 1 || echo 0) measure "jobq::claim（chat_generation・別キュー・limit 1）" "UPDATE job_queue SET visible_at = now() + 300 * interval '1 second', attempts = attempts + 1
      WHERE id IN (SELECT id FROM job_queue WHERE queue = 'chat_generation' AND visible_at <= now()
                   ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED)
      RETURNING id"
    PLAN=0 measure "jobq::delete_tenant（小テナント）" "DELETE FROM job_queue WHERE tenant_id = 'tenant-5'"
    psql_t -F ' | ' <<'SQL' | sed 's/^/| /; s/$/ |/'
SELECT 'HOL: 小テナントの最初のジョブの前に並ぶ件数',
       (SELECT count(*) FROM job_queue
        WHERE queue = 'rag_ingest'
          AND id < (SELECT min(id) FROM job_queue WHERE queue = 'rag_ingest' AND tenant_id <> 'tenant-0'))::text,
       'size ' || pg_size_pretty(pg_total_relation_size('job_queue'));
SQL
  done
  psql_q -c 'TRUNCATE job_queue, job_queue_dead RESTART IDENTITY'
}

# --------------------------------------------------------------------------
# pgbench スクリプト（consumer 名はファイル生成時に埋め込む）
# --------------------------------------------------------------------------
write_pgbench_scripts() {
  cp "$HERE/emit.pgbench.sql" "$HERE/relay.pgbench.sql" "$HERE/gc.pgbench.sql" "$WORK/"
  sed "s/@CONSUMER@/workflow/g" "$HERE/ledger.pgbench.sql" >"$WORK/ledger-workflow.sql"
  sed "s/@CONSUMER@/miniapp-functions/g; s/LIMIT 64/LIMIT 50/" "$HERE/ledger.pgbench.sql" >"$WORK/ledger-miniapp.sql"
}

reset_stream_tables() {
  psql_q <<'SQL'
TRUNCATE storage_event_outbox, outbox_delivery, job_queue, job_queue_dead RESTART IDENTITY CASCADE;
DROP TABLE IF EXISTS bench_latency;
CREATE UNLOGGED TABLE bench_latency (consumer text NOT NULL, lat_ms double precision NOT NULL);
SQL
}

pgb() { pgbench -n "$@" "$PGURL"; }

cmd_emit() {
  write_pgbench_scripts
  local t
  for t in 1 1000; do
    prep_nodes "$t"
    reset_stream_tables
    local lsn0 lsn1 cnt
    lsn0="$(psql_t -c 'SELECT pg_current_wal_lsn()')"
    hdr "emit（outbox INSERT のみ・待ち無し・4 クライアント・20 秒・T=$t）"
    pgb -c 4 -j 2 -T 20 -D nodes="$NODES" -f "$WORK/emit.pgbench.sql" 2>&1 | grep -E 'tps|latency average'
    lsn1="$(psql_t -c 'SELECT pg_current_wal_lsn()')"
    cnt="$(psql_t -c 'SELECT count(*) FROM storage_event_outbox')"
    psql_t -c "SELECT 'WAL/イベント: ' || round(pg_wal_lsn_diff('$lsn1', '$lsn0') / $cnt) || ' B（' || $cnt || ' 件）'"
  done
}

# 書込 RATE ev/s を SECS 秒流し、コンシューマを CONSUMERS（db|code）の周期で同時に回す。
cmd_mixed() {
  local rate="$1" secs="$2" mode="${CONSUMERS:-db}"
  write_pgbench_scripts
  prep_nodes 1000
  reset_stream_tables
  hdr "mixed RATE=${rate} ev/s SECS=${secs} CONSUMERS=${mode}（T=1000）"
  echo "loadavg(start): $(cut -d' ' -f1-3 /proc/loadavg)"
  local relay_r wf_r mf_r gc_r
  if [[ "$mode" == code ]]; then
    # 本番コードの定数（rag/src/config.rs relay_poll_ms=500・workflow_runtime tick_secs=5・
    # miniapp_triggers 5s・outbox_gc 20 バッチ/60s）。pgbench -R はポアソン到着なので平均周期が一致する。
    relay_r=(-R 2) wf_r=(-R 0.2) mf_r=(-R 0.2) gc_r=(-R 0.3333)
  else
    relay_r=() wf_r=() mf_r=() gc_r=(-R 5)
  fi
  local emit_c=$(((rate + 499) / 500))
  pgb -c "$emit_c" -j "$emit_c" -R "$rate" -T "$secs" -D nodes="$NODES" -f "$WORK/emit.pgbench.sql" >"$WORK/emit.log" 2>&1 &
  local p_emit=$!
  pgb -c 1 "${relay_r[@]}" -T "$secs" -f "$WORK/relay.pgbench.sql" >"$WORK/relay.log" 2>&1 &
  local p_relay=$!
  pgb -c 1 "${wf_r[@]}" -T "$secs" -f "$WORK/ledger-workflow.sql" >"$WORK/wf.log" 2>&1 &
  local p_wf=$!
  pgb -c 1 "${mf_r[@]}" -T "$secs" -f "$WORK/ledger-miniapp.sql" >"$WORK/mf.log" 2>&1 &
  local p_mf=$!
  pgb -c 1 "${gc_r[@]}" -T "$secs" -f "$WORK/gc.pgbench.sql" >"$WORK/gc.log" 2>&1 &
  local p_gc=$!
  wait "$p_emit" "$p_relay" "$p_wf" "$p_mf" "$p_gc"
  echo "loadavg(end): $(cut -d' ' -f1-3 /proc/loadavg)"
  local f
  for f in emit relay wf mf gc; do
    printf '%-6s %s\n' "$f" "$(grep -hE '^tps|latency average' "$WORK/$f.log" | tr '\n' ' ' | sed 's/ $//')"
  done
  psql_t -F ' | ' <<'SQL' | sed 's/^/| /; s/$/ |/'
SELECT '投入イベント総数（outbox id 採番数）', last_value::text, '' FROM storage_event_outbox_id_seq
UNION ALL
SELECT 'outbox 残存行（GC 後）', count(*)::text, 'max id ' || coalesce(max(id), 0) FROM storage_event_outbox
UNION ALL
SELECT 'relay 未処理残', count(*)::text, '' FROM storage_event_outbox WHERE processed_at IS NULL
UNION ALL
SELECT 'relay 済み（job_queue 投入）', count(*)::text,
       'p50 ' || round(percentile_cont(0.5) WITHIN GROUP (ORDER BY lat)::numeric, 1) || ' ms / p99 ' ||
       round(percentile_cont(0.99) WITHIN GROUP (ORDER BY lat)::numeric, 1) || ' ms / max ' || round(max(lat)::numeric, 1) || ' ms'
FROM (SELECT extract(epoch FROM enqueued_at - (payload->>'emitted_at')::timestamptz) * 1000 AS lat FROM job_queue) j
UNION ALL
SELECT 'ledger ' || consumer || ' 配送済み', count(*)::text,
       'p50 ' || round(percentile_cont(0.5) WITHIN GROUP (ORDER BY lat_ms)::numeric, 1) || ' ms / p99 ' ||
       round(percentile_cont(0.99) WITHIN GROUP (ORDER BY lat_ms)::numeric, 1) || ' ms / max ' || round(max(lat_ms)::numeric, 1) || ' ms'
FROM bench_latency GROUP BY consumer
UNION ALL
SELECT 'ledger ' || c.name || ' 未配送残', count(o.id)::text, ''
FROM outbox_consumer c LEFT JOIN storage_event_outbox o
  ON NOT EXISTS (SELECT 1 FROM outbox_delivery d WHERE d.consumer = c.name AND d.event_id = o.id)
GROUP BY c.name;
SQL
}

truncate_all() {
  psql_q <<'SQL'
TRUNCATE storage_event_outbox, outbox_delivery, job_queue, job_queue_dead RESTART IDENTITY CASCADE;
TRUNCATE node CASCADE;
DROP TABLE IF EXISTS bench_node, bench_latency;
VACUUM FULL storage_event_outbox, outbox_delivery, job_queue;
SQL
}

main() {
  echo "# outbox pool bench $(date -u +%Y-%m-%dT%H:%MZ)"
  echo "server: $(psql_t -c 'SELECT version()')"
  echo "shared_buffers=$(psql_t -c 'SHOW shared_buffers') work_mem=$(psql_t -c 'SHOW work_mem') nproc=$(nproc)"
  case "${1:-}" in
    scan) cmd_scan ;;
    jobs) cmd_jobs ;;
    emit) cmd_emit; truncate_all ;;
    mixed) cmd_mixed "${2:?RATE}" "${3:?SECS}"; truncate_all ;;
    all)
      cmd_scan
      cmd_jobs
      cmd_emit
      CONSUMERS=db cmd_mixed 1000 60
      CONSUMERS=db cmd_mixed 3000 60
      CONSUMERS=code cmd_mixed 100 120
      truncate_all
      ;;
    *) echo "usage: $0 {scan|jobs|emit|mixed RATE SECS|all}" >&2; exit 2 ;;
  esac
}

main "$@"
