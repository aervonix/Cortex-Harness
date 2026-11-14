//! Landlock and seccomp, applied by the launcher to itself before it
//! becomes the command (`docs/decisions/0032-what-the-linux-sandbox-needs.md`).
//!
//! # Landlock: the file system
//!
//! Every right of the version the harness counted on is handled, except
//! two. `ioctl` on a device file is left to the device's own permissions:
//! a device can be opened for writing only under a write root (`/dev/null`
//! aside), so a program that drives one (a GPU) needs its device among
//! `sandbox.write`. And the resolving of a Unix socket's path, which the
//! seccomp filter below settles on every kernel alike. Then:
//!
//! - reading and running are allowed beneath `/`, or beneath each read
//!   root;
//! - everything is allowed beneath each write root;
//! - `/dev/null` may be written and truncated, so that `> /dev/null` works;
//! - from version 6, the command's signals and its connections to abstract
//!   Unix sockets stay inside the sandbox.
//!
//! The rights are asked for as a hard requirement: if the kernel cannot
//! give every one of them, nothing is applied in part and the command does
//! not run. A root that does not exist is skipped: a grant that is not made
//! only narrows.
//!
//! # seccomp: what Landlock does not cover
//!
//! A filter refuses, with `EPERM`:
//!
//! - with the network off, `socket` for every family but netlink, and
//!   netlink for every protocol but the route one: no TCP, no UDP,
//!   loopback included, no raw socket, and no Unix socket, so no D-Bus, no
//!   SSH agent and no Docker socket. `socketpair` stays, so a command's own
//!   processes can still talk to each other;
//! - with the network on, `socket` for every family but the internet's
//!   and netlink's route protocol: still no Unix socket (the session bus,
//!   an agent or the Docker socket would let a command out of the sandbox
//!   whatever the network setting), no virtual machine's socket to its
//!   host, no raw or packet socket;
//! - `io_uring`, which can open a socket without the `socket` call;
//! - a new user namespace (`unshare` and `clone` with `CLONE_NEWUSER`);
//! - the kernel's keyrings, `bpf`, `perf_event_open` and `userfaultfd`;
//! - pushing characters into a terminal's input (`TIOCSTI`, `TIOCLINUX`).
//!
//! A second filter answers `clone3` with `ENOSYS`, because its flags are in
//! memory a filter cannot read: the C library then falls back to `clone`,
//! whose flags the first filter can. A third answers `setsid` as done
//! without doing it: every process the command starts stays in its
//! session, which the tree kills whole (`tree::unix::kill_session`), so
//! nothing it started writes the workspace after the look at protected
//! paths that follows the command. `setpgid` stays: `timeout` and every
//! tool that signals its child's group need it. A 32-bit system call kills
//! the process (the filter is for the build's own architecture), and on
//! x86-64 the x32 form of each refused call is refused too. Tracing other processes is left to Landlock, which
//! keeps it inside the sandbox.

#![allow(
    clippy::disallowed_methods,
    reason = "the launcher is a process of its own, with one thread and no runtime: it looks at \
              the roots it is about to grant before it becomes the command"
)]

use std::collections::BTreeMap;
use std::path::Path;

use landlock::{
    ABI, Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, Scope,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use crate::policy::{Network, Policy, Reach};

/// Confines this process, and everything it will start, by `policy`.
///
/// # Errors
///
/// With what could not be applied. Nothing is half-applied that the
/// caller could mistake for a sandbox: on an error the caller does not run
/// the command.
pub(crate) fn apply(policy: &Policy) -> Result<(), String> {
    landlock(policy).map_err(|error| format!("Landlock: {error}"))?;
    seccomp(policy.network).map_err(|error| format!("seccomp: {error}"))?;
    Ok(())
}

/// The Landlock version the harness counted on, as the library names it.
fn abi(version: u8) -> Result<ABI, String> {
    Ok(match version {
        3 => ABI::V3,
        4 => ABI::V4,
        5 => ABI::V5,
        6 => ABI::V6,
        7 => ABI::V7,
        8 => ABI::V8,
        9 => ABI::V9,
        other => {
            return Err(format!(
                "version {other} is not one the sandbox enforces with"
            ));
        }
    })
}

fn landlock(policy: &Policy) -> Result<(), String> {
    let abi = abi(policy.landlock)?;
    let mut handled = AccessFs::from_all(abi);
    handled.remove(AccessFs::IoctlDev);
    handled.remove(AccessFs::ResolveUnix);
    let read = AccessFs::from_read(abi) & handled;

    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(handled)
        .map_err(|error| error.to_string())?;
    if abi >= ABI::V6 {
        ruleset = ruleset
            .scope(Scope::from_all(abi))
            .map_err(|error| error.to_string())?;
    }
    let mut created = ruleset.create().map_err(|error| error.to_string())?;

    match &policy.read {
        Reach::All => {
            created = grant(created, Path::new("/"), read, abi)?;
        }
        Reach::Only(roots) => {
            for root in roots {
                created = grant(created, root, read, abi)?;
            }
        }
    }
    for root in &policy.write {
        created = grant(created, root, handled, abi)?;
    }
    let null = (AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate) & handled;
    created = grant(created, Path::new("/dev/null"), null, abi)?;

    let status = created.restrict_self().map_err(|error| error.to_string())?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!(
            "the kernel enforced the rules only as {:?}",
            status.ruleset
        ));
    }
    if !status.no_new_privs {
        return Err("the process could not be kept from gaining privileges".to_owned());
    }
    Ok(())
}

