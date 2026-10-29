-- ワークフローを消したら、自動実行と動いている実行を止める（BUG-128・D-105）。
--
-- ワークフローは artifact の論理削除（deleted_at）で消えるので、外部キーの on delete cascade は効かない。これまでは
-- 消しても自動実行の登録・きっかけ・動いている run（承認待ちを含む）・委譲が残り、承認者の受信箱に消したワークフロー
-- の依頼が残って、承認すると続きが動いた。スケジュールは発火のたびに定義を読めず起動に失敗した。
--
-- 削除の経路を問わないよう、論理削除の UPDATE に掛けるトリガで DB の側を同じ Tx の中で止める（0114 と同じ形・D-77）。
--
-- - 自動実行の登録を無効にし、同意（consented_scopes）も空にする（「権限も取り消す」と同じ。戻したワークフローは
--   自動実行がオフのままで、オンにするには同意し直す）。
-- - きっかけを止める（無効化と同じ `enabled = false`）。
-- - 動いている run（queued・running）に取り消しの印（cancel_requested）を付ける。印の付いた run は claim されず、
--   受信箱にも出ない。ドレイン（実行中でない step の取り消し・承認の依頼の取り下げ・待ちの購読の消し込み）は run の
--   取り消しの通常の経路が行う: API の削除が commit の後にその場で、取りこぼしは scheduler の tick が行う。
-- - 委譲（OpenFGA のタプル）は DB の外なので、API の削除が commit の後に「権限も取り消す」と同じ経路で外す。外せな
--   かったものは scheduler の掃除が取り消す。

create function workflow_deleted_stop() returns trigger
language plpgsql as $$
begin
    update workflow_registration
       set status = 'disabled', consented_scopes = '{}'::text[], updated_at = now()
     where tenant_id = new.tenant_id
       and workflow_id = new.id;
    update workflow_trigger
       set enabled = false
     where tenant_id = new.tenant_id
       and workflow_id = new.id
       and enabled;
    update workflow_run
       set cancel_requested = true, updated_at = now()
     where tenant_id = new.tenant_id
       and workflow_id = new.id
       and status in ('queued', 'running')
       and not cancel_requested;
    return null;
end;
$$;

create trigger artifact_workflow_deleted_stop
    after update of deleted_at on artifact
    for each row
    when (new.kind = 'workflow' and old.deleted_at is null and new.deleted_at is not null)
    execute function workflow_deleted_stop();
