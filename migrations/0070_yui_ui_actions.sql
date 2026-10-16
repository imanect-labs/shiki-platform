-- 会話の図（回答の本文の ```ui ブロック・@imanect/yui）のボタン（#604）。
--
-- 図は本文の文字のまま保存される（generative_ui のような別ブロックを持たない）。ボタンを押すと
-- サーバーが本文からブロックを読み直し、値を部品の定義で検証してから実行する。ボタンの id は
-- `{ブロックのソースの SHA-256 の先頭 16 桁}:{a1}`（crates/yui の `action_key`）。
--
-- 会話に送るボタン（ask・plan・`-> chat`）の「1 回だけ」は既存の `ui_action_invocation`（0059）
-- をそのまま使う。ここで足すのはワークフローを起動するボタンの 2 つの表。

-- ① 束縛（宣言）: 回答が確定したとき、本文の `-> workflow 名前` を**回答した主体**の権限で
-- 解決し、版を固定して残す。画面から送れるのは id と値と冪等キーだけで、束縛は送れない。
-- 行の無いボタンは押せない（解決できなかった・権限が無かった）。
create table ui_action_binding (
    tenant_id        text        not null,
    thread_id        uuid        not null references thread (id) on delete cascade,
    message_id       uuid        not null references message (id) on delete cascade,
    action_id        text        not null,
    workflow_id      uuid        not null,
    workflow_version bigint      not null,
    -- 解決した時点の名前（表示と、モデルへの履歴に使う）。
    workflow_name    text        not null,
    created_at       timestamptz not null default now(),
    primary key (tenant_id, thread_id, message_id, action_id)
);
create index ui_action_binding_thread_idx on ui_action_binding (thread_id);
create index ui_action_binding_message_idx on ui_action_binding (message_id);

-- ② 起動の記録: 押すたびに 1 行。同じ冪等キーは 1 回だけ起動する（同じ送信の再送・二度押しで
-- run を増やさない）。会話の中の「起動した・終わった・失敗した」の正本で、画面はメッセージと
-- 一緒に受け取り、モデルは次の回答の履歴で読む。
create table ui_action_run (
    tenant_id       text        not null,
    thread_id       uuid        not null references thread (id) on delete cascade,
    message_id      uuid        not null references message (id) on delete cascade,
    action_id       text        not null,
    -- 画面が送信ごとに作る鍵（英数字・`-`・`_` の 8〜64 文字）。
    idempotency_key text        not null,
    org             text        not null,
    -- 押したユーザー（principal.id）。共有した会話で「誰が起動したか」を残す。
    invoked_by      text        not null,
    invoked_at      timestamptz not null default now(),
    -- ボタンの文言と起動したワークフローの名前（押した時点のもの）。
    label           text        not null,
    workflow_name   text        not null,
    -- 送った値の表示（[{label, value}]・部品の表示名と表示用の値）。
    summary         jsonb       not null default '[]'::jsonb,
    -- 起動した run（null = 起動の途中。起動に失敗した行は消すので、残るのは途中か成功）。
    run_id          uuid,
    -- 終わった状態（succeeded・failed・cancelled・skipped）。null は未確定で、表示のときに
    -- run の状態を読み、終わっていたらここへ書き戻す（以後は run が消えても残る）。
    status          text,
    finished_at     timestamptz,
    primary key (tenant_id, thread_id, message_id, action_id, idempotency_key),
    constraint ui_action_run_status_check
        check (status is null or status in ('succeeded', 'failed', 'cancelled', 'skipped'))
);
create index ui_action_run_thread_idx on ui_action_run (thread_id);
create index ui_action_run_message_idx on ui_action_run (message_id);
