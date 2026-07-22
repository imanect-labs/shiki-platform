-- 自律エージェントの承認 3 モード（#350）。
--
-- Claude Code の権限モードに対応する 3 値を thread 単位で持ち、既定を「承認必須」にする:
--   * require_approval（既定）= deny_all。破壊系（fs_write/fs_edit/fs_delete/shell/各 doc edit/csv）
--     が全て承認カードで止まる。
--   * auto = 版管理で復元可能な書込（fs_write/fs_edit/document.edit/slide.edit/csv.patch/csv.write/
--     office.edit）のみ自動承認。不可逆・高影響（fs_delete/shell/office.live_edit）は承認維持。
--   * bypass = allow_all（危険・明示オプトイン）。org 管理者ポリシ（tenant.allow_autonomous_bypass）
--     で禁止できる。
-- モード→ApprovalPolicy の写像は crates/chat/src/autonomous.rs に集約する。

-- thread ごとの承認モード（実行中トグル可・UI のセレクタが更新する）。
alter table thread add column if not exists autonomous_mode text not null
    default 'require_approval'
    check (autonomous_mode in ('require_approval', 'auto', 'bypass'));

-- モードを最後に設定した principal.id。**実行中の緩和はこの設定者 = run の actor の場合のみ**
-- 有効にする（共有スレッドの別編集者が他人の権限で走る run の承認を緩められない・confused-deputy 防御）。
alter table thread add column if not exists autonomous_mode_set_by text;

-- run にはメッセージ投入時点のモードをスナップショットする（0027 の autonomous と同パターン・
-- 発話者はモードを見て投稿する＝そのモードでの実行に同意している）。
alter table generation_run add column if not exists autonomous_mode text not null
    default 'require_approval'
    check (autonomous_mode in ('require_approval', 'auto', 'bypass'));

-- org 管理者キャップ: false で bypass（全自動）を禁止する（エンタープライズ要件・#350）。
-- 未登録テナント（開発環境）は行が無い＝許可扱い（既定 true と同じ）。
alter table tenant add column if not exists allow_autonomous_bypass boolean not null default true;
