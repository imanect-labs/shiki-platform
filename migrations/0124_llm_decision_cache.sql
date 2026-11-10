-- 判定の決定キャッシュ（ai.decide の step 属性 cache・FEAT-1 の T10・D-52 の H6）。
--
-- 同じ決定器の版・同じ質問・同じ判定材料の答えを、同じワークフローの中で使い回す。既定は使わない（ノードに
-- cache を書いたときだけ）。範囲は (tenant_id, org, workflow_id)。org 全体で共有すると、他人が同じ文面を
-- 判定させたかを当たり外れで推測できる経路ができる（H6）。
--
-- キーは sha256(キーの版 ‖ 決定器の版 ‖ 正規化した質問 ‖ 正規化した判定材料)。判定材料そのものは持たない。
-- 答え（DecideResponse の JSON・使用量は 0 にしたもの）は持つので、期限（expires_at）を過ぎた行は書き込みの
-- たびにテナントの中で消し、読むときは期限内の行だけを当たりとみなす。
--
-- 当たっても組織の許可（判定用のモデルと元の chat モデルの両方）はチョークポイント（LlmGateway::decide_cached）が
-- 毎回確かめる。会計と利用上限は通さない（決定器を呼ばないので費用が出ない）。

create table llm_decision_cache (
    tenant_id      text        not null,
    org            text        not null,
    workflow_id    uuid        not null,
    cache_key      bytea       not null check (octet_length(cache_key) = 32),
    -- 答えた決定器の版（キーにも入っている。版ごとに消す運用の手がかり）。
    resolved_model text        not null,
    response       jsonb       not null,
    created_at     timestamptz not null default now(),
    expires_at     timestamptz not null,
    primary key (tenant_id, org, workflow_id, cache_key)
);

-- 期限切れの掃除（テナントの中で古い順に少しずつ消す）。
create index llm_decision_cache_expiry_idx on llm_decision_cache (tenant_id, expires_at);

-- RLS（0114 と同じポリシー・移行モード）。新しいテナント表は、作る migration で付ける（docs/guides/db-maintenance.md §5）。
alter table llm_decision_cache enable row level security;
alter table llm_decision_cache force row level security;
create policy shiki_tenant_isolation on llm_decision_cache
    using ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()))
    with check ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()));
