//! The Windows half of the terminal layer: the console's modes and code
//! pages, the window's size, waiting for input, and putting the console back
//! on every way out.
//!
//! The standard library can tell whether a handle is a console, and no more.
//! The console's modes, its code pages, its input records, its control
//! events and the window's size go through the Windows API here, and every
//! `unsafe` block says why it is sound.
//!
//! # Virtual-terminal processing
//!
//! The old console host shows every escape sequence as text unless a
//! program turns on `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; Windows Terminal
//! turns it on by itself. [`enter`] turns it on for output, and turns on
//! virtual-terminal input, so that keys arrive as the same sequences a Linux
//! terminal sends and one decoder serves both systems. Every way out puts
//! back the modes and code pages it found: a normal exit ([`leave`]), a panic
//! and a console control event ([`emergency`]).

#![allow(
    unsafe_code,
    reason = "the console's modes, code pages, input records and control events are only \
              reachable through the Windows API; cortex-term is where such calls live"
)]

use std::io;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{FALSE, HANDLE, TRUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
    ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo,
    GetNumberOfConsoleInputEvents, GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleCtrlHandler, SetConsoleMode,
    SetConsoleOutputCP, WINDOW_BUFFER_SIZE_EVENT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};

use super::{Ready, restore_sequence};

/// UTF-8, as a console code page.
const UTF8: u32 = 65001;

/// The modes and code pages found at the first [`enter`], which every way
/// out puts back.
static INPUT_MODE: AtomicU32 = AtomicU32::new(0);
static OUTPUT_MODE: AtomicU32 = AtomicU32::new(0);
static INPUT_CP: AtomicU32 = AtomicU32::new(0);
static OUTPUT_CP: AtomicU32 = AtomicU32::new(0);
static SAVED: AtomicBool = AtomicBool::new(false);
/// Whether the console is in our modes now.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The event that wakes a wait. 0 until it exists.
static WAKE: AtomicIsize = AtomicIsize::new(0);
static WOKEN: AtomicBool = AtomicBool::new(false);
/// A resize seen in the console's input, not reported yet.
static RESIZED: AtomicBool = AtomicBool::new(false);
static HANDLER: AtomicBool = AtomicBool::new(false);
/// The first half of a character outside the Basic Multilingual Plane,
/// waiting for its second half in the next record.
static HIGH_SURROGATE: AtomicU32 = AtomicU32::new(0);

fn handle(which: u32) -> HANDLE {
    // SAFETY: `GetStdHandle` reads nothing but its argument.
    unsafe { GetStdHandle(which) }
}

fn console_mode(console: HANDLE) -> Option<u32> {
    let mut mode = 0;
    // SAFETY: `mode` is a local the call fills.
    (unsafe { GetConsoleMode(console, &raw mut mode) } != 0).then_some(mode)
}

fn set_mode(console: HANDLE, mode: u32) -> bool {
    // SAFETY: setting a mode reads nothing but the arguments.
    unsafe { SetConsoleMode(console, mode) != 0 }
}

/// Whether standard input and standard output are both a console.
pub(crate) fn is_terminal() -> bool {
    console_mode(handle(STD_INPUT_HANDLE)).is_some()
        && console_mode(handle(STD_OUTPUT_HANDLE)).is_some()
}

/// The output mode the terminal layer draws in.
fn our_output_mode(found: u32) -> u32 {
    found
        | ENABLE_PROCESSED_OUTPUT
        | ENABLE_VIRTUAL_TERMINAL_PROCESSING
        | DISABLE_NEWLINE_AUTO_RETURN
}

/// The input mode the terminal layer reads in: keys one at a time, not
/// echoed, Ctrl+C as a key, and as virtual-terminal sequences, with resizes
/// reported.
fn our_input_mode(found: u32) -> u32 {
    (found | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT)
        & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_MOUSE_INPUT)
}

