"""テストの中で動かす abeval.proxy（ループバックの一時のポート・上流は偽の上流）と、その制御のクライアント。"""

from __future__ import annotations

import threading
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from abeval.config import load_config
from abeval.proxy import ProxyServer, ProxyState
from abeval.proxy_control import ProxyControl


@dataclass
class RunningProxy:
    url: str
    state: ProxyState

    def control(self) -> ProxyControl:
        return ProxyControl(self.url, self.state.token)


@contextmanager
def running_proxy(upstream_url: str, capture: bool = False):
    cfg = load_config()
    state = ProxyState(cfg, upstream=upstream_url, capture=capture)
    server = ProxyServer(state, "127.0.0.1", 0)
    # テストでは応答のスレッドの終わりを待って閉じる（daemon のままだと、テストの後も動くスレッドがインタプリタの
    # 終了と重なって異常終了することがある）。
    server.daemon_threads = False
    server.block_on_close = True
    state.write_token()
    t = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    t.start()
    try:
        yield RunningProxy(server.url, state)
    finally:
        server.shutdown()
        server.server_close()


def config_for(proxy_url: str, upstream_url: str, tmp_path: Path, extra: str = "") -> Path:
    """eval.toml の写しで、中継の口だけを一時のポートにしたもの（ABEVAL_CONFIG に渡す）。"""
    from abeval.config import DEFAULT_CONFIG

    host_port = proxy_url.removeprefix("http://")
    text = DEFAULT_CONFIG.read_text(encoding="utf-8")
    text = text.replace('listen = "127.0.0.1:18200"', f'listen = "{host_port}"')
    text = text.replace('upstream = "http://127.0.0.1:8787"', f'upstream = "{upstream_url}"')
    p = tmp_path / "eval.test.toml"
    p.write_text(text + extra, encoding="utf-8")
    return p
