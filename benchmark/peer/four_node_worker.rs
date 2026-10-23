//! Static, dependency-free real-guest worker for B-FOUR-PEER-ENG.
//! Build: rustc --edition=2024 -O --target x86_64-unknown-linux-musl FILE -o netbench
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

const PORT: u16 = 9020;
const MAGIC: &[u8; 8] = b"PV4NET01";

#[derive(Clone, Copy)]
struct Request {
    mode: u8,
    source: u8,
    hops: u8,
    size: u32,
    count: u32,
    seed: u64,
}

impl Request {
    fn write(self, stream: &mut TcpStream) -> io::Result<()> {
        let mut bytes = [0; 28];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..11].copy_from_slice(&[self.mode, self.source, self.hops]);
        bytes[12..16].copy_from_slice(&self.size.to_be_bytes());
        bytes[16..20].copy_from_slice(&self.count.to_be_bytes());
        bytes[20..28].copy_from_slice(&self.seed.to_be_bytes());
        stream.write_all(&bytes)
    }
    fn read(stream: &mut TcpStream) -> io::Result<Self> {
        let mut bytes = [0; 28];
        stream.read_exact(&mut bytes)?;
        assert_eq!(&bytes[..8], MAGIC);
        let request = Self {
            mode: bytes[8],
            source: bytes[9],
            hops: bytes[10],
            size: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
            count: u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            seed: u64::from_be_bytes(bytes[20..28].try_into().unwrap()),
        };
        assert!((1..=4).contains(&request.mode));
        assert!(request.source < 4 && request.hops <= 3);
        assert!((8..=65536).contains(&request.size));
        assert!((1..=65536).contains(&request.count));
        Ok(request)
    }
}

fn connect(address: &str) -> TcpStream {
    let stream = TcpStream::connect((address, PORT)).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    stream
}

fn pattern(bytes: &mut [u8], seed: u64, sequence: u32) {
    let salt = seed ^ (u64::from(sequence).wrapping_mul(0x9e3779b97f4a7c15));
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = ((index as u64).wrapping_mul(37) ^ salt.rotate_right((index % 61) as u32)) as u8;
    }
    bytes[..8].copy_from_slice(&(seed ^ u64::from(sequence)).to_be_bytes());
}

fn checksum(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(1099511628211);
    }
    hash
}

fn serve_connection(mut stream: TcpStream, node: usize, peers: &[String]) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(40)))?;
    stream.set_write_timeout(Some(Duration::from_secs(40)))?;
    let request = Request::read(&mut stream)?;
    let mut relay = if request.mode == 3 && request.hops > 0 {
        let mut downstream = connect(&peers[(node + 1) % 4]);
        Request {
            hops: request.hops - 1,
            ..request
        }
        .write(&mut downstream)?;
        Some(downstream)
    } else {
        None
    };
    // Slow application reader, independent of all the other server threads.
    let mut bytes = vec![0; request.size as usize];
    let mut expected = bytes.clone();
    let mut hash = 14695981039346656037;
    for sequence in 0..request.count {
        stream.read_exact(&mut bytes)?;
        if request.mode == 4 && sequence == 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        pattern(&mut expected, request.seed, sequence);
        assert_eq!(
            bytes, expected,
            "payload corruption at node {node}, sequence {sequence}"
        );
        if matches!(request.mode, 2 | 4) {
            hash = checksum(hash, &bytes);
        } else {
            if let Some(next) = relay.as_mut() {
                next.write_all(&bytes)?;
                next.read_exact(&mut expected)?;
                assert_eq!(bytes, expected, "ring response corruption");
            }
            stream.write_all(&bytes)?;
        }
    }
    if matches!(request.mode, 2 | 4) {
        stream.write_all(&hash.to_be_bytes())?;
    }
    Ok(())
}

struct FlowResult {
    bytes: u64,
    seconds: f64,
    rtt_us: Vec<f64>,
}

fn flow(
    node: usize,
    target: usize,
    peers: &[String],
    request: Request,
    gate: Arc<Barrier>,
) -> FlowResult {
    let mut stream = connect(&peers[target]);
    request.write(&mut stream).unwrap();
    let mut payload = vec![0; request.size as usize];
    let mut response = payload.clone();
    let mut rtt_us = Vec::new();
    let mut hash = 14695981039346656037;
    gate.wait();
    let started = Instant::now();
    for sequence in 0..request.count {
        pattern(&mut payload, request.seed, sequence);
        let start = Instant::now();
        stream.write_all(&payload).unwrap();
        if matches!(request.mode, 2 | 4) {
            hash = checksum(hash, &payload);
        } else {
            stream.read_exact(&mut response).unwrap();
            assert_eq!(response, payload, "reply corruption at node {node}");
            rtt_us.push(start.elapsed().as_secs_f64() * 1e6);
        }
    }
    if matches!(request.mode, 2 | 4) {
        let mut response = [0; 8];
        stream.read_exact(&mut response).unwrap();
        assert_eq!(
            u64::from_be_bytes(response),
            hash,
            "server checksum mismatch"
        );
    }
    FlowResult {
        bytes: u64::from(request.size) * u64::from(request.count),
        seconds: started.elapsed().as_secs_f64(),
        rtt_us,
    }
}

