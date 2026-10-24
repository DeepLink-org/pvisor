//! Small application fixture: atomic file replacement must give a whole old or
//! new HTTP body. An open file and its metadata refer to the same inode.
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn serve(mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(3)))?;
    client.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut header = Vec::with_capacity(1024);
    let mut buffer = [0; 1024];
    while !header.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = client.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        header.extend_from_slice(&buffer[..count]);
        if header.len() > 8192 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    if !header.starts_with(b"GET /state HTTP/1.") {
        client.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    }
    let mut file = File::open("/srv/state")?;
    let length = file.metadata()?.len();
    write!(
        client,
        "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )?;
    let copied = io::copy(&mut file, &mut client)?;
    if copied != length {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let server = TcpListener::bind("0.0.0.0:8080")?;
    for client in server.incoming() {
        let client = client?;
        std::thread::spawn(move || {
            // Deliberately abandoned/reset clients are part of the workload.
            // Per-connection I/O failure must not terminate the listener.
            let _ = serve(client);
        });
    }
    Ok(())
}
