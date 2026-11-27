//! A host RLIMIT_FSIZE forces a short virtio-fs WRITE followed by EFBIG.

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::Path,
};

const LIMIT: u64 = 1 << 30;

fn handshake(step: &str) {
    println!("{step}");
    io::stdout().flush().unwrap();
    let mut go = [0; 1];
    io::stdin().read_exact(&mut go).unwrap();
    assert_eq!(go, *b"\n");
}

fn warm_overwrite(path: &Path) {
    let mut writer = OpenOptions::new().write(true).open(path).unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut bytes = [0; 12];
    reader.read_exact_at(&mut bytes, LIMIT - 8).unwrap();
    assert_eq!(&bytes, b"cold....keep");
    handshake("warm");
    writer.seek(SeekFrom::Start(LIMIT - 3)).unwrap();
    assert_eq!(writer.write(b"partial").unwrap(), 3);
    assert_eq!(writer.stream_position().unwrap(), LIMIT);
    assert_eq!(writer.write(b"!").unwrap_err().raw_os_error(), Some(27));
    assert_eq!(writer.stream_position().unwrap(), LIMIT);
    handshake("written");
    reader.read_exact_at(&mut bytes, LIMIT - 8).unwrap();
    assert_eq!(&bytes, b"HOST.parkeep");
    assert_eq!(reader.metadata().unwrap().len(), LIMIT + 4);
    writer.sync_all().unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args.len(), 3, "usage: write_ack write|verify|warm FILE");
    let path = &args[2];
    if args[1] == "warm" {
        return warm_overwrite(Path::new(path));
    }
    if args[1] == "write" {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        assert_eq!(file.write(b"full").unwrap(), 4);
        file.seek(SeekFrom::Start(LIMIT - 3)).unwrap();
        assert_eq!(file.write(b"partial").unwrap(), 3);
        assert_eq!(file.stream_position().unwrap(), LIMIT);
        assert_eq!(file.write(b"!").unwrap_err().raw_os_error(), Some(27)); // EFBIG
        assert_eq!(file.stream_position().unwrap(), LIMIT);
        file.sync_all().unwrap();
    } else {
        assert_eq!(args[1], "verify");
    }
    let file = File::open(path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), LIMIT);
    let mut bytes = [0xff; 5];
    file.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(&bytes, b"full\0");
    assert_eq!(file.read_at(&mut bytes, LIMIT - 4).unwrap(), 4);
    assert_eq!(&bytes[..4], b"\0par");
}
