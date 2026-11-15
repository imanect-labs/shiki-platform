# ADの不変GUIDグループマッパー

Keycloak **26.2.5 / Java 21** 用のLDAP SPI。標準マッパーはグループ名で同期先を解決するため、別OUの同名CNや、削除後に再利用されたSAM名で認可の対応先が入れ替わる。Shikiでは`objectGUID`を内部名とし、CNは表示名、DNと`memberOf`は階層の元情報として扱う。

標準マッパーへ`objectGUID`を指定するだけでは、生バイトのBase64名とUUID検索の表示形式が一致しない。このSPIはLDAPIdentityStoreがADのバイト順を復号した`LDAPObject.getUuid()`を使い、全件同期・ログイン時の所属取得・名前からの再検索を同じ36文字のUUIDに正規化する。改名・OU移動では同じ識別子になり、同名で再作成した別オブジェクトは異なる識別子になる。

- provider ID: `shiki-ad-guid-group-mapper`
- 親providerの`uuidLDAPAttribute`: `objectGUID`
- `group.name.ldap.attribute`: `objectGUID`
- `mode`: `READ_ONLY`
- `preserve.group.inheritance`: `false`（直接の`memberOf`をShikiが復元する）

LDAPへの作成・更新・所属変更・逆方向同期は拒否する。管理対象外のネイティブグループの所属操作は次のdelegateへ渡す。内部SPIに依存するため、Keycloakを更新する場合はバージョンを合わせてコンパイル・テストし直す。

## ビルドと配置

Composeは`deploy/keycloak/Dockerfile`でテストとjarビルドを実行し、Keycloakイメージへ組み込む。

```sh
docker build -t shiki-keycloak:26.2.5 deploy/keycloak
```

既存のKeycloakへ配置する場合はJava 21 / Mavenで`guid-mapper`ディレクトリの`mvn verify`を実行し、`target/shiki-ad-guid-mapper.jar`をKeycloakの`providers/`へ配置して`kc.sh build`・再起動する。Shikiの接続設定を保存するとこのproviderを使用する。旧CN/SAM方式からの更新では、画面で確認後に既存のグループ対応を解除し、旧同期グループを撤去して専用の`ad-guid-v1`配下に再作成し、同期後に再設定する。旧グループIDへのKeycloak内の外部参照も再設定が必要。ロール定義自体は削除しない。

ビルドネットワークがプロキシや独自CAを必要とする場合は、任意のBuildKit secrets `maven_settings`と`network_ca`を指定できる。これらは最終Keycloakイメージの設定や、ADのLDAPS truststoreにはコピーしない。

## 検証範囲

MavenテストはADのGUIDバイト順、名前変更／再作成の分離、設定拒否、読取専用操作、管理対象外グループへの委譲、SPI登録を検証する。CIのComposeテストでも拡張を組み込んだKeycloakを使用する。実企業ADの接続試験には、そのADの証明書と読取用資格情報が必要。
