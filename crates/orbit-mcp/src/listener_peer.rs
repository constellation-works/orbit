//! User id of the process that owns an accepted TCP peer.
//!
//! A TCP socket has no `SO_PEERCRED`. The listener admits a connection only
//! when the *client* socket — the endpoint whose local address is the accepted
//! peer — is owned by the server's effective uid. The server's own accepted
//! socket is the reversed tuple and always carries the server's uid, so it is
//! never consulted. A missing, unreadable, or ambiguous record is a refusal:
//! a remote TCP peer has no local socket to inspect.

use std::net::SocketAddr;

/// Why an accepted peer is not admitted. `None` from [`peer_refusal`] admits.
pub(crate) type PeerRefusal = Option<&'static str>;

/// Uids of the two established rows for one connection.
///
/// `client` owns the socket whose local address is the accepted peer.
/// `server` owns the reversed row. Tests supply both so a lookup that reads
/// the reversed row cannot pass as the client. Production always passes `None`.
#[derive(Clone, Copy)]
pub(crate) struct TcpUidRows {
    client: u32,
    server: u32,
}

impl TcpUidRows {
    #[cfg(feature = "test-support")]
    pub(crate) fn new(client: u32, server: u32) -> Self {
        Self { client, server }
    }
}

/// `Some` when the peer must be closed before any byte is read.
pub(crate) fn peer_refusal(
    local: SocketAddr,
    peer: SocketAddr,
    allowed_uid: u32,
    rows: Option<TcpUidRows>,
) -> PeerRefusal {
    match client_uid(local, peer, rows) {
        ClientUid::Uid(uid) if uid == allowed_uid => None,
        ClientUid::Uid(_) => Some("peer uid differs from the server"),
        ClientUid::Missing => Some("no local socket matches the peer"),
        ClientUid::Ambiguous => Some("more than one uid matches the peer"),
        ClientUid::Unreadable => Some("peer uid could not be read"),
    }
}

/// Effective uid of this process, captured when the listener binds.
///
/// Callers of [`crate::McpListener::bind`] cannot supply a uid. `uid_t` is
/// `u32` on the platforms Orbit ships.
#[cfg(unix)]
pub(crate) fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
pub(crate) fn current_euid() -> u32 {
    // No peer credential is available, so the listener admits nobody.
    u32::MAX
}

enum ClientUid {
    Uid(u32),
    Missing,
    Ambiguous,
    Unreadable,
}

fn client_uid(local: SocketAddr, peer: SocketAddr, rows: Option<TcpUidRows>) -> ClientUid {
    // The fixture is the same two-row table the Linux reader parses, built
    // from the accepted addresses. It runs ahead of the platform lookup so
    // the direction check does not depend on a second operating-system user.
    if let Some(rows) = rows {
        return table::from_rows(local, peer, rows);
    }
    #[cfg(target_os = "linux")]
    {
        linux::client_uid(local, peer)
    }
    #[cfg(target_os = "macos")]
    {
        macos::client_uid(local, peer)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (local, peer);
        ClientUid::Missing
    }
}

/// `/proc/net/tcp` row parser. Linux reads that file; the test fixture builds
/// the same text from the accepted addresses and uses this parser, so a
/// reversed-row mistake fails the fixture on every platform.
mod table {
    use super::{ClientUid, SocketAddr, TcpUidRows};
    use std::fmt::Write as _;
    use std::net::{SocketAddrV4, SocketAddrV6};

    pub(super) fn from_rows(local: SocketAddr, peer: SocketAddr, rows: TcpUidRows) -> ClientUid {
        let Some((row_local, row_remote)) = endpoints(peer, local) else {
            return ClientUid::Missing;
        };
        // Client row first, then the reversed server row. The header is what
        // [`scan`] skips. `retrnsmt` sits immediately before the uid.
        let text = format!(
            "sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
             0: {row_local} {row_remote} 01 00000000:00000000 00:00000000 00000000 {client} 0 0 1 1 0\n\
             1: {row_remote} {row_local} 01 00000000:00000000 00:00000000 00000000 {server} 0 0 2 1 0\n",
            client = rows.client,
            server = rows.server,
        );
        scan(&text, &row_local, &row_remote)
    }

    pub(super) fn endpoints(peer: SocketAddr, local: SocketAddr) -> Option<(String, String)> {
        // The client socket's local address is the accepted peer, and its
        // remote address is this accepted socket.
        match (peer, local) {
            (SocketAddr::V4(peer), SocketAddr::V4(local)) => {
                Some((format_v4(peer), format_v4(local)))
            }
            (SocketAddr::V6(peer), SocketAddr::V6(local)) => {
                Some((format_v6(peer), format_v6(local)))
            }
            _ => None,
        }
    }

