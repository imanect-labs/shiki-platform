-- ミニアプリの画面（Yui の ```ui を含む Markdown）のボタンで起動したワークフローの記録（#604）。
--
-- 画面のボタンを押すたびに 1 行。同じ冪等キーは 1 回だけ起動する（同じ送信の再送・二度押しで
-- run を増やさない）。会話の `ui_action_run`（0070）と同じ形で、鍵がメッセージではなく
-- アプリと押した本人になる。記録は押した本人だけが読む（共有したアプリで他人の入力値を見せない）。
create table miniapp_action_run (
    tenant_id       text        not null,
    app_id          uuid        not null,
    -- 押したユーザー（principal.id）。
    invoked_by      text        not null,
    -- ボタンの id（`{ブロックのソースの SHA-256 の先頭 16 桁}:{a1}`）。
    action_id       text        not null,
    -- 画面が送信ごとに作る鍵（英数字・`-`・`_` の 8〜64 文字）。
    idempotency_key text        not null,
    org             text        not null,
    -- 押した版。
    app_version     bigint      not null,
    invoked_at      timestamptz not null default now(),
    -- ボタンの文言と起動したワークフローの別名（押した時点のもの）。
    label           text        not null,
    workflow_name   text        not null,
    -- 送った値の表示（[{label, value}]・部品の表示名と表示用の値）。
    summary         jsonb       not null default '[]'::jsonb,
    -- 起動した run（null = 起動の途中。起動に失敗した行は消すので、残るのは途中か成功）。
    run_id          uuid,
    -- 終わった状態（succeeded・failed・cancelled・skipped）。null は未確定。
    status          text,
    finished_at     timestamptz,
    primary key (tenant_id, app_id, invoked_by, action_id, idempotency_key),
    -- アプリを消したら記録も消す。テナントのリネーム（0061）に合わせて遅延可能にする。
    foreign key (tenant_id, app_id) references artifact (tenant_id, id)
        on update cascade on delete cascade
        deferrable initially immediate,
    constraint miniapp_action_run_status_check
        check (status is null or status in ('succeeded', 'failed', 'cancelled', 'skipped'))
);
create index miniapp_action_run_mine_idx
    on miniapp_action_run (tenant_id, app_id, invoked_by, invoked_at desc);
create index miniapp_action_run_run_idx
    on miniapp_action_run (tenant_id, run_id) where run_id is not null;

-- 起動した run の終端を記録へ写す（実行履歴の GC で run が消えても結果が残るように・0070 と同じ理由）。
create function miniapp_action_run_record_terminal() returns trigger
language plpgsql as $$
begin
    update miniapp_action_run
       set status = new.status, finished_at = now()
     where tenant_id = new.tenant_id
       and run_id = new.run_id
       and status is distinct from new.status;
    return null;
end;
$$;

create trigger workflow_run_miniapp_action_terminal
    after update of status on workflow_run
    for each row
    when (new.status in ('succeeded', 'failed', 'cancelled')
          and old.status is distinct from new.status)
    execute function miniapp_action_run_record_terminal();
