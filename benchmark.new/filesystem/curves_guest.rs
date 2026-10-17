//! Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), phase-clock curves.
//! One standalone Linux binary is used unchanged by native and VM conditions.
//! Usage: curves_guest [--verify] PLAN.tsv WORKSPACE [ROUNDS=3]. No fixture is prepared
//! before CURVES_READY. All outputs remain for independent parent verification.
//!
//! Plan, without a header: id<TAB>stream|smallfiles|read<TAB>block_bytes<TAB>
//! workers<TAB>bytes_per_worker<TAB>sync_every_ops (0 means final sync only).
//! Read cells require sync_every_ops=0 and reuse their prepared inode across
//! rounds. Preparations warm the host; these are not cold-disk measurements.
//!
//! Pattern: FNV1a64(id), offset basis 0xcbf29ce484222325, multiplier
//! 0x100000001b3. seed = id_hash XOR ((worker+1)*0x9e3779b97f4a7c15)
//! XOR (pattern_round*0xd1b54a32d192ed03), all arithmetic modulo 2^64.
//! For byte offset x, take byte x%8 of little-endian mix(seed+(x/8)*GOLDEN),
//! where mix applies xor>>30,*0xbf58476d1ce4e5b9,xor>>27,*0x94d049bb133111eb,
//! xor>>31. Writes use 1-based round; reads use pattern_round=0.
//! Smallfile fNNNNNNNN.bin holds the block-sized slice at N*block_bytes.

use std::collections::BTreeSet;
use std::ffi::c_ulong;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

unsafe extern "C" {
    fn dup(fd: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn ioctl(fd: i32, request: c_ulong, ...) -> i32;
}

const GOLDEN: u64 = 0x9e3779b97f4a7c15;
const ROUND_KEY: u64 = 0xd1b54a32d192ed03;
const MAX_CELL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILES: u64 = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Workload {
    Stream,
    Smallfiles,
    Read,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Smallfiles => "smallfiles",
            Self::Read => "read",
        }
    }
}

