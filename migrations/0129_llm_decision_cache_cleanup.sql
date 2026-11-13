-- 判定の決定キャッシュ（llm_decision_cache・0128）の掃除（FEAT-1 のレビュー指摘）。
--
-- 0128 は期限切れの行を「同じテナントで次に書き込んだとき」に消すだけで、ワークフローを消しても、書き込みが
-- 止まっても、判定の答え（確率込み）が残り続けた。答えは run の step の出力と同じく機微な判断を含み得るので、
-- 実行履歴の保持期間を超えて積み増さない（D-52 の H5・H6）。
--
-- - ワークフローを消したら、その答えを同じトランザクションの中で消す。ワークフローは artifact の論理削除
--   （deleted_at）で消えるので、外部キーの on delete cascade は効かない。削除の経路を問わないよう、論理削除の
--   UPDATE に掛けるトリガで消す。
-- - 期限切れの行・実行履歴の保持期間（tenant.workflow_retention_days）より前に書いた行・ワークフローが無い行
--   （削除と同時に走っていた run が後から書いたもの・テナントの消去で artifact ごと消えたもの）は、日次の
--   実行履歴 GC（workflow-engine の run/store/gc.rs）がテナントごとにバッチで消す。

create function llm_decision_cache_forget_workflow() returns trigger
language plpgsql as $$
begin
    delete from llm_decision_cache
     where tenant_id = new.tenant_id
       and workflow_id = new.id;
    return null;
end;
$$;

create trigger artifact_workflow_deleted_forget_decisions
    after update of deleted_at on artifact
    for each row
    when (new.kind = 'workflow' and old.deleted_at is null and new.deleted_at is not null)
    execute function llm_decision_cache_forget_workflow();

-- ワークフロー単位で引く（トリガ・GC）。主キーは (tenant_id, org, workflow_id, cache_key) で org が間にある。
create index llm_decision_cache_workflow_idx on llm_decision_cache (tenant_id, workflow_id);
