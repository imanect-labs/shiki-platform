-- #444: ワークフロー実行履歴の保持期間 GC（engine.md §12.2）。
--
-- 現状、run を実行するたびに workflow_run / step_execution / run_event / effect_journal が
-- 増え続け、**消す仕組みがどこにも無い**。1 日 1000 run × 5 step でも年間 180 万行、
-- イベントを含めればその数倍で、3 年運用すれば 1000 万行級になる。
-- テーブルが大きいほど vacuum のコストも上がるため、これは #440 の効きも削る。
--
-- 起算は **run が terminal になった時刻**（実行中の run は保持期間を跨いでも対象外・§12.2）。

-- 保持期間はテナント設定（engine.md §12.2 の初期値 90 日）。
alter table tenant
    add column if not exists workflow_retention_days int not null default 90
        check (workflow_retention_days > 0);

-- GC 対象の走査。terminal かつ finished_at 順に古いものから拾う。
-- 述語に status を畳み込んであるので、実行中/待機中の run は index に載らない
-- （＝走査が「消してよい候補」だけを見る）。
create index if not exists workflow_run_gc_idx
    on workflow_run (finished_at)
    where status in ('succeeded', 'failed', 'cancelled');

-- effect_journal は run に FK で紐づかない（キーは (tenant_id, idempotency_key)・script 内の
-- `#cN` 連番も入る）ので、run 削除では消えない。§7.3 のとおり run 保持期間と同じ TTL で消す。
create index if not exists effect_journal_gc_idx on effect_journal (created_at);

-- wait_subscription は (tenant_id, run_id, step_path) を持ちながら **FK が無かった**。
-- step_execution / run_event は ON DELETE CASCADE を持つのに、ここだけ抜けている。
-- このままだと run を消した瞬間に確実に孤児が残る（GC 側で消し忘れれば永久に）ため、
-- 構造として保証する。
--
-- DEFERRABLE INITIALLY IMMEDIATE は migration 0061 の規約（tenant_id を参照列に含む FK は
-- テナントリネームのため commit 時検査へ遅延できる必要がある）。通常運用の検査タイミングは不変。
--
-- 既存の孤児（過去の run 削除経路は無いので理論上ゼロだが、テスト DB 等で作られ得る）を
-- 先に掃除してから制約を張る。
delete from wait_subscription w
 where not exists (
     select 1 from workflow_run r
      where r.tenant_id = w.tenant_id and r.run_id = w.run_id);

alter table wait_subscription
    drop constraint if exists wait_subscription_tenant_id_run_id_fkey;
alter table wait_subscription
    add constraint wait_subscription_tenant_id_run_id_fkey
        foreign key (tenant_id, run_id) references workflow_run (tenant_id, run_id)
        on delete cascade
        deferrable initially immediate;

-- 日次ジョブの投入状態。スケジューラリーダーの tick が「前回投入から 24h 経ったか」を見て
-- jobq へ 1 件だけ積む（重い削除を tick ループに同居させない・§12.2）。
--
-- job_name をキーにした汎用の台帳にしてあるのは、日次で回したいメンテナンスが今後増えるため
-- （outbox GC・統計の再計算など）。単一行テーブルを都度足さない。
create table if not exists maintenance_schedule (
    job_name         text        not null primary key,
    -- 最後に jobq へ積んだ時刻。リーダーが交代しても状態は DB にあるので引き継がれる。
    last_enqueued_at timestamptz not null default now(),
    updated_at       timestamptz not null default now()
);

-- 単一行の更新が tick ごとに走るので、#440 と同じ扱いにする（HOT が成立する: 更新列は
-- last_enqueued_at / updated_at で PK は動かず、他に index が無い）。
alter table maintenance_schedule set (
    fillfactor                     = 50,
    autovacuum_vacuum_scale_factor = 0,
    autovacuum_vacuum_threshold    = 50,
    autovacuum_vacuum_cost_delay   = 0
);
