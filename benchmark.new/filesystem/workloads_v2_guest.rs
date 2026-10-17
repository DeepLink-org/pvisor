//! B-FS-ENG filesystem V2: one case per fresh environment, resident rounds.
//! Usage: workloads_v2_guest [--verify] CASE WORK_ROOT FIXTURE_ROOT ROUNDS [RAM_MIB].
//! Ready is emitted before preparation. Setup/validation are outside task clocks.
//! File sync and checked close are retained; completion excludes validation/exit.
//! Fixture: meta/f00000000.bin..f00002047.bin (64 bytes), sentinel.bin (64),
//! read.bin (4 MiB), all fixture bytes follow fill_pattern(FIXTURE_SEED, offset).
//! Resource cases reuse exactly four upper files, write 2*configured RAM total,
//! and hold at most 1 MiB of worker payload buffers. Validation streams bytes.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

#[path = "workloads_v2_scenarios.rs"]
mod scenarios;

unsafe extern "C" {
    fn close(fd: i32) -> i32;
}

pub const FIXTURE_SEED: u64 = 0x4653563246495854;
const MIB: u64 = 1024 * 1024;
const GOLDEN: u64 = 0x9e3779b97f4a7c15;

#[derive(Default, Debug)]
pub struct Row {
    pub setup_ns: u128,
    pub work_ns: u128,
    pub final_sync_ns: u128,
    pub completed_ns: u128,
    pub validation_ns: u128,
    pub foreground_read_ns: u128,
    pub bytes_written: u64,
    pub bytes_read: u64,
    pub write_ops: u64,
    pub read_ops: u64,
    pub metadata_ops: u64,
    pub periodic_fsyncs: u64,
    pub final_fsyncs: u64,
    pub checked_closes: u64,
    pub files_verified: u64,
    pub bytes_verified: u64,
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
pub fn checked(condition: bool, message: impl Into<String>) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}
pub fn checked_close(file: File) -> io::Result<()> {
    if unsafe { close(file.into_raw_fd()) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn sync_dir(path: &Path) -> io::Result<()> {
    let directory = File::open(path)?;
    directory.sync_all()?;
    checked_close(directory)
}
pub fn fill_pattern(seed: u64, offset: u64, bytes: &mut [u8]) {
    let mut filled = 0;
    while filled < bytes.len() {
        let absolute = offset + filled as u64;
        let mut word = seed.wrapping_add((absolute / 8).wrapping_mul(GOLDEN));
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
        word ^= word >> 31;
        let lane = (absolute % 8) as usize;
        let count = (8 - lane).min(bytes.len() - filled);
        bytes[filled..filled + count].copy_from_slice(&word.to_le_bytes()[lane..lane + count]);
        filled += count;
    }
}
fn seed(id: &str, round: usize, file: usize) -> u64 {
    let hash = id.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    hash ^ (round as u64).wrapping_mul(0xd1b54a32d192ed03) ^ (file as u64 + 1).wrapping_mul(GOLDEN)
}
fn regular(path: &Path, length: u64) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    checked(
        metadata.is_file() && metadata.len() == length && metadata.ino() != 0,
        format!("wrong regular file/length: {}", path.display()),
    )
}
fn write_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    checked(file.write_at(bytes, offset)? == bytes.len(), "short pwrite")
}
fn read_pattern(
    file: &File,
    offset: u64,
    observed: &mut [u8],
    expected: &mut [u8],
    pattern_seed: u64,
) -> io::Result<()> {
    checked(
        file.read_at(observed, offset)? == observed.len(),
        "short pread",
    )?;
    fill_pattern(pattern_seed, offset, expected);
    checked(
        observed == expected,
        format!("wrong bytes at offset {offset}"),
    )
}
fn verify_pattern(path: &Path, length: u64, pattern_seed: u64) -> io::Result<()> {
    regular(path, length)?;
    let file = File::open(path)?;
    let mut observed = vec![0; 64 * 1024];
    let mut expected = vec![0; 64 * 1024];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(observed.len() as u64) as usize;
        read_pattern(
            &file,
            offset,
            &mut observed[..count],
            &mut expected[..count],
            pattern_seed,
        )?;
        offset += count as u64;
    }
    checked_close(file)
}
fn data_path(root: &Path, index: usize) -> PathBuf {
    root.join(format!("data-{index:02}.bin"))
}
fn progress(id: &str, round: usize, bytes: u64, elapsed_ns: u128) {
    println!(
        "WORKLOADS_V2_PROGRESS {{\"id\":\"{id}\",\"round\":{round},\"worker\":0,\"scope\":\"worker_submission\",\"bytes_written\":{bytes},\"elapsed_ns\":{elapsed_ns}}}"
    );
    let _ = io::stdout().flush();
}
fn prepare_directory(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)
}

