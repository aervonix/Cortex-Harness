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
/// | `netlink-user -` | opens a user netlink socket, which reaches other processes |
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
        "netlink" => netlink(libc::NETLINK_ROUTE),
        "netlink-user" => netlink(libc::NETLINK_USERSOCK),
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
fn netlink(protocol: libc::c_int) -> std::io::Result<()> {
    // SAFETY: the call takes three integers and returns a new descriptor
    // or -1. The descriptor is owned at once and closed when dropped.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            protocol,
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

/// Makes one system call the sandbox refuses, with arguments that are
/// harmless outside it, prints `call: <result>` (`ok`, or the error's
/// name), and exits.
///
/// | Call | What it asks for |
/// |---|---|
/// | `io_uring` | `io_uring_setup` with no entries |
/// | `keyctl` | the session keyring's id |
/// | `bpf` | a map with no attributes |
/// | `perf` | a counter with no attributes |
/// | `userfaultfd` | a user-mode fault descriptor |
/// | `unshare-user` | a new user namespace |
/// | `clone-user` | a child in a new user namespace |
/// | `clone3` | `clone3` with no arguments |
/// | `tiocsti` | a character pushed into standard input |
/// | `setsid` | a session of its own; `call: same` or `call: new` says whether the session changed |
/// | `setpgid` | a process group of its own; `same` or `new` likewise |
#[allow(
    unsafe_code,
    reason = "a test of the sandbox makes the raw system calls its filter refuses"
)]
pub(crate) fn call(name: &str) -> ! {
    let errno = || std::io::Error::last_os_error();
    let said = |result: libc::c_long| -> String {
        if result < 0 {
            let error = errno();
            match error.raw_os_error() {
                Some(libc::EPERM) => "EPERM".to_owned(),
                Some(libc::ENOSYS) => "ENOSYS".to_owned(),
                Some(code) => format!("errno {code}"),
                None => error.to_string(),
            }
        } else {
            "ok".to_owned()
        }
    };
    // SAFETY: for every call below, each takes integers, or a null pointer
    // where the kernel reads nothing for these arguments, or a pointer to a
    // local that lives for the length of the call. A child made by `clone`
    // exits at once without touching anything of the parent's.
    let verdict = unsafe {
        match name {
            "io_uring" => said(libc::syscall(
                libc::SYS_io_uring_setup,
                0u32,
                std::ptr::null_mut::<libc::c_void>(),
            )),
            "keyctl" => said(libc::syscall(
                libc::SYS_keyctl,
                0,  /* KEYCTL_GET_KEYRING_ID */
                -3, /* session */
                0,
            )),
            "bpf" => said(libc::syscall(
                libc::SYS_bpf,
                0, /* BPF_MAP_CREATE */
                std::ptr::null_mut::<libc::c_void>(),
                0u32,
            )),
            "perf" => said(libc::syscall(
                libc::SYS_perf_event_open,
                std::ptr::null_mut::<libc::c_void>(),
                0,
                -1,
                -1,
                0,
            )),
            "userfaultfd" => said(libc::syscall(
                libc::SYS_userfaultfd,
                1 /* UFFD_USER_MODE_ONLY */ | libc::O_CLOEXEC,
            )),
            "unshare-user" => said(libc::syscall(libc::SYS_unshare, libc::CLONE_NEWUSER)),
            "clone-user" => {
                let child = libc::syscall(
                    libc::SYS_clone,
                    libc::CLONE_NEWUSER | libc::SIGCHLD,
                    0,
                    0,
                    0,
                    0,
                );
                if child == 0 {
                    libc::_exit(0);
                }
                if child > 0 {
                    let mut status = 0;
                    libc::waitpid(i32::try_from(child).unwrap_or(-1), &raw mut status, 0);
                }
                said(child)
            }
            "clone3" => {
                let mut arguments = [0u64; 11];
                arguments[4] = u64::try_from(libc::SIGCHLD).unwrap_or(0);
                let child = libc::syscall(
                    libc::SYS_clone3,
                    arguments.as_mut_ptr(),
                    std::mem::size_of_val(&arguments),
                );
                if child == 0 {
                    libc::_exit(0);
                }
                if child > 0 {
                    let mut status = 0;
                    libc::waitpid(i32::try_from(child).unwrap_or(-1), &raw mut status, 0);
                }
                said(child)
            }
            "tiocsti" => {
                let byte = b'x';
                said(libc::c_long::from(libc::ioctl(
                    0,
                    libc::TIOCSTI,
                    &raw const byte,
                )))
            }
            // A process group of its own, then the process waits to be
            // killed: what a tree must still reach.
            "own-group" => {
                let made = libc::setpgid(0, 0);
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "\ngroup: {} {made}", libc::getpid());
                let _ = out.flush();
                drop(out);
                loop {
                    std::thread::park();
                }
            }
            "setsid" | "setpgid" => {
                let (session, group) = (libc::getsid(0), libc::getpgid(0));
                let result = if name == "setsid" {
                    libc::c_long::from(libc::setsid())
                } else {
                    libc::c_long::from(libc::setpgid(0, 0))
                };
                let moved = libc::getsid(0) != session || libc::getpgid(0) != group;
                if result < 0 {
                    said(result)
                } else if moved {
                    "new".to_owned()
                } else {
                    "same".to_owned()
                }
            }
            // Metadata of a file in the temporary directory (a write root
            // inside the sandbox): a mode change is allowed, an owner or an
            // extended-attribute change is refused.
            "chmod" | "chown" | "setxattr" => {
                use std::os::unix::ffi::OsStrExt;
                let path = std::env::temp_dir().join(format!("cortex-meta-{}", libc::getpid()));
                let mut bytes = path.as_os_str().as_bytes().to_vec();
                bytes.push(0);
                let fd = libc::creat(bytes.as_ptr().cast(), 0o644);
                if fd >= 0 {
                    libc::close(fd);
                }
                let verdict = match name {
                    "chmod" => said(libc::c_long::from(libc::chmod(
                        bytes.as_ptr().cast(),
                        0o600,
                    ))),
                    "chown" => said(libc::c_long::from(libc::chown(
                        bytes.as_ptr().cast(),
                        libc::getuid(),
                        libc::getgid(),
                    ))),
                    _ => {
                        let name = c"user.cortex";
                        let value = b"x";
                        said(libc::syscall(
                            libc::SYS_setxattr,
                            bytes.as_ptr(),
                            name.as_ptr(),
                            value.as_ptr(),
                            value.len(),
                            0,
                        ))
                    }
                };
                libc::unlink(bytes.as_ptr().cast());
                verdict
            }
            other => panic!("no such call: {other}"),
        }
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "\ncall: {verdict}");
    let _ = out.flush();
    #[expect(clippy::exit, reason = "a helper ends with its verdict")]
    std::process::exit(0);
}
