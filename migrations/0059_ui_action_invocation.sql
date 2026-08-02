-- issue #410: 1 回だけ実行できる UI アクションの実行台帳。
--
-- 質問カード・計画カードの「送信済み」はコンポーネントのローカル state にしか無かったため、
-- 会話が再描画されるとカードが未回答へ巻き戻り、**同じカードからもう一度送信できた**。
-- 計画カードの「この計画で開始」は調査 run をまるごと作るので、二度押しは実費の事故になる。
--
-- 「どのメッセージのどの action が実行されたか」を一級の状態として持ち、
--   ① 実行前にここを確保する（取れなければ実行しない＝二重送信を入口で潰す）
--   ② メッセージ取得時に一緒に返し、カードの「送信済み」表示の根拠にする
-- の 2 つに使う。`run_approval` が (run_id, tool_call_id) で二重決定を潰しているのと同じ形。
--
-- 監査（audit_log の ui_action.invoke）にも同じ事実は残るが、あれは追記専用の台帳で保持期間も
-- アクセス経路も別物なので、**UI の描画根拠にはしない**（保持ポリシを変えた瞬間に表示が壊れる）。
--
-- 対象は「1 回だけ」が意味を持つ束縛のみ（正本は crates/gui/src/action.rs の
-- `ActionBinding::single_use`）。検索ツール束縛のような繰り返して当然の操作は記録しない。

create table ui_action_invocation (
    tenant_id  text        not null,
    thread_id  uuid        not null references thread (id) on delete cascade,
    message_id uuid        not null references message (id) on delete cascade,
    -- スペックに宣言された action id（メッセージ内で一意）。
    action_id  text        not null,
    org        text        not null,
    -- 実行したユーザー（principal.id）。共有スレッドで「誰が押したか」を残す。
    invoked_by text        not null,
    invoked_at timestamptz not null default now(),
    -- 実行が**完了**した時刻（null = 確保しただけ）。確保はハンドラ実行の前に取るため、
    -- 確保とハンドラ完了の間でプロセスが落ちると「発話も run も無いのに送信済み」の行が
    -- 残る。UI へ「送信済み」と見せるのはここが埋まった行だけにし、埋まらないまま古く
    -- なった確保は次の押下で**引き継げる**ようにして、詰まりを自己回復させる。
    completed_at timestamptz,
    -- 実行で生まれた run（chat.submit の生成 run 等）。監査・調査との突合用。
    run_id     uuid,
    -- 二重送信の抑止はこの主キーそのもの（insert ... on conflict do nothing で確保する）。
    -- 先頭 2 列がスレッド単位の一覧（メッセージ取得時の同梱）にもそのまま効く。
    primary key (tenant_id, thread_id, message_id, action_id)
);

-- 参照側の外部キー列の索引。主キーは tenant_id 始まりなので、`on delete cascade` が使う
-- 「thread_id = ?」「message_id = ?」の検索には効かない（スレッド/メッセージ削除が全表走査になる）。
create index ui_action_invocation_thread_idx on ui_action_invocation (thread_id);
create index ui_action_invocation_message_idx on ui_action_invocation (message_id);
