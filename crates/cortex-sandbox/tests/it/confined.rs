//! The Linux sandbox, applied for real: commands started through the
//! launcher (this test binary, entered as [`LAUNCH`]) under a policy, and
//! what they can and cannot do (`docs/decisions/0032-what-the-linux-sandbox-needs.md`).
//!
//! Each test makes a workspace, a temporary directory and a directory
//! outside both, in the system's temporary directory, and runs a shell
//! command or this binary as a helper inside the sandbox.

use std::io::Write;
use std::net::{TcpListener, UdpSocket};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cortex_sandbox::launch::{FAILED, Launcher, MARK};
use cortex_sandbox::policy::{Network, Policy, Reach};
use cortex_sandbox::support::{Assessment, detect};
use cortex_testkit::scratch::Scratch;

use crate::{DENY, ENTRY, LAUNCH, NET};

/// The Landlock version this kernel gives the sandbox, or `None` when it
/// would not enforce. On CI it must enforce: a runner without Landlock is a
/// failure here, never a skip, so the tests below cannot be turned off by a
/// change of image.
fn enforcing() -> Option<u8> {
    match detect().assess() {
        Assessment::Enforcing { landlock, .. } => Some(landlock),
        other => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI must run the sandbox's tests on a kernel where it enforces: {other:?} \
                 ({:?})",
                detect()
            );
            println!("the sandbox does not enforce here ({other:?}): nothing to test");
            None
        }
    }
}

/// A workspace, a temporary directory, and a directory outside both.
struct Place {
    scratch: Scratch,
}

impl Place {
    fn new(test: &str) -> Self {
        let scratch = Scratch::outside("cortex-sandbox-confined", test);
        scratch.file("ws/README", "workspace\n");
        scratch.dir("tmp");
        scratch.file("outside/keep", "keep me\n");
        Self { scratch }
    }

    fn ws(&self) -> PathBuf {
        self.scratch.join("ws")
    }

    fn tmp(&self) -> PathBuf {
        self.scratch.join("tmp")
    }

    fn outside(&self) -> PathBuf {
        self.scratch.join("outside")
    }

    fn policy(&self, network: Network, landlock: u8) -> Policy {
        Policy {
            read: Reach::All,
            write: vec![self.ws(), self.tmp()],
            network,
            temp: Some(self.tmp()),
            landlock,
        }
    }
}

/// What a confined command did.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    output: String,
}

