//! Host-side egress proxy for sandboxed runs (ADR-0019).
//!
//! A sandbox with an allowlist network policy still has no network interface
//! of its own. Its only way out is this proxy, reached through a Unix socket
//! bind-mounted into the sandbox. The proxy accepts nothing but
//! `CONNECT host:port`, and only for an exactly allowlisted host name on an
//! allowed port. It refuses IP literals, resolves names itself, and (unless
//! explicitly permitted) refuses names that resolve to loopback, private,
//! link-local or other non-public addresses, which blocks DNS rebinding into
//! the host's own services. After the tunnel opens it reads the TLS
//! ClientHello and requires its server name (SNI) to equal the requested
//! host, which blocks domain fronting through a shared CDN address and any
//! non-TLS protocol. Every request is logged, allowed or denied; attempts
//! beyond the connection limit are refused unread and only counted.

use serde::Serialize;
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_REQUEST_HEAD: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CLIENT_HELLO: usize = 16 * 1024;

/// What one run may reach.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    /// Exact host names (compared case-insensitively). No wildcards.
    pub hosts: BTreeSet<String>,
    pub ports: BTreeSet<u16>,
    /// Permit destinations that resolve to loopback, private, link-local or
    /// other non-public addresses (local testing only).
    pub allow_private_destinations: bool,
    /// Connection attempts (allowed or denied) the run may make.
    pub max_connections: usize,
    /// Total bytes (both directions, all connections) the run may move.
    pub max_bytes: u64,
}

/// Everything the proxy saw during one run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EgressLog {
    pub events: Vec<EgressEvent>,
    /// Attempts refused unread because the connection limit was reached.
    pub refused_over_limit: u64,
}

/// One proxy request, as recorded in the run's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressEvent {
    pub host: String,
    pub port: u16,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

struct Logged {
    host: String,
    port: u16,
    allowed: bool,
    reason: Option<String>,
    up: Arc<AtomicU64>,
    down: Arc<AtomicU64>,
}

struct Shared {
    policy: EgressPolicy,
    log: Mutex<Vec<Logged>>,
    connections: AtomicUsize,
    over_limit: AtomicU64,
    bytes: AtomicU64,
}

