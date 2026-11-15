//! Fixed, integrity-checked TCP workloads for B-NETWORK-PROFILE-ENG.
//! Build the same static binary for the host server, native client and guest.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const BLOCK: usize = 64 * 1024;
const BULK: usize = 64 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);
const SCENARIOS: &[&str] = &[
    "connect",
    "rpc_fresh",
    "rpc_keepalive",
    "dns",
    "stream",
    "download_1",
    "upload_1",
    "download_8",
    "upload_8",
    "download_32",
    "upload_32",
];

fn pattern(position: usize, salt: u8) -> u8 {
    ((position.wrapping_mul(31)) ^ (position >> 8) ^ (position >> 16)) as u8 ^ salt
}

fn fill(bytes: &mut [u8], position: usize, salt: u8) {
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = pattern(position + index, salt);
    }
}

fn check(bytes: &[u8], position: usize, salt: u8) -> io::Result<()> {
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != pattern(position + index, salt) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload mismatch",
            ));
        }
    }
    Ok(())
}

fn send_payload(stream: &mut TcpStream, length: usize, salt: u8) -> io::Result<()> {
    let mut block = vec![0; BLOCK.min(length)];
    let mut position = 0;
    while position < length {
        let count = block.len().min(length - position);
        fill(&mut block[..count], position, salt);
        stream.write_all(&block[..count])?;
        position += count;
    }
    Ok(())
}

fn receive_payload(stream: &mut TcpStream, length: usize, salt: u8) -> io::Result<()> {
    let mut block = vec![0; BLOCK.min(length)];
    let mut position = 0;
    while position < length {
        let count = stream.read(&mut block[..BLOCK.min(length - position)])?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        check(&block[..count], position, salt)?;
        position += count;
    }
    Ok(())
}

fn configure(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    // The workload opts out of application Nagle in both host and guest. This
    // does not change the independent pVisor connector/stack socket defaults.
    stream.set_nodelay(true)
}

fn connect(address: SocketAddr) -> io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&address, DEADLINE)?;
    configure(&stream)?;
    Ok(stream)
}

fn header(stream: &mut TcpStream, op: u8, length: usize, repeats: u32, salt: u8) -> io::Result<()> {
    let mut bytes = [0; 16];
    bytes[0] = op;
    bytes[1] = salt;
    bytes[4..12].copy_from_slice(&(length as u64).to_be_bytes());
    bytes[12..16].copy_from_slice(&repeats.to_be_bytes());
    stream.write_all(&bytes)
}

fn serve_connection(mut stream: TcpStream) -> io::Result<()> {
    configure(&stream)?;
    let mut bytes = [0; 16];
    if let Err(error) = stream.read_exact(&mut bytes) {
        // A pure connect/close intentionally has no application header.
        if error.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(());
        }
        return Err(error);
    }
    let salt = bytes[1];
    let length = u64::from_be_bytes(bytes[4..12].try_into().unwrap()) as usize;
    let repeats = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
    if length == 0 || length > BULK || repeats == 0 || repeats > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request",
        ));
    }
    match bytes[0] {
        b'D' => send_payload(&mut stream, length, salt)?,
        b'U' => {
            receive_payload(&mut stream, length, salt)?;
            stream.write_all(b"VERIFIED")?;
        }
        b'E' if length <= BLOCK => {
            let mut body = vec![0; length];
            for _ in 0..repeats {
                stream.read_exact(&mut body)?;
                check(&body, 0, salt)?;
                stream.write_all(&body)?;
            }
        }
        b'S' if length == 64 && repeats == 32 => {
            for event in 0..repeats {
                send_payload(&mut stream, length, event as u8)?;
                if event + 1 < repeats {
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown request",
            ))
        }
    }
    stream.shutdown(Shutdown::Write)
}

