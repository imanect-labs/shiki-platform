# first-party skill バンドル（10.15・#344）

公式提供の skill（外部連携は **http.request ラップの skill** として配布し、ネイティブコネクタは
作らない方針の実証・[miniapp-platform.md](../../docs/miniapp-platform.md) §2.4/§4）。

## 掲載（#387）

**first-party は署名 publish された時点で、同一 tenant・同一 org のメンバー全員のカタログに載る
（インストール不要）。** 掲載も読取も **tenant ＋ org** の境界に閉じている（org は隔離境界・#371）。
`/deep-research` のような公式スキルが「最初から在る」ようにするため。

- 読める根拠は publish 時に張られる `organization#member → viewer`（`artifact` の viewer は
  `organization#member` を受理する・`crates/authz/model/authorization-model.fga`）。掲載は
  `SkillInstallService::list_first_party_summaries`（tenant ＋ **org** で絞る）。
- **in_house は従来どおり明示インストールが必要**（掲載＝「明示的な人間の行為」の原則を維持）。
- yank すると掲載から外れる（新規利用を止める意味を掲載側にも効かせる）。
- 認可は artifact チョークポイントのまま。掲載は権限の代わりにならない。

## 配布経路（エアギャップと同一・マイグレーションに業務コンテンツを埋めない）

1. 管理者が信頼鍵を登録する（`POST /admin/trusted-keys`・ミニアプリと同じ台帳）。
2. 署名対象は **name/version に束縛した signing digest** =
   `app_platform::registry_signing_digest(name, version, app_platform::value_digest(body))` へ
   ed25519 で署名する（秘密鍵はオフライン保持・サーバに置かない。署名ヘルパ: `app_platform::sign_digest`）。
   body だけの署名だと別名で再 import して公式スキルをスプーフィングできてしまうため name/version を織り込む。
3. `POST /skills/registry/import { name, version, body, signature_base64 }` —
   登録済み信頼鍵で **signing digest** を検証し、artifact 化 → **first-party** として publish される
   （同一 name+version の再 import は 409・不変）。
4. first-party は**この時点で全ユーザーのカタログに載る**（上記「掲載」）。
   `POST /skills/installations { name }` は in_house 用（バージョンを固定したい場合は
   first-party でも使える）。

### CLI（手順 2〜3 の実行）

```bash
# 署名鍵が無ければ鍵ペアを生成し、信頼鍵の登録コマンドを表示して終了する
node sdk/cli/src/index.ts skill import-first-party --api http://localhost:8080 --tenant default
# 管理者が公開鍵を登録したら、秘密鍵を渡して import（同一 name+version の再実行は skip）
SHIKI_SIGNING_KEY=<hex> node sdk/cli/src/index.ts skill import-first-party --api http://localhost:8080
```

`SHIKI_COOKIE` にセッション Cookie（`shiki_session=...; shiki_csrf=...`）が必要。

## deep-research

- 実体: instructions（手順書）が本体。専用エンジンは作らない（design §4.4 / FR-4）。
  自律プロファイル（長ホライズン・作業ファイル・予算ガード）に載る。
- 起動: `/deep-research <依頼>`（質問→計画→実行）／`/deep-research auto <依頼>`（確認を省略）。
  スラッシュコマンドは `command` 宣言から生成される（`SkillBody.command`・#387）。
- 作業ファイル（brief / outline / notes / report）は**システム領域**へ書かれる
  （ドライブ非表示・RAG 非索引・#392）。ユーザーに渡るのは本文のレポートと
  `save_note` の保存ボタンだけ。
- 品質の要点は instructions 内に数値で埋めてある（クエリ 3 分類の予算表・最低ツールコール数・
  停止条件 4 層・証拠 ID に紐づかない主張の掃除）。変更時は
  `crates/gui/tests/first_party_skills.rs` が上限と宣言を守る。

## grilling

- 実体: instructions（手順書）が本体。計画・決定・アイデアを**設計ツリー**として開き、
  前提が片付いた決定＝**フロンティア**を 1 ラウンドずつ質問カードで聞き、回答で木を組み替えて
  次のフロンティアを聞く、を木が尽きるまで繰り返す面接プリミティブ。
- 起動: `/grill <お題>`（フロンティアが空になるまで回す）／`/grill quick <お題>`（1 ラウンドで
  切り上げ、埋めた仮定を計画カードに明記する）。
- 出し方はこのプラットフォームの機構に合わせてある: 質問は `emit_ui` の `question_card`
  （推奨案を先頭の選択肢にして「（推奨）」を付け、`description` には帰結を書く・`allow_other` 必須）、
  事実の調べ物は `subagent` へ委譲、決定と理由は作業ファイル `tree.md` へ `fs_append`、
  最後に `plan_card` で共通理解を確認してから実行へ移る。
- **`command.variants` に `phase` は宣言しない**（＝ゲートは掛からない）。`plan_first` は
  「質問カード 1 回 → 計画カード → 実行」を想定した段階遷移で、grilling とは 2 箇所で噛み合わない
  （`crates/chat/src/worker/gate.rs`）。
  - **調べられない**: `Clarify` 段階で渡るツールは `emit_ui` **だけ**（`allows`）。`subagent` も
    `doc_search` も `grep` も無い。grilling の鉄則「環境を見れば分かることをユーザーに聞かない」を
    1 ラウンド目から守れず、調べずに書いた質問しか出せなくなる。
  - **質問できない**: 最初の質問カードが出た時点でスレッドは `Plan` へ移り（`stage_from_cards`）、
    以降 `emit_ui` は `plan_card` しか通さない（`allowed_cards`）。2 ラウンド目が出せない。

  「調べられるようになった頃には、もう質問カードが出せない」という順序になるため、宣言しないのが正しい。
  代償として「承認まで実行ツールを渡さない」機械的保証は持たず、手順書で回している
  （多ラウンド面接向けの段階が要るなら別途 `Interview` 相当を設計する）。
- 破壊系（`fs_delete` / `shell` / `office.live_edit`）は `allowed_tools` に宣言しない。
  宣言と上限は `crates/gui/tests/first_party_skills.rs` が守る。
- 出典: [mattpocock/skills](https://github.com/mattpocock/skills) の
  `skills/productivity/grilling` に由来する派生物（MIT・Copyright (c) 2026 Matt Pocock）。
  帰属とライセンス全文は [`grilling/NOTICE`](grilling/NOTICE)。

## slack-notify

- 実体: `.shiki` script が `Shiki.http.request` で Slack Web API（`chat.postMessage`）を呼ぶ。
- 前提: シークレット `slack-bot-token`（Bot User OAuth Token）を**宛先束縛 `slack.com`** で登録し、
  実行主体（対話なら本人・スケジュールならワークフロープリンシパル）に `can_use` を付与すること。
- 使い方: ワークフローの skill ノード `skill:slack-notify@<version>`（入力 `{ channel, text }`）。
  declared_scopes に `http.egress` が必要（scope ceiling）。
- 防御: トークン平文は script に渡らない（ホスト側でヘッダ注入）。宛先束縛 × egress allowlist の
  AND を URL ホスト部リテラルで照合・リダイレクトは一律拒否。監査は status + host のみ（redact）。
