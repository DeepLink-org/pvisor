//! Integrity-checked, bounded-retry guest UDP probe for B-PEER-CHAOS-ENG.
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

const PORT: u16 = 9021;
const MAGIC: &[u8; 8] = b"PVUDP001";
const SIZES: [usize; 6] = [32, 64, 256, 512, 1200, 1472];
const COUNT: u32 = 180;

fn packet(source: u8, target: u8, token: u64, sequence: u32) -> Vec<u8> {
    let mut bytes = vec![0; SIZES[sequence as usize % SIZES.len()]];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (token.rotate_left((index % 64) as u32)
            ^ u64::from(sequence).wrapping_mul(37)
            ^ index as u64) as u8;
    }
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..16].copy_from_slice(&token.to_be_bytes());
    bytes[16..18].copy_from_slice(&[source, target]);
    bytes[18..22].copy_from_slice(&sequence.to_be_bytes());
    bytes
}

fn validate(bytes: &[u8], node: u8) -> (u8, u64, u32) {
    assert!(bytes.len() >= 32);
    assert_eq!(&bytes[..8], MAGIC);
    assert!(bytes[16] < 4 && bytes[16] != node);
    assert_eq!(bytes[17], node);
    let token = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let sequence = u32::from_be_bytes(bytes[18..22].try_into().unwrap());
    assert_eq!(bytes, packet(bytes[16], node, token, sequence));
    (bytes[16], token, sequence)
}

fn serve(node: u8) {
    let listener = TcpListener::bind(("0.0.0.0", PORT + 1)).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            std::thread::spawn(move || {
                let mut stream = stream.unwrap();
                stream.set_nodelay(true).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(40)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(40)))
                    .unwrap();
                loop {
                    let mut prefix = [0; 4];
                    match stream.read_exact(&mut prefix) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(error) => panic!("persistent TCP server: {error}"),
                    }
                    let size = u32::from_be_bytes(prefix) as usize;
                    assert!(SIZES.contains(&size));
                    let mut bytes = vec![0; size];
                    stream.read_exact(&mut bytes).unwrap();
                    validate(&bytes, node);
                    stream.write_all(&bytes).unwrap();
                }
            });
        }
    });
    let socket = UdpSocket::bind(("0.0.0.0", PORT)).unwrap();
    println!("UDP_READY {node}");
    io::stdout().flush().unwrap();
    let mut bytes = [0; 2048];
    loop {
        let (size, sender) = socket.recv_from(&mut bytes).unwrap();
        let (source, _, _) = validate(&bytes[..size], node);
        assert_eq!(
            sender.ip().to_string(),
            format!("10.231.70.{}", 11 + source)
        );
        assert_eq!(socket.send_to(&bytes[..size], sender).unwrap(), size);
    }
}

fn hold(node: u8, target: u8, peer: &str, seconds: u64) {
    let mut stream = TcpStream::connect((peer, PORT + 1)).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    println!("HOLD_READY {node}");
    io::stdout().flush().unwrap();
    let started = Instant::now();
    let (mut requests, mut bytes) = (0u32, 0u64);
    while started.elapsed() < Duration::from_secs(seconds) {
        let payload = packet(node, target, 20261024, requests);
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&payload).unwrap();
        let mut reply = vec![0; payload.len()];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(reply, payload);
        bytes += payload.len() as u64;
        requests += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "{{\"verified\":true,\"node\":{node},\"target\":{target},\"requests\":{requests},\"bytes\":{bytes},\"seconds\":{}}}",
        started.elapsed().as_secs_f64()
    );
}

fn probe(node: u8, target: u8, address: String, token: u64) -> (u64, u64, u64, u64) {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    socket.connect((address.as_str(), PORT)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let (mut attempts, mut timeouts, mut duplicates, mut payload) = (0, 0, 0, 0);
    let mut response = [0; 2048];
    for sequence in 0..COUNT {
        let bytes = packet(node, target, token, sequence);
        let mut verified = false;
        for _ in 0..8 {
            attempts += 1;
            assert_eq!(socket.send(&bytes).unwrap(), bytes.len());
            // At most seven late echoes from retries of each older request.
            // This receive loop is bounded even if a peer floods duplicates.
            for _ in 0..(COUNT * 8) {
                match socket.recv(&mut response) {
                    Ok(size) => {
                        assert!(size >= 32);
                        assert_eq!(response[16], node);
                        assert_eq!(response[17], target);
                        assert_eq!(
                            u64::from_be_bytes(response[8..16].try_into().unwrap()),
                            token
                        );
                        let received = u32::from_be_bytes(response[18..22].try_into().unwrap());
                        assert!(received <= sequence, "future UDP response");
                        assert_eq!(&response[..size], packet(node, target, token, received));
                        if received < sequence {
                            duplicates += 1;
                            continue;
                        }
                        verified = true;
                        break;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        timeouts += 1;
                        break;
                    }
                    Err(error) => panic!("UDP node {node} to {target}: {error}"),
                }
            }
            if verified {
                break;
            }
        }
        assert!(
            verified,
            "UDP node {node} to {target}, sequence {sequence}: retry budget exhausted"
        );
        payload += bytes.len() as u64;
    }
    (attempts, timeouts, duplicates, payload)
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let node: u8 = args[2].parse().unwrap();
    assert!(node < 4);
    if args[1] == "serve" {
        serve(node);
        return;
    }
    if args[1] == "hold" {
        hold(
            node,
            args[3].parse().unwrap(),
            &args[4],
            args[5].parse().unwrap(),
        );
        return;
    }
    assert_eq!(args[1], "run");
    let peers: Vec<String> = args[3].split(',').map(str::to_owned).collect();
    assert_eq!(peers.len(), 4);
    let token: u64 = args[4].parse().unwrap();
    let tasks: Vec<_> = (0..4u8)
        .filter(|target| *target != node)
        .map(|target| {
            let address = peers[target as usize].clone();
            std::thread::spawn(move || probe(node, target, address, token ^ u64::from(target)))
        })
        .collect();
    let (mut attempts, mut timeouts, mut duplicates, mut bytes) = (0, 0, 0, 0);
    for task in tasks {
        let result = task.join().unwrap();
        attempts += result.0;
        timeouts += result.1;
        duplicates += result.2;
        bytes += result.3;
    }
    println!(
        "{{\"verified\":true,\"node\":{node},\"requests\":{},\"attempts\":{attempts},\"timeouts\":{timeouts},\"duplicates\":{duplicates},\"bytes\":{bytes}}}",
        COUNT * 3
    );
}
