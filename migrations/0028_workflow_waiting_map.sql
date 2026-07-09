-- Phase 10 #178: map の動的 fan-out（waiting_map）＋要素ごと per-step 入力。
--
-- waiting_map = control.map が領域の子（要素）完了を待つ非 terminal 状態。map ノードは
-- 実行時に要素ごと `<map_step_path>[<index>].<region_node>` を動的挿入し、自身は waiting_map で
-- 待ち合わせ、全要素の出口 step が terminal 化した時点で集約して terminal 化する（engine.md §4.5）。
--
-- step_execution.input = 要素ごとの each コンテキスト（{ "each": { "item": …, "index": i } }）と
-- map ノードのメタ（{ "map": { "count": N, "on_item_error": … } }）。静的ノードは NULL（run 入力を使う）。

alter table step_execution drop constraint if exists step_execution_status_check;
alter table step_execution add constraint step_execution_status_check
    check (status in ('pending', 'ready', 'running', 'waiting_timer', 'waiting_event',
                      'waiting_map', 'succeeded', 'failed', 'skipped', 'cancelled'));

alter table step_execution add column if not exists input jsonb;
