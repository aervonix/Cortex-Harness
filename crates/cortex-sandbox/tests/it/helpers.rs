//! What this test binary does when a test starts it as a helper inside, or
//! in front of, the sandbox.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};

/// Tries one kind of socket, prints the verdict, and exits.
///
/// | Kind | What it does |
/// |---|---|
/// | `tcp ADDR` | connects |
/// | `udp ADDR` | sends one datagram |
/// | `listen ADDR` | listens |
/// | `unix PATH` | connects to a Unix socket by its path |
/// | `abstract NAME` | connects to an abstract Unix socket |
/// | `pair -` | makes a connected pair and passes a byte through it |
/// | `netlink -` | opens a route netlink socket |
pub(crate) fn net(kind: &str, at: &str) -> ! {
    let outcome: std::io::Result<()> = match kind {
        "tcp" => TcpStream::connect(at).map(drop),
        "udp" => UdpSocket::bind("0.0.0.0:0").and_then(|socket| socket.send_to(b"x", at).map(drop)),
        "listen" => TcpListener::bind(at).map(drop),
        "unix" => UnixStream::connect(at).map(drop),
        "abstract" => SocketAddr::from_abstract_name(at.as_bytes())
            .and_then(|address| UnixStream::connect_addr(&address))
            .map(drop),
        "pair" => UnixStream::pair().and_then(|(mut a, mut b)| {
            a.write_all(b"x")?;
            let mut byte = [0u8; 1];
            b.read_exact(&mut byte)
        }),
        "netlink" => netlink(),
        other => panic!("no such helper: {other}"),
    };
    let mut out = std::io::stdout().lock();
    let _ = match outcome {
        Ok(()) => writeln!(out, "\nnet: ok"),
        Err(error) => writeln!(out, "\nnet: refused {:?}", error.kind()),
    };
    let _ = out.flush();
    #[expect(clippy::exit, reason = "a helper ends with its verdict")]
    std::process::exit(0);
}

/// Opens a route netlink socket, as the C library's `getifaddrs` does to
/// list the interfaces. The standard library has no netlink socket.
#[allow(
    unsafe_code,
    reason = "a test of the sandbox makes the one socket the standard library cannot"
)]
fn netlink() -> std::io::Result<()> {
    // SAFETY: the call takes three integers and returns a new descriptor
    // or -1. The descriptor is owned at once and closed when dropped.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by `socket` and nothing else owns it.
    drop(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
    Ok(())
}

/// Installs a filter that refuses `call` with `EPERM`, then becomes `rest`.
#[cfg(feature = "backends")]
pub(crate) fn deny(call: &str, rest: &[String]) -> ! {
    use std::collections::BTreeMap;
    use std::os::unix::process::CommandExt;

    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};

    let number = match call {
        "landlock_create_ruleset" => libc::SYS_landlock_create_ruleset,
        "seccomp" => libc::SYS_seccomp,
        other => panic!("no such call to refuse: {other}"),
    };
    let mut rules = BTreeMap::new();
    rules.insert(number, Vec::new());
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(u32::try_from(libc::EPERM).expect("a small number")),
        TargetArch::try_from(std::env::consts::ARCH).expect("a known architecture"),
    )
    .expect("a filter");
    let program: BpfProgram = filter.try_into().expect("a program");
    seccompiler::apply_filter(&program).expect("the filter loads");
    let error = std::process::Command::new(&rest[0]).args(&rest[1..]).exec();
    panic!("{} could not be started: {error}", rest[0]);
}
