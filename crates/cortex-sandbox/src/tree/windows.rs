//! The Windows half of [`super`]: Job Objects and console events.
//!
//! The standard library can start a process in a group of its own, start it
//! suspended, and kill one process. It cannot make a Job Object, put a
//! process in one, let a suspended process run, send a console event, or
//! cancel a read another thread is blocked in. Those go through the Windows
//! API here, and every block says why it is sound.
//!
//! How it is kept small:
//!
//! - **Handles are owned by standard-library types** from the moment they
//!   exist, so each is closed once, on every path, and none is used after
//!   it is closed.
//! - **The process is started by the standard library.** Its command line,
//!   its environment and its standard streams are built by code that is
//!   not here.
//! - **Every structure handed to Windows is plain data** of a fixed size,
//!   made by zeroing it and setting the fields that are used.

#![allow(
    unsafe_code,
    reason = "decision 0030: Job Objects and console events are only reachable through \
              the Windows API; cortex-sandbox is where such calls live"
)]

use std::io::{self, PipeReader, Read};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, GetExitCodeProcess, INFINITE, OpenProcess,
    OpenThread, PROCESS_SYNCHRONIZE, ResumeThread, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

use super::guard::Guard;
use super::{Exit, ReadOutcome, ended, others_among};

/// The exit code of a process the harness killed.
const KILLED: u32 = 1;

/// How long [`Inner::end`] waits for the command to be gone after its job
/// was terminated. Termination is asynchronous and takes milliseconds; the
/// bound is there so that a failure cannot hang the harness.
const END_WAIT_MS: u32 = 5000;

/// One tree: the command, the job it and its descendants are in, and how
/// far along it is.
#[derive(Debug)]
pub(super) struct Inner {
    pid: u32,
    /// The command itself. Holding the handle keeps its process id from
    /// being given to another process.
    process: OwnedHandle,
    /// The job. It is set to kill its processes when its last handle
    /// closes, and this is the only handle: it is not inheritable, so no
    /// process of the tree has one.
    job: OwnedHandle,
    /// Whether the tree has ended.
    state: Mutex<bool>,
    /// How the command ended, once known. The lock is held by the thread
    /// that is waiting for it.
    exit: Mutex<Option<Exit>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Inner {
    pub(super) fn spawn(mut command: Command, _guard: Option<&Guard>) -> io::Result<Self> {
        let job = create_job()?;
        // Suspended, so that it cannot start a process of its own before
        // it is in the job: a process started earlier would be outside it.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        // The command holds the caller's ends of its pipes. Let go of them
        // now, so that the pipes close when the tree's last writer does.
        drop(command);
        let pid = child.id();
        let started = assign(&job, child.as_raw_handle()).and_then(|()| resume(pid));
        if let Err(error) = started {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            pid,
            process: OwnedHandle::from(child),
            job,
            state: Mutex::new(false),
            exit: Mutex::new(None),
        })
    }

    pub(super) fn id(&self) -> u32 {
        self.pid
    }

