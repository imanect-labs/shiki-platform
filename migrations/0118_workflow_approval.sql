-- 承認待ち（human.approval・GAP-08・D-24・D-27）の受信箱の台帳。
--
-- 承認はテーブルの 1 行（FSM で管理する）に束縛し、判断は承認者本人の FSM 遷移で行う。誰が承認できるかの正は
-- FSM の actor 述語 1 か所で、この台帳の担当者は「誰の受信箱に出すか」だけを決める（OpenFGA の relation は
-- 増やさない）。台帳は run と一緒に消える（判断の証跡は監査と data の revision に残る）。
--
-- workflow_approval:
--   from_state / suspend_rev = 停止時の行の状態と rev。rev が suspend_rev 以下の遷移では起きない（直前の自分の
--                              遷移で取り下げにしない）。受信箱は今の状態が from_state の行だけを出す。
--   decisions                = 判断の選択肢（[{kind, to, label, comment}]・kind は approve / reject / return）。
--   status                   = pending → approved / rejected / returned / withdrawn / expired / cancelled。
--   decided_*                = 起床した遷移（decided_by は revision の changed_by）。
--   reconciled_at            = 受付の突合（購読の commit より前に起きた遷移を拾う）を済ませた時刻。
-- workflow_approval_assignee: 受信箱に出す相手（user は principal id・role は所属ロール）。
-- workflow_approval_comment:  判断 API が添えたコメント。遷移後の rev をキーにし、起床時に actor と rev が
--                             一致するものだけを採る（失敗した判断のコメントを拾わない）。

create table workflow_approval (
    tenant_id     text        not null,
    approval_id   uuid        not null,
    org           text        not null,
    workflow_id   uuid        not null,
    run_id        uuid        not null,
    step_path     text        not null,
    node_id       text        not null,
    table_id      uuid        not null,
    record_id     uuid        not null,
    title         text        not null check (char_length(title) between 1 and 120),
    instructions  text        check (char_length(instructions) <= 2000),
    show_fields   text[]      not null default '{}',
    decisions     jsonb       not null,
    from_state    text        not null,
    suspend_rev   bigint      not null,
    status        text        not null default 'pending'
        check (status in ('pending', 'approved', 'rejected', 'returned', 'withdrawn', 'expired',
                          'cancelled')),
    deadline      timestamptz,
    decided_by    text,
    decided_at    timestamptz,
    decided_to    text,
    decided_rev   bigint,
    comment       text,
    reconciled_at timestamptz,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    primary key (tenant_id, approval_id),
    unique (tenant_id, run_id, step_path),
    foreign key (tenant_id, run_id) references workflow_run (tenant_id, run_id) on delete cascade
);

-- 行の削除・遷移で、その行に束縛した保留中の承認を引く。
create index workflow_approval_record_idx
    on workflow_approval (tenant_id, table_id, record_id)
    where status = 'pending';

-- 受付の突合の走査（まだ突き合わせていない保留中の承認だけ）。
create index workflow_approval_reconcile_idx
    on workflow_approval (created_at)
    where status = 'pending' and reconciled_at is null;

-- 受信箱の keyset（新しい順）。
create index workflow_approval_inbox_idx
    on workflow_approval (tenant_id, created_at desc, approval_id desc)
    where status = 'pending';

create table workflow_approval_assignee (
    tenant_id   text not null,
    approval_id uuid not null,
    kind        text not null check (kind in ('user', 'role')),
    subject     text not null,
    primary key (tenant_id, approval_id, kind, subject),
    foreign key (tenant_id, approval_id) references workflow_approval (tenant_id, approval_id)
        on delete cascade
);

create index workflow_approval_assignee_inbox_idx
    on workflow_approval_assignee (tenant_id, kind, subject);

create table workflow_approval_comment (
    tenant_id   text        not null,
    approval_id uuid        not null,
    rev         bigint      not null,
    author      text        not null,
    decision    text        not null check (decision in ('approve', 'reject', 'return')),
    body        text        not null check (char_length(body) <= 2000),
    created_at  timestamptz not null default now(),
    primary key (tenant_id, approval_id, rev),
    foreign key (tenant_id, approval_id) references workflow_approval (tenant_id, approval_id)
        on delete cascade
);