fn run(node: usize, scenario: &str, peers: &[String]) {
    let (mode, size, count) = match scenario {
        "rpc" => (1, 256, 500),
        "bulk" => (2, 65536, 64),
        "ring" => (3, 256, 200),
        "slow" => (1, 256, 500),
        "healthy" => (1, 256, 500),
        _ => panic!("unknown scenario"),
    };
    let targets: Vec<_> = if scenario == "healthy" {
        vec![match node {
            0 => 2,
            2 => 3,
            3 => 0,
            _ => panic!("offline node"),
        }]
    } else if scenario == "ring" {
        vec![(node + 1) % 4]
    } else if scenario == "slow" {
        vec![(node + 2) % 4]
    } else {
        (0..4).filter(|target| *target != node).collect()
    };
    let gate = Arc::new(Barrier::new(
        targets.len() + 1 + usize::from(scenario == "slow" && node == 0),
    ));
    let mut tasks = Vec::new();
    for target in targets {
        let request = Request {
            mode,
            source: node as u8,
            hops: if mode == 3 { 3 } else { 0 },
            size,
            count,
            seed: 0x504e455400000000 | ((node as u64) << 16) | target as u64,
        };
        let peers = peers.to_vec();
        let gate = gate.clone();
        tasks.push(std::thread::spawn(move || {
            flow(node, target, &peers, request, gate)
        }));
    }
    let slow = if scenario == "slow" && node == 0 {
        let peers = peers.to_vec();
        let gate = gate.clone();
        Some(std::thread::spawn(move || {
            flow(
                node,
                1,
                &peers,
                Request {
                    mode: 4,
                    source: 0,
                    hops: 0,
                    size: 65536,
                    count: 128,
                    seed: 91,
                },
                gate,
            )
        }))
    } else {
        None
    };
    // Connections are established, but no payload is sent before the common GO.
    println!("READY {node} {scenario}");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "GO");
    let started = Instant::now();
    gate.wait();
    let results: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
    let healthy_seconds = started.elapsed().as_secs_f64();
    if let Some(task) = slow {
        let _ = task.join().unwrap();
    }
    let elapsed = started.elapsed().as_secs_f64();
    let bytes: u64 = results.iter().map(|result| result.bytes).sum();
    let rtts: Vec<_> = results
        .iter()
        .flat_map(|result| result.rtt_us.iter().copied())
        .collect();
    let flow_seconds: Vec<_> = results.iter().map(|result| result.seconds).collect();
    println!(
        "{{\"node\":{node},\"scenario\":\"{scenario}\",\"verified\":true,\"bytes\":{bytes},\"seconds\":{elapsed},\"healthy_seconds\":{healthy_seconds},\"flow_seconds\":{flow_seconds:?},\"rtt_us\":{rtts:?}}}"
    );
}

fn main() {
    let arguments: Vec<_> = std::env::args().collect();
    if arguments[1] == "resolve" {
        let addresses: Vec<_> = (arguments[2].as_str(), PORT)
            .to_socket_addrs()
            .unwrap()
            .collect();
        assert!(
            addresses
                .iter()
                .any(|address| address.ip().to_string() == arguments[3])
        );
        println!("RESOLVED {} {addresses:?}", arguments[2]);
        return;
    }
    if arguments[1] == "info" {
        for (name, path) in [
            ("MEMORY", "/proc/meminfo"),
            ("KERNEL", "/proc/version"),
            ("CGROUP", "/proc/self/cgroup"),
            ("ROUTE", "/proc/net/route"),
            ("MTU", "/sys/class/net/eth0/mtu"),
        ] {
            println!(
                "{name}:\n{}",
                std::fs::read_to_string(path).unwrap_or_default()
            );
        }
        return;
    }
    let node: usize = arguments[2].parse().unwrap();
    assert!(node < 4);
    let peers: Vec<_> = arguments[3].split(',').map(str::to_owned).collect();
    assert_eq!(peers.len(), 4);
    if arguments[1] == "serve" {
        let listener = TcpListener::bind(("0.0.0.0", PORT)).unwrap();
        println!("SERVER_READY {node}");
        io::stdout().flush().unwrap();
        for stream in listener.incoming() {
            let peers = peers.clone();
            std::thread::spawn(move || {
                if let Err(error) = serve_connection(stream.unwrap(), node, &peers) {
                    eprintln!("server node {node}: {error}");
                }
            });
        }
    } else {
        assert_eq!(arguments[1], "run");
        run(node, &arguments[4], &peers);
    }
}
