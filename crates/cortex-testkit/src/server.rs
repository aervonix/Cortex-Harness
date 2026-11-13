//! The scripted SSE server: a provider's HTTP endpoint on the loopback
//! interface (`docs/testing-and-ci.md`, "Deterministic agent tests without
//! a network").
//!
//! It is for what only a real socket can show: that the real HTTP client
//! streams a body as it arrives, that cancelling a request closes the
//! connection, that a time limit ends a stalled response. A test gives it
//! [`ServerReply`]s, one per request, and each reply is a script of steps:
//! send these bytes, tell the test this point was reached, wait for the
//! test, hold the connection until the client hangs up, or drop it.
//!
//! Nothing here waits for a fixed time. The server and the test take turns
//! through [`Gate`]s. The limits that appear are bounds, there so that a
//! mistake is a failed test and not a hung one. Dropping the server opens
//! every gate one of its threads is still waiting on, so that a test that
//! ends early does not wait for them.
//!
//! One thing is looked at on an interval, and only while a server thread
//! has nothing to read: whether the server is being dropped. Closing a
//! socket's reading side from another thread wakes a read that is waiting
//! on it on Linux, and Windows does not promise that it does, so a waiting
//! read also wakes every few milliseconds to ask. A test never waits on
//! that interval: at most, dropping a server that still holds a connection
//! takes that much longer.
//!
//! The server speaks just enough HTTP/1.1 for one request per connection,
//! answers with a chunked body and closes. It listens on `127.0.0.1` only,
//! on a port the system picks, and uses plain threads, so it needs no async
//! runtime and can also serve another process.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::gate::Gate;

/// The longest a server thread waits for the test to open a gate.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often a read that has nothing to read asks whether the server is
/// being dropped.
const STOP_CHECK: Duration = Duration::from_millis(20);

/// A connection's reading side, as a server thread reads it: a read that
/// waits gives up, as at the end of the stream, when the server is dropped.
struct UntilStopped<'a> {
    connection: &'a TcpStream,
    shared: &'a Shared,
}

impl Read for UntilStopped<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.connection.read(buffer) {
                // The read timed out: nothing arrived. The two kinds are
                // what Unix and Windows call it.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.shared.stopping.load(Ordering::SeqCst) {
                        return Ok(0);
                    }
                }
                other => return other,
            }
        }
    }
}

/// One scripted answer of the server.
#[derive(Debug, Clone)]
pub struct ServerReply {
    /// Opened by the test before the status line may be sent.
    hold: Option<Gate>,
    /// Close the connection without answering.
    hang_up: bool,
    status: u16,
    headers: Vec<(String, String)>,
    steps: Vec<Step>,
}

#[derive(Debug, Clone)]
enum Step {
    Send(Vec<u8>),
    Mark(Gate),
    Wait(Gate),
    Hang(Gate),
    Drop,
}

impl ServerReply {
    /// A `200` response with the content type of an event stream and
    /// nothing in its body yet.
    #[must_use]
    pub fn sse() -> Self {
        Self::status(200).header("content-type", "text/event-stream")
    }

    /// A response with this status and nothing in its body yet.
    #[must_use]
    pub fn status(status: u16) -> Self {
        Self {
            hold: None,
            hang_up: false,
            status,
            headers: Vec::new(),
            steps: Vec::new(),
        }
    }

    /// No response at all: the server reads the request and closes the
    /// connection.
    #[must_use]
    pub fn hang_up() -> Self {
        Self {
            hang_up: true,
            ..Self::status(0)
        }
    }

    /// Sends nothing at all, not even the status, until the test opens
    /// `gate`: a server that has taken the request and not answered yet.
    #[must_use]
    pub fn hold_until(mut self, gate: &Gate) -> Self {
        self.hold = Some(gate.clone());
        self
    }

    /// Adds a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Sends these bytes as one piece of the body, and flushes.
    #[must_use]
    pub fn send(mut self, bytes: impl AsRef<[u8]>) -> Self {
        self.steps.push(Step::Send(bytes.as_ref().to_vec()));
        self
    }

    /// Opens `gate` when the script gets here, so that the test knows
    /// everything before this point has been written.
    #[must_use]
    pub fn mark(mut self, gate: &Gate) -> Self {
        self.steps.push(Step::Mark(gate.clone()));
        self
    }

    /// Waits here until the test opens `gate`.
    #[must_use]
    pub fn wait(mut self, gate: &Gate) -> Self {
        self.steps.push(Step::Wait(gate.clone()));
        self
    }

    /// Sends nothing more and keeps the connection open until the client
    /// closes it, then opens `closed`. This is a stalled response, and it
    /// is how a test sees that a cancelled request really hung up. Dropping
    /// the server ends the hold too, and opens the gate.
    #[must_use]
    pub fn hang_until_closed(mut self, closed: &Gate) -> Self {
        self.steps.push(Step::Hang(closed.clone()));
        self
    }

    /// Drops the connection here, with the body unfinished.
    #[must_use]
    pub fn disconnect(mut self) -> Self {
        self.steps.push(Step::Drop);
        self
    }
}

