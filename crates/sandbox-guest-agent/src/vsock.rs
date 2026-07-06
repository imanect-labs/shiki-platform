//! vsock リスナ/接続の薄いラッパ（nix の安全ラッパ＋accept FD の所有化のみ unsafe）。

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use nix::sys::socket::{
    accept, bind, listen, socket, AddressFamily, Backlog, SockFlag, SockType, VsockAddr,
};

/// VMADDR_CID_ANY（任意 CID からの接続を受ける）。
const VMADDR_CID_ANY: u32 = 0xFFFF_FFFF;

fn errno(e: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

/// vsock 接続（読み書きは nix の read/write）。
pub(crate) struct VsockConn {
    fd: OwnedFd,
}

impl Read for VsockConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        nix::unistd::read(self.fd.as_raw_fd(), buf).map_err(errno)
    }
}

impl Write for VsockConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        nix::unistd::write(&self.fd, buf).map_err(errno)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// vsock リスナ。
pub(crate) struct VsockListener {
    fd: OwnedFd,
}

impl VsockListener {
    /// 任意 CID・指定ポートで待ち受ける。
    pub(crate) fn bind_any(port: u32) -> io::Result<Self> {
        let fd = socket(
            AddressFamily::Vsock,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .map_err(errno)?;
        let addr = VsockAddr::new(VMADDR_CID_ANY, port);
        bind(fd.as_raw_fd(), &addr).map_err(errno)?;
        listen(&fd, Backlog::new(8).unwrap_or(Backlog::MAXCONN)).map_err(errno)?;
        Ok(VsockListener { fd })
    }

    /// 1 接続を受け付ける。
    pub(crate) fn accept(&self) -> io::Result<VsockConn> {
        let raw = accept(self.fd.as_raw_fd()).map_err(errno)?;
        // accept が返す生 FD を所有型へ包む（唯一の unsafe）。
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(VsockConn { fd })
    }
}