/// Turns on virtual-terminal processing and input, UTF-8, and raw input.
pub(crate) fn enter() -> io::Result<()> {
    let (input, output) = (handle(STD_INPUT_HANDLE), handle(STD_OUTPUT_HANDLE));
    let (Some(input_mode), Some(output_mode)) = (console_mode(input), console_mode(output)) else {
        return Err(io::Error::other(
            "standard input and standard output must both be a console",
        ));
    };
    if !SAVED.swap(true, Ordering::SeqCst) {
        INPUT_MODE.store(input_mode, Ordering::SeqCst);
        OUTPUT_MODE.store(output_mode, Ordering::SeqCst);
        // SAFETY: both read nothing.
        INPUT_CP.store(unsafe { GetConsoleCP() }, Ordering::SeqCst);
        // SAFETY: as above.
        OUTPUT_CP.store(unsafe { GetConsoleOutputCP() }, Ordering::SeqCst);
    }
    wake_event()?;
    if !HANDLER.swap(true, Ordering::SeqCst) {
        // SAFETY: the handler is a function with the signature Windows
        // asks for, and stays for the life of the process.
        unsafe { SetConsoleCtrlHandler(Some(on_control), TRUE) };
    }
    ACTIVE.store(true, Ordering::SeqCst);
    // SAFETY: setting a code page reads nothing but its argument.
    unsafe {
        SetConsoleOutputCP(UTF8);
        SetConsoleCP(UTF8);
    }
    let saved_output = OUTPUT_MODE.load(Ordering::SeqCst);
    if !set_mode(output, our_output_mode(saved_output))
        // A console that refuses the line-feed flag may still take the rest.
        && !set_mode(
            output,
            our_output_mode(saved_output) & !DISABLE_NEWLINE_AUTO_RETURN,
        )
    {
        let error = io::Error::other(
            "this console cannot process escape sequences: virtual-terminal processing could \
             not be turned on",
        );
        leave();
        return Err(error);
    }
    if !set_mode(input, our_input_mode(INPUT_MODE.load(Ordering::SeqCst))) {
        let error = io::Error::last_os_error();
        leave();
        return Err(error);
    }
    Ok(())
}

/// Puts the modes and code pages back without writing anything. Safe to
/// call more than once.
pub(crate) fn leave() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        restore_modes();
    }
}

fn restore_modes() {
    set_mode(handle(STD_INPUT_HANDLE), INPUT_MODE.load(Ordering::SeqCst));
    set_mode(
        handle(STD_OUTPUT_HANDLE),
        OUTPUT_MODE.load(Ordering::SeqCst),
    );
    // SAFETY: setting a code page reads nothing but its argument.
    unsafe {
        SetConsoleCP(INPUT_CP.load(Ordering::SeqCst));
        SetConsoleOutputCP(OUTPUT_CP.load(Ordering::SeqCst));
    }
}

/// Writes the restore sequence, while virtual-terminal processing is still
/// on, then puts the modes and code pages back.
pub(crate) fn emergency() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = write_all(restore_sequence());
    restore_modes();
}

unsafe extern "system" fn on_control(_: u32) -> windows_sys::core::BOOL {
    emergency();
    // Not handled: the next handler, in the end the system's, ends the
    // process as the event asks.
    FALSE
}