fn metadata_case(id: &str, root: &Path, fixture: &Path, round: usize) -> io::Result<Row> {
    let mut row = Row::default();
    let setup = Instant::now();
    prepare_directory(root)?;
    let meta = fixture.join("meta");
    row.setup_ns = setup.elapsed().as_nanos();
    let start = Instant::now();
    match id {
        "M01" => {
            for index in 0..2048 {
                let path = meta.join(format!("f{index:08}.bin"));
                regular(&path, 64)?;
                row.metadata_ops += 1;
            }
        }
        "M02" => {
            for index in 0..2048 {
                let result = fs::symlink_metadata(meta.join(format!("missing{index:08}.bin")));
                match result {
                    Err(error) if error.raw_os_error() == Some(2) => (),
                    Err(error) => return Err(error),
                    Ok(_) => return Err(invalid("negative lookup unexpectedly found path")),
                }
                row.metadata_ops += 1;
            }
        }
        "M03" => {
            let mut names = BTreeSet::new();
            for entry in fs::read_dir(&meta)? {
                let entry = entry?;
                checked(entry.file_type()?.is_file(), "enumeration found non-file")?;
                checked(names.insert(entry.file_name()), "duplicate directory entry")?;
                row.metadata_ops += 1;
            }
            let expected: BTreeSet<_> = (0..2048).map(|i| format!("f{i:08}.bin").into()).collect();
            checked(names == expected, "directory inventory differs")?;
        }
        _ => unreachable!(),
    }
    row.work_ns = start.elapsed().as_nanos();
    row.completed_ns = row.work_ns;
    let verify = Instant::now();
    verify_pattern(&fixture.join("sentinel.bin"), 64, FIXTURE_SEED)?;
    row.validation_ns = verify.elapsed().as_nanos();
    row.files_verified = 1;
    row.bytes_verified = 64;
    let _ = round;
    Ok(row)
}

fn read_case(id: &str, root: &Path, fixture: &Path, round: usize) -> io::Result<Row> {
    let mut row = Row::default();
    let setup = Instant::now();
    prepare_directory(root)?;
    let mut observed = vec![0; if id == "M04" { 65536 } else { 4096 }];
    let mut expected = vec![0; observed.len()];
    row.setup_ns = setup.elapsed().as_nanos();
    let path = fixture.join("read.bin");
    let passes = if id == "M04" { 3 } else { 1 };
    let mut pass_times = Vec::new();
    let start = Instant::now();
    for pass in 0..passes {
        let pass_start = Instant::now();
        let file = File::open(&path)?;
        checked(
            file.metadata()?.len() == 4 * MIB,
            "read fixture size differs",
        )?;
        let blocks = (4 * MIB) / observed.len() as u64;
        for index in 0..blocks {
            // Odd multiplication is a bijection on this power-of-two block set.
            let block = if id == "M05" {
                (index * 405 + 113) % blocks
            } else {
                index
            };
            read_pattern(
                &file,
                block * observed.len() as u64,
                &mut observed,
                &mut expected,
                FIXTURE_SEED,
            )?;
            row.bytes_read += observed.len() as u64;
            row.read_ops += 1;
        }
        checked_close(file)?;
        row.checked_closes += 1;
        pass_times.push((pass + 1, pass_start.elapsed().as_nanos()));
    }
    row.work_ns = start.elapsed().as_nanos();
    row.completed_ns = row.work_ns;
    row.files_verified = passes;
    row.bytes_verified = row.bytes_read;
    for (pass, elapsed_ns) in pass_times {
        eprintln!(
            "WORKLOADS_V2_READ_PASS {{\"id\":\"{id}\",\"round\":{round},\"pass\":{pass},\"elapsed_ns\":{elapsed_ns}}}"
        );
    }
    Ok(row)
}