    pub(super) fn scan(text: &str, local: &str, remote: &str) -> ClientUid {
        let mut found = None;
        // The first line is the column header.
        for line in text.lines().skip(1) {
            let mut fields = line.split_whitespace();
            let Some(row_local) = fields.nth(1) else {
                continue;
            };
            let Some(row_remote) = fields.next() else {
                continue;
            };
            let Some(state) = fields.next() else {
                continue;
            };
            // 01 is TCP_ESTABLISHED. The listening socket and the server's
            // accepted socket are different rows and must not match.
            if state != "01" || !same_addr(row_local, local) || !same_addr(row_remote, remote) {
                continue;
            }
            // tx:rx, tr:tm->when, retrnsmt, then uid.
            let Some(uid_text) = fields.nth(3) else {
                return ClientUid::Ambiguous;
            };
            let Ok(uid) = uid_text.parse::<u32>() else {
                return ClientUid::Ambiguous;
            };
            match found {
                None => found = Some(uid),
                Some(previous) if previous == uid => {}
                Some(_) => return ClientUid::Ambiguous,
            }
        }
        match found {
            Some(uid) => ClientUid::Uid(uid),
            None => ClientUid::Missing,
        }
    }

    fn same_addr(row: &str, needle: &str) -> bool {
        row.len() == needle.len() && row.eq_ignore_ascii_case(needle)
    }

    /// Linux prints each address word with `%08X` of that word after `ntohl`.
    /// [`Ipv4Addr::to_bits`] is the numeric big-endian value; [`u32::from_be`]
    /// is `ntohl` on both endiannesses.
    fn format_v4(addr: SocketAddrV4) -> String {
        let displayed = u32::from_be(addr.ip().to_bits());
        format!("{displayed:08X}:{:04X}", addr.port())
    }

    fn format_v6(addr: SocketAddrV6) -> String {
        let mut out = String::with_capacity(37);
        let octets = addr.ip().octets();
        for word in octets.as_chunks::<4>().0 {
            let displayed = u32::from_be(u32::from_be_bytes(*word));
            let _ = write!(out, "{displayed:08X}");
        }
        let _ = write!(out, ":{:04X}", addr.port());
        out
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ClientUid, SocketAddr};

    /// How many times to re-read a missing row. The handshake is complete
    /// before `accept` returns, so the client row is normally already
    /// published; a few immediate re-reads cover the publication race without
    /// delaying a remote peer whose row never appears.
    const MISSING_REREADS: usize = 4;

    pub(super) fn client_uid(local: SocketAddr, peer: SocketAddr) -> ClientUid {
        let mut last = ClientUid::Missing;
        for _ in 0..MISSING_REREADS {
            last = read_once(local, peer);
            if !matches!(last, ClientUid::Missing) {
                return last;
            }
        }
        last
    }

    fn read_once(local: SocketAddr, peer: SocketAddr) -> ClientUid {
        let Some((row_local, row_remote)) = super::table::endpoints(peer, local) else {
            return ClientUid::Missing;
        };
        let path = match local {
            SocketAddr::V4(_) => "/proc/net/tcp",
            SocketAddr::V6(_) => "/proc/net/tcp6",
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => return ClientUid::Unreadable,
        };
        super::table::scan(&text, &row_local, &row_remote)
    }
}

