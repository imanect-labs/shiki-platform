"use client";
import * as React from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { request, post, type Connection, type ConnectionInput } from "./api";
export function ConnectionDialog({
  allowed,
  onClose,
  onSaved,
}: {
  allowed: string[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const [input, setInput] = React.useState<ConnectionInput>({
    connection_url: "",
    users_dn: "",
    groups_dn: "",
    bind_dn: "",
    bind_password: null,
    preserve_group_inheritance: false,
  });
  const [savedBinding, setSavedBinding] = React.useState<{
    url: string;
    dn: string;
  }>();
  const [stored, setStored] = React.useState(false);
  const [loading, setLoading] = React.useState(true);
  const [busy, setBusy] = React.useState(false);
  const [message, setMessage] = React.useState("");
  const [error, setError] = React.useState("");
  React.useEffect(() => {
    const controller = new AbortController();
    request<Connection>("/connection", { signal: controller.signal })
      .then((status) => {
        setStored(status.credential_saved);
        setSavedBinding({
          url: status.connection_url ?? "",
          dn: status.bind_dn ?? "",
        });
        setInput({
          connection_url: status.connection_url ?? "",
          users_dn: status.users_dn ?? "",
          groups_dn: status.groups_dn ?? "",
          bind_dn: status.bind_dn ?? "",
          bind_password: null,
          preserve_group_inheritance: status.preserve_group_inheritance,
        });
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, []);
  const run = async (save: boolean) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      if (save) {
        await request<Connection>("/connection", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(input),
        });
        setStored(true);
        setSavedBinding({ url: input.connection_url, dn: input.bind_dn });
        setInput((old) => ({ ...old, bind_password: null }));
        setMessage(
          "接続設定を保存しました。ADを同期するとグループを取得できます。",
        );
        onSaved();
      } else {
        await post<boolean>("/connection/test", input);
        setMessage("ADへの接続・バインド認証に成功しました。");
      }
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const fields = [
    {
      key: "connection_url",
      label: "接続先（LDAPS）",
      placeholder: "ldaps://ad.example.co.jp:636",
    },
    {
      key: "users_dn",
      label: "ユーザー検索ベースDN",
      placeholder: "OU=Users,DC=example,DC=co,DC=jp",
    },
    {
      key: "groups_dn",
      label: "グループ検索ベースDN",
      placeholder: "OU=Groups,DC=example,DC=co,DC=jp",
    },
    {
      key: "bind_dn",
      label: "バインドDN",
      placeholder: "CN=ShikiReader,OU=Service,DC=example,DC=co,DC=jp",
    },
  ] as const;
  const reusable =
    stored &&
    savedBinding?.url === input.connection_url &&
    savedBinding.dn === input.bind_dn;
  const valid =
    fields.every((f) => input[f.key].trim()) &&
    (reusable || input.bind_password) &&
    allowed.length > 0;
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent className="max-h-[90dvh] overflow-y-auto">
        <DialogTitle>AD接続設定</DialogTitle>
        <DialogDescription>
          Keycloak経由でADを読み取ります。ADへの書込みは行いません。
        </DialogDescription>
        <p className="text-[11px] leading-5 text-muted-foreground">
          {allowed.length
            ? `接続を許可されたホスト: ${allowed.join("、")}`
            : "接続先が運用設定で許可されていません。auth.directory_ldap_allowed_hostsの設定が必要です。"}
        </p>
        {loading ? (
          <p role="status" className="text-xs">
            設定を取得中…
          </p>
        ) : (
          <fieldset disabled={busy} className="space-y-3">
            {fields.map((field) => (
              <label
                key={field.key}
                className="block space-y-1.5 text-xs font-medium"
              >
                <span>{field.label}</span>
                <Input
                  value={input[field.key]}
                  placeholder={field.placeholder}
                  onChange={(e) => {
                    setInput({ ...input, [field.key]: e.target.value });
                    setMessage("");
                  }}
                />
              </label>
            ))}
            <label className="block space-y-1.5 text-xs font-medium">
              <span>バインド用パスワード</span>
              <Input
                type="password"
                autoComplete="new-password"
                value={input.bind_password ?? ""}
                placeholder={
                  reusable
                    ? "保存済み・変更する場合だけ入力"
                    : "パスワードを入力"
                }
                onChange={(e) =>
                  setInput({ ...input, bind_password: e.target.value || null })
                }
              />
            </label>
            {stored && !reusable && (
              <p className="text-[11px] leading-5 text-muted-foreground">
                接続先またはバインドDNを変更した場合は、パスワードを再入力してください。
              </p>
            )}
            <label className="flex items-center gap-2 text-xs">
              <input
                type="checkbox"
                checked={input.preserve_group_inheritance ?? false}
                onChange={(e) =>
                  setInput({
                    ...input,
                    preserve_group_inheritance: e.target.checked,
                  })
                }
              />
              ADグループの入れ子を保持する
            </label>
            <p className="text-[11px] leading-5 text-muted-foreground">
              複数の親や循環があるADでは、階層保持の同期が失敗します。その場合は保持を解除し、OUの配置からロールを作成できます。
            </p>
          </fieldset>
        )}
        {error && (
          <p role="alert" className="text-xs leading-5 text-destructive">
            {error}
          </p>
        )}
        <p role="status" className="text-xs leading-5">
          {busy ? "AD接続を確認中…" : message}
        </p>
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="ghost" disabled={busy} onClick={onClose}>
            閉じる
          </Button>
          <Button
            variant="outline"
            disabled={busy || loading || !valid}
            onClick={() => run(false)}
          >
            接続をテスト
          </Button>
          <Button
            disabled={busy || loading || !valid}
            onClick={() => run(true)}
          >
            テストして保存
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
