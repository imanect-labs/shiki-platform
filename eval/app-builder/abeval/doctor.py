"""評価の前の点検（`python -m abeval.doctor`・GAP-30 §4.4）。

点検の項目（どれかが NG なら終了コード 1）:

- 共通: `/healthz`・alice・bob・charlie のログインと `/me` の org（alice と bob は a-corp、charlie は b-corp）・
  `/llm/models`（eval は claude-haiku-5-5 と default、stub は default）・番兵・alice のカスタム指示が空
  （404 は「その機能が無い版」で空とみなす）・OpenAPI の機能の有無（情報）。
- eval のとき（`--go` の前）: API のポートが 10067・:8080 の組が動いていない（警告）・llm-eval.toml（権限 600・
  anthropic の base_url が abeval.proxy・api_key がダミー・session_header・モデルの組）・abeval.proxy の
  `GET /abeval/spend`・手元の中継の台帳の累計（読むだけ・8787 へは繋がない）・評価の台帳の累計と残り。
- api.anthropic.com へは繋がない（鍵を持たないので、401 の確かめは要らない）。

`--init-sentinel` を付けたときだけ、alice のルート直下に番兵（フォルダー `abeval-sentinel`）を作る。番兵は
「評価用のサーバー」の印で、無いサーバーでは fixture の作成も片付けも拒む。
"""

from __future__ import annotations

import argparse
import os
import stat
import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit

import httpx

from abeval import fixtures
from abeval.config import Config, load_config
from abeval.ledger import Ledger, LedgerCorrupt, read_relay_total_usd
from abeval.pricing import micros_to_usd
from abeval.proxy import CONTROL_HEADER, DUMMY_CREDENTIAL, TOKEN_FILE
from abeval.shiki import SEED_ORGS, LoginError, ShikiClient, ShikiError

EVAL_PORT = 10067
CAMPAIGN_URL = "http://localhost:8080"
EVAL_MODELS = frozenset({"claude-haiku-5-5", "default"})
STUB_MODELS = frozenset({"default"})
LLM_EVAL_CONFIG = Path("~/.config/shiki-dev/llm-eval.toml")
EMPTY_INSTRUCTIONS = {"about": "", "style": "", "tone": "default"}


@dataclass
class Check:
    name: str
    status: str  # ok | ng | warn | info
    detail: str = ""

    def line(self) -> str:
        mark = {"ok": "[ok]  ", "ng": "[NG]  ", "warn": "[warn]", "info": "[info]"}[self.status]
        return f"{mark} {self.name}: {self.detail}" if self.detail else f"{mark} {self.name}"


def _instructions_empty(ci: dict) -> bool:
    return (
        str(ci.get("about", "")).strip() == ""
        and str(ci.get("style", "")).strip() == ""
        and ci.get("tone", "default") == "default"
    )


def check_server(
    client_for: Callable[[str], ShikiClient], *, mode: str, http: httpx.Client, api_url: str
) -> list[Check]:
    """サーバーの点検（ログイン・モデル・番兵・カスタム指示・機能）。"""
    out: list[Check] = []
    try:
        code = http.get(f"{api_url}/healthz").status_code
        out.append(Check("healthz", "ok" if code == 200 else "ng", f"{api_url}/healthz → {code}"))
    except httpx.HTTPError as e:
        out.append(Check("healthz", "ng", f"{api_url} に届きません: {type(e).__name__}"))
        return out
    alice = None
    for user, org in SEED_ORGS.items():
        try:
            c = client_for(user)
            me = c.login()
        except (LoginError, ShikiError, httpx.HTTPError) as e:
            out.append(Check(f"login {user}", "ng", str(e)[:300]))
            continue
        got = me.get("org")
        out.append(Check(f"login {user}", "ok" if got == org else "ng", f"org={got}（期待 {org}）"))
        if user == "alice":
            alice = c
    if alice is None:
        out.append(Check("alice の点検", "ng", "alice でログインできないので、残りを点検しません"))
        return out
    want = EVAL_MODELS if mode == "eval" else STUB_MODELS
    try:
        ids = {m.get("id") for m in alice.llm_models().get("models", [])}
        missing = sorted(want - ids)
        extra = sorted(i for i in ids - want if i)
        detail = f"{sorted(ids)}" + (f"（無い: {missing}）" if missing else "")
        if extra and mode == "eval":
            detail += f"（評価の設定に無いモデル: {extra}）"
        out.append(Check("llm models", "ng" if missing else "ok", detail))
    except ShikiError as e:
        out.append(Check("llm models", "ng", str(e)[:300]))
    try:
        node = fixtures.find_sentinel(alice)
        out.append(
            Check("番兵", "ok", f"{fixtures.SENTINEL_NAME}（{node['id']}）")
            if node
            else Check("番兵", "ng", "無い（評価用のサーバーなら --init-sentinel で作る）")
        )
    except ShikiError as e:
        out.append(Check("番兵", "ng", str(e)[:300]))
    try:
        ci = alice.custom_instructions()
        if ci is None:
            out.append(Check("カスタム指示", "ok", "口が無い版（空とみなす）"))
        elif _instructions_empty(ci):
            out.append(Check("カスタム指示", "ok", "空"))
        else:
            sizes = {k: len(str(ci.get(k, ""))) for k in ("about", "style")}
            out.append(
                Check(
                    "カスタム指示", "ng", f"空ではない（文字数 {sizes}・tone={ci.get('tone')!r}）"
                )
            )
    except ShikiError as e:
        out.append(Check("カスタム指示", "ng", str(e)[:300]))
    try:
        out.append(Check("機能（OpenAPI）", "info", str(alice.capabilities())))
    except (ShikiError, httpx.HTTPError, ValueError) as e:
        out.append(Check("機能（OpenAPI）", "ng", str(e)[:300]))
    return out