    pub(super) fn interrupt(&self) -> io::Result<()> {
        let ended_now = lock(&self.state);
        if *ended_now {
            return Err(ended());
        }
        // The event is addressed to a process group by its leader's id. It
        // is only sent while the leader is alive, so that the id names the
        // group that was started here and nothing else.
        if self.has_exited() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the command has exited",
            ));
        }
        // SAFETY: the call takes two integers. The second is the id of a
        // process group this process created and whose leader it holds a
        // handle to.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows has no stage between the console event and the kill",
        ))
    }

    pub(super) fn kill(&self) -> io::Result<()> {
        let ended_now = lock(&self.state);
        if *ended_now {
            return Err(ended());
        }
        self.terminate_job()
    }

    fn terminate_job(&self) -> io::Result<()> {
        // SAFETY: `self.job` is an open job handle with full access, owned
        // by `self` for the length of the call.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), KILLED) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn has_exited(&self) -> bool {
        // SAFETY: `self.process` is an open process handle owned by `self`.
        // A wait of zero milliseconds only reads its state.
        unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    pub(super) fn others_running(&self) -> io::Result<usize> {
        let ended_now = lock(&self.state);
        if *ended_now {
            return Err(ended());
        }
        // The members are asked for by id, and not counted
        // (`ActiveProcesses`): a job goes on counting a process for a
        // moment after the process has exited. A count taken in that
        // moment said that a command which had just exited, and left
        // nothing, had left one process running: itself. So the command is
        // left out by its id, and so is any member that has exited.
        let members = self.members()?;
        Ok(others_among(&members, self.pid, has_exited_by_id))
    }

    /// The process ids of the job's members: the command and everything it
    /// started that the job still lists. A process that has just exited
    /// may still be among them.
    fn members(&self) -> io::Result<Vec<u32>> {
        // The list is a header of two counts and then one id for each
        // member. The buffer is made of `u64`s so that it is aligned for
        // the header and for the ids, and is made larger until every
        // member fits.
        let header = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() - size_of::<usize>();
        let mut room = 32usize;
        loop {
            let bytes = header + room * size_of::<usize>();
            let mut buffer = vec![0u64; bytes.div_ceil(size_of::<u64>())];
            let size = u32::try_from(buffer.len() * size_of::<u64>())
                .map_err(|_| io::Error::other("the list of a job's members is too large"))?;
            // SAFETY: `self.job` is open, the class matches the structure
            // the buffer is read as, and the pointer and the size describe
            // `buffer`, which is writable and aligned for the structure
            // for the length of the call. The last argument may be null.
            let asked = unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    size,
                    ptr::null_mut(),
                )
            };
            let list: *const JOBOBJECT_BASIC_PROCESS_ID_LIST = buffer.as_ptr().cast();
            // SAFETY: the buffer is at least as large as the structure,
            // aligned for it, and initialised (zeroes, or what the call
            // wrote); the two counts are plain integers.
            let (assigned, listed) = unsafe {
                (
                    (*list).NumberOfAssignedProcesses as usize,
                    (*list).NumberOfProcessIdsInList as usize,
                )
            };
            if asked == 0 {
                let error = io::Error::last_os_error();
                // "More data": the job has more members than there was
                // room for, and says how many.
                if error.raw_os_error() == Some(ERROR_MORE_DATA.cast_signed()) && assigned > room {
                    room = assigned + 16;
                    continue;
                }
                return Err(error);
            }
            let listed = listed.min(room);
            // SAFETY: the ids begin at the structure's last field and the
            // buffer has room for `room` of them, of which the call wrote
            // `listed`; they are plain integers, and the buffer outlives
            // the slice.
            let ids = unsafe {
                std::slice::from_raw_parts(
                    (&raw const (*list).ProcessIdList).cast::<usize>(),
                    listed,
                )
            };
            return Ok(ids
                .iter()
                .filter_map(|id| u32::try_from(*id).ok())
                .collect());
        }
    }

    pub(super) fn wait(&self) -> io::Result<Exit> {
        let mut exit = lock(&self.exit);
        if let Some(exit) = *exit {
            return Ok(exit);
        }
        let found = self.wait_for(INFINITE)?;
        *exit = Some(found);
        Ok(found)
    }

    fn wait_for(&self, milliseconds: u32) -> io::Result<Exit> {
        let process = self.process.as_raw_handle();
        // SAFETY: `process` is an open process handle owned by `self` for
        // the length of the call.
        match unsafe { WaitForSingleObject(process, milliseconds) } {
            WAIT_OBJECT_0 => {}
            windows_sys::Win32::Foundation::WAIT_TIMEOUT => {
                return Err(io::ErrorKind::TimedOut.into());
            }
            _ => return Err(io::Error::last_os_error()),
        }
        let mut code: u32 = 0;
        // SAFETY: `process` is as above, and `code` is a valid place for
        // the one `u32` the call writes.
        if unsafe { GetExitCodeProcess(process, &raw mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Windows gives the code as 32 unsigned bits. A crash's code has
        // the top bit set, and is shown as the negative number it is.
        Ok(Exit::Code(i32::from_ne_bytes(code.to_ne_bytes())))
    }

    pub(super) fn end(&self) {
        {
            let ended_now = lock(&self.state);
            if *ended_now {
                return;
            }
            let _ = self.terminate_job();
        }
        // A thread in `wait` returns now, and gives up the lock.
        let mut exit = lock(&self.exit);
        if exit.is_none() {
            *exit = Some(
                self.wait_for(END_WAIT_MS)
                    .unwrap_or(Exit::Code(i32::from_ne_bytes(KILLED.to_ne_bytes()))),
            );
        }
        *lock(&self.state) = true;
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.end();
        // The job's handle closes after this, which would kill the tree if
        // anything of it were left.
    }
}

/// Whether the process with this id has exited, or is gone altogether.
fn has_exited_by_id(pid: u32) -> bool {
    // SAFETY: the call takes three integers: the one right that is needed,
    // no inheritance, and a process id.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        // Nothing has this id any more, or it cannot be asked: there is
        // nothing to wait for. A member of a job this process made can be
        // opened by it for as long as it is there.
        return true;
    }
    // SAFETY: `process` is a handle that was just opened and that nothing
    // else owns.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    // SAFETY: `process` is an open process handle with the right to wait.
    // A wait of zero milliseconds only reads its state.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

/// A job that kills its processes when its last handle is closed.
fn create_job() -> io::Result<OwnedHandle> {
    // SAFETY: both arguments may be null: default security, which makes
    // the handle not inheritable, and no name.
    let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a job handle that was just created and that
    // nothing else owns.
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    // SAFETY: the structure is plain data (integers and sizes), for which
    // all zeroes is a valid value meaning "no limit".
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("the limits structure is too large"))?;
    // SAFETY: `job` is open, the class matches the structure, and the
    // pointer and the size describe `limits`, which lives for the length
    // of the call. The call reads the structure and keeps no pointer.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Puts a process in a job. Everything it starts from then on is in the
/// job too, and cannot leave it.
fn assign(job: &OwnedHandle, process: HANDLE) -> io::Result<()> {
    // SAFETY: `job` is an open job handle. `process` is the handle of a
    // child the caller owns, with the full access a creator gets.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Lets a process that was started suspended run.
///
/// The standard library keeps the first thread's handle to itself, so the
/// thread is found by its owner in a list of the system's threads. A
/// process that has not run yet has exactly one.
fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: the call takes two integers. For a list of threads the
    // second is not used.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `snapshot` is a handle that was just created and that
    // nothing else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // SAFETY: the structure is plain data (seven integers), for which all
    // zeroes is a valid value.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = u32::try_from(size_of::<THREADENTRY32>())
        .map_err(|_| io::Error::other("the thread entry is too large"))?;
    // SAFETY: `snapshot` is open, and `entry` is a valid, writable entry
    // whose `dwSize` says how large it is, as the call requires.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    let mut resumed = 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            resume_thread(entry.th32ThreadID)?;
            resumed += 1;
        }
        // SAFETY: as for `Thread32First`.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    if resumed == 0 {
        return Err(io::Error::other("the command's first thread was not found"));
    }
    Ok(())
}