/// Adds a rule that allows `access` beneath `root`. A root that is not
/// there is skipped; a file is given the rights that apply to a file.
///
/// The root is opened with no symbolic link resolved on the way to it, so
/// that a root, or any directory on the way to it, that a confined command
/// replaced with a link since the harness resolved the roots is refused,
/// not followed to wherever it leads (finding 6 of the third review of
/// step 26). The harness resolves the roots once, when the session's
/// policy is made (`cortex-core`'s `policy_of`), so a link the user set up
/// on purpose is already followed then and its real path is what is
/// granted here; only a link that appeared since is refused.
fn grant(
    created: landlock::RulesetCreated,
    root: &Path,
    access: BitFlags<AccessFs>,
    abi: ABI,
) -> Result<landlock::RulesetCreated, String> {
    let Ok(metadata) = std::fs::symlink_metadata(root) else {
        // Not there at all: nothing to grant. A grant not made only
        // narrows.
        return Ok(created);
    };
    if metadata.file_type().is_symlink() {
        // The root itself is a link now. It was a real directory, or did
        // not exist, when the roots were resolved: a confined command
        // swapped it. Fail closed.
        return Err(format!(
            "{}: a write or read root is a symbolic link now, which it was not when the \
             session started: it was replaced",
            root.display()
        ));
    }
    let access = if metadata.is_dir() {
        access
    } else {
        access & AccessFs::from_file(abi)
    };
    let fd = match open_no_symlinks(root) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            // A directory on the way to the root is a link now: openat2
            // refused to follow it. Fail closed.
            return Err(format!(
                "{}: a directory on the way to a root is a symbolic link now, which it was \
                 not when the session started: it was replaced",
                root.display()
            ));
        }
        // Gone between the look and the open: nothing to grant.
        Err(_) if std::fs::symlink_metadata(root).is_err() => return Ok(created),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    created
        .add_rule(PathBeneath::new(fd, access))
        .map_err(|error| format!("{}: {error}", root.display()))
}

