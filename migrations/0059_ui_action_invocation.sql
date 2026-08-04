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
    -- 実行が**完了**した時刻（null = 確保しただけ）。確保はハンドラ実行の前に取るので、
    -- 確保とハンドラ完了の間でプロセスが落ちると未完了の行が残る。
    --
    -- **確保は奪わない**。`chat.submit` は非冪等なので、「古いから」と引き継ぐと副作用が
    -- コミットした直後に落ちたケースと区別できず、発話と生成 run を二度作ってしまう
    -- （この台帳が防ぐはずのもの）。よって未完了の行も UI へは「送信済み」として返す
    -- ——奪えない以上そのカードはもう押せないので、未送信と描くと押せないのに押せそうに
    -- 見える。詰まった行の回復は**その行の削除**（運用操作）。この列は「実行が本当に
    -- 終わったか」の監査と、詰まりの発見（completed_at is null の古い行）に使う。
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

-- 既に実行済みのカードを埋め戻す。
--
-- この表を空で作ると、0059 適用前に回答/開始したカードは `invoked_actions` に出ず、
-- デプロイ後の再読込で**未送信の顔に戻って再度押せる**——まさにこの issue が塞ぐ事故が、
-- 過去の会話にだけ残ってしまう。
--
-- 出所は `ui_action.invoke` の Allow 監査。**実行時の描画根拠に監査を使わない**方針は
-- 変えない（保持ポリシに引きずられる）。ここは一度きりの移行で、その時点の事実を
-- 一級の状態へ写し取るだけ。取り込むのは「チャット由来の handler 束縛」＝単発の
-- `chat.submit` に限る（tool/workflow は繰り返せるので台帳に載せない）。
-- 監査が消えていた分は埋まらないが、その場合も**空で作るより厳密に良い**。
insert into ui_action_invocation
    (tenant_id, thread_id, message_id, action_id, org, invoked_by, invoked_at, completed_at, run_id)
select distinct on (a.tenant_id, thread_id, message_id, a.object_id)
       a.tenant_id,
       (a.metadata -> 'source' ->> 'thread_id')::uuid  as thread_id,
       (a.metadata -> 'source' ->> 'message_id')::uuid as message_id,
       a.object_id,
       a.org,
       a.actor,
       a.created_at,
       -- 監査に残っている＝実行は完了している（Allow は実行後に書かれる）。
       a.created_at,
       case when a.metadata ->> 'run_id' ~ '^[0-9a-fA-F-]{36}$'
            then (a.metadata ->> 'run_id')::uuid end
from audit_log a
where a.action = 'ui_action.invoke'
  and a.decision = 'allow'
  and a.metadata -> 'source' ->> 'kind' = 'chat_message'
  and a.metadata ->> 'binding' = 'handler'
  -- 消えたスレッド/メッセージは外部キーで弾かれるので、存在するものだけに絞る。
  and exists (
      select 1 from message m
      where m.id = (a.metadata -> 'source' ->> 'message_id')::uuid
        and m.thread_id = (a.metadata -> 'source' ->> 'thread_id')::uuid
        and m.tenant_id = a.tenant_id
  )
order by a.tenant_id, thread_id, message_id, a.object_id, a.created_at
on conflict do nothing;