/// Layout of xnu `socket_fdinfo` (`bsd/sys/proc_info.h`). libc exposes the
/// syscalls and `vinfo_stat` but not this structure. The union is sized by
/// the Unix-domain member (`SOCK_MAXADDRLEN` is 255); a smaller member does
/// not change `size_of`. A `proc_pidfdinfo` result that is not exactly that
/// size is ignored, so a layout drift fails closed instead of reading a
/// neighboring field. The uid comes from the owning process, because xnu
/// leaves the socket's `soi_stat` zeroed.
#[cfg(target_os = "macos")]
mod macos {
    use super::{ClientUid, SocketAddr};
    use std::io;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr as StdSocketAddr};

    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKINFO_TCP: libc::c_int = 2;
    const TSI_S_ESTABLISHED: libc::c_int = 4;
    const INI_IPV4: u8 = 1;
    const INI_IPV6: u8 = 2;
    /// `SOCK_MAXADDRLEN` in `<sys/socket.h>`.
    const SOCK_MAXADDRLEN: usize = 255;

    pub(super) fn client_uid(local: SocketAddr, peer: SocketAddr) -> ClientUid {
        let pids = match list_pids() {
            Ok(pids) => pids,
            Err(_) => return ClientUid::Unreadable,
        };
        let mut found = None;
        for pid in pids {
            for fd in socket_fds(pid) {
                let Some(info) = socket_info(pid, fd) else {
                    continue;
                };
                if info.psi.soi_kind != SOCKINFO_TCP {
                    continue;
                }
                // SAFETY: `soi_kind` is `SOCKINFO_TCP`, so the kernel wrote
                // `pri_tcp` as the active member of `soi_proto`.
                let tcp = unsafe { &info.psi.soi_proto.pri_tcp };
                if tcp.tcpsi_state != TSI_S_ESTABLISHED {
                    continue;
                }
                let Some((socket_local, socket_foreign)) = endpoints(&tcp.tcpsi_ini) else {
                    continue;
                };
                // Client socket: its local address is the accepted peer and
                // its foreign address is this accepted socket.
                if !same_endpoint(socket_local, peer) || !same_endpoint(socket_foreign, local) {
                    continue;
                }
                // xnu leaves `soi_stat` zeroed for sockets, so the owner is
                // the effective uid of the process holding the client socket.
                // A process we cannot read is skipped, which fails closed.
                let Some(uid) = process_uid(pid) else {
                    continue;
                };
                match found {
                    None => found = Some(uid),
                    Some(previous) if previous == uid => {}
                    Some(_) => return ClientUid::Ambiguous,
                }
            }
        }
        match found {
            Some(uid) => ClientUid::Uid(uid),
            None => ClientUid::Missing,
        }
    }

    fn list_pids() -> io::Result<Vec<libc::pid_t>> {
        // SAFETY: a null buffer asks for the byte count the pid list needs.
        let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(io::Error::last_os_error());
        }
        let width = std::mem::size_of::<libc::pid_t>();
        let count = (needed as usize / width).saturating_add(32);
        let mut buf: Vec<libc::pid_t> = vec![0; count];
        let bytes = buf.len().saturating_mul(width);
        let bytes = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
        // SAFETY: `buf` is writable for `bytes` bytes and contains `pid_t`s.
        let written = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        let n = (written as usize) / width;
        buf.truncate(n.min(buf.len()));
        buf.retain(|pid| *pid > 0);
        Ok(buf)
    }

    fn socket_fds(pid: libc::pid_t) -> Vec<libc::proc_fdinfo> {
        let width = std::mem::size_of::<libc::proc_fdinfo>();
        // SAFETY: a null buffer asks for the byte count of this process's fd list.
        // A pid we cannot inspect (another user, or one that exited) returns
        // a non-positive count and is skipped; that fail-closes a peer whose
        // socket we cannot see.
        let needed =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Vec::new();
        }
        let count = (needed as usize / width).saturating_add(8);
        let mut buf = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            };
            count
        ];
        let bytes =
            libc::c_int::try_from(buf.len().saturating_mul(width)).unwrap_or(libc::c_int::MAX);
        // SAFETY: `buf` is writable for `bytes` bytes and contains `proc_fdinfo`s.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                buf.as_mut_ptr().cast(),
                bytes,
            )
        };
        if written <= 0 {
            return Vec::new();
        }
        let n = (written as usize) / width;
        buf.truncate(n.min(buf.len()));
        buf.retain(|fd| libc::c_int::try_from(fd.proc_fdtype) == Ok(libc::PROX_FDTYPE_SOCKET));
        buf
    }

    fn socket_info(pid: libc::pid_t, fd: libc::proc_fdinfo) -> Option<SocketFdInfo> {
        let mut info = std::mem::MaybeUninit::<SocketFdInfo>::uninit();
        let size = libc::c_int::try_from(std::mem::size_of::<SocketFdInfo>()).ok()?;
        // SAFETY: `info` is writable for `size` bytes. The result is read only
        // when the kernel reports a full `socket_fdinfo`.
        let written = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDSOCKETINFO,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: `proc_pidfdinfo` wrote exactly `size_of::<SocketFdInfo>()` bytes.
        Some(unsafe { info.assume_init() })
    }

    fn process_uid(pid: libc::pid_t) -> Option<u32> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` is writable for `size` bytes. The result is read only
        // when the kernel reports a full `proc_bsdinfo`.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: `proc_pidinfo` wrote exactly `size_of::<proc_bsdinfo>()` bytes.
        Some(unsafe { info.assume_init() }.pbi_uid)
    }

    fn endpoints(ini: &InSockInfo) -> Option<(StdSocketAddr, StdSocketAddr)> {
        // `insi_fport` / `insi_lport` hold the port in network byte order in
        // the low 16 bits (`inp_fport` assigned straight through).
        let local_port = u16::from_be(ini.insi_lport as u16);
        let foreign_port = u16::from_be(ini.insi_fport as u16);
        let (local_ip, foreign_ip) = match ini.insi_vflag {
            INI_IPV4 => {
                // SAFETY: `insi_vflag` is `INI_IPV4`, so the address unions
                // hold `ina_46`. `s_addr` is network order.
                let local = unsafe { ini.insi_laddr.ina_46.addr4.s_addr };
                let foreign = unsafe { ini.insi_faddr.ina_46.addr4.s_addr };
                (
                    Ipv4Addr::from(u32::from_be(local)).into(),
                    Ipv4Addr::from(u32::from_be(foreign)).into(),
                )
            }
            INI_IPV6 => {
                // SAFETY: `insi_vflag` is `INI_IPV6`, so the address unions
                // hold `ina_6` octets in network order.
                let local = unsafe { ini.insi_laddr.ina_6.s6_addr };
                let foreign = unsafe { ini.insi_faddr.ina_6.s6_addr };
                (Ipv6Addr::from(local).into(), Ipv6Addr::from(foreign).into())
            }
            _ => return None,
        };
        Some((
            StdSocketAddr::new(local_ip, local_port),
            StdSocketAddr::new(foreign_ip, foreign_port),
        ))
    }

    fn same_endpoint(left: StdSocketAddr, right: SocketAddr) -> bool {
        left.port() == right.port() && left.ip() == right.ip()
    }

    #[repr(C)]
    struct SocketFdInfo {
        _pfi: ProcFileInfo,
        psi: SocketInfo,
    }

    #[repr(C)]
    struct ProcFileInfo {
        _fi_openflags: u32,
        _fi_status: u32,
        _fi_offset: libc::off_t,
        _fi_type: i32,
        _fi_guardflags: u32,
    }

    #[repr(C)]
    struct SocketInfo {
        _soi_stat: libc::vinfo_stat,
        _soi_so: u64,
        _soi_pcb: u64,
        _soi_type: libc::c_int,
        _soi_protocol: libc::c_int,
        _soi_family: libc::c_int,
        _soi_options: libc::c_short,
        _soi_linger: libc::c_short,
        _soi_state: libc::c_short,
        _soi_qlen: libc::c_short,
        _soi_incqlen: libc::c_short,
        _soi_qlimit: libc::c_short,
        _soi_timeo: libc::c_short,
        _soi_error: libc::c_ushort,
        _soi_oobmark: u32,
        _soi_rcv: SockBufInfo,
        _soi_snd: SockBufInfo,
        soi_kind: libc::c_int,
        _rfu_1: u32,
        soi_proto: SocketInfoProto,
    }

    #[repr(C)]
    struct SockBufInfo {
        _sbi_cc: u32,
        _sbi_hiwat: u32,
        _sbi_mbcnt: u32,
        _sbi_mbmax: u32,
        _sbi_lowat: u32,
        _sbi_flags: libc::c_short,
        _sbi_timeo: libc::c_short,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union SocketInfoProto {
        pri_tcp: TcpSockInfo,
        /// Largest member of the kernel union. Present so `size_of` matches
        /// `socket_fdinfo` even though the bytes are not read.
        _pri_un: UnSockInfo,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TcpSockInfo {
        tcpsi_ini: InSockInfo,
        tcpsi_state: libc::c_int,
        _tcpsi_timer: [libc::c_int; 4],
        _tcpsi_mss: libc::c_int,
        _tcpsi_flags: u32,
        _rfu_1: u32,
        _tcpsi_tp: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InSockInfo {
        insi_fport: libc::c_int,
        insi_lport: libc::c_int,
        _insi_gencnt: u64,
        _insi_flags: u32,
        _insi_flow: u32,
        insi_vflag: u8,
        _insi_ip_ttl: u8,
        _rfu_1: u32,
        insi_faddr: InSiAddr,
        insi_laddr: InSiAddr,
        _insi_v4: InSiV4,
        _insi_v6: InSiV6,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InSiV4 {
        _in4_tos: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InSiV6 {
        _in6_hlim: u8,
        _in6_cksum: libc::c_int,
        _in6_ifindex: libc::c_ushort,
        _in6_hops: libc::c_short,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union InSiAddr {
        ina_46: In4In6Addr,
        ina_6: libc::in6_addr,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct In4In6Addr {
        _i46a_pad32: [u32; 3],
        addr4: libc::in_addr,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UnSockInfo {
        _unsi_conn_so: u64,
        _unsi_conn_pcb: u64,
        _unsi_addr: [u8; SOCK_MAXADDRLEN],
        _unsi_caddr: [u8; SOCK_MAXADDRLEN],
    }
}