fn write_case(id: &str, root: &Path, round: usize) -> io::Result<Row> {
    let mut row = Row::default();
    let setup = Instant::now();
    prepare_directory(root)?;
    let path = data_path(root, 0);
    let length = if id == "M10" { MIB } else { 4 * MIB };
    let block = match id {
        "M06" => 1024,
        "M07" => MIB as usize,
        _ => 4096,
    };
    if id == "M08" && round == 1 {
        let initial = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        initial.set_len(length)?;
        initial.sync_all()?;
        checked_close(initial)?;
        sync_dir(root)?;
    }
    let mut bytes = vec![0; block];
    row.setup_ns = setup.elapsed().as_nanos();
    let start = Instant::now();
    let file = OpenOptions::new()
        .create(id != "M08")
        .truncate(id != "M08")
        .write(true)
        .open(&path)?;
    let blocks = length / block as u64;
    for index in 0..blocks {
        let block_index = if id == "M08" {
            (index * 405 + 113) % blocks
        } else {
            index
        };
        let offset = block_index * block as u64;
        fill_pattern(seed(id, round, 0), offset, &mut bytes);
        write_at(&file, &bytes, offset)?;
        row.bytes_written += block as u64;
        row.write_ops += 1;
        if id == "M10" {
            file.sync_all()?;
            row.periodic_fsyncs += 1;
        }
    }
    row.work_ns = start.elapsed().as_nanos();
    let sync = Instant::now();
    file.sync_all()?;
    checked_close(file)?;
    sync_dir(root)?;
    row.final_sync_ns = sync.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    row.final_fsyncs = 2;
    row.checked_closes = 2;
    let verify = Instant::now();
    verify_pattern(&path, length, seed(id, round, 0))?;
    row.validation_ns = verify.elapsed().as_nanos();
    row.files_verified = 1;
    row.bytes_verified = length;
    Ok(row)
}

fn smallfiles_case(root: &Path, round: usize) -> io::Result<Row> {
    let mut row = Row::default();
    let setup = Instant::now();
    prepare_directory(root)?;
    let mut bytes = vec![0; 65536];
    row.setup_ns = setup.elapsed().as_nanos();
    let start = Instant::now();
    for index in 0..256 {
        fill_pattern(seed("M09", round, index), 0, &mut bytes);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(data_path(root, index))?;
        write_at(&file, &bytes, 0)?;
        checked_close(file)?;
        row.write_ops += 1;
        row.bytes_written += bytes.len() as u64;
        row.checked_closes += 1;
    }
    row.work_ns = start.elapsed().as_nanos();
    let sync = Instant::now();
    for index in 0..256 {
        let file = OpenOptions::new()
            .write(true)
            .open(data_path(root, index))?;
        file.sync_all()?;
        checked_close(file)?;
        row.final_fsyncs += 1;
        row.checked_closes += 1;
    }
    sync_dir(root)?;
    row.final_fsyncs += 1;
    row.checked_closes += 1;
    row.final_sync_ns = sync.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    let verify = Instant::now();
    for index in 0..256 {
        verify_pattern(&data_path(root, index), 65536, seed("M09", round, index))?;
    }
    checked(
        fs::read_dir(root)?.count() == 256,
        "smallfile inventory differs",
    )?;
    row.validation_ns = verify.elapsed().as_nanos();
    row.files_verified = 256;
    row.bytes_verified = row.bytes_written;
    Ok(row)
}

