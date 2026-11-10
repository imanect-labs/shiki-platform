-- 自動実行の起動の見送りを作成者に見せる（GAP-67）。
--
-- 自動実行（schedule / event）の起動を見送った理由（定義を読めない・BUG-146、委譲の不成立・BUG-170、実行の前に止める
-- 設定・BUG-51）は、これまでサーバーのログ（版ごとに 1 回）と、理由の無い trigger_firing.outcome = 'skipped' にしか
-- 残らず、登録の API は status を enabled のまま返していた。作成者は自動化が止まったことに気づけない。
--
-- 見送りは起動口（WorkflowRunLauncher）が、理由を型で持っているところで記録する。ワークフローごとに 1 行で、続く
-- 見送りは回数（count）と最後の時刻（last_at）を進める。版か理由が変わったら数え直す。自動実行が run を作れたら
-- 消し、オンにし直す・オフにすると消す（いま見送り続けているものだけを見せる）。
--
--   version = 見送った有効化した版。
--   code    = 理由の code（definition_unreadable / delegation_invalid / not_ready）。
--   message = 作成者向けの理由の文（内部の文・ほかの人の情報は書かない）。直し方は読むときに code から組む。
--
-- 発火の記録（trigger_firing・schedule_occurrence）には書かない。発火の行はスケジューラが行ロックを持ったまま
-- 起動口を呼ぶので起動口からは書けず、スケジューラは理由を文でしか受け取らない（RunLauncher の戻り値）。

create table workflow_launch_skip (
    tenant_id   text        not null,
    workflow_id uuid        not null,
    version     bigint      not null,
    code        text        not null
                check (code in ('definition_unreadable', 'delegation_invalid', 'not_ready')),
    message     text        not null,
    first_at    timestamptz not null default now(),
    last_at     timestamptz not null default now(),
    count       bigint      not null default 1 check (count >= 1),
    primary key (tenant_id, workflow_id),
    foreign key (tenant_id, workflow_id) references workflow_registration (tenant_id, workflow_id)
        on delete cascade
        deferrable initially immediate
);

-- RLS（0114 と同じポリシー・移行モード）。新しいテナント表は、作る migration で付ける（docs/guides/db-maintenance.md §5）。
alter table workflow_launch_skip enable row level security;
alter table workflow_launch_skip force row level security;
create policy shiki_tenant_isolation on workflow_launch_skip
    using ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()))
    with check ((select shiki_rls_tenant()) is null or tenant_id = (select shiki_rls_tenant()));
