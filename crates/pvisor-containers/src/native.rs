use crate::{
    api::*,
    store::{self, Record, lock, read},
    transport::Broker,
};
use anyhow::{Context, bail, ensure};
use pvisor_guest::control::{self, Reply, Request, read_frame, write_frame};
use pvisor_vm::api::{VmBuilder, VmConfiguration, VmRuntime};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
mod disk;
mod ipc;
mod job;
mod spawn;

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum HostRequest {
    Wait,
    Input,
    Acknowledged,
    Guest {
        request: Request,
        #[serde(default)]
        interactive: bool,
    },
    Kill,
}
type Port = Arc<(Mutex<Option<Arc<Broker>>>, Condvar)>;
fn owner_pipe(id: &str) -> String {
    format!(r"\\.\pipe\pvisor-container-{id}")
}
fn data_pipe(id: &str, kind: &str) -> String {
    format!(r"\\.\pipe\pvisor-guest-{id}-{kind}")
}
fn failure(error: impl std::fmt::Display) -> Reply {
    Reply::Failure {
        errno: 5,
        message: error.to_string(),
    }
}
fn generation_live(directory: &std::path::Path, generation: &str) -> anyhow::Result<bool> {
    let record = match read::<Record>(&directory.join("record.json")) {
        Ok(record) => record,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    Ok(record.info.generation == generation && store::live(directory)?)
}
fn make_port(name: String) -> anyhow::Result<Port> {
    let pipe = ipc::create_duplex(&name)?;
    let slot: Port = Arc::new((Mutex::new(None), Condvar::new()));
    let output = slot.clone();
    std::thread::spawn(move || match ipc::accept_duplex(pipe) {
        Ok((reader, writer)) => {
            *output.0.lock().unwrap() = Some(Broker::new(reader, writer));
            output.1.notify_all();
        }
        Err(e) => eprintln!("guest pipe connection: {e:#}"),
    });
    Ok(slot)
}
fn connected(port: &Port) -> anyhow::Result<Arc<Broker>> {
    let guard = port
        .0
        .lock()
        .map_err(|_| anyhow::anyhow!("poisoned guest port"))?;
    let (guard, timeout) = port
        .1
        .wait_timeout_while(guard, Duration::from_secs(10), |s| s.is_none())
        .map_err(|_| anyhow::anyhow!("poisoned guest port"))?;
    ensure!(
        !timeout.timed_out() && guard.is_some(),
        "guest console did not connect"
    );
    let broker = guard.as_ref().unwrap().clone();
    drop(guard);
    Ok(broker)
}
fn exchange(port: &Port, request: Request, output: &mut impl Write) -> anyhow::Result<()> {
    connected(port)?.exchange(request, output)
}
fn rpc(generation: &str, request: &HostRequest) -> anyhow::Result<ipc::Duplex> {
    ensure!(store::valid_id(generation), "invalid container generation");
    let mut stream = ipc::connect_control(&owner_pipe(generation))?;
    write_frame(&mut stream, request)?;
    Ok(stream)
}
fn receive(
    stream: &mut impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> anyhow::Result<i32> {
    loop {
        match read_frame::<Reply>(stream)? {
            Reply::Ready => (),
            Reply::Started { .. } => (),
            Reply::Output { stream, bytes } => {
                if stream == 2 {
                    stderr.write_all(&bytes)?;
                    stderr.flush()?;
                } else {
                    stdout.write_all(&bytes)?;
                    stdout.flush()?;
                }
            }
            Reply::Exit { code } => return Ok(code),
            Reply::Failure { message, .. } => bail!("{message}"),
            Reply::Error { message } => {
                writeln!(stderr, "{message}")?;
            }
            _ => bail!("unexpected container reply"),
        }
    }
}

fn validate_process_options(request: &Run) -> anyhow::Result<()> {
    // Image defaults are not available at preflight. Use the guest root only
    // for omitted cwd; GuestConfig::default intentionally has no launch path.
    let validation = pvisor_guest::GuestConfig {
        argv: vec!["/bin/true".into()],
        cwd: request.cwd.clone().unwrap_or_else(|| "/".into()),
        env: request.env.clone(),
        process: Some(pvisor_guest::ProcessConfig {
            user: request.user.clone(),
            tty: request.tty,
            new_session: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    validation.command()?;
    Ok(())
}

pub(crate) fn run(store: &Store, request: Run) -> anyhow::Result<Container> {
    ensure!(
        !request.image.is_empty()
            && request.cpus > 0
            && request.cpus <= 64
            && (128..=65536).contains(&request.memory_mib),
        "invalid image or VM resources"
    );
    if let Some(name) = &request.name {
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && !name.starts_with('.')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
            "invalid container name"
        );
    }
    ensure!(
        request
            .env
            .keys()
            .chain(request.unset_env.iter())
            .all(|s| !s.is_empty() && !s.contains(['=', '\0'])),
        "invalid environment name"
    );
    ensure!(
        request
            .command
            .iter()
            .chain(request.env.values())
            .chain(request.entrypoint.iter())
            .all(|s| !s.contains('\0')),
        "arguments contain NUL"
    );
    // Validate caller process options before registry or instance side effects.
    validate_process_options(&request)?;
    let image = match store.image_record(&request.image)? {
        Some(image) => image,
        None => {
            store.pull(&request.image)?;
            store
                .image_record(&request.image)?
                .context("published image")?
        }
    };
    let mut guest = image.guest;
    if request.tty.is_some() {
        guest
            .env
            .entry("TERM".into())
            .or_insert_with(|| "xterm".into());
    }
    guest
        .process
        .as_mut()
        .context("image process configuration missing")?
        .tty = request.tty;
    guest.process.as_mut().unwrap().stdin = if request.interactive {
        pvisor_guest::StdinMode::Pipe
    } else {
        pvisor_guest::StdinMode::Closed
    };
    if let Some(entrypoint) = &request.entrypoint {
        guest.argv.clear();
        if !entrypoint.is_empty() {
            guest.argv.push(entrypoint.clone());
        }
        guest.argv.extend(request.command.clone());
    } else if !request.command.is_empty() {
        guest.argv.truncate(image.entrypoint);
        guest.argv.extend(request.command.clone());
    }
    for key in &request.unset_env {
        guest.env.remove(key);
    }
    guest.env.extend(request.env.clone());
    if let Some(cwd) = &request.cwd {
        guest.cwd = cwd.clone();
    }
    if let Some(user) = &request.user {
        guest.process.get_or_insert_default().user = Some(user.clone());
    }
    guest.command()?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let mut info = Container {
        id: id.clone(),
        generation: uuid::Uuid::new_v4().simple().to_string(),
        name: request
            .name
            .clone()
            .unwrap_or_else(|| format!("pvisor-{}", &id[..12])),
        image: request.image.clone(),
        image_id: image.summary.id,
        command: guest.argv.clone(),
        state: State::Starting,
        exit_code: None,
        network: request.network,
        ipv4: None,
    };
    let directory = store.directory(&id)?;
    {
        let _lease = lock(&store.root.join("catalog.lock"))?;
        ensure!(
            !store.records()?.iter().any(|r| r.info.name == info.name),
            "container name already exists: {}",
            info.name
        );
        if request.network == Network::Bridge {
            let address = store.allocate_address()?;
            info.ipv4 = Some(address);
            guest.network = Some(pvisor_guest::NetworkConfig {
                address: address.octets(),
                gateway: [172, 17, 0, 1],
            });
        }
        ensure!(
            serde_json::to_vec(&guest)?.len() <= 1 << 20,
            "guest launch message exceeds 1 MiB"
        );
        fs::create_dir(&directory)?;
        let preparation = (|| {
            fs::create_dir(directory.join("bootstrap"))?;
            disk::create(&directory.join("root.ext4"))?;
            store.save(&Record {
                schema: 1,
                info: info.clone(),
                request: request.clone(),
                guest,
                stop_signal: image.stop_signal,
                prepared: false,
                startup_error: None,
            })?;
            spawn::owner(&store.root, &id, &info.generation)?;
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(e) = preparation {
            let _ = fs::remove_dir_all(&directory);
            return Err(e);
        }
    }
    if request.detach {
        let deadline = Instant::now() + Duration::from_secs(125);
        loop {
            match read::<Record>(&directory.join("record.json")) {
                Ok(r) if r.info.state != State::Starting => {
                    ensure!(
                        r.startup_error.is_none(),
                        "container failed to start: {:?}",
                        r.startup_error
                    );
                    return Ok(r.info);
                }
                Err(_) if request.auto_remove && !directory.exists() => return Ok(info),
                Err(e) => return Err(e),
                _ => (),
            }
            ensure!(
                Instant::now() < deadline,
                "container startup did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(info)
}

pub(crate) fn start(store: &Store, selector: &str) -> anyhow::Result<Container> {
    let info = {
        let _catalog = lock(&store.root.join("catalog.lock"))?;
        let record = store.resolve(selector)?;
        let dir = store.directory(&record.info.id)?;
        if store::live(&dir)? {
            ensure!(
                record.info.state == State::Running,
                "container is starting or stopping"
            );
            return Ok(record.info);
        }
        // Fence the old runner's disk while its owner's Job finishes teardown.
        let _disk = lock(&dir.join("vm.lock"))?;
        let previous = record;
        let record = store::restarted(&previous)?;
        store.save(&record)?;
        if let Err(error) = spawn::owner(&store.root, &record.info.id, &record.info.generation) {
            store.save(&previous)?;
            return Err(error);
        }
        record.info
    };
    let dir = store.directory(&info.id)?;
    let deadline = Instant::now() + Duration::from_secs(125);
    loop {
        let record: Record = read(&dir.join("record.json"))?;
        ensure!(
            record.info.generation == info.generation,
            "container generation changed during start"
        );
        if record.info.state != State::Starting {
            ensure!(
                record.startup_error.is_none(),
                "container failed to start: {:?}",
                record.startup_error
            );
            return Ok(record.info);
        }
        ensure!(
            Instant::now() < deadline,
            "container startup did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn wait(
    store: &Store,
    container: &Container,
    input: Option<&mut dyn InputSource>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> anyhow::Result<i32> {
    if let Ok(record) = store.resolve(&container.id) {
        ensure!(
            record.info.generation == container.generation,
            "stale container attachment"
        );
        if record.info.state == State::Exited && !store::live(&store.directory(&record.info.id)?)? {
            store.logs(&record.info.id, false, stdout, stderr)?;
            return record
                .info
                .exit_code
                .context("terminal record has no exit code");
        }
    }
    let stream = rpc(&container.generation, &HostRequest::Wait)?;
    if let Some(input) = input {
        let record = store.resolve(&container.id)?;
        ensure!(record.request.interactive, "container stdin was not opened");
        let input_stream = rpc(&container.generation, &HostRequest::Input)?;
        return with_input(input, input_stream, Some(stream.clone()), true, || {
            wait_output(store, container, stream, stdout, stderr)
        });
    }
    wait_output(store, container, stream, stdout, stderr)
}
fn input_link_closed(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::UnexpectedEof
        || matches!(error.raw_os_error(), Some(109 | 232 | 233))
}
fn input_ready(stream: &mut ipc::Duplex) -> anyhow::Result<bool> {
    match read_frame::<Reply>(stream) {
        Ok(Reply::Ready) => Ok(true),
        // The process may finish before its stdin attachment is admitted.
        // Completion/output remains authoritative; input closure is not an
        // alternate exit status and must not turn a fast success into failure.
        Ok(Reply::Failure { errno: 32, .. }) => Ok(false),
        Ok(Reply::Failure { message, .. }) => bail!("{message}"),
        Ok(_) => bail!("unexpected guest stdin acknowledgment"),
        Err(error) if input_link_closed(&error) => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn pump_input(
    source: &mut dyn InputSource,
    mut stream: ipc::Duplex,
    cancel: &AtomicBool,
    acknowledgments: bool,
) -> anyhow::Result<()> {
    let run = (|| {
        if acknowledgments && !input_ready(&mut stream)? {
            return Ok(());
        }
        let mut bytes = [0; 16384];
        loop {
            let size = source.read(&mut bytes, cancel)?;
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            let sent = write_frame(
                &mut stream,
                &if size == 0 {
                    control::StdinFrame::Eof {}
                } else {
                    control::StdinFrame::Data {
                        bytes: bytes[..size].to_vec(),
                    }
                },
            );
            match sent {
                Ok(()) => (),
                Err(error) if input_link_closed(&error) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
            if acknowledgments && !input_ready(&mut stream)? {
                return Ok(());
            }
            if size == 0 {
                return Ok(());
            }
        }
    })();
    if cancel.load(Ordering::Acquire) {
        Ok(())
    } else {
        run
    }
}
fn with_input<T>(
    input: &mut dyn InputSource,
    input_stream: ipc::Duplex,
    output_stream: Option<ipc::Duplex>,
    acknowledgments: bool,
    operation: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let link = input_stream.clone();
        let flag = &cancel;
        let worker = scope.spawn(move || {
            let result = pump_input(input, link, flag, acknowledgments);
            if result.is_err() {
                if let Some(output) = output_stream {
                    output.cancel();
                }
            }
            result
        });
        let result = operation();
        cancel.store(true, Ordering::Release);
        input_stream.cancel();
        let input_result = worker
            .join()
            .map_err(|_| anyhow::anyhow!("stdin worker panicked"))?;
        input_result?;
        result
    })
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn process_preflight_defaults_only_an_omitted_cwd_to_the_guest_root() {
        let mut request = Run::default();
        validate_process_options(&request).unwrap();
        request.tty = Some([37, 91]);
        validate_process_options(&request).unwrap();
        request.cwd = Some("C:\\host-path".into());
        assert!(validate_process_options(&request).is_err());
        request.cwd = Some("/work".into());
        validate_process_options(&request).unwrap();
    }

    fn pair() -> (ipc::Duplex, ipc::Duplex) {
        let name = format!(
            r"\\.\pipe\pvisor-input-test-{}",
            uuid::Uuid::new_v4().simple()
        );
        let server = ipc::create_duplex(&name).unwrap();
        let client = ipc::connect_control(&name).unwrap();
        (ipc::accept_control(server).unwrap(), client)
    }

    struct Source {
        fail: bool,
    }
    impl InputSource for Source {
        fn read(&mut self, _: &mut [u8], cancel: &AtomicBool) -> std::io::Result<usize> {
            if self.fail {
                return Err(std::io::Error::other("source failure fixture"));
            }
            while !cancel.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(0)
        }
    }

    #[test]
    fn an_early_guest_input_close_does_not_become_an_alternate_exit_status() {
        let (mut server, client) = pair();
        write_frame(
            &mut server,
            &Reply::Failure {
                errno: 32,
                message: "already exited".into(),
            },
        )
        .unwrap();
        // A source error would expose an accidental read after remote closure.
        pump_input(
            &mut Source { fail: true },
            client,
            &AtomicBool::new(false),
            true,
        )
        .unwrap();
        let (mut server, client) = pair();
        write_frame(&mut server, &failure("admission rejected")).unwrap();
        assert!(
            pump_input(
                &mut Source { fail: false },
                client,
                &AtomicBool::new(false),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn source_errors_cancel_output_but_idle_input_does_not_retain_completion() {
        let (mut server, client) = pair();
        write_frame(&mut server, &Reply::Ready).unwrap();
        assert_eq!(
            with_input(&mut Source { fail: false }, client, None, true, || Ok(7)).unwrap(),
            7
        );

        let (mut server, client) = pair();
        write_frame(&mut server, &Reply::Ready).unwrap();
        let (_open_output_peer, output) = pair();
        let mut reader = output.clone();
        let error = with_input(
            &mut Source { fail: true },
            client,
            Some(output),
            true,
            || read_frame::<Reply>(&mut reader).map_err(anyhow::Error::from),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source failure fixture"));
    }
}

fn wait_output(
    store: &Store,
    container: &Container,
    mut stream: ipc::Duplex,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> anyhow::Result<i32> {
    // Existing stopped records have no owner. The initial foreground run uses
    // the live owner, whose auto-remove handshake retains files through delivery.
    let mut auto_remove = false;
    if let Ok(r) = store.resolve(&container.id) {
        ensure!(
            r.info.generation == container.generation,
            "stale container attachment"
        );
        auto_remove = r.request.auto_remove;
        if r.info.state == State::Exited && !store::live(&store.directory(&r.info.id)?)? {
            store.logs(&r.info.id, false, stdout, stderr)?;
            return r.info.exit_code.context("terminal record has no exit code");
        }
    }
    let code = receive(&mut stream, stdout, stderr)?;
    write_frame(&mut stream, &HostRequest::Acknowledged)?;
    let mut byte = [0; 1];
    match stream.read(&mut byte) {
        Ok(0) => (),
        Err(e) if matches!(e.raw_os_error(), Some(109 | 233)) => (),
        Err(e) => return Err(e.into()),
        Ok(_) => bail!("unexpected data after completion acknowledgment"),
    }
    let dir = store.directory(&container.id)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while dir.exists() && (auto_remove || store::live(&dir)?) {
        ensure!(
            Instant::now() < deadline,
            "container teardown did not finish after output delivery"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(code)
}
pub(crate) fn exec(
    store: &Store,
    selector: &str,
    mut launch: control::ExecLaunch,
    unset_env: Vec<String>,
    input: Option<&mut dyn InputSource>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> anyhow::Result<i32> {
    ensure!(
        !launch.argv.is_empty()
            && !launch.argv[0].is_empty()
            && launch.argv.iter().all(|s| !s.contains('\0')),
        "exec requires a literal command"
    );
    let r = store.resolve(selector)?;
    ensure!(
        r.info.state == State::Running && store::live(&store.directory(&r.info.id)?)?,
        "container is not running"
    );
    if launch.tty.is_some()
        && !r.guest.env.contains_key("TERM")
        && !unset_env.iter().any(|key| key == "TERM")
    {
        launch
            .env
            .entry("TERM".into())
            .or_insert_with(|| "xterm".into());
    }
    ensure!(
        unset_env
            .iter()
            .all(|key| !key.is_empty() && !key.contains(['=', '\0'])),
        "invalid environment name"
    );
    if !launch.replace_env {
        let mut env = r.guest.env.clone();
        for key in unset_env {
            env.remove(&key);
        }
        env.extend(launch.env);
        launch.env = env;
        launch.replace_env = true;
    }
    let mut stream = rpc(
        &r.info.generation,
        &HostRequest::Guest {
            request: Request::Start { launch },
            interactive: input.is_some(),
        },
    )?;
    if let Some(input) = input {
        with_input(input, stream.clone(), Some(stream.clone()), false, || {
            receive(&mut stream, stdout, stderr)
        })
    } else {
        receive(&mut stream, stdout, stderr)
    }
}
pub(crate) fn stop(
    store: &Store,
    selector: &str,
    timeout: Duration,
    signal: Option<i32>,
) -> anyhow::Result<()> {
    let r = store.resolve(selector)?;
    let dir = store.directory(&r.info.id)?;
    if !store::live(&dir)? {
        return Ok(());
    }
    // The signal request must not wait for an exec's separate command channel.
    let signal = signal.unwrap_or(r.stop_signal);
    let generation = r.info.generation.clone();
    std::thread::spawn(move || {
        if let Ok(mut stream) = rpc(
            &generation,
            &HostRequest::Guest {
                request: Request::Stop { signal },
                interactive: false,
            },
        ) {
            let _ = read_frame::<Reply>(&mut stream);
        }
    });
    let deadline = if timeout == Duration::MAX {
        None
    } else {
        Some(
            Instant::now()
                .checked_add(timeout)
                .context("stop timeout exceeds monotonic clock range")?,
        )
    };
    while generation_live(&dir, &r.info.generation)?
        && !deadline.is_some_and(|deadline| Instant::now() >= deadline)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    if generation_live(&dir, &r.info.generation)? {
        let mut stream = rpc(&r.info.generation, &HostRequest::Kill)?;
        match read_frame::<Reply>(&mut stream)? {
            Reply::Ready => (),
            reply => bail!("force-stop rejected: {reply:?}"),
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while generation_live(&dir, &r.info.generation)? {
            ensure!(
                Instant::now() < deadline,
                "runner teardown did not complete"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
}

fn follow(
    path: &std::path::Path,
    offset: &mut u64,
    channel: u8,
    stream: &mut impl Write,
) -> anyhow::Result<()> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    file.seek(SeekFrom::Start(*offset))?;
    let mut bytes = [0; 16384];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        write_frame(
            stream,
            &Reply::Output {
                stream: channel,
                bytes: bytes[..n].to_vec(),
            },
        )?;
        *offset += n as u64;
    }
    Ok(())
}
fn serve_owner(
    mut stream: ipc::Duplex,
    root: PathBuf,
    id: String,
    command: Port,
    signal: Port,
    kill: Arc<AtomicBool>,
    delivered: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let store = Store::open(Some(root))?;
    match read_frame::<HostRequest>(&mut stream)? {
        HostRequest::Acknowledged => bail!("completion acknowledgment requires an attachment"),
        HostRequest::Kill => {
            kill.store(true, Ordering::Release);
            write_frame(&mut stream, &Reply::Ready)?;
        }
        HostRequest::Input => {
            let broker = connected(&command)?;
            let result = (|| -> anyhow::Result<()> {
                let mut ticket = broker.begin(Request::Input {})?;
                let input = ticket.input();
                let first = ticket.recv()?;
                write_frame(&mut stream, &first)?;
                ensure!(matches!(first, Reply::Ready), "guest stdin unavailable");
                loop {
                    let frame: control::StdinFrame = read_frame(&mut stream)?;
                    let eof = matches!(frame, control::StdinFrame::Eof {});
                    input.send(frame)?;
                    if eof {
                        let reply = ticket.recv()?;
                        ensure!(matches!(reply, Reply::Ready), "guest stdin did not close");
                        write_frame(&mut stream, &reply)?;
                        break;
                    }
                    write_frame(&mut stream, &Reply::Ready)?;
                }
                ipc::drain(&stream)?;
                Ok(())
            })();
            if result.is_err() && broker.is_closed() {
                write_frame(
                    &mut stream,
                    &Reply::Failure {
                        errno: 32,
                        message: "guest stdin closed with the workload".into(),
                    },
                )?;
                ipc::drain(&stream)?;
                return Ok(());
            }
            return result;
        }
        HostRequest::Guest {
            request,
            interactive,
        } => {
            let signals = matches!(request, Request::Stop { .. });
            ensure!(
                signals
                    || matches!(
                        request,
                        Request::Ping {} | Request::Exec { .. } | Request::Start { .. }
                    ),
                "unsupported owner request"
            );
            let port = if signals { &signal } else { &command };
            if interactive {
                ensure!(
                    matches!(request, Request::Start { .. } | Request::Exec { .. }),
                    "interactive request must execute a process"
                );
                let mut ticket = connected(port)?.begin(request)?;
                let input = ticket.input();
                let mut reader = stream.clone();
                let done = AtomicBool::new(false);
                let result = std::thread::scope(|scope| {
                    let input = input.clone();
                    let done = &done;
                    let worker = scope.spawn(move || {
                        let result = (|| -> anyhow::Result<()> {
                            loop {
                                let frame: control::StdinFrame = read_frame(&mut reader)?;
                                let eof = matches!(frame, control::StdinFrame::Eof {});
                                input.send(frame)?;
                                if eof {
                                    break;
                                }
                            }
                            Ok(())
                        })();
                        if result.is_err() && !done.load(Ordering::Acquire) {
                            input.cancel();
                        }
                    });
                    let result = (|| -> anyhow::Result<()> {
                        loop {
                            let reply = ticket.recv()?;
                            write_frame(&mut stream, &reply)?;
                            if ticket.complete() {
                                break;
                            }
                        }
                        ipc::drain(&stream)?;
                        Ok(())
                    })();
                    done.store(true, Ordering::Release);
                    if result.is_err() {
                        ticket.input().cancel();
                    }
                    stream.cancel();
                    let _ = worker.join();
                    result
                });
                return result;
            } else {
                exchange(port, request, &mut stream)?;
            }
        }
        HostRequest::Wait => {
            let dir = store.directory(&id)?;
            let (mut out, mut err) = (0, 0);
            loop {
                follow(&dir.join("stdout.log"), &mut out, 1, &mut stream)?;
                follow(&dir.join("stderr.log"), &mut err, 2, &mut stream)?;
                let r: Record = read(&dir.join("record.json"))?;
                if r.info.state == State::Exited {
                    follow(&dir.join("stdout.log"), &mut out, 1, &mut stream)?;
                    follow(&dir.join("stderr.log"), &mut err, 2, &mut stream)?;
                    if r.info.exit_code == Some(125) {
                        let mut boot = 0;
                        follow(&dir.join("boot.stderr"), &mut boot, 2, &mut stream)?;
                    }
                    write_frame(
                        &mut stream,
                        &Reply::Exit {
                            code: r.info.exit_code.context("missing exit status")?,
                        },
                    )?;
                    ensure!(
                        matches!(
                            read_frame::<HostRequest>(&mut stream)?,
                            HostRequest::Acknowledged
                        ),
                        "completion was not acknowledged"
                    );
                    delivered.store(true, Ordering::Release);
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    ipc::drain(&stream)?;
    Ok(())
}

fn owner(store: Store, id: &str, generation: &str) -> anyhow::Result<()> {
    let dir = store.directory(id)?;
    let owner_lease = lock(&dir.join("owner.lock"))?;
    let record: Record = read(&dir.join("record.json"))?;
    ensure!(
        record.schema == 1
            && record.info.id == id
            && record.info.generation == generation
            && record.info.state == State::Starting,
        "stale container owner invocation"
    );
    let command = make_port(data_pipe(generation, "command"))?;
    let signal = make_port(data_pipe(generation, "signal"))?;
    let listener = ipc::create_control(&owner_pipe(generation), true)?;
    let kill = Arc::new(AtomicBool::new(false));
    let delivered = Arc::new(AtomicBool::new(false));
    {
        let root = store.root.clone();
        let id = id.to_owned();
        let command = command.clone();
        let signal = signal.clone();
        let kill = kill.clone();
        let delivered = delivered.clone();
        let pipe_name = owner_pipe(generation);
        std::thread::spawn(move || {
            let mut listener = listener;
            loop {
                let stream = match ipc::accept_control(listener) {
                    Ok(f) => f,
                    Err(_) => break,
                };
                let next = ipc::create_control(&pipe_name, false);
                let (root, id, command, signal, kill, delivered) = (
                    root.clone(),
                    id.clone(),
                    command.clone(),
                    signal.clone(),
                    kill.clone(),
                    delivered.clone(),
                );
                std::thread::spawn(move || {
                    let mut response = stream.clone();
                    if let Err(e) = serve_owner(stream, root, id, command, signal, kill, delivered)
                    {
                        let _ = write_frame(&mut response, &failure(e));
                        let _ = ipc::drain(&response);
                    }
                });
                listener = match next {
                    Ok(f) => f,
                    Err(_) => break,
                };
            }
        });
    }
    let boot_out = File::create(dir.join("boot.stdout"))?;
    let boot_err = File::create(dir.join("boot.stderr"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--pvisor-container-vm")
        .arg(&store.root)
        .arg(id)
        .arg(generation)
        .stdin(Stdio::null())
        .stdout(boot_out)
        .stderr(boot_err)
        .creation_flags(0x00000004 | 0x00000200)
        .spawn()?;
    let job = job::own_and_resume(&mut child)?;
    let ready = Arc::new(AtomicBool::new(false));
    let readiness = {
        let command = command.clone();
        let root = store.root.clone();
        let id = id.to_owned();
        let ready = ready.clone();
        let generation = generation.to_owned();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                connected(&command)?.wait_ready()?;
                let store = Store::open(Some(root))?;
                let _catalog = lock(&store.root.join("catalog.lock"))?;
                let path = store.directory(&id)?.join("record.json");
                let mut r: Record = read(&path)?;
                ensure!(r.info.generation == generation, "stale readiness response");
                r.prepared = true;
                if r.info.state == State::Starting {
                    r.info.state = State::Running;
                }
                store.save(&r)?;
                ready.store(true, Ordering::Release);
                Ok(())
            })();
            if let Err(e) = result {
                eprintln!("container readiness: {e:#}");
            }
        })
    };
    let started = Instant::now();
    let mut forced = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if kill.load(Ordering::Acquire)
            || (!ready.load(Ordering::Acquire) && started.elapsed() > Duration::from_secs(120))
        {
            forced = true;
            child.kill()?;
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(job);
    let _ = readiness.join();
    {
        let _catalog = lock(&store.root.join("catalog.lock"))?;
        let mut record: Record = read(&dir.join("record.json"))?;
        ensure!(
            record.info.generation == generation,
            "stale runner completion"
        );
        record.info.state = State::Exited;
        record.info.exit_code = Some(if forced {
            137
        } else {
            status
                .code()
                .filter(|c| (0..=255).contains(c))
                .unwrap_or(125)
        });
        if !ready.load(Ordering::Acquire) {
            record.startup_error = Some(format!(
                "guest supervisor did not confirm launch; runner status {:?}",
                record.info.exit_code
            ));
        }
        store.save(&record)?;
    }
    // A fast workload can exit before the attachment thread sees its terminal
    // record. Retain the owner through reply drain for every foreground launch,
    // not only auto-remove, or the client receives EOF instead of its exit code.
    if !record.request.detach {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !delivered.load(Ordering::Acquire)
            && !kill.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if record.request.auto_remove {
        let _catalog = lock(&store.root.join("catalog.lock"))?;
        drop(owner_lease);
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn runner(store: Store, id: &str, generation: &str) -> anyhow::Result<()> {
    if let Ok(filter) = std::env::var("PVISOR_VM_LOG") {
        use pvisor_vm::api::{RuntimeSupport, VmPlatform};
        VmPlatform::init_logging(&filter);
    }
    let dir = store.directory(id)?;
    let _vm_lease = lock(&dir.join("vm.lock"))?;
    ensure!(store::live(&dir)?, "runner has no admitted owner");
    let mut r: Record = read(&dir.join("record.json"))?;
    ensure!(
        r.schema == 1
            && r.info.id == id
            && r.info.generation == generation
            && r.info.state == State::Starting,
        "stale runner invocation"
    );
    if r.prepared {
        r.guest
            .root_disk
            .as_mut()
            .context("container has no root disk")?
            .layers
            .clear();
    }
    let mut vm = VmBuilder::new(r.request.cpus, r.request.memory_mib)?;
    let _network = if r.info.network == Network::Bridge {
        use pvisor_overlaynet::{
            BandwidthRegistry, EgressContext, EgressRuntime, PeerNetworkConfig,
            vm::{VmNetwork, VmNetworkConfig},
        };
        use std::os::windows::io::IntoRawHandle;
        let address = r.info.ipv4.context("bridge instance has no address")?;
        let networks = store.root.join("networks");
        ipc::private_directory(&networks)?;
        let authority = networks.join("bridge");
        ipc::private_directory(&authority)?;
        let policy = pvisor_overlaynet::NetworkPolicy::compile(&Default::default())?;
        let egress = EgressRuntime::for_peer_network(policy, BandwidthRegistry::default())?;
        let mut config = VmNetworkConfig::new(
            egress,
            EgressContext {
                run_id: Some(id.into()),
                execution_id: Some(generation.into()),
            },
        );
        config.peer_network = Some(PeerNetworkConfig {
            directory: authority.canonicalize()?,
            address,
            gateway: std::net::Ipv4Addr::new(172, 17, 0, 1),
            aliases: Vec::new(),
        });
        let (backend, guest) = ipc::network_pair(&data_pipe(generation, "ethernet"))?;
        let network = VmNetwork::start_with_io(config, move || unsafe {
            tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(
                backend.into_raw_handle(),
            )
        })?;
        vm.network(guest, pvisor_overlaynet::vm::guest_mac(address))?;
        Some(network)
    } else {
        None
    };
    vm.filesystem("/dev/root", &dir.join("bootstrap"), 0)?;
    vm.read_only_filesystem("pvisor-layers", &store.root.join("blobs"))?;
    ensure!(
        vm.raw_disk(dir.join("root.ext4"), false)? == 0,
        "unexpected root disk index"
    );
    vm.console_file("krun-stdout".into(), dir.join("stdout.log"))?;
    vm.console_file("krun-stderr".into(), dir.join("stderr.log"))?;
    vm.console_pipe(
        control::SERIAL_COMMAND_PORT.into(),
        data_pipe(generation, "command").into(),
    )?;
    vm.console_pipe(
        control::SERIAL_SIGNAL_PORT.into(),
        data_pipe(generation, "signal").into(),
    )?;
    vm.virtual_file(
        "/dev/root",
        pvisor_guest::CONFIG_PATH,
        serde_json::to_vec(&r.guest)?,
        0o400,
        true,
    )?;
    vm.run(|_| Ok(()))?;
    bail!("VM returned without an authenticated exit")
}
pub(crate) fn internal() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    let mode = args.get(1).and_then(|s| s.to_str()).unwrap_or("");
    if !matches!(mode, "--pvisor-container-owner" | "--pvisor-container-vm") {
        return Ok(false);
    }
    ensure!(args.len() == 5, "invalid private container invocation");
    let id = args[3].to_str().context("invalid container identity")?;
    ensure!(store::valid_id(id), "invalid container identity");
    let generation = args[4].to_str().context("invalid container generation")?;
    ensure!(store::valid_id(generation), "invalid container generation");
    let store = Store::open(Some(PathBuf::from(&args[2])))?;
    if mode.ends_with("-owner") {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(store.directory(id)?.join("owner.log"))?;
        ensure!(
            unsafe { SetStdHandle(STD_ERROR_HANDLE, log.as_raw_handle().cast()) } != 0,
            "install owner diagnostics"
        );
        if let Err(error) = owner(store, id, generation) {
            let _ = writeln!(log, "container owner: {error:#}");
            return Err(error);
        }
    } else {
        runner(store, id, generation)?;
    }
    Ok(true)
}