fn mixed_case(root: &Path, fixture: &Path, round: usize) -> io::Result<Row> {
    let mut row = Row::default();
    let setup = Instant::now();
    prepare_directory(root)?;
    let barrier = Arc::new(Barrier::new(3));
    let reader_barrier = Arc::clone(&barrier);
    let writer_barrier = Arc::clone(&barrier);
    let read_path = fixture.join("read.bin");
    let write_path = data_path(root, 0);
    let reader = thread::spawn(move || -> io::Result<u128> {
        let mut observed = vec![0; 4096];
        let mut expected = vec![0; 4096];
        reader_barrier.wait();
        let start = Instant::now();
        let file = File::open(read_path)?;
        for offset in (0..4 * MIB).step_by(4096) {
            read_pattern(&file, offset, &mut observed, &mut expected, FIXTURE_SEED)?;
        }
        checked_close(file)?;
        Ok(start.elapsed().as_nanos())
    });
    let writer = thread::spawn(move || -> io::Result<File> {
        let mut bytes = vec![0; 4096];
        writer_barrier.wait();
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(write_path)?;
        for offset in (0..4 * MIB).step_by(4096) {
            fill_pattern(seed("M11", round, 0), offset, &mut bytes);
            write_at(&file, &bytes, offset)?;
        }
        Ok(file)
    });
    row.setup_ns = setup.elapsed().as_nanos();
    let start = Instant::now();
    barrier.wait();
    let read_result = reader.join().map_err(|_| invalid("reader panicked"))?;
    let write_result = writer.join().map_err(|_| invalid("writer panicked"))?;
    row.foreground_read_ns = read_result?;
    let file = write_result?;
    row.work_ns = start.elapsed().as_nanos();
    let sync = Instant::now();
    file.sync_all()?;
    checked_close(file)?;
    sync_dir(root)?;
    row.final_sync_ns = sync.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    row.final_fsyncs = 2;
    row.checked_closes = 3;
    row.bytes_read = 4 * MIB;
    row.bytes_written = 4 * MIB;
    row.read_ops = 1024;
    row.write_ops = 1024;
    let verify = Instant::now();
    verify_pattern(&data_path(root, 0), 4 * MIB, seed("M11", round, 0))?;
    row.validation_ns = verify.elapsed().as_nanos();
    row.files_verified = 2;
    row.bytes_verified = 8 * MIB;
    Ok(row)
}

fn namespace_case(root: &Path, fixture: &Path, round: usize) -> io::Result<Row> {
    let mut row = Row::default();
    let setup = Instant::now();
    prepare_directory(root)?;
    let sentinel = root.join("sentinel.bin");
    if round == 1 {
        let mut bytes = [0; 64];
        fill_pattern(FIXTURE_SEED, 0, &mut bytes);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sentinel)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        checked_close(file)?;
        sync_dir(root)?;
    }
    let mut bytes = vec![0; 4096];
    let mut observed = vec![0; 4096];
    let mut expected = vec![0; 4096];
    row.setup_ns = setup.elapsed().as_nanos();
    let start = Instant::now();
    for index in 0..128 {
        let old = root.join(format!("temp-{index:04}.bin"));
        let renamed = root.join(format!("renamed-{index:04}.bin"));
        let file = OpenOptions::new().write(true).create_new(true).open(&old)?;
        fill_pattern(seed("M12", round, index), 0, &mut bytes);
        write_at(&file, &bytes, 0)?;
        file.sync_all()?;
        checked_close(file)?;
        fs::rename(&old, &renamed)?;
        regular(&renamed, 4096)?;
        let read_file = File::open(&renamed)?;
        read_pattern(
            &read_file,
            0,
            &mut observed,
            &mut expected,
            seed("M12", round, index),
        )?;
        checked_close(read_file)?;
        checked(!old.try_exists()?, "old namespace survived rename")?;
        fs::remove_file(&renamed)?;
        row.bytes_written += 4096;
        row.write_ops += 1;
        row.bytes_read += 4096;
        row.read_ops += 1;
        row.periodic_fsyncs += 1;
        row.checked_closes += 2;
        row.metadata_ops += 3; // create, rename, unlink; oracle stats are separate.
    }
    row.work_ns = start.elapsed().as_nanos();
    let sync = Instant::now();
    sync_dir(root)?;
    row.final_sync_ns = sync.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    row.final_fsyncs = 1;
    row.checked_closes += 1;
    let verify = Instant::now();
    verify_pattern(&sentinel, 64, FIXTURE_SEED)?;
    verify_pattern(&fixture.join("sentinel.bin"), 64, FIXTURE_SEED)?;
    let names = fs::read_dir(root)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    checked(
        names.len() == 1 && names[0] == "sentinel.bin",
        "namespace deletion inventory differs",
    )?;
    row.validation_ns = verify.elapsed().as_nanos();
    row.files_verified = 130;
    row.bytes_verified = 128 * 4096 + 128;
    Ok(row)
}