/// Opens `path` as an `O_PATH` descriptor, refusing to follow a symbolic
/// link anywhere on the way to it or as its last component
/// (`RESOLVE_NO_SYMLINKS`), through `openat2` (Linux 5.6; every kernel the
/// sandbox enforces on has it). The descriptor is for Landlock to name the
/// tree by, never read or written.
#[allow(
    unsafe_code,
    reason = "decision 0030: the no-link open is only reachable through a system call; \
              cortex-sandbox is where such calls live"
)]
fn open_no_symlinks(path: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let mut raw: Vec<u8> = path.as_os_str().as_bytes().to_vec();
    raw.push(0);
    // SAFETY: `open_how` is a plain C struct of integers; all-zero is its
    // documented empty form, which the fields below then fill.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    // `O_PATH` opens the node itself, for naming only; `O_CLOEXEC` so the
    // command does not inherit it. `O_DIRECTORY` is not set, so that a file
    // root (`/dev/null`) opens too.
    how.flags = (libc::O_PATH | libc::O_CLOEXEC) as u64;
    how.resolve = libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: `openat2` takes a directory fd (`AT_FDCWD`, since the path is
    // absolute), a NUL-terminated path, a pointer to `open_how` and its
    // size. It returns a new descriptor or -1; the descriptor is owned at
    // once.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            raw.as_ptr(),
            std::ptr::addr_of!(how),
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(|_| std::io::Error::from(std::io::ErrorKind::Other))?;
    // SAFETY: `fd` was just returned by `openat2` and nothing else owns it.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// What x32 system calls add to a number on x86-64.
const X32: i64 = 0x4000_0000;

/// `ioctl`'s number in the x32 table, which is not the x86-64 one.
const X32_IOCTL: i64 = 514;

/// The flag of `clone` and `unshare` that makes a user namespace.
const NEW_USER: u64 = libc::CLONE_NEWUSER as u64;

fn seccomp(network: Network) -> Result<(), String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|error| error.to_string())?;
    let mut refused: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    let always = |rules: &mut BTreeMap<i64, Vec<SeccompRule>>, call: libc::c_long| {
        rules.insert(call, Vec::new());
    };
    for call in [
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_userfaultfd,
    ] {
        always(&mut refused, call);
    }
    let when = |conditions: Vec<SeccompCondition>| SeccompRule::new(conditions);
    let condition = |arg: u8, len: SeccompCmpArgLen, op: SeccompCmpOp, value: u64| {
        SeccompCondition::new(arg, len, op, value).map_err(|error| error.to_string())
    };
    let new_user = condition(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::MaskedEq(NEW_USER),
        NEW_USER,
    )?;
    refused.insert(
        libc::SYS_unshare,
        vec![when(vec![new_user.clone()]).map_err(|error| error.to_string())?],
    );
    refused.insert(
        libc::SYS_clone,
        vec![when(vec![new_user]).map_err(|error| error.to_string())?],
    );
    let mut terminal = Vec::new();
    for request in [libc::TIOCSTI, libc::TIOCLINUX] {
        terminal.push(
            when(vec![condition(
                1,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Eq,
                u64::from(u32::try_from(request).map_err(|error| error.to_string())?),
            )?])
            .map_err(|error| error.to_string())?,
        );
    }
    refused.insert(libc::SYS_ioctl, terminal);
    let number = |value: libc::c_int| u64::try_from(value).map_err(|error| error.to_string());
    let socket = match network {
        // Only the kernel's own interface list: no family that reaches
        // another machine, or another process (netlink's other protocols
        // include one that does).
        Network::Off => vec![
            when(vec![condition(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                number(libc::AF_NETLINK)?,
            )?])
            .map_err(|error| error.to_string())?,
            when(vec![
                condition(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Eq,
                    number(libc::AF_NETLINK)?,
                )?,
                condition(
                    2,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    number(libc::NETLINK_ROUTE)?,
                )?,
            ])
            .map_err(|error| error.to_string())?,
        ],
        // The network, and nothing else: the internet's families and
        // netlink's route protocol. No Unix socket (the session bus, an
        // agent, the Docker socket would let a command out), no virtual
        // machine's socket to its host, no raw or packet socket (found by
        // the reviews of step 26).
        Network::Full => vec![
            when(vec![
                condition(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    number(libc::AF_INET)?,
                )?,
                condition(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    number(libc::AF_INET6)?,
                )?,
                condition(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    number(libc::AF_NETLINK)?,
                )?,
            ])
            .map_err(|error| error.to_string())?,
            when(vec![
                condition(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Eq,
                    number(libc::AF_NETLINK)?,
                )?,
                condition(
                    2,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    number(libc::NETLINK_ROUTE)?,
                )?,
            ])
            .map_err(|error| error.to_string())?,
        ],
    };
    refused.insert(libc::SYS_socket, socket);
    if arch == TargetArch::x86_64 {
        let x32: Vec<(i64, Vec<SeccompRule>)> = refused
            .iter()
            .filter(|(call, _)| **call != libc::SYS_ioctl)
            .map(|(call, rules)| (call | X32, rules.clone()))
            .collect();
        refused.extend(x32);
        // `ioctl` has a number of its own in the x32 table.
        if let Some(rules) = refused.get(&libc::SYS_ioctl).cloned() {
            refused.insert(X32_IOCTL | X32, rules);
        }
    }

    let eperm = u32::try_from(libc::EPERM).map_err(|error| error.to_string())?;
    let main = SeccompFilter::new(
        refused,
        SeccompAction::Allow,
        SeccompAction::Errno(eperm),
        arch,
    )
    .map_err(|error| error.to_string())?;
    let enosys = u32::try_from(libc::ENOSYS).map_err(|error| error.to_string())?;
    let mut clone3 = BTreeMap::new();
    clone3.insert(libc::SYS_clone3, Vec::new());
    if arch == TargetArch::x86_64 {
        clone3.insert(libc::SYS_clone3 | X32, Vec::new());
    }
    let clone3 = SeccompFilter::new(
        clone3,
        SeccompAction::Allow,
        SeccompAction::Errno(enosys),
        arch,
    )
    .map_err(|error| error.to_string())?;
    // Making a session of its own is answered as done and not done: every
    // process stays in the command's session, which the tree kills whole
    // when the command ends, so nothing the command started can write the
    // workspace after the look that follows the command. A process group
    // of its own stays allowed: `timeout` and every tool that signals its
    // child's group need one, and the session still holds it.
    let mut stay = BTreeMap::new();
    stay.insert(libc::SYS_setsid, Vec::new());
    if arch == TargetArch::x86_64 {
        stay.insert(libc::SYS_setsid | X32, Vec::new());
    }
    let stay = SeccompFilter::new(stay, SeccompAction::Allow, SeccompAction::Errno(0), arch)
        .map_err(|error| error.to_string())?;
    for filter in [main, clone3, stay] {
        let program: BpfProgram = filter
            .try_into()
            .map_err(|error: seccompiler::BackendError| error.to_string())?;
        seccompiler::apply_filter(&program).map_err(|error| error.to_string())?;
    }
    Ok(())
}
