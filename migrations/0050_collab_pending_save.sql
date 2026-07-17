-- 共同編集ドキュメントの「ファイル保存が未完了」の durable マーカー（レビュー指摘対応）。
--
-- 保存失敗→全接続離脱（アンロード）→外部書込→再ロードの順で起きると、未保存の CRDT 編集
-- （collab_doc/collab_update には永続済み）が外部インポートの全置換で消える窓があった。
-- pending_save=true の間はロード時の外部インポートを抑止し、CRDT を正としてデバウンス保存の
-- 再試行に回す（外部版はバージョン履歴に残る）。ファイル保存成功時に false へ戻す。
alter table collab_doc add column pending_save boolean not null default false;