impl Ran {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// The launcher this test binary is, as the harness would make it.
fn launcher(policy: Policy) -> Launcher {
    Launcher::new(
        std::env::current_exe().expect("the test binary has a path"),
        ["--exact", ENTRY, "--nocapture", LAUNCH],
        policy,
    )
}

/// Runs `program` with `arguments` through the launcher, in `cwd`, with
/// the policy bytes given (normally the launcher's own).
fn run_with(
    launcher: &Launcher,
    stdin: &[u8],
    before: &[&str],
    cwd: &Path,
    program: &str,
    arguments: &[&str],
) -> Ran {
    let arguments: Vec<std::ffi::OsString> = arguments.iter().map(Into::into).collect();
    let wrapped = launcher.wrap(program.as_ref(), &arguments);
    let mut command = match before.split_first() {
        Some((first, rest)) => {
            let mut command = Command::new(first);
            command.args(rest).arg(&wrapped.program);
            command
        }
        None => Command::new(&wrapped.program),
    };
    command
        .args(&wrapped.arguments)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("start the launcher");
    {
        let mut input = child.stdin.take().expect("its input");
        // A launcher that stopped before reading shows as a broken pipe.
        let _ = input.write_all(stdin);
    }
    let output = child.wait_with_output().expect("wait for the launcher");
    Ran {
        code: output.status.code(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn sh(place: &Place, policy: Policy, script: &str) -> Ran {
    let launcher = launcher(policy);
    let stdin = launcher.policy().encode();
    run_with(&launcher, &stdin, &[], &place.ws(), "/bin/sh", &["-c", script])
}

#[test]
fn sys_the_workspace_and_the_temporary_directory_are_writable_and_nothing_else() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("writes");
    let outside = place.outside();
    let script = format!(
        "echo a > in.txt && mkdir -p sub && mv in.txt sub/ && echo b > \"$TMPDIR/t\" && \
         mv \"$TMPDIR/t\" ./from-tmp && echo quiet > /dev/null && echo err >&2 && \
         echo late > /dev/stderr && printf '%s' \"$TMPDIR\" > tmpdir.txt; \
         echo c > {o}/new; truncate -s 0 {o}/keep; rm -f {o}/keep; mkdir {o}/d; \
         ln -s /etc/passwd {o}/link; mv sub/in.txt {o}/moved; echo done",
        o = outside.display()
    );
    let ran = sh(&place, place.policy(Network::Off, landlock), &script);
    assert!(ran.output.contains("done"), "{ran:?}");
    // Inside: written, moved, renamed across the two roots.
    assert_eq!(place.scratch.read_text("ws/sub/in.txt"), "a\n");
    assert_eq!(place.scratch.read_text("ws/from-tmp"), "b\n");
    assert_eq!(
        place.scratch.read_text("ws/tmpdir.txt"),
        place.tmp().display().to_string()
    );
    // Outside: nothing made, nothing emptied, nothing removed or moved.
    assert_eq!(place.scratch.listing("outside"), vec!["keep".to_owned()]);
    assert_eq!(place.scratch.read_text("outside/keep"), "keep me\n");
    assert!(ran.output.contains("Permission denied"), "{ran:?}");

    // The model is told which write was refused.
    let line = launcher(place.policy(Network::Off, landlock))
        .explain(Some(1), &ran.output)
        .expect("a refused write is explained");
    assert!(line.contains(&outside.display().to_string()), "{line}");
}

#[test]
fn sys_reading_is_not_restricted_by_default() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("reads");
    let script = format!("cat {}/keep", place.outside().display());
    let ran = sh(&place, place.policy(Network::Off, landlock), &script);
    assert!(ran.ok() && ran.output.contains("keep me"), "{ran:?}");
}

#[test]
fn sys_a_read_policy_keeps_reads_to_its_roots() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("read_roots");
    let mut policy = place.policy(Network::Off, landlock);
    // The system's directories, so that the shell itself can run.
    let mut roots: Vec<PathBuf> = ["/usr", "/bin", "/lib", "/lib64", "/etc", "/proc"]
        .iter()
        .map(PathBuf::from)
        .collect();
    roots.push(place.ws());
    policy.read = Reach::Only(roots);
    let script = format!(
        "cat README; cat {}/keep || echo refused",
        place.outside().display()
    );
    let ran = sh(&place, policy, &script);
    assert!(ran.output.contains("workspace"), "{ran:?}");
    assert!(ran.output.contains("refused") && !ran.output.contains("keep me"), "{ran:?}");
}

/// Runs this binary as a network helper inside the sandbox.
fn net(place: &Place, network: Network, landlock: u8, what: &[&str]) -> Ran {
    let launcher = launcher(place.policy(network, landlock));
    let stdin = launcher.policy().encode();
    let exe = std::env::current_exe().expect("the test binary has a path");
    let mut arguments = vec!["--exact", ENTRY, "--nocapture", NET];
    arguments.extend_from_slice(what);
    run_with(
        &launcher,
        &stdin,
        &[],
        &place.ws(),
        exe.to_str().expect("the test binary's path is text"),
        &arguments,
    )
}

/// The verdict a network helper printed: `ok`, or the kind of error that
/// refused it. The sandbox refuses with a permission error before anything
/// is sent, which is how its refusal is told from a peer's.
fn verdict(ran: &Ran) -> String {
    ran.output
        .lines()
        .find_map(|line| line.strip_prefix("net: "))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("the helper said nothing: {ran:?}"))
}

#[test]
fn sys_with_the_network_off_no_socket_reaches_anything() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("network_off");
    let tcp = TcpListener::bind("127.0.0.1:0").expect("listen on loopback");
    let tcp6 = TcpListener::bind("[::1]:0").ok();
    let udp = UdpSocket::bind("127.0.0.1:0").expect("a UDP socket on loopback");
    let unix_path = place.scratch.join("tmp/listener.sock");
    let _unix = UnixListener::bind(&unix_path).expect("a Unix socket");
    let abstract_name = format!("cortex-sandbox-test-{}", std::process::id());
    let _abstract = UnixListener::bind_addr(
        &std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .expect("an abstract name"),
    )
    .expect("an abstract Unix socket");
    let tcp_at = tcp.local_addr().unwrap().to_string();
    let udp_at = udp.local_addr().unwrap().to_string();
    let unix_at = unix_path.display().to_string();

    let mut cases = vec![
        ("tcp", tcp_at.clone()),
        ("udp", udp_at),
        // TEST-NET-1: refused before anything is sent, so no wait.
        ("tcp", "192.0.2.1:80".to_owned()),
        ("unix", unix_at.clone()),
        ("abstract", abstract_name.clone()),
        ("listen", "127.0.0.1:0".to_owned()),
    ];
    if let Some(tcp6) = &tcp6 {
        cases.push(("tcp", tcp6.local_addr().unwrap().to_string()));
    }
    for (kind, at) in &cases {
        let ran = net(&place, Network::Off, landlock, &[kind, at]);
        assert_eq!(verdict(&ran), "refused PermissionDenied", "{kind} {at}: {ran:?}");
    }
    // A pair of connected Unix sockets reaches only the command's own
    // processes, and netlink only the kernel: both stay.
    for kind in ["pair", "netlink"] {
        let ran = net(&place, Network::Off, landlock, &[kind, "-"]);
        assert_eq!(verdict(&ran), "ok", "{kind}: {ran:?}");
    }

    // With the network on, the same connections are made.
    for (kind, at) in [("tcp", tcp_at.as_str()), ("unix", unix_at.as_str())] {
        let ran = net(&place, Network::Full, landlock, &[kind, at]);
        assert_eq!(verdict(&ran), "ok", "{kind} {at} with the network on: {ran:?}");
    }
}

