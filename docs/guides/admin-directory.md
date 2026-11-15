# AD連携とロール管理

組織管理者は「アクセス管理 → AD連携」で、Keycloak経由のAD接続、グループ同期、ロールとの対応付けを管理する。「ロール」は同じ管理APIと保存内容を使用し、「メンバー」は最終認証同期時の所属を表示する。

## 運用者の事前設定

Keycloak 26.2系のLDAP federationと管理RESTを使用する。APIの`auth.provisioner_client_id` / `auth.provisioner_client_secret`を設定し、対象realmのユーザー・グループ・LDAP componentを管理できるサービスアカウントを用意する。管理資格情報はブラウザへ渡さない。

`config/default.toml`と同じ形式の運用設定の`[auth]`に、接続を認めるADホストを明示する。

```toml
[auth]
directory_ldap_allowed_hosts = ["ad.example.co.jp"]
```

許可リストの既定は空で、未設定では新規保存・接続テストを拒否する。LDAPSの636番だけを許可する。KeycloakがADへ到達できることと、KeycloakのJava truststoreがADの証明書を信頼することを事前に確認する。

## 接続・同期

1. 接続先、ユーザー／グループの検索ベースDN、読み取り用バインドDN・パスワードを入力して「テストして保存」する。
2. 「ADを同期」でグループを取得する。処理は永続キューで継続し、進捗と履歴を画面で確認できる。
3. OUをたどるか、全OUからグループ名・OU名を検索する。ロールも階層移動とサーバ検索で探す。

パスワードはKeycloakに保存し、ShikiのDB、操作キュー、監査メタデータ、API応答へ保存しない。入力を空欄にすると保存済みcredentialを維持する。接続先またはバインドDNを変更する場合はパスワードの再入力を必須とし、保存済みcredentialを別ホストへ転送しない。LDAPはREAD_ONLYで、シキのロール定義はLDAP同期先の外に作る。

接続と固定tenant／orgマッパーは組織ごとに分離し、AD属性で認証先のtenantを変更できない構成にする。同期先は`/tenants/<tenant>/<org>/ad`、ロール定義は`/__shiki_roles/<tenant>/<org>`。後者をLDAP mapperの同期先に含める構成は拒否する。ロール定義のグループはAD所属の正本には使用しない。

接続先・検索DN・階層保持の設定を変えると、旧スナップショットは保存処理の前に無効化されます。再同期が成功するまでグループ一覧・自動作成・対応付けに使用しません。同期後、既存の対応表は同一グループIDのものだけ復帰します。

## 自動作成・CSV・確認

「ADから自動作成」は選択したグループ、またはすべてを対象にする。OUの配置とグループの入れ子は別の情報であり、OU配下にあるだけで所属は付与されない。OUの元情報はADの`distinguishedName`属性から取得する。

グループの入れ子を利用する場合は接続設定で階層保持を有効にして再同期する。Keycloakの階層保持は複数親や循環を表現できない。その場合はOU方式を使用する。シキのこの管理画面では、既存のOpenFGA `role#member`による上位ロールへの所属継承を使い、認可relationを追加しない。

確認画面では作成後のロール階層、既存ロールの変更前／変更後、グループごとの対応前／対応後を確認する。「編集内容に取り込む」はまだ保存しない。最終確認の「反映する」で処理を受け付ける。自動作成済みの同じ元から再度実行してもロールは重複作成しない。自動作成後に名前や親を手編集した場合は、再生成で上書きせず確認エラーにする。

CSVはUTF-8、ヘッダー`group_id,role_id`。1グループに複数ロールを指定する場合は複数行にする。`role_id`が空の行は対応解除、ファイルにないグループは変更しない。group_idはKeycloakのグループIDで、ADのobjectGUIDとは別である。Keycloakでグループを再作成した場合は対応を再設定する。IDはグループの識別情報とロール編集画面で確認する。全行を検証してから確認案を作り、不明IDや重複、解除と付与の混在を拒否する。

## 所属への反映と再試行

既存のIdP管理ロールは参照専用で、改名・親変更・所属の追加はIdP側で行う。Shiki管理のロールは組織ごとの専用ID領域で作成し、この管理画面で編集・対応付けできる。

ロールの所属は検証済みIdP claimsを根拠とする。保存した対応表をログイン／セッション更新時に解決して既存の所属同期へ渡す。対応解除やAD所属の削除も同じ経路で剥奪する。保存直後の全セッション即時書換えは行わない。対応表が読めない場合は誤剥奪を避けて同期を保留し、エラーをサーバログへ記録する。

Keycloak、OpenFGA、PostgreSQLをまたぐ反映は全件一括の原子処理ではない。新規IDは外部への書込み前にDBで所有権を予約し、他組織のclaims同期による衝突・取り込みを防ぐ。予約途中のロールは一覧へ公開しない。各段階を冪等にし、確定した変更と進捗・監査を同じDBトランザクションに保存する。途中停止時は操作を停止状態で表示し、「続きから再実行」で再開する。同じ組織の未完了操作がある間は、別のロール・対応表・AD同期操作を受け付けない。接続設定はpending／running中の保存を拒否し、failed時には復旧のため変更できる。実行時にも管理者権限を再確認する。

版競合は古い確認案を自動上書きしない。未反映の編集は再読込前に確認する。テナント撤去は実行中の管理操作と排他にし、所有するLDAP接続と管理ロール定義も撤去する。外部状態を含むため、この管理情報が存在するテナントのretenantは既存の副産物移行チェックで拒否される。

一覧は1ページ最大100件（画面の通常取得は40件）。AD同期・自動作成は各10,000グループ／ロール、階層は128段まで、CSVは1MB・1,000行まで。確認案・一操作に含める対応表のロールID合計、およびロールの親変更による配下への影響範囲は20,000件まで。上限超過は切り捨てず拒否する。大量の部署は接続の検索ベースを分ける運用を検討する。

## 参照

- [Keycloak管理ガイド：LDAP](https://www.keycloak.org/docs/26.2.0/server_admin/index.html#_ldap)
- [Keycloak 26.2のGroupLDAPStorageMapper実装](https://github.com/keycloak/keycloak/blob/26.2.0/federation/ldap/src/main/java/org/keycloak/storage/ldap/mappers/membership/group/GroupLDAPStorageMapper.java)
- [ADのOU設計](https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/plan/reviewing-ou-design-concepts)
- [ADセキュリティグループ](https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/understand-security-groups)
