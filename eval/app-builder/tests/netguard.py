"""socket の見張り（conftest が掛け、test_guard が確かめる）。

手元の中継（127.0.0.1:8787・本物の鍵を付けて Anthropic へ送る）と、ループバック以外への接続を例外にする。
"""

from __future__ import annotations

import socket
import threading

from abeval.config import is_loopback_host

RELAY_PORT = 8787

_real_connect = socket.socket.connect
_real_connect_ex = socket.socket.connect_ex
_real_getaddrinfo = socket.getaddrinfo
_blocked: list[str] = []
_blocked_lock = threading.Lock()


class BlockedConnection(RuntimeError):
    """テストの中で許さない接続（OSError ではないので、通信の誤りとして握りつぶされない）。"""


def allowed_address(address: object) -> bool:
    """接続してよい宛先か（ループバックで、手元の中継のポートでないもの・AF_UNIX のパス）。"""
    if not isinstance(address, tuple):
        return True
    host, port = str(address[0]), int(address[1])
    if port == RELAY_PORT:
        return False
    return is_loopback_host(host)


def _block(what: str) -> None:
    with _blocked_lock:
        _blocked.append(what)
    raise BlockedConnection(f"テストの中で許さない接続です: {what}")


def _guarded_connect(self, address):
    if not allowed_address(address):
        _block(repr(address))
    return _real_connect(self, address)


def _guarded_connect_ex(self, address):
    if not allowed_address(address):
        _block(repr(address))
    return _real_connect_ex(self, address)


def _guarded_getaddrinfo(host, port, *args, **kwargs):
    if host is not None:
        name = host.decode() if isinstance(host, bytes) else str(host)
        if not is_loopback_host(name):
            _block(f"getaddrinfo({name!r})")
    try:
        p = int(port) if port is not None else None
    except (TypeError, ValueError):
        p = None
    if p == RELAY_PORT:
        _block(f"getaddrinfo({host!r}, {port!r})")
    return _real_getaddrinfo(host, port, *args, **kwargs)


def guard_installed() -> bool:
    return (
        socket.socket.connect is _guarded_connect
        and socket.socket.connect_ex is _guarded_connect_ex
        and socket.getaddrinfo is _guarded_getaddrinfo
    )


def take_blocked() -> list[str]:
    """記録された試みを取り出して消す（わざと試みるテストが使う）。"""
    with _blocked_lock:
        out = list(_blocked)
        _blocked.clear()
    return out


def install() -> None:
    socket.socket.connect = _guarded_connect
    socket.socket.connect_ex = _guarded_connect_ex
    socket.getaddrinfo = _guarded_getaddrinfo


def uninstall() -> None:
    socket.socket.connect = _real_connect
    socket.socket.connect_ex = _real_connect_ex
    socket.getaddrinfo = _real_getaddrinfo