/// A request the server received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedRequest {
    /// The method: `POST`.
    pub method: String,
    /// The path and query, as sent.
    pub path: String,
    /// The headers, as sent, in order.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl ReceivedRequest {
    /// The value of a header, by name in any case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A local HTTP server that plays a script. It stops, and closes every
/// connection, when it is dropped.
#[derive(Debug)]
pub struct SseServer {
    address: SocketAddr,
    shared: Arc<Shared>,
    acceptor: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct Shared {
    stopping: AtomicBool,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    replies: VecDeque<ServerReply>,
    requests: Vec<ReceivedRequest>,
    /// A handle on every connection, to close them all at the end.
    connections: Vec<TcpStream>,
    workers: Vec<JoinHandle<()>>,
    /// The gates server threads are waiting on, to let them all go at the
    /// end.
    waiting_on: Vec<Gate>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the test to open `gate`. Returns whether to go on: not
    /// when the server is stopping, and not when the test never opened it.
    fn wait_for(&self, gate: &Gate) -> bool {
        {
            let mut state = self.lock();
            // Checked under the lock the stopping server takes to find the
            // gates to open, so a thread either sees that it is stopping or
            // has its gate opened.
            if self.stopping.load(Ordering::SeqCst) {
                return false;
            }
            state.waiting_on.push(gate.clone());
        }
        gate.wait(PATIENCE) && !self.stopping.load(Ordering::SeqCst)
    }
}

impl SseServer {
    /// Starts a server on a free port of the loopback interface.
    #[must_use]
    pub fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let address = listener.local_addr().expect("the listener has an address");
        let shared = Arc::new(Shared::default());
        let for_thread = Arc::clone(&shared);
        let acceptor = std::thread::spawn(move || accept(&listener, &for_thread));
        Self {
            address,
            shared,
            acceptor: Some(acceptor),
        }
    }

    /// The server's base URL: `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The URL of a path on the server. `path` starts with `/`.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url())
    }

    /// Adds a reply to the end of the script. Each request takes the next.
    pub fn push(&self, reply: ServerReply) {
        self.shared.lock().replies.push_back(reply);
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<ReceivedRequest> {
        self.shared.lock().requests.clone()
    }
}

impl Drop for SseServer {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        // Wake the accepting thread, which is blocked waiting for a client.
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        // Closing the connections ends every thread that is reading or
        // writing one, and opening the gates ends every thread that is
        // waiting for the test.
        let (connections, workers, gates) = {
            let mut state = self.shared.lock();
            (
                std::mem::take(&mut state.connections),
                std::mem::take(&mut state.workers),
                std::mem::take(&mut state.waiting_on),
            )
        };
        for connection in connections {
            let _ = connection.shutdown(Shutdown::Both);
        }
        for gate in gates {
            gate.open();
        }
        for worker in workers {
            let _ = worker.join();
        }
    }
}

fn accept(listener: &TcpListener, shared: &Arc<Shared>) {
    for connection in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(connection) = connection else { continue };
        let Ok(handle) = connection.try_clone() else {
            continue;
        };
        let for_worker = Arc::clone(shared);
        let worker = std::thread::spawn(move || {
            // A connection that breaks is the client's business: the test
            // on the other end sees it as a failed request.
            let _ = serve(&connection, &for_worker);
            // The server keeps a second handle on the socket, so dropping
            // this one would not close it. Say so to the client.
            let _ = connection.shutdown(Shutdown::Both);
        });
        let mut state = shared.lock();
        state.connections.push(handle);
        state.workers.push(worker);
    }
}