def check_llm_config(path: Path, cfg: Config) -> list[Check]:
    """llm-eval.toml の点検（鍵の値は出さない）。"""
    path = Path(path).expanduser()
    try:
        st = path.stat()
    except FileNotFoundError:
        return [Check("llm-eval.toml", "ng", f"ありません: {path}")]
    out: list[Check] = []
    mode = stat.S_IMODE(st.st_mode)
    out.append(
        Check("llm-eval.toml の権限", "ok" if mode == 0o600 else "ng", f"{mode:03o}（600 にする）")
    )
    try:
        with path.open("rb") as f:
            raw = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError) as e:
        return [*out, Check("llm-eval.toml", "ng", f"読めません: {type(e).__name__}")]
    llm = raw.get("llm") or {}
    providers = [p for p in llm.get("providers") or [] if p.get("backend") == "anthropic"]
    if len(providers) != 1:
        return [
            *out,
            Check("llm-eval.toml の anthropic", "ng", f"{len(providers)} 個（1 個にする）"),
        ]
    p = providers[0]
    proxy = f"http://{cfg.proxy.host}:{cfg.proxy.port}"
    base = str(p.get("base_url", "")).rstrip("/")
    out.append(Check("base_url", "ok" if base == proxy else "ng", f"{base}（期待 {proxy}）"))
    dummy = p.get("api_key") == DUMMY_CREDENTIAL
    out.append(
        Check(
            "api_key",
            "ok" if dummy else "ng",
            "ダミー" if dummy else "ダミーではない（値は出さない）",
        )
    )
    sh = str(p.get("session_header", "")).lower()
    want_sh = cfg.proxy.session_header
    out.append(
        Check("session_header", "ok" if sh == want_sh else "ng", f"{sh!r}（期待 {want_sh!r}）")
    )
    models = {m.get("id") for m in llm.get("models") or []}
    out.append(
        Check(
            "llm-eval.toml のモデル",
            "ok" if models == EVAL_MODELS else "ng",
            f"{sorted(m for m in models if m)}（期待 {sorted(EVAL_MODELS)}）",
        )
    )
    pid = p.get("id")
    real = sorted(
        m.get("id") for m in llm.get("models") or [] if m.get("provider") == pid and m.get("id")
    )
    outside = [m for m in real if m not in cfg.proxy.allowed_models]
    out.append(
        Check(
            "anthropic のモデル",
            "ng" if outside else "ok",
            f"{real}" + (f"（中継の許可リストに無い: {outside}）" if outside else ""),
        )
    )
    return out


