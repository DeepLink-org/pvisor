//! Fixed 64 MiB live allocation for the OpenSandbox memory comparison.
//! Blocks on a guest-local Unix socket; every observation checks every word.
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

const WORDS: usize = 64 * 1024 * 1024 / 8;

fn value(index: usize, seed: u64) -> u64 {
    let mut x = (index as u64)
        .wrapping_add(seed)
        .wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn verify(data: &[u64], seed: u64) -> String {
    let mut sum = 0u64;
    for (i, word) in std::hint::black_box(data).iter().enumerate() {
        assert_eq!(*word, value(i, seed), "live data changed at word {i}");
        sum = sum.wrapping_add(*word);
    }
    format!(
        "{{\"bytes\":{},\"seed\":{seed},\"sum\":\"{sum:016x}\",\"verified\":true}}\n",
        data.len() * 8
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args[1].as_str() {
        "client" => {
            let mut socket = UnixStream::connect(&args[2]).unwrap();
            writeln!(socket, "{}", args[3..].join(" ")).unwrap();
            let mut reply = String::new();
            socket.read_to_string(&mut reply).unwrap();
            assert!(
                reply.contains("\"verified\":true"),
                "invalid worker response: {reply}"
            );
            print!("{reply}");
        }
        "serve" => {
            let mut seed: u64 = args[4].parse().unwrap();
            let mut data: Vec<u64> = (0..WORDS).map(|i| value(i, seed)).collect();
            let listener = UnixListener::bind(&args[2]).unwrap();
            fs::write(&args[3], verify(&data, seed)).unwrap();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut command = String::new();
                BufReader::new(&stream).read_line(&mut command).unwrap();
                let command: Vec<_> = command.split_whitespace().collect();
                let _ = verify(&data, seed);
                match command[0] {
                    "check" | "quit" => {}
                    "unique" => {
                        seed = command[1].parse().unwrap();
                        for (i, word) in data.iter_mut().enumerate() {
                            *word = value(i, seed);
                        }
                    }
                    _ => panic!("unknown worker command"),
                }
                stream.write_all(verify(&data, seed).as_bytes()).unwrap();
                if command[0] == "quit" {
                    break;
                }
            }
            fs::remove_file(&args[2]).unwrap();
        }
        _ => panic!("serve or client required"),
    }
}
