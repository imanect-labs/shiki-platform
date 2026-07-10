# Phase 11 — Office 統合（Collabora）

> 目的: Office 系（docx/pptx/xlsx）のブラウザ内編集・共同編集を **Collabora Online**（共同編集は内蔵に委任・
> 自作しない）で提供する。**AI は非セッション時のファイルレベル編集**（design §4.8）。
> md 系（ノート・Yjs 共同編集・AI リアルタイム編集）は **[Phase 11-pre](./phase-11-pre.md) へ移動**した
> （旧 Task 11.1〜11.4 → 11P.1〜11P.4、旧 11.10 → 11P.10。ID は新フェーズ側で採番し直し）。
> 旧 V2 トラック（parallel-tracks）は本フェーズに統合。
> 完了の定義(DoD): Collabora で Office 文書を共同編集でき、保存が WOPI→StorageService→バージョニング→
> RAG 再索引に流れる。スプレッドシートのカスタム関数/マクロが shiki script で動く。
> ⚠️ **着手前に [設計上の落とし穴](../design-caveats.md) の PIT-11（WOPI トークンと共有解除の即時性）を確認すること。**

| ID | タイトル | area | 依存 |
|----|---------|------|------|
| 11.5 | OfficeSuite トレイト＋Collabora デプロイ（SaaS 共有プール/オンプレ同梱） | docgen | – |
| 11.6 | WOPI ホスト（StorageService クライアント・毎呼び出し ReBAC・バージョニング） | docgen | 11.5, 1.x |
| 11.7 | Office 共同編集＋保存→RAG 再索引（旧 V2.2〜V2.4 統合） | docgen | 11.6, 2.x |
| 11.8 | AI Office 編集（非セッション時ファイルレベル・セッション中は提案保存） | ai | 11.6, 7.x |
| 11.9 | スプレッドシート×shiki script（カスタム関数/マクロ） | data | 11.7, 10.7 |

---

## 詳細

### Task 11.5: OfficeSuite トレイト＋Collabora デプロイ
- **area**: docgen / **path**: `crates/office`, `deploy/`
- **仕様**: `OfficeSuite` トレイト（OnlyOffice への差し替え退路）。Collabora(CODE) コンテナを
  SaaS=テナント共有プール／オンプレ・エアギャップ=同梱で配備。
- **受け入れ条件**:
  - [ ] compose/k8s に Collabora が入り docx/pptx/xlsx が開ける
  - [ ] エアギャップ構成で外部接続なしに動く

### Task 11.6: WOPI ホスト
- **area**: docgen / **path**: `crates/office`
- **仕様**: CheckFileInfo/GetFile/PutFile を **StorageService の一クライアント**として実装（チョークポイント維持・
  直バケット禁止）。WOPI access_token=（実行主体×ファイル×短寿命）＋**毎呼び出し ReBAC 再チェック**
  （HIGHER_CONSISTENCY・共有解除の即時反映）。トークンにテナント境界を焼き込む。PutFile→新バージョン→書込イベント。
- **受け入れ条件**:
  - [ ] 共有解除後の既存編集セッションが次の WOPI 呼び出しで拒否される
  - [ ] PutFile が監査・バージョニング・RAG 再索引に乗る
  - [ ] 他テナントのファイルにトークンを流用できない

### Task 11.7: Office 共同編集＋RAG 還流
- **area**: docgen / **path**: `crates/office`, `web/`
- **仕様**: iframe 組込・複数ユーザー同時編集（Collabora 内蔵）。保存→再索引の整合（旧チャンク失効）。
- **受け入れ条件**:
  - [ ] 2ユーザーが同一 pptx を同時編集できる
  - [ ] 保存後の検索が新内容を反映し旧バージョンのチャンクが混入しない

### Task 11.8: AI Office 編集
- **area**: ai / **path**: `crates/office`, `ingestion-worker/`
- **仕様**: read=Docling パースを正（convert-to は補助）。edit=WOPI ロックで判定し、
  非セッション時のみファイルレベル編集（ingestion-worker の生成系を編集に拡張）→新バージョン。
  セッション中は「提案として保存」（別バージョン/コメント）。ライブ参加はスコープ外（将来）。
- **受け入れ条件**:
  - [ ] AI が pptx/docx/xlsx を読み・編集し新バージョンとして保存できる
  - [ ] 編集セッション中の AI 編集要求が上書きせず提案保存に落ちる

### Task 11.9: スプレッドシート×shiki script
- **area**: data / **path**: `crates/office`, `crates/script-runtime`
- **仕様**: シートのカスタム関数/マクロを shiki script（10.7 ランタイム再利用・ms 級起動）で実行。
  実行主体=操作ユーザー（対話トリガと同じ交差則）。
- **受け入れ条件**:
  - [ ] セル関数として script が評価され再計算に耐えるレイテンシで返る
  - [ ] script がユーザーの読めないデータに到達できない
