//! Plaintext local PostgreSQL proxy for independently observed COMMIT boundaries.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct CommitProxy {
    address: SocketAddr,
    connection: String,
    armed: Arc<AtomicBool>,
    committed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    LostAck,
    WarningBeforeComplete,
    WarningBeforeReady,
    FragmentedWarning,
    WarningAfterReady,
    WarningBeforeCommit,
    BenignMessages,
    UnclassifiedNotice,
}

impl CommitProxy {
    pub fn start() -> Self {
        Self::with_fault(Fault::LostAck)
    }
    pub fn with_fault(fault: Fault) -> Self {
        let source: postgres::Config = std::env::var("HTLK_TEST_POSTGRES_URL")
            .unwrap()
            .parse()
            .unwrap();
        let host = match source.get_hosts().first().expect("local TCP test host") {
            postgres::config::Host::Tcp(host) => host.clone(),
            _ => panic!("commit proxy requires a TCP test database"),
        };
        let port = source.get_ports().first().copied().unwrap_or(5432);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut connection = format!(
            "host=127.0.0.1 port={} sslmode=disable user={} dbname={}",
            address.port(),
            quote(source.get_user().unwrap_or("postgres")),
            quote(source.get_dbname().unwrap_or("postgres"))
        );
        if let Some(password) = source.get_password() {
            connection.push_str(&format!(
                " password={}",
                quote(std::str::from_utf8(password).unwrap())
            ));
        }
        let armed = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let a = armed.clone();
        let c = committed.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            let client = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && !stopping.load(Ordering::Acquire) =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    _ => return,
                }
            };
            let server = TcpStream::connect((host.as_str(), port)).unwrap();
            client.set_nonblocking(false).unwrap();
            client.set_nodelay(true).unwrap();
            server.set_nodelay(true).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let pending = Arc::new(AtomicBool::new(false));
            let front_pending = pending.clone();
            let front_armed = a.clone();
            let mut front = client.try_clone().unwrap();
            let mut upstream = server.try_clone().unwrap();
            let forward = thread::spawn(move || {
                let result = (|| -> io::Result<()> {
                    // SSL is disabled explicitly; StartupMessage has no type byte.
                    let startup = packet(&mut front)?;
                    upstream.write_all(&startup)?;
                    loop {
                        let mut kind = [0];
                        front.read_exact(&mut kind)?;
                        let frame = packet(&mut front)?;
                        if kind[0] == b'Q'
                            && &frame[4..] == b"COMMIT\0"
                            && front_armed.load(Ordering::Acquire)
                        {
                            front_pending.store(true, Ordering::Release);
                        }
                        upstream.write_all(&kind)?;
                        upstream.write_all(&frame)?;
                    }
                })();
                // Release the opposite proxy reader on client disconnect instead
                // of waiting for its I/O timeout during ordinary test cleanup.
                let _ = upstream.shutdown(Shutdown::Both);
                result
            });
            let mut downstream = client;
            let mut back = server;
            let result = (|| -> io::Result<()> {
                loop {
                    let mut kind = [0];
                    back.read_exact(&mut kind)?;
                    let frame = packet(&mut back)?;
                    if kind[0] == b'I'
                        && fault == Fault::WarningBeforeCommit
                        && a.swap(false, Ordering::AcqRel)
                    {
                        downstream.write_all(&notice(Some("WARNING")))?;
                    }
                    if kind[0] == b'C'
                        && &frame[4..] == b"COMMIT\0"
                        && pending.swap(false, Ordering::AcqRel)
                        && a.swap(false, Ordering::AcqRel)
                    {
                        c.store(true, Ordering::Release);
                        if fault == Fault::LostAck {
                            // Neither completion frame reaches the embedding client.
                            return Ok(());
                        }
                        let mut ready_kind = [0];
                        back.read_exact(&mut ready_kind)?;
                        let ready = packet(&mut back)?;
                        assert_eq!(ready_kind[0], b'Z');
                        let mut complete = vec![b'C'];
                        complete.extend_from_slice(&frame);
                        let mut ready_frame = vec![b'Z'];
                        ready_frame.extend_from_slice(&ready);
                        let messages = match fault {
                            Fault::WarningBeforeComplete => {
                                [notice(Some("WARNING")), complete, ready_frame].concat()
                            }
                            Fault::WarningBeforeReady | Fault::FragmentedWarning => {
                                [complete, notice(Some("WARNING")), ready_frame].concat()
                            }
                            Fault::WarningAfterReady => {
                                [complete, ready_frame, notice(Some("WARNING"))].concat()
                            }
                            Fault::BenignMessages => [
                                complete,
                                notice(Some("NOTICE")),
                                notification(),
                                ready_frame,
                            ]
                            .concat(),
                            Fault::UnclassifiedNotice => {
                                [complete, notice(None), ready_frame].concat()
                            }
                            Fault::LostAck | Fault::WarningBeforeCommit => unreachable!(),
                        };
                        // Deliberately coalesce completion and asynchronous messages
                        // into one write: scheduling cannot repair a bad classifier.
                        if fault == Fault::FragmentedWarning {
                            for byte in &messages {
                                downstream.write_all(std::slice::from_ref(byte))?;
                            }
                        } else {
                            downstream.write_all(&messages)?;
                        }
                        continue;
                    }
                    downstream.write_all(&kind)?;
                    downstream.write_all(&frame)?;
                }
            })();
            let _ = result;
            let _ = downstream.shutdown(Shutdown::Both);
            let _ = back.shutdown(Shutdown::Both);
            let _ = forward.join();
        });
        Self {
            address,
            connection,
            armed,
            committed,
            stop,
            worker: Some(worker),
        }
    }
    pub fn connection(&self) -> &str {
        &self.connection
    }
    pub fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }
    pub fn observed_commit(&self) -> bool {
        self.committed.load(Ordering::Acquire)
    }
}
impl Drop for CommitProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address).map(|stream| stream.shutdown(Shutdown::Both));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn packet(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if !(4..=8 * 1024 * 1024).contains(&size) {
        return Err(io::Error::other("invalid test protocol frame"));
    }
    let mut result = vec![0; size];
    result[..4].copy_from_slice(&header);
    stream.read_exact(&mut result[4..])?;
    Ok(result)
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn notice(severity: Option<&str>) -> Vec<u8> {
    // Localized S, machine-readable V, generic SQLSTATE: classification must use
    // structured severity, not English diagnostics or a special warning code.
    let mut fields = b"Savertissement\0C01000\0Mprotected fixture diagnostic\0".to_vec();
    if let Some(severity) = severity {
        fields.push(b'V');
        fields.extend_from_slice(severity.as_bytes());
        fields.push(0);
    }
    fields.push(0);
    message(b'N', &fields)
}

fn notification() -> Vec<u8> {
    message(b'A', b"\0\0\0\x01htlk_fixture\0ordinary notification\0")
}

fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut message = vec![kind];
    message.extend_from_slice(&(u32::try_from(body.len()).unwrap() + 4).to_be_bytes());
    message.extend_from_slice(body);
    message
}
