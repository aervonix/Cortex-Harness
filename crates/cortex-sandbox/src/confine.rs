//! Landlock and seccomp, applied by the launcher to itself before it
//! becomes the command (`docs/decisions/0032-what-the-linux-sandbox-needs.md`).
//!
//! # Landlock: the file system
//!
//! Every right of the version the harness counted on is handled, except
//! two: `ioctl` on a device file, so that a program that drives a device
//! (a GPU, a terminal it opened) is not stopped, and the resolving of a
//! Unix socket's path, which the seccomp filter below settles on every
//! kernel alike. Then:
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
//! - with the network off, `socket` for every family but netlink: no TCP,
//!   no UDP, loopback included, no raw socket, and no Unix socket, so no
//!   D-Bus, no SSH agent and no Docker socket. `socketpair` stays, so a
//!   command's own processes can still talk to each other;
//! - `io_uring`, which can open a socket without the `socket` call;
//! - a new user namespace (`unshare` and `clone` with `CLONE_NEWUSER`);
//! - the kernel's keyrings, `bpf`, `perf_event_open` and `userfaultfd`;
//! - pushing characters into a terminal's input (`TIOCSTI`, `TIOCLINUX`).
//!
//! A second filter answers `clone3` with `ENOSYS`, because its flags are in
//! memory a filter cannot read: the C library then falls back to `clone`,
//! whose flags the first filter can. A 32-bit system call kills the
//! process (the filter is for the build's own architecture), and on x86-64
//! the x32 form of each refused call is refused too.

#![allow(
    clippy::disallowed_methods,
    reason = "the launcher is a process of its own, with one thread and no runtime: it looks at \
              the roots it is about to grant before it becomes the command"
)]

use std::collections::BTreeMap;
use std::path::Path;

use landlock::{
    ABI, Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
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
        other => return Err(format!("version {other} is not one the sandbox enforces with")),
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

    let status = created
        .restrict_self()
        .map_err(|error| error.to_string())?;
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
fn grant(
    created: landlock::RulesetCreated,
    root: &Path,
    access: BitFlags<AccessFs>,
    abi: ABI,
) -> Result<landlock::RulesetCreated, String> {
    let Ok(metadata) = std::fs::metadata(root) else {
        return Ok(created);
    };
    let access = if metadata.is_dir() {
        access
    } else {
        access & AccessFs::from_file(abi)
    };
    let fd = match PathFd::new(root) {
        Ok(fd) => fd,
        // Gone between the look and the open: nothing to grant.
        Err(_) if std::fs::symlink_metadata(root).is_err() => return Ok(created),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    created
        .add_rule(PathBeneath::new(fd, access))
        .map_err(|error| format!("{}: {error}", root.display()))
}

/// What x32 system calls add to a number on x86-64.
const X32: i64 = 0x4000_0000;

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
    if network == Network::Off {
        let netlink = u64::try_from(libc::AF_NETLINK).map_err(|error| error.to_string())?;
        refused.insert(
            libc::SYS_socket,
            vec![
                when(vec![condition(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    netlink,
                )?])
                .map_err(|error| error.to_string())?,
            ],
        );
    }
    if arch == TargetArch::x86_64 {
        let x32: Vec<(i64, Vec<SeccompRule>)> = refused
            .iter()
            .map(|(call, rules)| (call | X32, rules.clone()))
            .collect();
        refused.extend(x32);
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
    for filter in [main, clone3] {
        let program: BpfProgram = filter.try_into().map_err(|error: seccompiler::BackendError| error.to_string())?;
        seccompiler::apply_filter(&program).map_err(|error| error.to_string())?;
    }
    Ok(())
}