#[test]
fn sys_a_signal_to_a_process_outside_the_sandbox_is_refused() {
    let Some(landlock) = enforcing() else { return };
    if landlock < 6 {
        println!("Landlock {landlock} does not scope signals: the gap the status names");
        return;
    }
    let place = Place::new("signals");
    let mut outsider = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("a process outside the sandbox");
    let script = format!(
        "kill -0 {} && echo reached || echo refused; sleep 30 & kill $! && echo own",
        outsider.id()
    );
    let ran = sh(&place, place.policy(Network::Off, landlock), &script);
    let _ = outsider.kill();
    let _ = outsider.wait();
    assert!(ran.output.contains("refused") && !ran.output.contains("reached"), "{ran:?}");
    // Its own processes it can still signal.
    assert!(ran.output.contains("own"), "{ran:?}");
}

#[test]
fn sys_a_new_user_namespace_is_refused() {
    let Some(landlock) = enforcing() else { return };
    if Command::new("unshare").arg("--version").output().is_err() {
        assert!(std::env::var_os("CI").is_none(), "CI has util-linux");
        return;
    }
    let place = Place::new("user_namespace");
    let ran = sh(
        &place,
        place.policy(Network::Off, landlock),
        "unshare --user true && echo made || echo refused",
    );
    assert!(ran.output.contains("refused") && !ran.output.contains("made"), "{ran:?}");
}

#[test]
fn sys_a_policy_that_is_not_understood_runs_nothing() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("garbled");
    let launcher = launcher(place.policy(Network::Off, landlock));
    let mut stdin = launcher.policy().encode();
    let at = stdin.len() - 4;
    stdin.splice(at..at, b"share\0/\0".iter().copied());
    let ran = run_with(&launcher, &stdin, &[], &place.ws(), "/bin/sh", &["-c", "echo ran > ran"]);
    assert_eq!(ran.code, Some(FAILED), "{ran:?}");
    assert!(ran.output.contains(MARK) && ran.output.contains("`share`"), "{ran:?}");
    assert!(!place.ws().join("ran").exists());
    let line = launcher.explain(ran.code, &ran.output).expect("explained");
    assert!(line.contains("not run"), "{line}");
}

/// Runs `echo ran > ran` through the launcher under a filter, set up by
/// this binary before it becomes the launcher, that refuses `call`.
fn under_a_filter_that_refuses(place: &Place, landlock: u8, call: &str) -> Ran {
    let launcher = launcher(place.policy(Network::Off, landlock));
    let stdin = launcher.policy().encode();
    let exe = std::env::current_exe().expect("the test binary has a path");
    let exe = exe.to_str().expect("text");
    run_with(
        &launcher,
        &stdin,
        &[exe, "--exact", ENTRY, "--nocapture", DENY, call, "--"],
        &place.ws(),
        "/bin/sh",
        &["-c", "echo ran > ran"],
    )
}

#[test]
fn sys_landlock_that_cannot_be_applied_runs_nothing() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("landlock_refused");
    let ran = under_a_filter_that_refuses(&place, landlock, "landlock_create_ruleset");
    assert_eq!(ran.code, Some(FAILED), "{ran:?}");
    assert!(ran.output.contains("Landlock"), "{ran:?}");
    assert!(!place.ws().join("ran").exists());
}

#[test]
fn sys_a_seccomp_filter_that_does_not_load_runs_nothing() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("seccomp_refused");
    let ran = under_a_filter_that_refuses(&place, landlock, "seccomp");
    assert_eq!(ran.code, Some(FAILED), "{ran:?}");
    assert!(ran.output.contains("seccomp"), "{ran:?}");
    assert!(!place.ws().join("ran").exists());
}

#[test]
fn sys_a_command_that_cannot_be_found_is_said_so() {
    let Some(landlock) = enforcing() else { return };
    let place = Place::new("not_found");
    let launcher = launcher(place.policy(Network::Off, landlock));
    let stdin = launcher.policy().encode();
    let ran = run_with(&launcher, &stdin, &[], &place.ws(), "/nowhere/sh", &[]);
    assert_eq!(ran.code, Some(127), "{ran:?}");
    assert!(ran.output.contains("could not be started"), "{ran:?}");
}