fn wake_event() -> io::Result<HANDLE> {
    let existing = WAKE.load(Ordering::SeqCst);
    if existing != 0 {
        return Ok(existing as HANDLE);
    }
    // SAFETY: an unnamed auto-reset event, not signalled, default security.
    let event = unsafe { CreateEventW(ptr::null(), FALSE, FALSE, ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    match WAKE.compare_exchange(0, event as isize, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => Ok(event),
        // Another thread made one first; this one is never closed, which is
        // one handle for the life of the process.
        Err(theirs) => Ok(theirs as HANDLE),
    }
}

/// Wakes [`wait`] from another thread.
pub(crate) fn wake() {
    WOKEN.store(true, Ordering::SeqCst);
    let event = WAKE.load(Ordering::SeqCst);
    if event != 0 {
        // SAFETY: the event lives for the life of the process.
        unsafe { SetEvent(event as HANDLE) };
    }
}

/// Stops the program, which Windows has no way to do: nothing happens.
pub(crate) fn suspend() {}

/// The window's size, columns then rows.
pub(crate) fn size() -> io::Result<(u16, u16)> {
    // SAFETY: a zeroed structure of plain integers, which the call fills.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { GetConsoleScreenBufferInfo(handle(STD_OUTPUT_HANDLE), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let window = info.srWindow;
    let cols = u16::try_from(i32::from(window.Right) - i32::from(window.Left) + 1).unwrap_or(1);
    let rows = u16::try_from(i32::from(window.Bottom) - i32::from(window.Top) + 1).unwrap_or(1);
    Ok((cols, rows))
}

/// Writes all of `bytes` to standard output: one call, unless the console
/// takes only part of it.
///
/// Once the console has been put back (a panic on another thread, a control
/// event), nothing more is written: a frame would turn modes on again in a
/// console that is no longer the program's, or show as text in one whose
/// virtual-terminal processing is off again.
pub(crate) fn write(bytes: &[u8]) -> io::Result<()> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(io::Error::other("the console has been put back"));
    }
    write_all(bytes)
}

fn write_all(bytes: &[u8]) -> io::Result<()> {
    let output = handle(STD_OUTPUT_HANDLE);
    let mut rest = bytes;
    while !rest.is_empty() {
        let length = u32::try_from(rest.len()).unwrap_or(u32::MAX);
        let mut written = 0;
        // SAFETY: the pointer and length are those of a live slice, and
        // `written` is a local the call fills.
        let ok = unsafe {
            WriteFile(
                output,
                rest.as_ptr(),
                length,
                &raw mut written,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        rest = &rest[usize::try_from(written).unwrap_or(rest.len())..];
    }
    Ok(())
}

/// Waits for input, a resize or a wake, for at most `timeout` (`None`: as
/// long as it takes). Input is added to `into` as UTF-8.
pub(crate) fn wait(timeout: Option<Duration>, into: &mut Vec<u8>) -> io::Result<Ready> {
    let input = handle(STD_INPUT_HANDLE);
    let wake = wake_event()?;
    let millis = timeout.map_or(u32::MAX, |timeout| {
        u32::try_from(timeout.as_nanos().div_ceil(1_000_000)).unwrap_or(u32::MAX - 1)
    });
    let handles = [input, wake];
    loop {
        if RESIZED.swap(false, Ordering::SeqCst) {
            return Ok(Ready::Resize);
        }
        if WOKEN.swap(false, Ordering::SeqCst) {
            return Ok(Ready::Woken);
        }
        // SAFETY: an array of two live handles, and its length.
        let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, millis) };
        if result == WAIT_OBJECT_0 + 1 {
            continue;
        }
        if result != WAIT_OBJECT_0 {
            if result == 0x102 {
                // WAIT_TIMEOUT
                return Ok(if WOKEN.swap(false, Ordering::SeqCst) {
                    Ready::Woken
                } else {
                    Ready::Timeout
                });
            }
            return Err(io::Error::last_os_error());
        }
        let mut count = 0;
        // SAFETY: `count` is a local the call fills.
        if unsafe { GetNumberOfConsoleInputEvents(input, &raw mut count) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            continue;
        }
        // SAFETY: zeroed records are plain data, filled by the call.
        let mut records: [INPUT_RECORD; 64] = unsafe { std::mem::zeroed() };
        let mut read = 0;
        // SAFETY: the array and its length; `read` is a local the call
        // fills.
        let ok = unsafe {
            ReadConsoleInputW(
                input,
                records.as_mut_ptr(),
                u32::try_from(records.len()).unwrap_or(0),
                &raw mut read,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        let before = into.len();
        for record in &records[..usize::try_from(read).unwrap_or(0)] {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    // SAFETY: the record says it holds a key event.
                    let key = unsafe { record.Event.KeyEvent };
                    // SAFETY: with virtual-terminal input every key event
                    // carries a character; the union holds it as UTF-16.
                    let unit = unsafe { key.uChar.UnicodeChar };
                    if key.bKeyDown != 0 && unit != 0 {
                        for _ in 0..key.wRepeatCount.max(1) {
                            push_unit(unit, into);
                        }
                    }
                }
                // Kept until it is reported: a read that also brought keys
                // reports them first, and the resize on the next wait.
                WINDOW_BUFFER_SIZE_EVENT => RESIZED.store(true, Ordering::SeqCst),
                _ => {}
            }
        }
        if into.len() > before {
            return Ok(Ready::Input);
        }
        if RESIZED.swap(false, Ordering::SeqCst) {
            return Ok(Ready::Resize);
        }
    }
}