fn resource_parameters(id: &str, memory_mib: u64) -> io::Result<(usize, u64, u64)> {
    checked(
        (16..=32768).contains(&memory_mib) && memory_mib % 16 == 0,
        "resource RAM must be 16..32768 MiB and divisible by 16",
    )?;
    let workers = if id == "R02" || id == "R04" { 4 } else { 1 };
    let numerator = if id == "R03" || id == "R04" { 5 } else { 1 };
    Ok((
        workers,
        memory_mib * MIB * numerator / 4,
        memory_mib * MIB * 2,
    ))
}
fn resource_case(id: &str, root: &Path, round: usize, memory_mib: u64) -> io::Result<Row> {
    let mut row = Row::default();
    let (workers, working_set, total) = resource_parameters(id, memory_mib)?;
    let per_file = working_set / 4;
    let setup = Instant::now();
    prepare_directory(root)?;
    if round == 1 {
        for index in 0..4 {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(data_path(root, index))?;
            file.set_len(per_file)?;
            file.sync_all()?;
            checked_close(file)?;
        }
        sync_dir(root)?;
    }
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut handles = Vec::new();
    for worker in 0..workers {
        let root = root.to_path_buf();
        let id = id.to_owned();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> io::Result<Vec<File>> {
            // Exactly four files in both worker variants. One worker interleaves
            // files; four workers own one each. Buffer sum never exceeds 1 MiB.
            let indices: Vec<_> = if workers == 1 {
                (0..4).collect()
            } else {
                vec![worker]
            };
            let mut bytes = vec![0; (MIB as usize / workers).min(4096)];
            let mut files = Vec::new();
            barrier.wait();
            for index in &indices {
                files.push(
                    OpenOptions::new()
                        .write(true)
                        .open(data_path(&root, *index))?,
                );
            }
            let ops = total / 4096 / workers as u64;
            let file_blocks = per_file / 4096;
            let submitted_start = Instant::now();
            for index in 0..ops {
                let local_file = index as usize % indices.len();
                let file_index = indices[local_file];
                let offset = (index / indices.len() as u64 % file_blocks) * 4096;
                fill_pattern(seed(&id, round, file_index), offset, &mut bytes);
                write_at(&files[local_file], &bytes, offset)?;
                if worker == 0 && (index + 1) % (64 * MIB / 4096) == 0 {
                    // Worker-0 progress, not a claim of globally applied bytes.
                    progress(
                        &id,
                        round,
                        (index + 1) * 4096,
                        submitted_start.elapsed().as_nanos(),
                    );
                }
            }
            Ok(files)
        }));
    }
    row.setup_ns = setup.elapsed().as_nanos();
    let start = Instant::now();
    barrier.wait();
    let mut files = Vec::new();
    let mut failure = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(mut result)) => files.append(&mut result),
            Ok(Err(error)) => {
                failure.get_or_insert(error);
            }
            Err(_) => {
                failure.get_or_insert_with(|| invalid("resource worker panicked"));
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    row.work_ns = start.elapsed().as_nanos();
    let sync = Instant::now();
    for file in files {
        file.sync_all()?;
        checked_close(file)?;
    }
    sync_dir(root)?;
    row.final_sync_ns = sync.elapsed().as_nanos();
    row.completed_ns = start.elapsed().as_nanos();
    row.bytes_written = total;
    row.write_ops = total / 4096;
    row.final_fsyncs = 5;
    row.checked_closes = 5;
    let verify = Instant::now();
    for index in 0..4 {
        verify_pattern(&data_path(root, index), per_file, seed(id, round, index))?;
    }
    checked(
        fs::read_dir(root)?.count() == 4,
        "resource file inventory differs",
    )?;
    row.validation_ns = verify.elapsed().as_nanos();
    row.files_verified = 4;
    row.bytes_verified = working_set;
    Ok(row)
}