#[derive(Clone)]
struct Cell {
    id: String,
    workload: Workload,
    block: usize,
    workers: usize,
    bytes: usize,
    sync_every: usize,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn checked(condition: bool, message: impl Into<String>) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

fn number(text: &str) -> io::Result<usize> {
    checked(
        !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
        "plan integers must contain ASCII decimal digits only",
    )?;
    text.parse().map_err(|_| invalid("plan integer overflow"))
}

fn read_plan(path: &Path) -> io::Result<Vec<Cell>> {
    let text = fs::read_to_string(path)?;
    checked(text.len() <= 1024 * 1024, "plan exceeds 1 MiB")?;
    let mut cells = Vec::new();
    let mut ids = BTreeSet::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        checked(
            fields.len() == 6,
            format!("line {} needs six TSV columns", line_number + 1),
        )?;
        let id = fields[0];
        checked(
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "cell ID must be 1..64 path-safe ASCII characters",
        )?;
        checked(ids.insert(id.to_owned()), "duplicate cell ID")?;
        let workload = match fields[1] {
            "stream" => Workload::Stream,
            "smallfiles" => Workload::Smallfiles,
            "read" => Workload::Read,
            _ => return Err(invalid("workload must be stream, smallfiles or read")),
        };
        let cell = Cell {
            id: id.to_owned(),
            workload,
            block: number(fields[2])?,
            workers: number(fields[3])?,
            bytes: number(fields[4])?,
            sync_every: number(fields[5])?,
        };
        checked(
            (1..=16 * 1024 * 1024).contains(&cell.block),
            "block must be 1..16 MiB bytes",
        )?;
        checked((1..=64).contains(&cell.workers), "workers must be 1..64")?;
        let total = (cell.bytes as u64).checked_mul(cell.workers as u64);
        checked(
            cell.bytes != 0 && total.is_some_and(|n| n <= MAX_CELL_BYTES),
            "cell must process 1..512 MiB across all workers",
        )?;
        checked(
            workload != Workload::Read || cell.sync_every == 0,
            "read cells require sync_every_ops=0",
        )?;
        checked(
            workload != Workload::Smallfiles
                || (cell.bytes.div_ceil(cell.block) as u64) * cell.workers as u64 <= MAX_FILES,
            "smallfiles cell exceeds 65536 output files",
        )?;
        cells.push(cell);
        checked(cells.len() <= 256, "plan exceeds 256 cells")?;
    }
    checked(!cells.is_empty(), "empty plan")?;
    Ok(cells)
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn seed(cell: &Cell, worker: usize, round: usize) -> u64 {
    fnv(cell.id.as_bytes())
        ^ (worker as u64 + 1).wrapping_mul(GOLDEN)
        ^ (round as u64).wrapping_mul(ROUND_KEY)
}

fn pattern(seed: u64, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    for (index, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut word = seed.wrapping_add((index as u64).wrapping_mul(GOLDEN));
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
        word ^= word >> 31;
        chunk.copy_from_slice(&word.to_le_bytes()[..chunk.len()]);
    }
    bytes
}

fn checked_close(file: File) -> io::Result<()> {
    // Never retry close after an error: the descriptor may already be closed.
    if unsafe { close(file.into_raw_fd()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn applied_close(file: &File) -> io::Result<()> {
    let duplicate = unsafe { dup(file.as_raw_fd()) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { close(duplicate) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn exact_write(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    let written = file.write_at(bytes, offset)?;
    if written != bytes.len() {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "short pwrite"));
    }
    Ok(())
}

fn verify_file(path: &Path, expected: &[u8]) -> io::Result<()> {
    checked(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "output must be a regular non-symlink file",
    )?;
    let file = File::open(path)?;
    checked(
        file.metadata()?.len() == expected.len() as u64,
        "wrong output file length",
    )?;
    let mut buffer = vec![0; expected.len().min(64 * 1024)];
    let mut offset = 0;
    while offset < expected.len() {
        let count = buffer.len().min(expected.len() - offset);
        checked(
            file.read_at(&mut buffer[..count], offset as u64)? == count,
            "short verification read",
        )?;
        checked(
            buffer[..count] == expected[offset..offset + count],
            "output bytes differ",
        )?;
        offset += count;
    }
    checked_close(file)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Accepted,
    Applied,
    Sync,
    Close,
    Verify,
}

#[derive(Default)]
struct Stats {
    write_ops: u64,
    read_ops: u64,
    bytes_written: u64,
    bytes_read: u64,
    files_created: u64,
    accepted_closes: u64,
    periodic_fsyncs: u64,
    periodic_fsync_worker_ns_sum: u128,
    applied_barriers: u64,
    final_fsyncs: u64,
    final_closes: u64,
    files_verified: u64,
    bytes_verified: u64,
}

impl Stats {
    fn add(&mut self, other: Self) {
        self.write_ops += other.write_ops;
        self.read_ops += other.read_ops;
        self.bytes_written += other.bytes_written;
        self.bytes_read += other.bytes_read;
        self.files_created += other.files_created;
        self.accepted_closes += other.accepted_closes;
        self.periodic_fsyncs += other.periodic_fsyncs;
        self.periodic_fsync_worker_ns_sum += other.periodic_fsync_worker_ns_sum;
        self.applied_barriers += other.applied_barriers;
        self.final_fsyncs += other.final_fsyncs;
        self.final_closes += other.final_closes;
        self.files_verified += other.files_verified;
        self.bytes_verified += other.bytes_verified;
    }
}

struct Job {
    cell: Cell,
    directory: PathBuf,
    data: Vec<u8>,
    observed: Vec<u8>,
    file: Option<File>,
}

impl Job {
    fn prepare(cell: &Cell, worker: usize, round: usize, directory: PathBuf) -> io::Result<Self> {
        let pattern_round = if cell.workload == Workload::Read {
            0
        } else {
            round
        };
        let data = pattern(seed(cell, worker, pattern_round), cell.bytes);
        let path = directory.join("data.bin");
        let file = match cell.workload {
            Workload::Smallfiles => {
                fs::create_dir(&directory)?;
                None
            }
            Workload::Stream => {
                fs::create_dir(&directory)?;
                Some(
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(path)?,
                )
            }
            Workload::Read => {
                if round == 1 {
                    fs::create_dir(&directory)?;
                    let file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    for (index, bytes) in data.chunks(1024 * 1024).enumerate() {
                        exact_write(&file, bytes, (index * 1024 * 1024) as u64)?;
                    }
                    file.sync_all()?;
                    checked_close(file)?;
                    let parent = File::open(&directory)?;
                    parent.sync_all()?;
                    checked_close(parent)?;
                }
                Some(File::open(path)?)
            }
        };
        let observed = if cell.workload == Workload::Read {
            vec![0; cell.bytes]
        } else {
            Vec::new()
        };
        Ok(Self {
            cell: cell.clone(),
            directory,
            data,
            observed,
            file,
        })
    }

    fn small_path(&self, index: usize) -> PathBuf {
        self.directory.join(format!("f{index:08}.bin"))
    }

    fn execute(&mut self, phase: Phase) -> io::Result<Stats> {
        let mut stats = Stats::default();
        match phase {
            Phase::Accepted => {
                for (index, bytes) in self.data.chunks(self.cell.block).enumerate() {
                    let offset = index * self.cell.block;
                    if self.cell.workload == Workload::Read {
                        let count = self.file.as_ref().unwrap().read_at(
                            &mut self.observed[offset..offset + bytes.len()],
                            offset as u64,
                        )?;
                        checked(count == bytes.len(), "short measured pread")?;
                        stats.read_ops += 1;
                        stats.bytes_read += count as u64;
                        continue;
                    }
                    let small = if self.cell.workload == Workload::Smallfiles {
                        stats.files_created += 1;
                        Some(
                            OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(self.small_path(index))?,
                        )
                    } else {
                        None
                    };
                    let file = small.as_ref().or(self.file.as_ref()).unwrap();
                    exact_write(file, bytes, if small.is_some() { 0 } else { offset as u64 })?;
                    stats.write_ops += 1;
                    stats.bytes_written += bytes.len() as u64;
                    if self.cell.sync_every != 0 && (index + 1) % self.cell.sync_every == 0 {
                        let started = Instant::now();
                        file.sync_all()?;
                        stats.periodic_fsync_worker_ns_sum += started.elapsed().as_nanos();
                        stats.periodic_fsyncs += 1;
                    }
                    if let Some(file) = small {
                        checked_close(file)?;
                        stats.accepted_closes += 1;
                    }
                }
            }
            Phase::Applied => {
                if self.cell.workload == Workload::Stream {
                    applied_close(self.file.as_ref().unwrap())?;
                    stats.applied_barriers = 1;
                }
            }
            Phase::Sync => match self.cell.workload {
                Workload::Read => {}
                Workload::Stream => {
                    self.file.as_ref().unwrap().sync_all()?;
                    stats.final_fsyncs = 1;
                }
                Workload::Smallfiles => {
                    for index in 0..self.data.len().div_ceil(self.cell.block) {
                        // These files closed in the accepted phase. Reopen,
                        // sync and checked-close costs all belong to this phase.
                        let file = File::open(self.small_path(index))?;
                        file.sync_all()?;
                        stats.final_fsyncs += 1;
                        checked_close(file)?;
                        stats.final_closes += 1;
                    }
                }
            },
            Phase::Close => {
                if let Some(file) = self.file.take() {
                    checked_close(file)?;
                    stats.final_closes = 1;
                }
            }
            Phase::Verify => {
                if self.cell.workload == Workload::Smallfiles {
                    for (index, bytes) in self.data.chunks(self.cell.block).enumerate() {
                        verify_file(&self.small_path(index), bytes)?;
                        stats.files_verified += 1;
                    }
                    checked(
                        fs::read_dir(&self.directory)?.count()
                            == self.data.len().div_ceil(self.cell.block),
                        "unexpected smallfile output member",
                    )?;
                } else {
                    if self.cell.workload == Workload::Read {
                        checked(self.observed == self.data, "measured read bytes differ")?;
                    }
                    verify_file(&self.directory.join("data.bin"), &self.data)?;
                    stats.files_verified = 1;
                }
                stats.bytes_verified = self.data.len() as u64;
            }
        }
        Ok(stats)
    }
}

struct Command {
    generation: u64,
    phase: Phase,
    stopping: bool,
}

type Control = Arc<(Mutex<Command>, Condvar)>;

struct Event {
    worker: usize,
    phase: Option<Phase>,
    result: Result<Stats, String>,
}

struct Team {
    control: Control,
    receiver: mpsc::Receiver<Event>,
    threads: Vec<JoinHandle<()>>,
    workers: usize,
}

impl Team {
    fn new(jobs: Vec<Job>) -> io::Result<Self> {
        let workers = jobs.len();
        let (sender, receiver) = mpsc::channel();
        let mut team = Self {
            control: Arc::new((
                Mutex::new(Command {
                    generation: 0,
                    phase: Phase::Accepted,
                    stopping: false,
                }),
                Condvar::new(),
            )),
            receiver,
            threads: Vec::new(),
            workers,
        };
        for (worker, mut job) in jobs.into_iter().enumerate() {
            let control = team.control.clone();
            let events = sender.clone();
            let failure = sender.clone();
            team.threads.push(
                thread::Builder::new()
                    .name(format!("curve-{worker}"))
                    .spawn(move || {
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                events
                                    .send(Event {
                                        worker,
                                        phase: None,
                                        result: Ok(Stats::default()),
                                    })
                                    .map_err(|_| "coordinator gone".to_owned())?;
                                let mut generation = 0;
                                loop {
                                    let phase = {
                                        let (lock, ready) = &*control;
                                        let mut state = lock
                                            .lock()
                                            .map_err(|_| "command lock poisoned".to_owned())?;
                                        while state.generation == generation && !state.stopping {
                                            state = ready
                                                .wait(state)
                                                .map_err(|_| "command lock poisoned".to_owned())?;
                                        }
                                        if state.stopping {
                                            return Ok::<(), String>(());
                                        }
                                        generation = state.generation;
                                        state.phase
                                    };
                                    let stats = job
                                        .execute(phase)
                                        .map_err(|error| format!("{phase:?}: {error}"))?;
                                    events
                                        .send(Event {
                                            worker,
                                            phase: Some(phase),
                                            result: Ok(stats),
                                        })
                                        .map_err(|_| "coordinator gone".to_owned())?;
                                }
                            }));
                        let error = match outcome {
                            Ok(Ok(())) => return,
                            Ok(Err(error)) => error,
                            Err(_) => "worker panicked".to_owned(),
                        };
                        let _ = failure.send(Event {
                            worker,
                            phase: None,
                            result: Err(error),
                        });
                    })?,
            );
        }
        drop(sender);
        team.collect(None)?;
        Ok(team)
    }

    fn collect(&self, phase: Option<Phase>) -> io::Result<Stats> {
        let mut seen = vec![false; self.workers];
        let mut stats = Stats::default();
        for _ in 0..self.workers {
            let event = self
                .receiver
                .recv()
                .map_err(|_| invalid("worker channel closed"))?;
            let result = event
                .result
                .map_err(|error| invalid(format!("worker {} failed: {error}", event.worker)))?;
            checked(
                event.worker < self.workers && !seen[event.worker] && event.phase == phase,
                "unexpected/duplicate worker completion",
            )?;
            seen[event.worker] = true;
            stats.add(result);
        }
        Ok(stats)
    }

    fn phase(&self, phase: Phase) -> io::Result<Stats> {
        {
            let (lock, ready) = &*self.control;
            let mut state = lock.lock().map_err(|_| invalid("command lock poisoned"))?;
            state.phase = phase;
            state.generation += 1;
            ready.notify_all();
        }
        self.collect(Some(phase))
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        let (lock, ready) = &*self.control;
        {
            let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
            state.stopping = true;
            ready.notify_all();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

struct Markers(Option<File>);

impl Markers {
    fn open() -> io::Result<Self> {
        match std::env::var_os("CURVES_PROFILE") {
            None => Ok(Self(None)),
            Some(value) if value == "0" => Ok(Self(None)),
            Some(value) if value == "1" => Ok(Self(Some(File::open("/")?))),
            Some(_) => Err(invalid("CURVES_PROFILE must be 0 or 1")),
        }
    }

    fn checkpoint(&self, token: u64) -> io::Result<()> {
        if let Some(file) = &self.0 {
            let token = token.to_le();
            if unsafe {
                ioctl(
                    file.as_raw_fd(),
                    0x4008_76f1 as c_ulong,
                    &token as *const u64,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

fn quote(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch.is_control() => result.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn emit(prefix: &str, fields: &[(&str, String)]) -> io::Result<()> {
    let object = fields
        .iter()
        .map(|(key, value)| format!("{}:{value}", quote(key)))
        .collect::<Vec<_>>()
        .join(",");
    println!("{prefix} {{{object}}}");
    io::stdout().flush()
}

fn run_cell(
    cell: &Cell,
    index: usize,
    cell_count: usize,
    round: usize,
    workspace: &Path,
    markers: &Markers,
) -> io::Result<()> {
    let preparation = Instant::now();
    let cell_dir = workspace.join("curves-output").join(&cell.id);
    if round == 1 {
        fs::create_dir(&cell_dir)?;
    }
    let name = if cell.workload == Workload::Read {
        "readset".to_owned()
    } else {
        format!("r{round:02}")
    };
    let round_dir = cell_dir.join(&name);
    if round == 1 || cell.workload != Workload::Read {
        fs::create_dir(&round_dir)?;
    }
    let jobs = (0..cell.workers)
        .map(|worker| Job::prepare(cell, worker, round, round_dir.join(format!("w{worker:03}"))))
        .collect::<io::Result<Vec<_>>>()?;
    let team = Team::new(jobs)?;
    let preparation_ns = preparation.elapsed().as_nanos();
    let token = (2 * ((round - 1) * cell_count + index) + 1) as u64;
    markers.checkpoint(token)?;
    let started = Instant::now();
    let mut stats = team.phase(Phase::Accepted)?;
    let accepted = Instant::now();
    stats.add(team.phase(Phase::Applied)?);
    let applied = Instant::now();
    stats.add(team.phase(Phase::Sync)?);
    let synced = Instant::now();
    stats.add(team.phase(Phase::Close)?);
    let closed = Instant::now();
    let mut directory_fsyncs = 0;
    if cell.workload != Workload::Read {
        let mut directories = (0..cell.workers)
            .map(|worker| round_dir.join(format!("w{worker:03}")))
            .collect::<Vec<_>>();
        directories.extend([
            round_dir.clone(),
            cell_dir,
            workspace.join("curves-output"),
            workspace.to_owned(),
        ]);
        for directory in directories {
            let file = File::open(directory)?;
            file.sync_all()?;
            checked_close(file)?;
            directory_fsyncs += 1;
        }
    }
    let finished = Instant::now();
    markers.checkpoint(token + 1)?;
    let verification = Instant::now();
    stats.add(team.phase(Phase::Verify)?);
    let verification_ns = verification.elapsed().as_nanos();
    let expected = cell.bytes as u64 * cell.workers as u64;
    checked(
        stats.bytes_verified == expected,
        "incomplete byte verification",
    )?;
    checked(
        stats.bytes_written + stats.bytes_read == expected,
        "wrong measured byte count",
    )?;
    let pattern_round = if cell.workload == Workload::Read {
        0
    } else {
        round
    };
    let seeds = (0..cell.workers)
        .map(|worker| seed(cell, worker, pattern_round).to_string())
        .collect::<Vec<_>>()
        .join(",");
    let fields = vec![
        ("schema", "1".into()),
        ("id", quote(&cell.id)),
        ("workload", quote(cell.workload.name())),
        ("round", round.to_string()),
        ("block_bytes", cell.block.to_string()),
        ("workers", cell.workers.to_string()),
        ("bytes_per_worker", cell.bytes.to_string()),
        ("sync_every_ops", cell.sync_every.to_string()),
        ("bytes", expected.to_string()),
        ("write_ops", stats.write_ops.to_string()),
        ("read_ops", stats.read_ops.to_string()),
        ("bytes_written", stats.bytes_written.to_string()),
        ("bytes_read", stats.bytes_read.to_string()),
        ("files_created_in_clock", stats.files_created.to_string()),
        ("accepted_closes", stats.accepted_closes.to_string()),
        ("periodic_fsyncs", stats.periodic_fsyncs.to_string()),
        (
            "periodic_fsync_worker_ns_sum",
            stats.periodic_fsync_worker_ns_sum.to_string(),
        ),
        ("applied_barriers", stats.applied_barriers.to_string()),
        ("final_fsyncs", stats.final_fsyncs.to_string()),
        ("final_closes", stats.final_closes.to_string()),
        ("directory_fsyncs", directory_fsyncs.to_string()),
        (
            "accepted_ns",
            accepted.duration_since(started).as_nanos().to_string(),
        ),
        (
            "applied_ns",
            applied.duration_since(accepted).as_nanos().to_string(),
        ),
        (
            "fsync_ns",
            synced.duration_since(applied).as_nanos().to_string(),
        ),
        (
            "close_ns",
            closed.duration_since(synced).as_nanos().to_string(),
        ),
        (
            "dirsync_ns",
            finished.duration_since(closed).as_nanos().to_string(),
        ),
        (
            "total_ns",
            finished.duration_since(started).as_nanos().to_string(),
        ),
        ("preparation_ns", preparation_ns.to_string()),
        ("verification_ns", verification_ns.to_string()),
        (
            "output_dir",
            quote(&format!("curves-output/{}/{name}", cell.id)),
        ),
        (
            "files_per_worker",
            if cell.workload == Workload::Smallfiles {
                cell.bytes.div_ceil(cell.block)
            } else {
                1
            }
            .to_string(),
        ),
        ("pattern", quote("fnv1a64-id-splitmix64-le-v1")),
        ("pattern_round", pattern_round.to_string()),
        ("pattern_seeds", format!("[{seeds}]")),
        (
            "applied_boundary",
            quote(match cell.workload {
                Workload::Stream => "dup_checked_close_flush_prefix",
                Workload::Smallfiles => "checked_close_per_file_inside_accepted",
                Workload::Read => "none_read_only",
            }),
        ),
        (
            "fsync_boundary",
            quote(if cell.workload == Workload::Smallfiles {
                "reopen_fsync_checked_close_each_file"
            } else if cell.workload == Workload::Read {
                "none_read_only"
            } else {
                "fsync_original_open_file"
            }),
        ),
        (
            "profile_interval",
            quote("work_through_directory_fsync_excludes_fixture_and_verification"),
        ),
        ("correctness", "true".into()),
    ];
    emit("CURVES_RESULT", &fields)
}

fn members(directory: &Path) -> io::Result<BTreeSet<String>> {
    fs::read_dir(directory)?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| invalid("non-UTF8 output member"))
        })
        .collect()
}

fn verify_outputs(cells: &[Cell], workspace: &Path, rounds: usize) -> io::Result<()> {
    let output = workspace.join("curves-output");
    checked(
        members(&output)? == cells.iter().map(|cell| cell.id.clone()).collect(),
        "wrong output cell set",
    )?;
    let mut files = 0u64;
    let mut bytes = 0u64;
    for cell in cells {
        let cell_dir = output.join(&cell.id);
        let round_names = if cell.workload == Workload::Read {
            ["readset".to_owned()].into_iter().collect()
        } else {
            (1..=rounds).map(|round| format!("r{round:02}")).collect()
        };
        checked(members(&cell_dir)? == round_names, "wrong output round set")?;
        for round in 1..=rounds {
            let name = if cell.workload == Workload::Read {
                "readset".to_owned()
            } else {
                format!("r{round:02}")
            };
            let round_dir = cell_dir.join(name);
            checked(
                members(&round_dir)?
                    == (0..cell.workers)
                        .map(|worker| format!("w{worker:03}"))
                        .collect(),
                "wrong output worker set",
            )?;
            for worker in 0..cell.workers {
                let directory = round_dir.join(format!("w{worker:03}"));
                let data = pattern(
                    seed(
                        cell,
                        worker,
                        if cell.workload == Workload::Read {
                            0
                        } else {
                            round
                        },
                    ),
                    cell.bytes,
                );
                if cell.workload == Workload::Smallfiles {
                    let count = cell.bytes.div_ceil(cell.block);
                    checked(
                        members(&directory)?
                            == (0..count).map(|index| format!("f{index:08}.bin")).collect(),
                        "wrong smallfile member set",
                    )?;
                    for (index, expected) in data.chunks(cell.block).enumerate() {
                        verify_file(&directory.join(format!("f{index:08}.bin")), expected)?;
                        files += 1;
                    }
                } else {
                    checked(
                        members(&directory)? == ["data.bin".to_owned()].into_iter().collect(),
                        "wrong data-file member set",
                    )?;
                    verify_file(&directory.join("data.bin"), &data)?;
                    files += 1;
                }
                bytes += data.len() as u64;
            }
        }
    }
    emit(
        "CURVES_VERIFIED",
        &[
            ("schema", "1".into()),
            ("rows", (cells.len() * rounds).to_string()),
            ("files", files.to_string()),
            ("bytes", bytes.to_string()),
            (
                "count_scope",
                quote("cell_round_verifications; reused readset counted each round"),
            ),
            ("verified", "true".into()),
        ],
    )
}

fn run() -> io::Result<()> {
    let mut arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let verify_only = arguments
        .first()
        .is_some_and(|argument| argument == "--verify");
    if verify_only {
        arguments.remove(0);
    }
    checked(
        (2..=3).contains(&arguments.len()),
        "usage: curves_guest [--verify] PLAN.tsv WORKSPACE [ROUNDS=3]",
    )?;
    let cells = read_plan(Path::new(&arguments[0]))?;
    let workspace = Path::new(&arguments[1]).canonicalize()?;
    checked(
        workspace.is_dir(),
        "workspace must be an existing directory",
    )?;
    let rounds = arguments
        .get(2)
        .map(|value| {
            value
                .to_str()
                .ok_or_else(|| invalid("rounds must be UTF-8 decimal"))
                .and_then(number)
        })
        .transpose()?
        .unwrap_or(3);
    checked((1..=16).contains(&rounds), "rounds must be 1..16")?;
    if verify_only {
        return verify_outputs(&cells, &workspace, rounds);
    }
    checked(
        !workspace.join("curves-output").try_exists()?,
        "workspace already contains curves-output; use a fresh trial directory",
    )?;
    // Validate the profile preference before Ready without opening device fds.
    checked(
        std::env::var_os("CURVES_PROFILE").is_none_or(|value| value == "0" || value == "1"),
        "CURVES_PROFILE must be 0 or 1",
    )?;
    emit(
        "CURVES_READY",
        &[
            ("schema", "1".into()),
            ("cells", cells.len().to_string()),
            ("rounds", rounds.to_string()),
        ],
    )?;
    let markers = Markers::open()?;
    fs::create_dir(workspace.join("curves-output"))?;
    for round in 1..=rounds {
        for (index, cell) in cells.iter().enumerate() {
            run_cell(cell, index, cells.len(), round, &workspace, &markers)?;
        }
    }
    emit(
        "CURVES_DONE",
        &[
            ("schema", "1".into()),
            ("rows", (cells.len() * rounds).to_string()),
            ("verified", "true".into()),
        ],
    )
}

fn main() {
    if let Err(error) = run() {
        eprintln!("CURVES_ERROR {error}");
        std::process::exit(1);
    }
}