/// Adds one UTF-16 unit of a key to `into` as UTF-8, joining the two halves
/// of a character outside the Basic Multilingual Plane.
fn push_unit(unit: u16, into: &mut Vec<u8>) {
    let mut buffer = [0; 4];
    match unit {
        0xd800..=0xdbff => HIGH_SURROGATE.store(u32::from(unit), Ordering::SeqCst),
        0xdc00..=0xdfff => {
            let high = HIGH_SURROGATE.swap(0, Ordering::SeqCst);
            let c = u16::try_from(high)
                .ok()
                .filter(|high| *high != 0)
                .and_then(|high| char::decode_utf16([high, unit]).next())
                .and_then(Result::ok)
                .unwrap_or('\u{fffd}');
            into.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
        _ => {
            let c = char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}');
            into.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
    }
}

/// A description of the console's modes and code pages, for tests that
/// check they were put back.
#[cfg(feature = "pty")]
pub(crate) fn modes() -> io::Result<String> {
    let input = console_mode(handle(STD_INPUT_HANDLE)).ok_or_else(io::Error::last_os_error)?;
    let output = console_mode(handle(STD_OUTPUT_HANDLE)).ok_or_else(io::Error::last_os_error)?;
    // SAFETY: both read nothing.
    let (input_cp, output_cp) = unsafe { (GetConsoleCP(), GetConsoleOutputCP()) };
    Ok(format!(
        "input={input:x} output={output:x} input-cp={input_cp} output-cp={output_cp}"
    ))
}

/// Turns virtual-terminal processing of this program's console output on
/// or off, for a test that plays the old console host, where it starts off.
#[cfg(feature = "pty")]
pub(crate) fn set_vt_output(on: bool) -> io::Result<()> {
    let output = handle(STD_OUTPUT_HANDLE);
    let mode = console_mode(output).ok_or_else(io::Error::last_os_error)?;
    let mode = if on {
        mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING
    } else {
        mode & !ENABLE_VIRTUAL_TERMINAL_PROCESSING
    };
    if set_mode(output, mode) {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Pseudo-consoles, for tests.
#[cfg(feature = "pty")]
pub(crate) mod pty {
    #![allow(
        clippy::disallowed_types,
        reason = "test support: it reads and writes a pseudo-console's pipes as files"
    )]

    use std::fs::File;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::ptr;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        COORD, CTRL_BREAK_EVENT, ClosePseudoConsole, CreatePseudoConsole, GenerateConsoleCtrlEvent,
        HPCON, ResizePseudoConsole,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetExitCodeProcess, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::FALSE;

    /// A program running in a pseudo-console of its own.
    #[derive(Debug)]
    pub(crate) struct Pty {
        console: HPCON,
        input: OwnedHandle,
        output: OwnedHandle,
        process: OwnedHandle,
        pid: u32,
    }

    // SAFETY: a pseudo-console handle may be used from any thread.
    unsafe impl Send for Pty {}

    fn last() -> io::Error {
        io::Error::last_os_error()
    }

    fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let (mut read, mut write): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut());
        // SAFETY: two locals the call fills, default security.
        if unsafe { CreatePipe(&raw mut read, &raw mut write, ptr::null(), 0) } == 0 {
            return Err(last());
        }
        // SAFETY: both handles were just made, and are ours alone.
        Ok(unsafe {
            (
                OwnedHandle::from_raw_handle(read),
                OwnedHandle::from_raw_handle(write),
            )
        })
    }

    fn coord(cols: u16, rows: u16) -> COORD {
        COORD {
            X: i16::try_from(cols).unwrap_or(i16::MAX),
            Y: i16::try_from(rows).unwrap_or(i16::MAX),
        }
    }

    /// One argument as `CommandLineToArgvW` and the C runtime read it back.
    fn quote(argument: &str, line: &mut String) {
        if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
            line.push_str(argument);
            return;
        }
        line.push('"');
        let mut backslashes = 0;
        for c in argument.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    line.push('"');
                    backslashes = 0;
                }
                _ => {
                    line.extend(std::iter::repeat_n('\\', backslashes));
                    line.push(c);
                    backslashes = 0;
                }
            }
        }
        line.extend(std::iter::repeat_n('\\', backslashes * 2));
        line.push('"');
    }

    impl Pty {
        pub(crate) fn spawn(
            program: &str,
            arguments: &[String],
            env: &[(String, String)],
            cols: u16,
            rows: u16,
        ) -> io::Result<Self> {
            if !env.is_empty() {
                return Err(io::Error::other(
                    "a program in a pseudo-console takes its environment from the test",
                ));
            }
            let (console_input, input) = pipe()?;
            let (output, console_output) = pipe()?;
            let mut console: HPCON = 0;
            // SAFETY: the two pipe ends are live; the console duplicates
            // them, so ours are closed when this function returns.
            let result = unsafe {
                CreatePseudoConsole(
                    coord(cols, rows),
                    console_input.as_raw_handle(),
                    console_output.as_raw_handle(),
                    0,
                    &raw mut console,
                )
            };
            if result < 0 {
                return Err(io::Error::from_raw_os_error(result));
            }
            drop((console_input, console_output));

            let mut line = String::new();
            quote(program, &mut line);
            for argument in arguments {
                line.push(' ');
                quote(argument, &mut line);
            }
            let mut line: Vec<u16> = line.encode_utf16().chain([0]).collect();

            let mut size = 0;
            // SAFETY: asks how large a list of one attribute is; it fails
            // with that size, by design.
            unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &raw mut size) };
            let mut list = vec![0u64; size.div_ceil(8)];
            let attributes = list.as_mut_ptr().cast();
            // SAFETY: the buffer is at least `size` bytes, and aligned.
            if unsafe { InitializeProcThreadAttributeList(attributes, 1, 0, &raw mut size) } == 0 {
                let error = last();
                // SAFETY: the console was made above and nothing uses it.
                unsafe { ClosePseudoConsole(console) };
                return Err(error);
            }
            // SAFETY: the attribute's value is the console handle itself,
            // passed as a pointer-sized value, as Windows documents.
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    attributes,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    console as *const core::ffi::c_void,
                    size_of::<HPCON>(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            // SAFETY: zeroed plain data, with its size and fields set below.
            let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
            startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap_or(0);
            startup.lpAttributeList = attributes;
            // The child must not take this process's standard handles, which
            // a test runner has redirected: invalid ones make it use the
            // console.
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            // SAFETY: zeroed plain data the call fills.
            let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let created = updated != 0
                // SAFETY: the command line is a writable, terminated UTF-16
                // string; the structures are complete; no handle is
                // inherited.
                && unsafe {
                    CreateProcessW(
                        ptr::null(),
                        line.as_mut_ptr(),
                        ptr::null(),
                        ptr::null(),
                        FALSE,
                        EXTENDED_STARTUPINFO_PRESENT,
                        ptr::null(),
                        ptr::null(),
                        &raw const startup.StartupInfo,
                        &raw mut info,
                    )
                } != 0;
            let error = last();
            // SAFETY: the list was initialised above.
            unsafe { DeleteProcThreadAttributeList(attributes) };
            drop(list);
            if !created {
                // SAFETY: the console was made above, and nothing runs in it.
                unsafe { ClosePseudoConsole(console) };
                return Err(error);
            }
            // SAFETY: both handles were just made for us.
            let (process, thread) = unsafe {
                (
                    OwnedHandle::from_raw_handle(info.hProcess),
                    OwnedHandle::from_raw_handle(info.hThread),
                )
            };
            drop(thread);
            Ok(Self {
                console,
                input,
                output,
                process,
                pid: info.dwProcessId,
            })
        }

        pub(crate) fn reader(&self) -> io::Result<File> {
            Ok(File::from(self.output.try_clone()?))
        }

        pub(crate) fn writer(&self) -> io::Result<File> {
            Ok(File::from(self.input.try_clone()?))
        }

        pub(crate) fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
            // SAFETY: the console lives as long as `self`.
            let result = unsafe { ResizePseudoConsole(self.console, coord(cols, rows)) };
            if result < 0 {
                Err(io::Error::from_raw_os_error(result))
            } else {
                Ok(())
            }
        }

        pub(crate) fn pid(&self) -> u32 {
            self.pid
        }

        pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            // SAFETY: the process handle lives as long as `self`.
            if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
                return Ok(None);
            }
            let mut code = 0;
            // SAFETY: `code` is a local the call fills.
            if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) } == 0 {
                return Err(last());
            }
            Ok(Some(ExitStatus::from_raw(code)))
        }

        pub(crate) fn kill(&mut self) -> io::Result<()> {
            // SAFETY: the process handle lives as long as `self`.
            if unsafe { TerminateProcess(self.process.as_raw_handle(), 1) } == 0 {
                return Err(last());
            }
            Ok(())
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            // SAFETY: closed once, here. A reader of the output sees its end
            // once the console is gone.
            unsafe { ClosePseudoConsole(self.console) };
        }
    }

    /// Whether a read error on the output means the other side has gone.
    pub(crate) fn is_end(error: &io::Error) -> bool {
        // ERROR_BROKEN_PIPE
        error.raw_os_error() == Some(109)
    }

    unsafe extern "system" fn ignore(_: u32) -> windows_sys::core::BOOL {
        super::TRUE
    }

    /// Makes this process stay through console control events: its handler
    /// says each one was handled.
    pub(crate) fn ignore_control_events() {
        // SAFETY: the handler is a function with the signature Windows asks
        // for, and stays for the life of the process.
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(ignore), super::TRUE)
        };
    }

    /// Sends a Ctrl+Break event to every process on this program's console.
    pub(crate) fn signal(_pid: u32, _signal: i32) -> io::Result<()> {
        // SAFETY: reads nothing but its arguments.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) } == 0 {
            return Err(last());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_modes_turn_on_what_is_needed_and_off_what_gets_in_the_way() {
        let output = our_output_mode(0x3);
        assert_ne!(output & ENABLE_VIRTUAL_TERMINAL_PROCESSING, 0);
        let input = our_input_mode(
            ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_MOUSE_INPUT
                | 0x40,
        );
        assert_ne!(input & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
        assert_ne!(input & ENABLE_WINDOW_INPUT, 0);
        assert_eq!(
            input & (ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT),
            0
        );
        assert_ne!(input & 0x40, 0, "a flag it does not know is kept");
    }

    #[test]
    fn nothing_is_written_while_the_console_is_not_the_programs() {
        assert!(!ACTIVE.load(Ordering::SeqCst));
        assert!(write(b"\x1b[?25l").is_err());
    }

    #[test]
    fn surrogate_halves_are_joined() {
        let mut out = Vec::new();
        for unit in "a😀é".encode_utf16() {
            push_unit(unit, &mut out);
        }
        assert_eq!(String::from_utf8(out).unwrap(), "a😀é");
        let mut out = Vec::new();
        push_unit(0xdc00, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "\u{fffd}");
    }
}