/// Reads one request, plays the next reply, and closes.
fn serve(connection: &TcpStream, shared: &Shared) -> std::io::Result<()> {
    connection.set_read_timeout(Some(STOP_CHECK))?;
    let mut reader = BufReader::new(UntilStopped { connection, shared });
    let mut writer = connection;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        // The connection made to wake the accepting thread sends nothing.
        return Ok(());
    };
    let mut request = ReceivedRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            request
                .headers
                .push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let length = request
        .header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    request.body = vec![0; length];
    reader.read_exact(&mut request.body)?;

    let reply = {
        let mut state = shared.lock();
        state.requests.push(request);
        state.replies.pop_front()
    };
    let reply = reply.unwrap_or_else(|| {
        ServerReply::status(500).send("the scripted server has no reply left for this request")
    });

    if let Some(hold) = &reply.hold
        && !shared.wait_for(hold)
    {
        return Ok(());
    }
    if reply.hang_up {
        return Ok(());
    }

    let mut head = format!("HTTP/1.1 {} Scripted\r\n", reply.status);
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("transfer-encoding: chunked\r\nconnection: close\r\n\r\n");
    writer.write_all(head.as_bytes())?;
    writer.flush()?;

    for step in reply.steps {
        match step {
            Step::Send(bytes) => {
                if !bytes.is_empty() {
                    write!(writer, "{:x}\r\n", bytes.len())?;
                    writer.write_all(&bytes)?;
                    writer.write_all(b"\r\n")?;
                    writer.flush()?;
                }
            }
            Step::Mark(gate) => gate.open(),
            Step::Wait(gate) => {
                if !shared.wait_for(&gate) {
                    return Ok(());
                }
            }
            Step::Hang(closed) => {
                // The client sends nothing more, so a read returns only
                // when it closes the connection, or the server is dropped.
                let mut rest = [0u8; 64];
                while matches!(reader.read(&mut rest), Ok(read) if read > 0) {}
                closed.open();
                return Ok(());
            }
            Step::Drop => return Ok(()),
        }
    }
    writer.write_all(b"0\r\n\r\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client with no HTTP library: writes a request, reads to the end.
    fn exchange(server: &SseServer, body: &str) -> String {
        let mut connection = TcpStream::connect(server.address).unwrap();
        write!(
            connection,
            "POST /v1/test HTTP/1.1\r\nHost: test\r\nX-Api-Key: k\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        connection.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn it_records_the_request_and_plays_the_reply_in_chunks() {
        let server = SseServer::start();
        server.push(
            ServerReply::sse()
                .header("x-request-id", "req_7")
                .send("data: one\n\n")
                .send("data: two\n\n"),
        );
        let response = exchange(&server, r#"{"model":"m"}"#);
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        assert!(
            response.contains("content-type: text/event-stream\r\n"),
            "{response}"
        );
        assert!(response.contains("x-request-id: req_7\r\n"), "{response}");
        // Two chunks of eleven bytes, then the end of the chunked body.
        assert!(
            response.ends_with("b\r\ndata: one\n\n\r\nb\r\ndata: two\n\n\r\n0\r\n\r\n"),
            "{response}"
        );

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/test");
        assert_eq!(requests[0].header("x-api-key"), Some("k"));
        assert_eq!(requests[0].body, br#"{"model":"m"}"#);
        assert!(server.url("/v1/test").starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn a_request_with_no_reply_left_is_a_500() {
        let server = SseServer::start();
        let response = exchange(&server, "");
        assert!(response.starts_with("HTTP/1.1 500 "), "{response}");
    }

    #[test]
    fn it_takes_turns_with_the_test_and_sees_the_client_hang_up() {
        let server = SseServer::start();
        let (reached, go_on, closed) = (Gate::new(), Gate::new(), Gate::new());
        server.push(
            ServerReply::sse()
                .send("data: first\n\n")
                .mark(&reached)
                .wait(&go_on)
                .send("data: second\n\n")
                .hang_until_closed(&closed),
        );
        let mut connection = TcpStream::connect(server.address).unwrap();
        connection
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();

        assert!(reached.wait(PATIENCE), "the server wrote the first piece");
        assert!(!closed.is_open());
        go_on.open();
        // Read until the second piece has arrived.
        let mut received = Vec::new();
        let mut buffer = [0u8; 256];
        while !received.windows(6).any(|window| window == b"second") {
            let read = connection.read(&mut buffer).unwrap();
            assert!(read > 0, "the server hung up early");
            received.extend_from_slice(&buffer[..read]);
        }
        assert!(!closed.is_open(), "the server is holding the connection");
        drop(connection);
        assert!(closed.wait(PATIENCE), "the server saw the client hang up");
    }

    #[test]
    fn dropping_the_server_ends_a_connection_it_was_holding() {
        let server = SseServer::start();
        let (reached, closed) = (Gate::new(), Gate::new());
        server.push(ServerReply::sse().mark(&reached).hang_until_closed(&closed));
        let mut connection = TcpStream::connect(server.address).unwrap();
        connection
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        assert!(reached.wait(PATIENCE));
        drop(server);
        assert!(closed.is_open(), "its thread was let go and joined");
        let mut rest = Vec::new();
        let _ = connection.read_to_end(&mut rest);
    }

    #[test]
    fn it_can_hold_the_whole_response_back_and_is_let_go_when_dropped() {
        let server = SseServer::start();
        let never_opened = Gate::new();
        server.push(
            ServerReply::sse()
                .hold_until(&never_opened)
                .send("data: x\n\n"),
        );
        let mut connection = TcpStream::connect(server.address).unwrap();
        connection
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        // Dropping the server must not wait out the bound on the gate.
        drop(server);
        let mut rest = Vec::new();
        let _ = connection.read_to_end(&mut rest);
        assert!(rest.is_empty(), "nothing was sent: {rest:?}");
    }

    #[test]
    fn a_disconnect_leaves_the_body_unfinished() {
        let server = SseServer::start();
        server.push(ServerReply::sse().send("data: half\n\n").disconnect());
        server.push(ServerReply::hang_up());
        let response = exchange(&server, "");
        assert!(response.contains("data: half"), "{response}");
        assert!(!response.ends_with("0\r\n\r\n"), "{response}");
        assert_eq!(
            exchange(&server, ""),
            "",
            "and a hang-up sends nothing at all"
        );
        assert_eq!(server.requests().len(), 2);
    }
}
