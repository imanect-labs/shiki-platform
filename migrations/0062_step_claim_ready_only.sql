-- Phase 10 #438: step claim を ready 専用に分離し O(1) 化する。
--
-- 従来の claim は「ready（実行待ち）」と「リース失効 running（回収）」を 1 本の OR で拾っていた。
-- 別々の partial index 2 本を OR で使うと BitmapOr になり index の並び順が失われるため、
-- `ORDER BY next_retry_at LIMIT 1` が「候補全件を集めて Sort してから 1 件」に退化する
-- （＝claim コストが実行待ち件数に比例し、backlog が深いほど遅くなる正のフィードバック）。
--
-- claim を ready 専用にしたうえで、全テナント横断ワーカー（tenant_scope=None・既定動作）が
-- index の並び順をそのまま使えるよう next_retry_at 先頭の partial index を足す。
-- 既存の step_ready_idx (tenant_id, next_retry_at) は tenant シャーディング claim
-- （tenant_scope=Some）が使い続けるため残す。
--
-- 実測（step_execution 100 万行・ready 500 件）:
--   OR 込み cross-tenant claim   : 2,539 バッファ / 7.43 ms（O(ready 件数)）
--   ready 専用＋本 index         :    23 バッファ / 0.58 ms（O(1)）
--
-- リース失効 running の回収は scheduler tick の sweeper（reclaim_expired_leases）へ移した。
-- そちらは既存の step_lease_idx を使う（running は「ワーカー数 × 並列数」で上限が決まる有界集合）。

create index if not exists step_ready_global_idx
    on step_execution (next_retry_at)
    where status = 'ready';