fn verify_final(
    id: &str,
    root: &Path,
    fixture: &Path,
    round: usize,
    memory_mib: u64,
) -> io::Result<()> {
    match id {
        "M01" | "M02" | "M03" => {
            verify_pattern(&fixture.join("sentinel.bin"), 64, FIXTURE_SEED)?;
            let expected: BTreeSet<_> = (0..2048).map(|i| format!("f{i:08}.bin").into()).collect();
            let mut actual = BTreeSet::new();
            for entry in fs::read_dir(fixture.join("meta"))? {
                let entry = entry?;
                verify_pattern(&entry.path(), 64, FIXTURE_SEED)?;
                actual.insert(entry.file_name());
            }
            checked(actual == expected, "fixture metadata inventory differs")
        }
        "M04" | "M05" => verify_pattern(&fixture.join("read.bin"), 4 * MIB, FIXTURE_SEED),
        "M06" | "M07" | "M08" | "M10" | "M11" => {
            let length = if id == "M10" { MIB } else { 4 * MIB };
            verify_pattern(&data_path(root, 0), length, seed(id, round, 0))?;
            checked(
                fs::read_dir(root)?.count() == 1,
                "stream file inventory differs",
            )?;
            if id == "M11" {
                verify_pattern(&fixture.join("read.bin"), 4 * MIB, FIXTURE_SEED)?;
            }
            Ok(())
        }
        "M09" => {
            for index in 0..256 {
                verify_pattern(&data_path(root, index), 65536, seed(id, round, index))?;
            }
            checked(
                fs::read_dir(root)?.count() == 256,
                "smallfiles inventory differs",
            )
        }
        "M12" => {
            verify_pattern(&root.join("sentinel.bin"), 64, FIXTURE_SEED)?;
            checked(
                fs::read_dir(root)?.count() == 1,
                "deleted namespace was retained",
            )
        }
        "R01" | "R02" | "R03" | "R04" => {
            let (_, working_set, _) = resource_parameters(id, memory_mib)?;
            for index in 0..4 {
                verify_pattern(
                    &data_path(root, index),
                    working_set / 4,
                    seed(id, round, index),
                )?;
            }
            checked(
                fs::read_dir(root)?.count() == 4,
                "resource inventory differs",
            )
        }
        "S01" | "S02" | "S03" => scenarios::verify_scenario(id, root, fixture, round),
        _ => Err(invalid("unknown case ID")),
    }
}

fn print_row(id: &str, round: usize, row: &Row) -> io::Result<()> {
    // Keep the actual end-to-end clock; include the boundary instructions in
    // the final-sync interval so the printed partition sums exactly.
    checked(
        row.completed_ns >= row.work_ns,
        "completion precedes work return",
    )?;
    let final_sync_ns = row.completed_ns - row.work_ns;
    println!(
        concat!(
            "WORKLOADS_V2_RESULT {{\"event\":\"round\",\"correctness\":true,\"id\":\"{}\",\"round\":{},\"setup_ns\":{},",
            "\"work_ns\":{},\"final_sync_ns\":{},\"completed_ns\":{},\"validation_ns\":{},",
            "\"foreground_read_ns\":{},\"bytes_written\":{},\"bytes_read\":{},",
            "\"write_ops\":{},\"read_ops\":{},\"metadata_ops\":{},\"periodic_fsyncs\":{},",
            "\"final_fsyncs\":{},\"checked_closes\":{},\"files_verified\":{},\"bytes_verified\":{}}}"
        ),
        id,
        round,
        row.setup_ns,
        row.work_ns,
        final_sync_ns,
        row.completed_ns,
        row.validation_ns,
        row.foreground_read_ns,
        row.bytes_written,
        row.bytes_read,
        row.write_ops,
        row.read_ops,
        row.metadata_ops,
        row.periodic_fsyncs,
        row.final_fsyncs,
        row.checked_closes,
        row.files_verified,
        row.bytes_verified
    );
    io::stdout().flush()
}