/// A running proxy. Stop it to get the request log.
pub struct EgressProxy {
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl EgressProxy {
    /// Listens on a new Unix socket at `socket_path`.
    pub fn start(socket_path: &Path, policy: EgressPolicy) -> io::Result<Self> {
        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            policy: EgressPolicy {
                hosts: policy
                    .hosts
                    .iter()
                    .map(|h| h.to_ascii_lowercase())
                    .collect(),
                ..policy
            },
            log: Mutex::new(Vec::new()),
            connections: AtomicUsize::new(0),
            over_limit: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        });
        let acceptor = {
            let stop = Arc::clone(&stop);
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let max = shared.policy.max_connections;
                            if shared.connections.fetch_add(1, Ordering::SeqCst) >= max {
                                shared.over_limit.fetch_add(1, Ordering::SeqCst);
                                drop(stream);
                                continue;
                            }
                            let shared = Arc::clone(&shared);
                            thread::spawn(move || handle(stream, &shared));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(20)),
                    }
                }
            })
        };
        Ok(Self {
            stop,
            acceptor: Some(acceptor),
            shared,
        })
    }

    /// Stops accepting and returns every request seen so far.
    pub fn stop(mut self) -> EgressLog {
        self.shutdown();
        EgressLog {
            events: self.events(),
            refused_over_limit: self.shared.over_limit.load(Ordering::SeqCst),
        }
    }

    fn events(&self) -> Vec<EgressEvent> {
        let log = self.shared.log.lock().map(|l| {
            l.iter()
                .map(|e| EgressEvent {
                    host: e.host.clone(),
                    port: e.port,
                    allowed: e.allowed,
                    reason: e.reason.clone(),
                    bytes_up: e.up.load(Ordering::SeqCst),
                    bytes_down: e.down.load(Ordering::SeqCst),
                })
                .collect()
        });
        log.unwrap_or_default()
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle(mut client: UnixStream, shared: &Shared) {
    let _ = client.set_read_timeout(Some(HEAD_TIMEOUT));
    let head = match read_head(&mut client) {
        Some(head) => head,
        None => return,
    };
    let (host, port) = match parse_connect(&head) {
        Ok(target) => target,
        Err(reason) => {
            deny(&mut client, shared, "", 0, reason);
            return;
        }
    };
    let policy = &shared.policy;
    if !policy.hosts.contains(&host) {
        deny(
            &mut client,
            shared,
            &host,
            port,
            "host is not on the allowlist".into(),
        );
        return;
    }
    if !policy.ports.contains(&port) {
        deny(
            &mut client,
            shared,
            &host,
            port,
            format!("port {port} is not allowed"),
        );
        return;
    }
    let addresses: Vec<SocketAddr> = match (host.as_str(), port).to_socket_addrs() {
        Ok(addresses) => addresses.collect(),
        Err(_) => {
            deny(
                &mut client,
                shared,
                &host,
                port,
                "host could not be resolved".into(),
            );
            return;
        }
    };
    let usable: Vec<SocketAddr> = addresses
        .into_iter()
        .filter(|a| policy.allow_private_destinations || is_public(a.ip()))
        .collect();
    if usable.is_empty() {
        deny(
            &mut client,
            shared,
            &host,
            port,
            "host resolves only to loopback, private or other non-public addresses".into(),
        );
        return;
    }
    let upstream = usable
        .iter()
        .find_map(|address| TcpStream::connect_timeout(address, CONNECT_TIMEOUT).ok());
    let Some(mut upstream) = upstream else {
        deny(
            &mut client,
            shared,
            &host,
            port,
            "host could not be reached".into(),
        );
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let hello = match read_client_hello(&mut client) {
        Ok(hello) => hello,
        Err(reason) => {
            log_denied(shared, &host, port, reason);
            return;
        }
    };
    match server_name(&hello) {
        Some(name) if name.eq_ignore_ascii_case(&host) => {}
        Some(name) => {
            log_denied(
                shared,
                &host,
                port,
                format!("TLS server name {name:?} does not match the requested host"),
            );
            return;
        }
        None => {
            log_denied(
                shared,
                &host,
                port,
                "TLS ClientHello has no server name".into(),
            );
            return;
        }
    }
    let hello_bytes = hello.len() as u64;
    if shared.bytes.fetch_add(hello_bytes, Ordering::SeqCst) + hello_bytes > policy.max_bytes
        || upstream.write_all(&hello).is_err()
    {
        return;
    }
    let up = Arc::new(AtomicU64::new(hello_bytes));
    let down = Arc::new(AtomicU64::new(0));
    if let Ok(mut log) = shared.log.lock() {
        log.push(Logged {
            host,
            port,
            allowed: true,
            reason: None,
            up: Arc::clone(&up),
            down: Arc::clone(&down),
        });
    }
    let _ = client.set_read_timeout(None);
    tunnel(client, upstream, shared, &up, &down);
}

fn read_head(client: &mut UnixStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_REQUEST_HEAD || client.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

/// `CONNECT host:port HTTP/1.x` → (lower-case host, port).
fn parse_connect(head: &str) -> Result<(String, u16), String> {
    let line = head.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let (method, target, version) = (parts.next(), parts.next(), parts.next());
    if method != Some("CONNECT") {
        return Err("only CONNECT (HTTPS tunnels) is allowed".into());
    }
    if !version.is_some_and(|v| v.starts_with("HTTP/1.")) {
        return Err("malformed CONNECT request".into());
    }
    let target = target.unwrap_or("");
    let (host, port) = target
        .rsplit_once(':')
        .ok_or_else(|| "CONNECT target must be host:port".to_string())?;
    let port: u16 = port.parse().map_err(|_| "invalid port".to_string())?;
    let host = host.to_ascii_lowercase();
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err("IP address destinations are not allowed; use an allowlisted name".into());
    }
    if !valid_host_name(&host) {
        return Err("invalid host name".into());
    }
    Ok((host, port))
}

/// An exact DNS name: letters, digits, '-' and '.', not an IP address.
pub(crate) fn valid_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.parse::<IpAddr>().is_err()
        && !host.starts_with(['.', '-'])
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn deny(client: &mut UnixStream, shared: &Shared, host: &str, port: u16, reason: String) {
    let body = format!("{reason}\n");
    let _ = write!(
        client,
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    log_denied(shared, host, port, reason);
}

fn log_denied(shared: &Shared, host: &str, port: u16, reason: String) {
    if let Ok(mut log) = shared.log.lock() {
        log.push(Logged {
            host: host.to_string(),
            port,
            allowed: false,
            reason: Some(reason),
            up: Arc::new(AtomicU64::new(0)),
            down: Arc::new(AtomicU64::new(0)),
        });
    }
}

/// Reads one complete TLS handshake record (the ClientHello).
fn read_client_hello(client: &mut UnixStream) -> Result<Vec<u8>, String> {
    let mut header = [0_u8; 5];
    client
        .read_exact(&mut header)
        .map_err(|_| "no TLS ClientHello was sent".to_string())?;
    if header[0] != 0x16 || header[1] != 0x03 {
        return Err("only TLS is allowed through the tunnel".into());
    }
    let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
    if length == 0 || length > MAX_CLIENT_HELLO {
        return Err("TLS ClientHello record has an invalid length".into());
    }
    let mut record = header.to_vec();
    record.resize(5 + length, 0);
    client
        .read_exact(&mut record[5..])
        .map_err(|_| "TLS ClientHello was incomplete".to_string())?;
    Ok(record)
}

/// The SNI host name from a ClientHello record, if any.
fn server_name(record: &[u8]) -> Option<String> {
    let mut at = Cursor {
        data: record.get(5..)?,
        at: 0,
    };
    if at.u8()? != 0x01 {
        return None; // not a ClientHello
    }
    let length = at.u24()?;
    at.data = at.data.get(..at.at + length)?;
    at.skip(2 + 32)?; // legacy version, random
    let session = usize::from(at.u8()?);
    at.skip(session)?;
    let suites = at.u16()?;
    at.skip(suites)?;
    let compression = usize::from(at.u8()?);
    at.skip(compression)?;
    let extensions_end = at.u16()? + at.at;
    while at.at + 4 <= extensions_end {
        let (kind, size) = (at.u16()?, at.u16()?);
        if kind != 0 {
            at.skip(size)?;
            continue;
        }
        let _list = at.u16()?;
        if at.u8()? != 0 {
            return None; // name type must be host_name
        }
        let name_length = at.u16()?;
        let name = at.data.get(at.at..at.at + name_length)?;
        return String::from_utf8(name.to_vec()).ok();
    }
    None
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn u8(&mut self) -> Option<u8> {
        let value = *self.data.get(self.at)?;
        self.at += 1;
        Some(value)
    }
    fn u16(&mut self) -> Option<usize> {
        Some(usize::from(self.u8()?) << 8 | usize::from(self.u8()?))
    }
    fn u24(&mut self) -> Option<usize> {
        Some(usize::from(self.u8()?) << 16 | self.u16()?)
    }
    fn skip(&mut self, count: usize) -> Option<()> {
        (self.at + count <= self.data.len()).then(|| self.at += count)
    }
}

fn tunnel(
    client: UnixStream,
    upstream: TcpStream,
    shared: &Shared,
    up: &AtomicU64,
    down: &AtomicU64,
) {
    let (Ok(client_reader), Ok(upstream_writer)) = (client.try_clone(), upstream.try_clone())
    else {
        return;
    };
    let max = shared.policy.max_bytes;
    thread::scope(|scope| {
        scope.spawn(|| {
            pump(client_reader, &upstream_writer, &shared.bytes, max, up);
            let _ = upstream_writer.shutdown(Shutdown::Write);
        });
        pump(&upstream, &client, &shared.bytes, max, down);
        let _ = client.shutdown(Shutdown::Write);
    });
    let _ = upstream.shutdown(Shutdown::Both);
    let _ = client.shutdown(Shutdown::Both);
}

/// Copies until EOF, an error, or the run's byte budget is spent.
fn pump<R: Read, W: Write>(
    mut from: R,
    mut to: W,
    total: &AtomicU64,
    max: u64,
    counter: &AtomicU64,
) {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read as u64,
        };
        if total.fetch_add(read, Ordering::SeqCst) + read > max {
            return;
        }
        if to.write_all(&buffer[..read as usize]).is_err() {
            return;
        }
        counter.fetch_add(read, Ordering::SeqCst);
    }
}

/// Public unicast addresses only.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b)) // shared address space (CGNAT)
                || (a == 192 && b == 0 && v4.octets()[2] == 0)) // IETF protocol assignments
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(mapped));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80) // link local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn temp_socket(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("cc-egress-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// A local TCP echo server.
    fn echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut reader = stream.try_clone().unwrap();
                    let mut writer = stream;
                    let _ = io::copy(&mut reader, &mut writer);
                });
            }
        });
        port
    }

    fn policy(port: u16, allow_private: bool) -> EgressPolicy {
        EgressPolicy {
            hosts: BTreeSet::from(["LocalHost".to_string()]),
            ports: BTreeSet::from([port]),
            allow_private_destinations: allow_private,
            max_connections: 16,
            max_bytes: 1024 * 1024,
        }
    }

    /// A minimal TLS 1.2-style ClientHello carrying `sni`.
    pub(crate) fn client_hello(sni: &str) -> Vec<u8> {
        let name = sni.as_bytes();
        let mut sni_ext = Vec::new();
        sni_ext.extend(((name.len() + 3) as u16).to_be_bytes());
        sni_ext.push(0);
        sni_ext.extend((name.len() as u16).to_be_bytes());
        sni_ext.extend(name);
        let mut extensions = vec![0x00, 0x00];
        extensions.extend((sni_ext.len() as u16).to_be_bytes());
        extensions.extend(sni_ext);
        let mut body = vec![0x03, 0x03];
        body.extend([7_u8; 32]);
        body.push(0); // session id
        body.extend([0x00, 0x02, 0x13, 0x01]); // one cipher suite
        body.extend([0x01, 0x00]); // null compression
        body.extend((extensions.len() as u16).to_be_bytes());
        body.extend(extensions);
        let mut handshake = vec![0x01];
        handshake.extend(&(body.len() as u32).to_be_bytes()[1..]);
        handshake.extend(body);
        let mut record = vec![0x16, 0x03, 0x01];
        record.extend((handshake.len() as u16).to_be_bytes());
        record.extend(handshake);
        record
    }

    fn request(socket: &Path, target: &str) -> (String, UnixStream) {
        let mut stream = UnixStream::connect(socket).unwrap();
        write!(
            stream,
            "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
        )
        .unwrap();
        let mut status = Vec::new();
        let mut byte = [0_u8; 1];
        while !status.ends_with(b"\r\n") {
            if !matches!(stream.read(&mut byte), Ok(1)) {
                break;
            }
            status.push(byte[0]);
        }
        (String::from_utf8_lossy(&status).trim().to_string(), stream)
    }

    #[test]
    fn allowlisted_tunnels_work_and_are_counted() {
        let port = echo_server();
        let socket = temp_socket("ok");
        let proxy = EgressProxy::start(&socket, policy(port, true)).unwrap();
        let (status, mut stream) = request(&socket, &format!("localhost:{port}"));
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
        let mut blank = [0_u8; 2];
        stream.read_exact(&mut blank).unwrap(); // end of response head
        let hello = client_hello("localhost");
        stream.write_all(&hello).unwrap();
        let mut reply = vec![0_u8; hello.len()];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(reply, hello);
        drop(stream);
        thread::sleep(Duration::from_millis(100));
        let events = proxy.stop().events;
        assert_eq!(events.len(), 1);
        assert!(events[0].allowed);
        let n = hello.len() as u64;
        assert_eq!((events[0].bytes_up, events[0].bytes_down), (n, n));
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn everything_else_is_refused_and_logged() {
        let port = echo_server();
        let socket = temp_socket("deny");
        let proxy = EgressProxy::start(&socket, policy(port, true)).unwrap();
        for (target, expected) in [
            (
                "example.com:443".to_string(),
                "host is not on the allowlist",
            ),
            (format!("127.0.0.1:{port}"), "IP address destinations"),
            (format!("[::1]:{port}"), "IP address destinations"),
            ("localhost:1".to_string(), "port 1 is not allowed"),
        ] {
            let (status, mut stream) = request(&socket, &target);
            assert_eq!(status, "HTTP/1.1 403 Forbidden", "{target}");
            let mut rest = String::new();
            stream.read_to_string(&mut rest).unwrap();
            assert!(rest.contains(expected), "{target}: {rest}");
        }
        let mut plain = UnixStream::connect(&socket).unwrap();
        plain
            .write_all(b"GET http://localhost/ HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        plain.read_to_string(&mut response).unwrap();
        assert!(response.contains("only CONNECT"));
        let events = proxy.stop().events;
        assert_eq!(events.len(), 5);
        assert!(events.iter().all(|e| !e.allowed && e.reason.is_some()));
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn names_resolving_to_private_addresses_are_refused_by_default() {
        let port = echo_server();
        let socket = temp_socket("private");
        let proxy = EgressProxy::start(&socket, policy(port, false)).unwrap();
        let (status, mut stream) = request(&socket, &format!("localhost:{port}"));
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        let mut rest = String::new();
        stream.read_to_string(&mut rest).unwrap();
        assert!(rest.contains("non-public"));
        drop(proxy);
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn connection_limits_and_address_classes() {
        let port = echo_server();
        let socket = temp_socket("limit");
        let mut limited = policy(port, true);
        limited.max_connections = 1;
        let proxy = EgressProxy::start(&socket, limited).unwrap();
        let (first, _keep) = request(&socket, &format!("localhost:{port}"));
        assert_eq!(first, "HTTP/1.1 200 Connection Established");
        let (second, _) = request(&socket, &format!("localhost:{port}"));
        assert_eq!(second, "", "over-limit attempts are dropped unread");
        assert_eq!(proxy.stop().refused_over_limit, 1);
        let _ = std::fs::remove_file(socket);

        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn tunnels_require_tls_with_the_requested_server_name() {
        assert_eq!(
            server_name(&client_hello("api.example.test")).as_deref(),
            Some("api.example.test")
        );
        assert_eq!(server_name(b"\x16\x03\x01\x00\x01\x01"), None);
        let port = echo_server();
        let socket = temp_socket("sni");
        let proxy = EgressProxy::start(&socket, policy(port, true)).unwrap();
        for (payload, expected) in [
            (client_hello("evil.example"), "does not match"),
            (b"GET / HTTP/1.1\r\n\r\n".to_vec(), "only TLS"),
        ] {
            let (status, mut stream) = request(&socket, &format!("localhost:{port}"));
            assert_eq!(status, "HTTP/1.1 200 Connection Established");
            let mut blank = [0_u8; 2];
            stream.read_exact(&mut blank).unwrap();
            stream.write_all(&payload).unwrap();
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest);
            assert!(rest.is_empty(), "nothing may come back: {rest:?}");
            let _ = expected;
        }
        thread::sleep(Duration::from_millis(100));
        let events = proxy.stop().events;
        assert_eq!(events.len(), 2);
        assert!(events[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("does not match"));
        assert!(events[1].reason.as_deref().unwrap().contains("only TLS"));
        assert!(events.iter().all(|e| !e.allowed && e.bytes_up == 0));
        let _ = std::fs::remove_file(socket);
    }
}
