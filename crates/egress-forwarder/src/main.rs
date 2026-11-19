//! cybercore-egress-forwarder --listen 127.0.0.1:3128 --socket /run/cybercore-egress.sock -- <cmd> [args...]
//!
//! Runs inside a sandbox that has no network interface besides loopback.
//! It binds a loopback TCP listener *before* starting the task, relays each
//! connection to the host egress proxy's Unix socket, runs the task with the
//! inherited environment and stdio, and exits with the task's status.
//!
//! It makes no policy decisions: every allow/deny happens in the host proxy.
//! It never resolves names, so it works as a statically linked binary.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode};
use std::thread;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (listen, socket, command) = match parse(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("cybercore-egress-forwarder: {message}");
            return ExitCode::from(125);
        }
    };
    let listener = match TcpListener::bind(listen) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cybercore-egress-forwarder: cannot listen on {listen}: {error}");
            return ExitCode::from(125);
        }
    };
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let socket = socket.clone();
            thread::spawn(move || relay(stream, &socket));
        }
    });
    let status = match Command::new(&command[0]).args(&command[1..]).status() {
        Ok(status) => status,
        Err(error) => {
            eprintln!(
                "cybercore-egress-forwarder: cannot start {}: {error}",
                command[0]
            );
            return ExitCode::from(127);
        }
    };
    match (status.code(), status.signal()) {
        (Some(code), _) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        (None, Some(signal)) => ExitCode::from(u8::try_from(128 + signal).unwrap_or(255)),
        _ => ExitCode::from(1),
    }
}

fn parse(args: &[String]) -> Result<(SocketAddrV4, String, Vec<String>), String> {
    let usage = "usage: --listen ADDR --socket PATH -- COMMAND [ARGS...]";
    let split = args.iter().position(|a| a == "--").ok_or(usage)?;
    let (options, command) = (&args[..split], &args[split + 1..]);
    if command.is_empty() {
        return Err(usage.into());
    }
    let mut listen = None;
    let mut socket = None;
    let mut iter = options.iter();
    while let Some(flag) = iter.next() {
        let value = iter.next().ok_or(usage)?.clone();
        match flag.as_str() {
            "--listen" => listen = Some(value),
            "--socket" => socket = Some(value),
            _ => return Err(usage.into()),
        }
    }
    let listen: SocketAddrV4 = listen
        .ok_or(usage)?
        .parse()
        .map_err(|_| "--listen must be IP:PORT".to_string())?;
    if !listen.ip().is_loopback() {
        return Err("--listen must be a loopback address".into());
    }
    Ok((listen, socket.ok_or(usage)?, command.to_vec()))
}

fn relay(tcp: TcpStream, socket: &str) {
    let Ok(unix) = UnixStream::connect(socket) else {
        let _ = tcp.shutdown(Shutdown::Both);
        return;
    };
    let (Ok(tcp_reader), Ok(unix_writer)) = (tcp.try_clone(), unix.try_clone()) else {
        return;
    };
    let upstream =
        thread::spawn(move || pump(tcp_reader, unix_writer, |w| w.shutdown(Shutdown::Write)));
    let _ = pump(unix, tcp, |w| w.shutdown(Shutdown::Write));
    let _ = upstream.join();
}

fn pump<R: Read, W: Write>(
    mut from: R,
    mut to: W,
    close: impl Fn(&W) -> io::Result<()>,
) -> io::Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = from.read(&mut buffer)?;
        if read == 0 {
            return close(&to);
        }
        to.write_all(&buffer[..read])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options_and_command() {
        let (listen, socket, command) = parse(&strings(&[
            "--listen",
            "127.0.0.1:3128",
            "--socket",
            "/run/x.sock",
            "--",
            "sh",
            "-c",
            "true",
        ]))
        .unwrap();
        assert_eq!(listen.to_string(), "127.0.0.1:3128");
        assert_eq!(socket, "/run/x.sock");
        assert_eq!(command, strings(&["sh", "-c", "true"]));
    }

    #[test]
    fn refuses_non_loopback_listeners_and_missing_parts() {
        assert!(parse(&strings(&[
            "--listen",
            "0.0.0.0:3128",
            "--socket",
            "/s",
            "--",
            "true"
        ]))
        .is_err());
        assert!(parse(&strings(&[
            "--listen",
            "127.0.0.1:1",
            "--socket",
            "/s",
            "--"
        ]))
        .is_err());
        assert!(parse(&strings(&["--listen", "127.0.0.1:1", "--", "true"])).is_err());
    }
}