def check_budget(cfg: Config, http: httpx.Client) -> list[Check]:
    """abeval.proxy・手元の中継の台帳・評価の台帳（読むだけ）。"""
    out: list[Check] = []
    proxy = f"http://{cfg.proxy.host}:{cfg.proxy.port}"
    token_path = cfg.paths.home / TOKEN_FILE
    try:
        token = token_path.read_text().strip()
    except OSError:
        out.append(
            Check("abeval.proxy", "ng", f"合言葉がありません: {token_path}（中継を起動する）")
        )
        token = None
    if token is not None:
        try:
            r = http.get(f"{proxy}/abeval/spend", headers={CONTROL_HEADER: token})
            if r.status_code == 200:
                s = r.json()
                out.append(
                    Check(
                        "abeval.proxy",
                        "ok",
                        f"{proxy}（評価の累計 ${micros_to_usd(s['eval_spent_micros']):.4f} / "
                        f"${micros_to_usd(s['eval_cap_micros']):.2f}・予約 "
                        f"${micros_to_usd(s['reserved_micros']):.4f}）",
                    )
                )
            else:
                out.append(Check("abeval.proxy", "ng", f"{proxy}/abeval/spend → {r.status_code}"))
        except (httpx.HTTPError, ValueError, KeyError) as e:
            out.append(Check("abeval.proxy", "ng", f"{proxy} に届きません: {type(e).__name__}"))
    cap = cfg.caps.global_cap_usd
    try:
        relay = read_relay_total_usd(cfg.paths.relay_ledger)
        out.append(
            Check(
                "手元の中継の台帳",
                "ok" if relay < cap else "ng",
                f"累計 ${relay:.4f} / ${cap:.2f}（残り ${cap - relay:.4f}）",
            )
        )
    except FileNotFoundError:
        out.append(Check("手元の中継の台帳", "warn", f"ありません: {cfg.paths.relay_ledger}"))
    except LedgerCorrupt as e:
        out.append(Check("手元の中継の台帳", "ng", str(e)[:300]))
    try:
        spent = Ledger(cfg.paths.ledger).refresh().total_micros
        rest = cfg.caps.eval_cap_micros - spent
        out.append(
            Check(
                "評価の台帳",
                "ok" if rest > 0 else "ng",
                f"累計 ${micros_to_usd(spent):.4f} / ${cfg.caps.eval_cap_usd:.2f}"
                f"（残り ${micros_to_usd(rest):.4f}・{cfg.paths.ledger}）",
            )
        )
    except LedgerCorrupt as e:
        out.append(Check("評価の台帳", "ng", str(e)[:300]))
    return out


def run_checks(
    *,
    mode: str,
    api_url: str,
    cfg: Config,
    llm_config: Path = LLM_EVAL_CONFIG,
    transport: httpx.BaseTransport | None = None,
    init_sentinel: bool = False,
) -> list[Check]:
    if mode not in ("eval", "stub"):
        raise ValueError(f"mode は eval か stub です: {mode!r}")
    clients: dict[str, ShikiClient] = {}

    def client_for(user: str) -> ShikiClient:
        if user not in clients:
            clients[user] = ShikiClient(
                api_url, user, transport=transport, stub_only=(mode == "stub")
            )
        return clients[user]

    out: list[Check] = []
    with httpx.Client(transport=transport, timeout=10.0, follow_redirects=False) as http:
        try:
            if mode == "eval":
                port = urlsplit(api_url).port
                out.append(
                    Check(
                        "ポート", "ok" if port == EVAL_PORT else "ng", f"{port}（期待 {EVAL_PORT}）"
                    )
                )
                try:
                    code = http.get(f"{CAMPAIGN_URL}/healthz").status_code
                    out.append(
                        Check(
                            "キャンペーンの組（:8080）", "warn", f"動いています（{code}）。重ねない"
                        )
                    )
                except httpx.HTTPError:
                    out.append(Check("キャンペーンの組（:8080）", "ok", "動いていない"))
            if init_sentinel:
                node, made = fixtures.init_sentinel(client_for("alice"))
                out.append(
                    Check(
                        "番兵を作る",
                        "ok",
                        ("作りました" if made else "既にあります") + f"（{node['id']}）",
                    )
                )
            out += check_server(client_for, mode=mode, http=http, api_url=api_url)
            if mode == "eval":
                out += check_llm_config(llm_config, cfg)
                out += check_budget(cfg, http)
        finally:
            for c in clients.values():
                c.close()
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python -m abeval.doctor", description="評価の前の点検")
    ap.add_argument("--mode", choices=("eval", "stub"), default="eval")
    ap.add_argument(
        "--api-url", help="既定は eval.toml の [server] api_url（stub は :8080）・ABEVAL_API_URL"
    )
    ap.add_argument("--llm-config", type=Path, default=LLM_EVAL_CONFIG)
    ap.add_argument("--init-sentinel", action="store_true", help="alice のルート直下に番兵を作る")
    args = ap.parse_args(argv)
    cfg = load_config()
    api_url = (
        args.api_url
        or os.environ.get("ABEVAL_API_URL")
        or (cfg.api_url if args.mode == "eval" else CAMPAIGN_URL)
    ).rstrip("/")
    print(f"abeval.doctor: mode={args.mode} api={api_url}")
    checks = run_checks(
        mode=args.mode,
        api_url=api_url,
        cfg=cfg,
        llm_config=args.llm_config,
        init_sentinel=args.init_sentinel,
    )
    for c in checks:
        print(c.line())
    bad = [c for c in checks if c.status == "ng"]
    print(f"=> {'NG ' + str(len(bad)) + ' 件' if bad else 'すべて合格'}")
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
