# 不具合報告の運用

本番の送り先が決まるまでは `bug_report.backend = "disabled"` を維持する。
ローカル受付を提供する場合のみ `enabled = true` にし、組織ポリシーも有効にする。
提出は held（外部未送信）となる。宛先の設定だけで held や承認待ちを配送しない。
本人が宛先を確認して再下書き・再提出する。preview GitHub 設定を本番に流用しない。
実環境への設定反映・外部照合・外部削除は運用者の承認の下で行う。

## 保持と消去

起動時と毎時、機能の enabled/backend にかかわらず保持処理を実行する。
下書きは最終更新から30日、提出済み本文は最後の提出から90日で消去する。
各提出 snapshot 自体も同意日時から90日で消去する。
case は expires_at（作成から24時間）で物理削除する。利用可否の24時間チェックは既存のサービスで強制する。
冪等応答には本文があるため30日で削除し、報告本文消去時はその報告の全応答を削除する。

消去対象は snapshot、payload、principal、却下理由、冪等応答。
配送台帳は本文を持たず、配送ID・外部ID/URL・hash・同意/承認日時・結果などを保持する。
消去済み報告は本人/管理者 API に返さず、再提出・再送も受け付けない。
未着手配送は held、送信中は delivery_unknown として fence を更新する。
既に外部へ出たリクエストを取り消せるとは保証しない。
監査は既存の tenant.audit_retention_days に従い、報告の90日処理では消さない。
tenant 消去は tenant_id を持つ表を依存順で導出する既存 purge に含まれる。
台帳の状態が deleting/deleted の tenant は claim/照合を停止する。
claim は org lock の取得後に台帳行を押さえて状態を再確認するため、撤去の開始（deleting への更新）は
進行中の claim の commit を待ち、開始後に新しい claim は成立しない。
ただし commit 済みの claim は lease の期限まで外部送信を続けられる。撤去の開始後でも、
外部送信の開始前に撤去が完了していれば、その 1 件は撤去後に送信されることがある。
lease の期限は送信の終了を保証しない（ワーカーが遅れると期限後に送信が始まることもある）。
また tenant 撤去の purge は配送台帳も消去する。送信済みの外部 issue は取り戻せないため、
撤去は次の順で行う。
1. tenant を deleting にして新しい claim を止める（purge はまだ始めない）。
2. 配送台帳の queued / sending / delivery_unknown / sent について、配送 ID（`bug_report_delivery.id`）・
   report_id・送り先 repository・外部 ID・URL を控える。照合マーカーは配送 ID なので、外部 ID・URL が
   まだ無い配送も配送 ID で控える。
3. purge を進め、撤去後に送り先で配送 ID のマーカーを照合し、外部に作られた issue の消去を送り先側で判断する。
   結果不明の配送は、送り先で見つからなくても未作成とは判断しない（後から作られる可能性を残して扱う）。

## 監視

構造化ログ `bug-report queue health` の数値をログ基盤のメトリクスとして収集する。
毎時の pending / queued / failed / unknown / rate_limited / oldest_seconds は全体集計で、
failed/unknown は本文消去後も配送台帳から集計する。
本文や tenant/user をラベルに含めない。unknown > 0、failed の増加、
queued の最長滞留が1時間以上、health/retention成功ログが2時間以上途絶えた場合を通知の初期目安とする。
承認待ち滞留は担当管理者へ確認する。`rate_limited` は台帳の現在件数で累積回数ではない。
`bug-report delivery completed` は outcome/error_code の閉じた分類だけを記録する。
`bug-report delivery batch failed` / `bug-report retention batch failed` は即時調査する。
GitHub応答本文・鍵・トークン・報告本文をログやチケットへ貼らない。

## 配送失敗と結果不明

1. failed は安全な error_code を確認する。認証/宛先は運用設定を調査する。
   明確な未送信（connect/rate_limit/credential_unavailable）だけ既存 retry を利用できる。
   自動試行は最大5回/24時間。Retry-After と installation 共通の待機を優先する。
2. delivery_unknown は POST を再実行しない。期限切れ sending も同じ扱い。
   ワーカーは公開ランダム配送UUID markerで照合し、見つかれば sent にする。
   見つからない結果は未作成の証明ではなく unknown を維持する。
3. 権限を持つ運用者が固定 repository と配送 UUID を確認する。
   外部ID、時刻、調査結果を内部の運用記録へ残し、本文を複製しない。
   初版に強制再送/手動結果変更 API はない。直接SQLで queued に戻さない。
   解消できなければ担当者へエスカレーションし、重複起票を避ける。
4. 遮断時は backend=disabled（機能全停止は enabled=false）へ変更し再起動する。
   再開前に tenant 状態・org許可・本人同意・宛先版・承認・試行期限を確認する。

## 外部保持・消去

ローカル消去は GitHub の本文を削除しない。外部への送信済み報告は、
権限を持つ運用者が配送UUID/外部IDを使って対象を照合し、承認された外部消去手順を実施する。
repository の保持方針、バックアップ、同意文面と外部消去のSLAは送り先決定時に確定する。
確定までは本番配送を有効化しない。