fn server() -> io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    println!("READY {}", listener.local_addr()?.port());
    io::stdout().flush()?;
    for stream in listener.incoming() {
        let stream = stream?;
        thread::spawn(move || {
            if let Err(error) = serve_connection(stream) {
                eprintln!("SERVER_ERROR {error}");
                // A corrupt/failed server transfer invalidates the cohort even
                // if a client happens to exit successfully.
                std::process::exit(3);
            }
        });
    }
    Ok(())
}

fn dns_query(socket: &UdpSocket, id: u16) -> io::Result<Ipv4Addr> {
    let mut request = Vec::from(id.to_be_bytes());
    request.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in ["localhost", "localdomain"] {
        request.push(label.len() as u8);
        request.extend_from_slice(label.as_bytes());
    }
    request.extend_from_slice(&[0, 0, 1, 0, 1]);
    socket.send(&request)?;
    let mut response = [0; 512];
    let length = socket.recv(&mut response)?;
    if length != request.len() + 16
        || response[..2] != id.to_be_bytes()
        || response[2] & 0x80 == 0
        || response[3] & 0x0f != 0
        || response[4..12] != [0, 1, 0, 1, 0, 0, 0, 0]
        || response[12..request.len()] != request[12..]
        || response[length - 6..length - 4] != [0, 4]
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid DNS response",
        ));
    }
    let address = Ipv4Addr::new(
        response[length - 4],
        response[length - 3],
        response[length - 2],
        response[length - 1],
    );
    if address.octets()[..2] != [198, 18] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a synthetic address",
        ));
    }
    Ok(address)
}

fn dns_socket() -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_read_timeout(Some(DEADLINE))?;
    socket.set_write_timeout(Some(DEADLINE))?;
    socket.connect("192.0.2.1:53")?;
    Ok(socket)
}

fn result(
    name: &str,
    elapsed: u128,
    bytes: usize,
    operations: usize,
    samples: &[u128],
    first: u128,
) {
    println!(
        "NET_RESULT {{\"scenario\":\"{name}\",\"elapsed_ns\":{elapsed},\"bytes\":{bytes},\
         \"operations\":{operations},\"samples_ns\":{samples:?},\"first_byte_ns\":{first}}}"
    );
}

fn rpc(stream: &mut TcpStream) -> io::Result<()> {
    let mut body = [0; 1024];
    fill(&mut body, 0, 7);
    stream.write_all(&body)?;
    stream.read_exact(&mut body)?;
    check(&body, 0, 7)
}

