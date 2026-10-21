# Ichitaro OpenOffice Filter Research

historical OpenOffice Ichitaro Document Filter は、現在 rjtd にとって最優先の調査 artifact である。

ダウンロードした `.oxt` は `third-party/ichitaro-filter/` 以下にローカル保存する。

追跡対象の知見は [openjtd-spec/rfc/0002-ichitaro-openoffice-filter.ja.md](../openjtd-spec/rfc/0002-ichitaro-openoffice-filter.ja.md) に記録する。

## Working Rule

法務レビューで別途許可されない限り、この artifact は compatibility と naming の参照としてのみ使う。

同梱 license は decompilation と reverse engineering を制限しているため、rjtd development は clean-room を維持しなければならない。

- metadata は有用である。
- OpenOffice filter/type registration は有用である。
- package file tree は有用である。
- visible strings は仮説作りの手がかりになる。
- implementation code を DLL から copy または reconstruct してはならない。
