//! B-FS-ENG: identical POSIX consumers of a stable share or a guest-private copy.

use std::{
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
    time::Instant,
};

const FILES: usize = 128;
const BYTES: usize = 32 * 1024;
const SHARE: &str = "/readonly";
const LOCAL: &str = "/dev/shm/read-input";
const SPARSE: [usize; 4] = [0, 43, 85, 127];

fn require(ok: bool, message: &str) -> io::Result<()> {
    ok.then_some(())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, message))
}

fn mode(index: usize) -> u32 {
    if index % 2 == 0 { 0o444 } else { 0o440 }
}

fn check_mount(mounts: &str, path: &str, kind: &str, access: &str) -> io::Result<()> {
    require(
        mounts.lines().any(|line| {
            let mut fields = line.split_whitespace().skip(1);
            fields.next() == Some(path)
                && fields.next() == Some(kind)
                && fields
                    .next()
                    .is_some_and(|v| v.split(',').any(|f| f == access))
        }),
        &format!("expected {kind} {access} mount at {path}: {mounts}"),
    )
}

fn hydrate(pack: &Path, local: &Path) -> io::Result<()> {
    let meta = pack.symlink_metadata()?;
    require(
        meta.is_file() && meta.len() == (FILES * (2 + BYTES)) as u64,
        "bad pack framing",
    )?;
    let mut input = BufReader::with_capacity(256 * 1024, File::open(pack)?);
    fs::DirBuilder::new().mode(0o700).create(local)?;
    let mut part = vec![0; BYTES];
    for i in 0..FILES {
        let mut header = [0; 2];
        input.read_exact(&mut header)?;
        let permissions = u16::from_le_bytes(header);
        require(matches!(permissions, 0o440 | 0o444), "bad pack mode")?;
        input.read_exact(&mut part)?;
        File::options()
            .create_new(true)
            .write(true)
            .mode(permissions.into())
            .open(local.join(format!("part-{i}")))?
            .write_all(&part)?;
    }
    require(input.read(&mut [0])? == 0, "extra pack data")
}

fn check_names(root: &Path) -> io::Result<()> {
    let mut seen = [false; FILES];
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        let index = name
            .to_str()
            .and_then(|s| s.strip_prefix("part-"))
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&i| i < FILES)
            .ok_or_else(|| io::Error::other("unexpected entry"))?;
        require(
            name == format!("part-{index}").as_str() && !seen[index],
            "bad name",
        )?;
        seen[index] = true;
    }
    require(seen.into_iter().all(|found| found), "missing file")
}

fn read_part(root: &Path, index: usize, buf: &mut [u8]) -> io::Result<u64> {
    let path = root.join(format!("part-{index}"));
    let meta = path.symlink_metadata()?;
    require(
        meta.is_file() && meta.len() == BYTES as u64 && meta.mode() & 0o7777 == mode(index),
        &format!("bad type, length or permissions: {path:?}"),
    )?;
    let mut file = File::open(path)?;
    file.read_exact(buf)?;
    require(file.read(&mut [0])? == 0, "expected EOF")?;
    let mut sum = 0;
    for (j, block) in buf.chunks_exact(1024).enumerate() {
        let want = (31 * index + j) as u8;
        require(block.iter().all(|&b| b == want), "incorrect input byte")?;
        sum += u64::from(block[0]);
    }
    Ok(sum)
}

/// Returns the number of successful file reads and the sum of their block labels.
fn workload(root: &Path, broad: bool, passes: usize) -> io::Result<(usize, u64)> {
    let mut buf = vec![0; BYTES];
    let mut files = 0;
    let mut sum = 0;
    for _ in 0..passes {
        if broad {
            check_names(root)?;
        }
        for i in 0..FILES {
            if broad || SPARSE.contains(&i) {
                sum += read_part(root, i, &mut buf)?;
                files += 1;
            }
        }
    }
    Ok((files, sum))
}

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    require(args.len() == 2, "expected direct|pack sparse|broad")?;
    let packed = args[0] == "pack";
    let broad = args[1] == "broad";
    require(
        (packed || args[0] == "direct") && (broad || args[1] == "sparse"),
        "bad arm or case",
    )?;
    let mounts = fs::read_to_string("/proc/self/mounts")?;
    check_mount(&mounts, SHARE, "virtiofs", "ro")?;
    check_mount(&mounts, "/dev/shm", "tmpfs", "rw")?;
    let root = Path::new(if packed { LOCAL } else { SHARE });
    let start = Instant::now();
    let import_ns = if packed {
        hydrate(&Path::new(SHARE).join("input.pack"), root)?;
        start.elapsed().as_nanos()
    } else {
        0
    };
    let start = Instant::now();
    let (files, sum) = workload(root, broad, if broad { 4 } else { 1 })?;
    println!(
        "RESULT {import_ns} {} {files} {sum}",
        start.elapsed().as_nanos()
    );
    io::stdout().flush()?;

    // The host must see the answer before any untimed full-tree audit begins.
    let mut ack = [0];
    io::stdin().read_exact(&mut ack)?;
    require(ack == *b"\n", "expected host acknowledgement")?;
    let (files, sum) = workload(root, true, 1)?;
    println!("AUDIT {files} {sum}");
    Ok(())
}