fn run(name: &str, address: SocketAddr) -> io::Result<()> {
    let mut samples = Vec::new();
    let started = Instant::now();
    match name {
        "connect" => {
            for _ in 0..128 {
                let t = Instant::now();
                let stream = connect(address)?;
                samples.push(t.elapsed().as_nanos());
                drop(stream);
            }
            result(name, started.elapsed().as_nanos(), 0, 128, &samples, 0);
        }
        "rpc_fresh" => {
            for _ in 0..128 {
                let t = Instant::now();
                let mut stream = connect(address)?;
                header(&mut stream, b'E', 1024, 1, 7)?;
                rpc(&mut stream)?;
                samples.push(t.elapsed().as_nanos());
            }
            result(
                name,
                started.elapsed().as_nanos(),
                128 * 2048,
                128,
                &samples,
                0,
            );
        }
        "rpc_keepalive" => {
            let mut stream = connect(address)?;
            header(&mut stream, b'E', 1024, 256, 7)?;
            let t = Instant::now();
            for _ in 0..256 {
                let one = Instant::now();
                rpc(&mut stream)?;
                samples.push(one.elapsed().as_nanos());
            }
            result(name, t.elapsed().as_nanos(), 256 * 2048, 256, &samples, 0);
        }
        "dns" => {
            let socket = dns_socket()?;
            for id in 0..128 {
                let t = Instant::now();
                let resolved = dns_query(&socket, id)?;
                if resolved != address.ip() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unstable DNS mapping",
                    ));
                }
                samples.push(t.elapsed().as_nanos());
            }
            result(name, started.elapsed().as_nanos(), 0, 128, &samples, 0);
        }
        "stream" => {
            let mut stream = connect(address)?;
            header(&mut stream, b'S', 64, 32, 0)?;
            let mut body = [0; 64];
            let mut previous = started;
            let mut first = 0;
            for event in 0..32 {
                stream.read_exact(&mut body)?;
                check(&body, 0, event)?;
                let now = Instant::now();
                if event == 0 {
                    first = now.duration_since(started).as_nanos();
                } else {
                    samples.push(now.duration_since(previous).as_nanos());
                }
                previous = now;
            }
            result(
                name,
                started.elapsed().as_nanos(),
                32 * 64,
                32,
                &samples,
                first,
            );
        }
        name if name.starts_with("download_") || name.starts_with("upload_") => {
            let connections: usize = name.rsplit('_').next().unwrap().parse().unwrap();
            let upload = name.starts_with("upload_");
            let barrier = Arc::new(Barrier::new(connections + 1));
            let mut workers = Vec::new();
            for index in 0..connections {
                let barrier = barrier.clone();
                workers.push(thread::spawn(move || -> io::Result<()> {
                    // Always reach the barrier, including connection failure.
                    let connection = connect(address);
                    barrier.wait();
                    barrier.wait();
                    let mut stream = connection?;
                    let length = BULK / connections;
                    let salt = index as u8;
                    header(
                        &mut stream,
                        if upload { b'U' } else { b'D' },
                        length,
                        1,
                        salt,
                    )?;
                    if upload {
                        send_payload(&mut stream, length, salt)?;
                        let mut ack = [0; 8];
                        stream.read_exact(&mut ack)?;
                        if &ack != b"VERIFIED" {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid upload receipt",
                            ));
                        }
                    } else {
                        receive_payload(&mut stream, length, salt)?;
                    }
                    Ok(())
                }));
            }
            // First await every connection, then release the requests only
            // after starting the clock. No server can prefill data beforehand.
            barrier.wait();
            let t = Instant::now();
            barrier.wait();
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| io::Error::other("worker panic"))??;
            }
            result(name, t.elapsed().as_nanos(), BULK, connections, &[], 0);
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown scenario",
            ))
        }
    }
    io::stdout().flush()
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("server") {
        return server();
    }
    if args.len() != 5 || args[1] != "client" {
        return Err(io::Error::other(
            "usage: worker server | worker client native|vm PORT SCENARIOS",
        ));
    }
    let port = args[3].parse::<u16>().map_err(io::Error::other)?;
    let ip = match args[2].as_str() {
        "native" => Ipv4Addr::LOCALHOST,
        "vm" => dns_query(&dns_socket()?, 0xbeef)?,
        _ => return Err(io::Error::other("unknown client kind")),
    };
    let address = SocketAddr::from((ip, port));
    println!(
        "NET_READY {{\"kind\":\"{}\",\"address\":\"{address}\"}}",
        args[2]
    );
    for name in args[4].split(',') {
        if !SCENARIOS.contains(&name) {
            return Err(io::Error::other("unknown scenario"));
        }
        if name != "dns" || args[2] == "vm" {
            run(name, address)?;
        }
    }
    println!("NET_COMPLETE");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_detects_wrong_position_and_corruption() {
        let mut bytes = vec![0; BLOCK];
        fill(&mut bytes, 123, 7);
        check(&bytes, 123, 7).unwrap();
        assert!(check(&bytes, 124, 7).is_err());
        bytes[1024] ^= 1;
        assert!(check(&bytes, 123, 7).is_err());
    }

    #[test]
    fn duplex_payload_oracle_and_upload_receipt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_connection(stream).unwrap();
        });
        let mut stream = connect(address).unwrap();
        header(&mut stream, b'U', BLOCK + 17, 1, 13).unwrap();
        send_payload(&mut stream, BLOCK + 17, 13).unwrap();
        let mut receipt = [0; 8];
        stream.read_exact(&mut receipt).unwrap();
        assert_eq!(&receipt, b"VERIFIED");
        server.join().unwrap();
    }
}
