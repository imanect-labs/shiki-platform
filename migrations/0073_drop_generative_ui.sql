-- 撤去した生成 UI（emit_ui・gui spec の JSON 部品）の旧データを消す（#604 の 7 つ目）。
--
-- 図は回答の本文に Yui の ```ui で書く（本文の文字として保存され、別ブロックを持たない）。
-- 旧方式のデータを読み書きするコードはもう無く、型の上でも表現できない（読めば壊れる）ので、
-- ここで残らず片付ける。

-- 回答の generative_ui ブロックは、消すと回答が空になる（カードだけの回答）ことがあるので、
-- 「旧形式の図」と分かる text ブロックへ置き換える（会話の文脈としても読める）。
update message m
   set content = (
       select jsonb_agg(
                  case when b.value->>'type' = 'generative_ui'
                       then jsonb_build_object('type', 'text', 'text', '（旧形式の図は表示できません）')
                       else b.value
                  end
                  order by b.ord)
         from jsonb_array_elements(m.content) with ordinality as b(value, ord)
   )
 where m.content @> '[{"type": "generative_ui"}]'::jsonb;

-- 生成の途中経過（再接続の再生用）の generative_ui イベント。
delete from generation_event where type = 'generative_ui';

-- UI スペックのアーティファクト（版は FK の cascade で消える）。
delete from artifact where kind = 'ui_spec';

-- 画面（screen）を持たない旧形式のミニアプリ（ui_spec をピンしていた）。どの版も画面を
-- 持たないものだけを消す（新形式で保存し直したアプリの旧い版は履歴として残す）。
delete from artifact a
 where a.kind = 'mini_app'
   and not exists (
       select 1
         from artifact_version v
        where v.tenant_id = a.tenant_id
          and v.artifact_id = a.id
          and v.body ? 'screen'
   );

-- 種別の閉集合から ui_spec を外す（Rust の ArtifactKind と一致させる）。
alter table artifact drop constraint artifact_kind_check;
alter table artifact add constraint artifact_kind_check
    check (kind in ('workflow', 'mini_app', 'skill', 'script', 'data_view', 'fsm', 'mini_app_code'));
