"use client";

/// 受信箱の値の見せ方（列の種別に応じて・ユーザーと部署は表示名で出す）と、期限の残りの文言。

import * as React from "react";

import type { components } from "@/generated/api";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";

type FieldType = components["schemas"]["FieldType"];

/// 受信箱に出てくるユーザー・部署の id の表示名を引き、引けたら描き直す。
export function useRefNames(users: string[], roles: string[]): number {
  const [tick, setTick] = React.useState(0);
  const userKey = users.join("\u0000");
  const roleKey = roles.join("\u0000");
  React.useEffect(() => {
    let alive = true;
    const u = userKey ? userKey.split("\u0000") : [];
    const r = roleKey ? roleKey.split("\u0000") : [];
    Promise.all([u.length ? resolveUserNames(u) : false, r.length ? resolveRoleNames(r) : false]).then(
      ([a, b]) => alive && (a || b) && setTick((t) => t + 1),
    );
    return () => {
      alive = false;
    };
  }, [userKey, roleKey]);
  return tick;
}

/// ユーザー id の表示名（引けなければ id の先頭）。
export function userLabel(id: string): string {
  return cachedUserName(id) || (id.length > 12 ? `${id.slice(0, 8)}…` : id);
}

/// 列の値の文字表現（空は「—」）。
export function fieldValueText(type: FieldType, value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  if (Array.isArray(value)) {
    return value.length ? value.map((v) => fieldValueText(type, v)).join("、") : "—";
  }
  if (type === "user_ref" && typeof value === "string") return userLabel(value);
  if (type === "role_ref" && typeof value === "string") return cachedRoleName(value) || value;
  if (typeof value === "number") return value.toLocaleString("ja-JP");
  if (typeof value === "boolean") return value ? "はい" : "いいえ";
  if (type === "date_time" && typeof value === "string") {
    const d = new Date(value);
    return Number.isNaN(d.getTime()) ? value : d.toLocaleString("ja-JP");
  }
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/// 期限の残り（なし・過ぎた・あと n 日 / 時間 / 分）。
export function deadlineText(deadline: string | null | undefined, now = Date.now()): string {
  if (!deadline) return "期限なし";
  const ms = new Date(deadline).getTime() - now;
  if (Number.isNaN(ms)) return "期限なし";
  if (ms <= 0) return "期限切れ";
  const min = Math.floor(ms / 60_000);
  if (min < 60) return `あと ${Math.max(1, min)} 分`;
  const hours = Math.floor(min / 60);
  if (hours < 48) return `あと ${hours} 時間`;
  return `あと ${Math.floor(hours / 24)} 日`;
}

/// 期限が近い（24 時間以内）か。
export function deadlineSoon(deadline: string | null | undefined, now = Date.now()): boolean {
  if (!deadline) return false;
  const ms = new Date(deadline).getTime() - now;
  return ms > 0 && ms < 24 * 3_600_000;
}