fn run() -> io::Result<()> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let verify = args.first().is_some_and(|a| a == "--verify");
    if verify {
        args.remove(0);
    }
    checked(
        args.len() == 4 || args.len() == 5,
        "usage: workloads_v2_guest [--verify] CASE WORK FIXTURE ROUNDS [RAM_MIB]",
    )?;
    let id = &args[0];
    checked(
        matches!(
            id.as_str(),
            "M01"
                | "M02"
                | "M03"
                | "M04"
                | "M05"
                | "M06"
                | "M07"
                | "M08"
                | "M09"
                | "M10"
                | "M11"
                | "M12"
                | "R01"
                | "R02"
                | "R03"
                | "R04"
                | "S01"
                | "S02"
                | "S03"
        ),
        "unknown case ID",
    )?;
    let root = Path::new(&args[1]);
    let fixture = Path::new(&args[2]);
    let rounds: usize = args[3]
        .parse()
        .map_err(|_| invalid("invalid round count"))?;
    checked((1..=10).contains(&rounds), "round count must be 1..10")?;
    let memory_mib = args
        .get(4)
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|_| invalid("invalid memory MiB"))?
        .unwrap_or(512);
    checked(root != fixture, "work and fixture paths must differ")?;
    if verify {
        verify_final(id, root, fixture, rounds, memory_mib)?;
        println!(
            "WORKLOADS_V2_VERIFIED {{\"id\":\"{id}\",\"final_round\":{rounds},\"ok\":true,\"verified\":true}}"
        );
        return Ok(());
    }
    let memtotal = fs::read_to_string("/proc/meminfo")?
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<u64>().ok())
        })
        .ok_or_else(|| invalid("MemTotal missing from /proc/meminfo"))?;
    println!("WORKLOADS_V2_READY {{\"actual_guest_memtotal_kib\":{memtotal}}}");
    io::stdout().flush()?;
    let initial_setup = Instant::now();
    if id.starts_with('S') {
        scenarios::prepare_scenario(id, root, fixture)?;
    }
    let scenario_setup_ns = initial_setup.elapsed().as_nanos();
    for round in 1..=rounds {
        let mut row = match id.as_str() {
            "M01" | "M02" | "M03" => metadata_case(id, root, fixture, round)?,
            "M04" | "M05" => read_case(id, root, fixture, round)?,
            "M06" | "M07" | "M08" | "M10" => write_case(id, root, round)?,
            "M09" => smallfiles_case(root, round)?,
            "M11" => mixed_case(root, fixture, round)?,
            "M12" => namespace_case(root, fixture, round)?,
            "R01" | "R02" | "R03" | "R04" => resource_case(id, root, round, memory_mib)?,
            _ => scenarios::run_scenario(id, root, fixture, round)?,
        };
        if id.starts_with('S') && round == 1 {
            row.setup_ns += scenario_setup_ns;
        }
        print_row(id, round, &row)?;
    }
    println!("WORKLOADS_V2_DONE {{\"id\":\"{id}\",\"rounds\":{rounds},\"correctness\":true}}");
    io::stdout().flush()?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("WORKLOADS_V2_ERROR: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pattern_is_offset_stable_across_unaligned_chunks() {
        let mut full = [0; 100];
        fill_pattern(FIXTURE_SEED, 0, &mut full);
        let mut tail = [0; 71];
        fill_pattern(FIXTURE_SEED, 3, &mut tail);
        assert_eq!(&full[3..74], &tail);
    }
    #[test]
    fn resource_controls_hold_work_and_files_fixed() {
        assert_eq!(
            resource_parameters("R01", 512).unwrap(),
            (1, 128 * MIB, 1024 * MIB)
        );
        assert_eq!(
            resource_parameters("R02", 512).unwrap(),
            (4, 128 * MIB, 1024 * MIB)
        );
        assert_eq!(
            resource_parameters("R03", 512).unwrap(),
            (1, 640 * MIB, 1024 * MIB)
        );
        assert_eq!(
            resource_parameters("R04", 512).unwrap(),
            (4, 640 * MIB, 1024 * MIB)
        );
        assert!(resource_parameters("R01", 17).is_err());
    }
    #[test]
    fn random_read_and_write_visit_each_block_once() {
        let permutation: BTreeSet<_> = (0..1024).map(|i| (i * 405 + 113) % 1024).collect();
        assert_eq!(permutation.len(), 1024);
    }
}