fn resume_thread(thread_id: u32) -> io::Result<()> {
    // SAFETY: the call takes three integers: the one right that is needed,
    // no inheritance, and a thread id.
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `thread` is a handle that was just opened and that nothing
    // else owns.
    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
    // SAFETY: `thread` is an open thread handle with the right to resume.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The reading end of a command's output.
#[derive(Debug)]
pub(super) struct OutputPipe {
    pipe: Arc<PipeReader>,
    stopped: Arc<AtomicBool>,
}

/// What stops a read: a flag, and the pipe itself, kept open so that the
/// handle it cancels on is the pipe's and no other.
#[derive(Debug, Clone)]
pub(super) struct StopHandle {
    pipe: Arc<PipeReader>,
    stopped: Arc<AtomicBool>,
}

impl OutputPipe {
    pub(super) fn new(pipe: PipeReader) -> io::Result<(Self, StopHandle)> {
        let pipe = Arc::new(pipe);
        let stopped = Arc::new(AtomicBool::new(false));
        Ok((
            Self {
                pipe: Arc::clone(&pipe),
                stopped: Arc::clone(&stopped),
            },
            StopHandle { pipe, stopped },
        ))
    }

    pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<ReadOutcome> {
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(ReadOutcome::Stopped);
        }
        match (&*self.pipe).read(buffer) {
            Ok(0) => Ok(ReadOutcome::Closed),
            Ok(bytes) => Ok(ReadOutcome::Bytes(bytes)),
            Err(_) if self.stopped.load(Ordering::SeqCst) => Ok(ReadOutcome::Stopped),
            // Windows reports the last writer's going as a broken pipe.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(ReadOutcome::Closed),
            Err(error) => Err(error),
        }
    }
}

impl StopHandle {
    pub(super) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // SAFETY: the handle is the pipe's, open for as long as `self`
        // holds the pipe. A null second argument cancels every read
        // pending on it, from any thread; with none pending the call fails
        // and changes nothing.
        unsafe {
            CancelIoEx(self.pipe.as_raw_handle(), ptr::null());
        }
    }
}
