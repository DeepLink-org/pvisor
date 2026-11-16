//! Loopback remote protocols: private HTTPS registry and authenticated cache proxy.
use super::{evidence::*, native::Docker, workloads};
use anyhow::{Context, Result, ensure};
use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::fd::AsRawFd,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
type Env = BTreeMap<String, String>;
const MAX_FRAME: usize = 1024 * 1024;

struct Exchange {
    child: OwnedChild,
    input: ChildStdin,
    output: ChildStdout,
    pending: Vec<u8>,
}
impl Exchange {
    fn new(argv: &[String], stderr: &Path) -> Result<Self> {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(stderr)?);
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("PVISOR_")) {
            cmd.env_remove(k);
        }
        let mut child = OwnedChild::new(cmd.spawn()?);
        let input = child.stdin().context("worker stdin")?;
        let output = child.stdout().context("worker stdout")?;
        nonblocking(input.as_raw_fd())?;
        nonblocking(output.as_raw_fd())?;
        Ok(Self {
            child,
            input,
            output,
            pending: Vec::new(),
        })
    }
    fn send(&mut self, request: &Value, deadline: Instant) -> Result<()> {
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        let mut sent = 0;
        while sent < bytes.len() {
            ensure!(Instant::now() < deadline, "private worker write deadline");
            match self.input.write(&bytes[sent..]) {
                Ok(0) => anyhow::bail!("worker stdin closed"),
                Ok(n) => sent += n,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut fd = libc::pollfd {
                        fd: self.input.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    unsafe {
                        libc::poll(&mut fd, 1, 10);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn receive(&mut self, request: Option<&Value>, seconds: u64) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        if let Some(request) = request {
            self.send(request, deadline)?;
        }
        loop {
            if let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                let rest = self.pending.split_off(end + 1);
                let line = std::mem::replace(&mut self.pending, rest);
                return Ok(serde_json::from_slice(&line).context("worker JSON protocol")?);
            }
            ensure!(
                Instant::now() < deadline,
                "private worker response deadline"
            );
            let mut fd = libc::pollfd {
                fd: self.output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut fd, 1, 10) };
            if rc < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if rc == 0 {
                continue;
            }
            let mut block = [0u8; 65536];
            match self.output.read(&mut block) {
                Ok(0) => anyhow::bail!("private worker exited before response"),
                Ok(n) => {
                    self.pending.extend_from_slice(&block[..n]);
                    ensure!(self.pending.len() <= MAX_FRAME, "worker response too large");
                }
                Err(error)
                    if [
                        std::io::ErrorKind::WouldBlock,
                        std::io::ErrorKind::Interrupted,
                    ]
                    .contains(&error.kind()) =>
                {
                    ()
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    fn close(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        self.send(&json!({"stop":true}), deadline)?;
        loop {
            if self.child.exited()? {
                let status = self.child.finish()?;
                ensure!(status.success(), "worker cleanup exited {status}");
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "worker cleanup deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
pub(super) struct Remote {
    worker: Exchange,
    config: Config,
    workload: String,
    image: Value,
    payload: Vec<String>,
}
impl Remote {
    pub fn close(&mut self, e: &mut Evidence) -> Result<()> {
        self.worker.close()?;
        ensure!(
            e.manifest["daemon_sha256"] == sha(&e.root.join("frozen/bin/pvisor-daemon"))?,
            "cache daemon changed"
        );
        Ok(())
    }
    pub fn prepare(e: &mut Evidence, config: &Config, workload: &str) -> Result<Self> {
        ensure!(config.daemon.is_file(), "remote requires daemon");
        let ubuntu = e
            .images
            .iter()
            .find(|i| i["name"] == "ubuntu-24.04")
            .context("Ubuntu input")?
            .clone();
        let python = e
            .images
            .iter()
            .find(|i| i["name"] == "python-3.12-slim-bookworm")
            .context("Python input")?
            .clone();
        let oracle = workloads::tree_oracle(&python)?;
        save(&e.root.join("mmap-oci-oracle.json"), &oracle)?;
        let image = if workload == "shell" {
            ubuntu.clone()
        } else {
            python.clone()
        };
        let mut payload = args(&[
            "/bin/sh",
            "-ec",
            "test ! -e /tmp/pv-private; printf check >/tmp/pv-private; test \"$(cat /tmp/pv-private)\" = check; exec \"$@\"",
            "probe",
        ]);
        payload.extend(workloads::payload(workload, &oracle, false)?);
        save(
            &e.root.join("workload.json"),
            &json!({"name":workload,"image":image["pinned"],"payload":payload}),
        )?;
        let frozen = e.root.join("frozen/bin");
        fs::create_dir_all(&frozen)?;
        fs::copy(&e.binary, frozen.join("pvisor"))?;
        fs::copy(&config.daemon, frozen.join("pvisor-daemon"))?;
        e.manifest["daemon_sha256"] = json!(sha(&frozen.join("pvisor-daemon"))?);
        e.command(
            args(&[
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &s(&e.root.join("registry-key.pem")),
                "-out",
                &s(&e.root.join("registry-cert.pem")),
                "-subj",
                "/CN=127.0.0.1",
                "-days",
                "1",
                "-addext",
                "subjectAltName=IP:127.0.0.1",
            ]),
            &Env::new(),
            30,
            false,
        )?;
        let source = e.root.join("staging/blobs/sha256");
        fs::create_dir_all(&source)?;
        let mut preparation = vec![];
        for image in [&ubuntu, &python] {
            let mut descriptors = vec![image["manifest"]["config"].clone()];
            descriptors.extend(image["manifest"]["layers"].as_array().unwrap().clone());
            for descriptor in descriptors {
                let key = digest_hex(descriptor["digest"].as_str().unwrap())?;
                if !source.join(&key).exists() {
                    fs::copy(
                        Path::new(image["source"].as_str().unwrap()).join(&key),
                        source.join(key),
                    )?;
                }
            }
            let start = Instant::now();
            let receipt: Value = serde_json::from_str(&e.command(
                args(&[
                    &s(&e.binary),
                    "cache",
                    "publish",
                    "--backend",
                    "filesystem",
                    "--location",
                    &s(&e.root.join("service/cache-v2")),
                    "--image-store",
                    &s(&e.root.join("staging")),
                    image["pinned"].as_str().unwrap(),
                ]),
                &Env::new(),
                600,
                false,
            )?)?;
            ensure!(
                receipt["digest"] == image["digest"],
                "server publication identity mismatch"
            );
            preparation.push(json!({"image":image["name"],"duration_ns":start.elapsed().as_nanos()as u64,"receipt":receipt}));
        }
        save(&e.root.join("preparation.json"), &preparation)?;
        save(
            &e.root.join("service-inventory.json"),
            &inventory(&e.root.join("service"))?,
        )?;
        save(
            &e.root.join("staging-inventory.json"),
            &inventory(&e.root.join("staging"))?,
        )?;
        let mut token = [0; 24];
        fs::File::open("/dev/urandom")?.read_exact(&mut token)?;
        let token = token.iter().map(|b| format!("{b:02x}")).collect::<String>();
        save(&e.root.join("remote-config.json"), &json!({"token":token}))?;
        fs::set_permissions(
            e.root.join("remote-config.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        let argv = args(&[
            "unshare",
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child=KILL",
            "--mount-proc",
            "--propagation",
            "private",
            &s(&config.worker),
            "remote-worker",
            &s(&e.root),
        ]);
        let mut worker = Exchange::new(&argv, &e.root.join("worker.stderr"))?;
        let receipt = worker.receive(None, 30)?;
        ensure!(receipt["ready"] == true, "private cache worker failed");
        for ns in ["user", "mnt", "pid"] {
            ensure!(
                receipt["namespaces"][ns].as_str()
                    != Some(&s(&fs::read_link(format!("/proc/self/ns/{ns}"))?)),
                "worker not isolated"
            );
        }
        save(&e.root.join("containment.json"), &receipt)?;
        e.manifest["scope"] = json!(
            "loopback HTTPS OCI vs authenticated framed cache TCP; headers/TLS/TCP excluded from byte counts but included in latency; no WAN or pure-format inference"
        );
        e.persist()?;
        Ok(Self {
            worker,
            config: config.clone(),
            workload: workload.into(),
            image,
            payload,
        })
    }
    pub fn pair(&mut self, e: &mut Evidence) -> Result<Value> {
        let order = e.rng.order();
        let mut pair = json!({"order":order,"workload":self.workload});
        for backend in order {
            let directory = e.trial(&format!("remote-{backend}"))?;
            if backend == "pvisor" {
                for state in ["cold", "warm"] {
                    let folder = directory.join(state);
                    fs::create_dir(&folder)?;
                    let metrics=self.worker.receive(Some(&json!({"directory":folder,"cache":directory.join("cache"),"image":self.image["pinned"],"digest":self.image["digest"],"payload":self.payload,"state":state})),150)?;
                    ensure!(
                        metrics["ready_ns"].as_u64().is_some()
                            && metrics["exit_ns"].as_u64().is_some(),
                        "cache worker rejected trial: {metrics}"
                    );
                    ensure!(
                        (metrics["content_bytes"].as_u64().context("content bytes")? > 0)
                            == (state == "cold"),
                        "invalid cold/warm content"
                    );
                    pair[format!("pvisor-{state}")] = metrics;
                }
            } else {
                let listener = TcpListener::bind(("127.0.0.1", 0))?;
                let port = listener.local_addr()?.port();
                drop(listener);
                let mut docker = Docker::new(e, &directory, None, &[format!("127.0.0.1:{port}")])?;
                let result = (|| -> Result<()> {
                    let config = directory.join("registry-config.json");
                    save(
                        &config,
                        &json!({"images":[self.image],"rows":directory.join("registry.jsonl"),"port":port,"cert":e.root.join("registry-cert.pem"),"key":e.root.join("registry-key.pem")}),
                    )?;
                    let mut registry = Exchange::new(
                        &args(&[
                            "nsenter",
                            "-t",
                            &docker.pid.to_string(),
                            "-U",
                            "-n",
                            "--",
                            "taskset",
                            "-c",
                            "2,3",
                            &s(&self.config.worker),
                            "registry",
                            &s(&config),
                        ]),
                        &directory.join("registry.stderr"),
                    )?;
                    let endpoint = registry.receive(None, 10)?;
                    ensure!(endpoint["ready"] == true, "registry not ready");
                    let local = format!(
                        "127.0.0.1:{port}/bench/image@{}",
                        self.image["digest"].as_str().unwrap()
                    );
                    for state in ["cold", "warm"] {
                        let folder = directory.join(state);
                        fs::create_dir(&folder)?;
                        let rows_path = directory.join("registry.jsonl");
                        let before = read_rows(&rows_path)?.len();
                        let mut argv = args(&["taskset", "-c", "0,1"]);
                        argv.extend(docker.argv.clone());
                        argv.extend(args(&[
                            "run",
                            "--rm",
                            "--network=none",
                            "--cpus=2",
                            "--memory=512m",
                            &local,
                            "/usr/bin/taskset",
                            "-c",
                            "0,1",
                        ]));
                        argv.extend(self.payload.clone());
                        let observation = capture(&argv, &Env::new(), &folder, 120, true)?;
                        let rows = read_rows(&rows_path)?
                            .into_iter()
                            .skip(before)
                            .collect::<Vec<_>>();
                        ensure!(
                            fs::metadata(directory.join("registry.stderr"))?.len() == 0,
                            "registry transfer error"
                        );
                        ensure!(
                            rows.iter().all(|r| r["status"] == 200),
                            "registry unsuccessful transfer"
                        );
                        let mut cmd = docker.argv.clone();
                        cmd.extend(args(&["ps", "-aq"]));
                        ensure!(
                            e.command(cmd, &Env::new(), 10, false)?.trim().is_empty(),
                            "container leaked"
                        );
                        let mut cmd = docker.argv.clone();
                        cmd.extend(args(&["image", "inspect", &local]));
                        let actual: Value =
                            serde_json::from_str(&e.command(cmd, &Env::new(), 20, false)?)?;
                        let config = load(
                            &Path::new(self.image["source"].as_str().unwrap()).join(digest_hex(
                                self.image["manifest"]["config"]["digest"].as_str().unwrap(),
                            )?),
                        )?;
                        ensure!(
                            actual[0]["Descriptor"]["digest"] == self.image["digest"]
                                && actual[0]["RootFS"]["Layers"] == config["rootfs"]["diff_ids"],
                            "registry image identity mismatch"
                        );
                        let sum = |part: &str| {
                            rows.iter()
                                .filter(|r| r["path"].as_str().unwrap_or("").contains(part))
                                .map(|r| r["response_bytes"].as_u64().unwrap())
                                .sum::<u64>()
                        };
                        let metrics = json!({"ready_ns":observation["ready_ns"],"exit_ns":observation["exit_ns"],"response_bytes":sum(""),"content_bytes":sum("/blobs/"),"metadata_bytes":sum("/manifests/"),"requests":rows.len()});
                        ensure!(
                            (metrics["content_bytes"].as_u64().unwrap() > 0) == (state == "cold"),
                            "invalid Docker cold/warm transfer"
                        );
                        save(&folder.join("requests.json"), &rows)?;
                        save(&folder.join("metrics.json"), &metrics)?;
                        pair[format!("docker-{state}")] = metrics;
                    }
                    // Cooperative stop is checked; the anchored owner handles
                    // any error path without signaling a reaped numerical ID.
                    registry.close()?;
                    Ok(())
                })();
                docker.close(e);
                result?;
            }
        }
        let directory = e.trial("remote-pair")?;
        save(&directory.join("pair.json"), &pair)?;
        Ok(pair)
    }
}
fn read_rows(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    fs::read_to_string(path)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

/// Read-only registry inside only the owned Docker network namespace.
pub(super) fn registry(path: &Path) -> Result<()> {
    let config = load(path)?;
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls())?;
    builder.set_private_key_file(config["key"].as_str().context("TLS key")?, SslFiletype::PEM)?;
    builder.set_certificate_chain_file(config["cert"].as_str().context("TLS cert")?)?;
    builder.check_private_key()?;
    let tls = Arc::new(builder.build());
    let mut objects: BTreeMap<String, (PathBuf, String)> = BTreeMap::new();
    for image in config["images"].as_array().context("images")? {
        objects.insert(
            image["digest"].as_str().unwrap().into(),
            (
                PathBuf::from(image["manifest_path"].as_str().unwrap()),
                image["manifest"]["mediaType"].as_str().unwrap().into(),
            ),
        );
        let mut ds = vec![image["manifest"]["config"].clone()];
        ds.extend(image["manifest"]["layers"].as_array().unwrap().clone());
        for d in ds {
            objects.insert(
                d["digest"].as_str().unwrap().into(),
                (
                    Path::new(image["source"].as_str().unwrap())
                        .join(digest_hex(d["digest"].as_str().unwrap())?),
                    d["mediaType"].as_str().unwrap().into(),
                ),
            );
        }
    }
    let objects = Arc::new(objects);
    let rows = Arc::new(Mutex::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config["rows"].as_str().context("rows")?)?,
    ));
    let listener =
        TcpListener::bind(("127.0.0.1", config["port"].as_u64().context("port")? as u16))?;
    listener.set_nonblocking(true)?;
    println!(
        "{}",
        json!({"ready":true,"port":listener.local_addr()?.port()})
    );
    std::io::stdout().flush()?;
    let (stop_tx, stop_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = stop_tx.send(());
    });
    let mut threads = vec![];
    while stop_rx.try_recv().is_err() {
        match listener.accept() {
            Ok((stream, _)) => {
                let tls = tls.clone();
                let objects = objects.clone();
                let rows = rows.clone();
                threads.push(std::thread::spawn(move||->Result<()>{
                    stream.set_read_timeout(Some(Duration::from_secs(30)))?;stream.set_write_timeout(Some(Duration::from_secs(30)))?;
                    let mut stream=tls.accept(stream).map_err(|e|anyhow::anyhow!("TLS: {e}"))?;
                    let mut reader=BufReader::new(&mut stream);
                    let mut line=String::new();reader.read_line(&mut line)?;
                    let parts=line.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
                    ensure!(parts.len()==3&&["GET","HEAD"].contains(&parts[0].as_str()),"unsupported registry request");
                    let mut headers=0;
                    loop{let mut line=String::new();ensure!(reader.read_line(&mut line)?>0,"partial HTTP headers");headers+=line.len();ensure!(headers<=65536,"HTTP headers too large");if line=="\r\n"{break;}}
                    drop(reader);
                    let method=&parts[0];let path=&parts[1];let key=path.rsplit('/').next().unwrap();
                    let descriptor=objects.get(key).filter(|_|path.contains("/manifests/")||path.contains("/blobs/"));
                    let(status,payload,media)=if ["/v2","/v2/"].contains(&path.as_str()){(200,b"{}".to_vec(),"application/json".to_owned())}
                        else if let Some((file,media))=descriptor{(200,fs::read(file)?,media.clone())}
                        else{(404,br#"{"errors":[{"code":"BLOB_UNKNOWN"}]}"#.to_vec(),"application/json".into())};
                    write!(stream,"HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nContent-Type: {media}\r\nDocker-Distribution-Api-Version: registry/2.0\r\nConnection: close\r\n",if status==200{"OK"}else{"Not Found"},payload.len())?;
                    if descriptor.is_some(){write!(stream,"Docker-Content-Digest: {key}\r\n")?;}
                    stream.write_all(b"\r\n")?;
                    if method=="GET"{stream.write_all(&payload)?;}stream.flush()?;
                    let row=json!({"path":path,"method":method,"status":status,"response_bytes":if method=="GET"{payload.len()}else{0},"body_sha256":if method=="GET"{Some(format!("{:x}",Sha256::digest(&payload)))}else{None}});
                    let mut rows=rows.lock().unwrap();serde_json::to_writer(&mut *rows,&row)?;rows.write_all(b"\n")?;rows.flush()?;
                    Ok(())
                }));
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(err) => return Err(err.into()),
        }
        // Report request errors immediately; a transfer failure invalidates cohort.
        let mut pending = vec![];
        for thread in threads.drain(..) {
            if thread.is_finished() {
                thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("registry thread panic"))??;
            } else {
                pending.push(thread);
            }
        }
        threads = pending;
    }
    for thread in threads {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("registry thread panic"))??;
    }
    Ok(())
}

#[derive(Default)]
struct Counts {
    active: usize,
    connections: u64,
    rows: Vec<Value>,
    errors: Vec<String>,
}
fn snapshot(counts: &Arc<Mutex<Counts>>) -> Result<(Vec<Value>, Vec<String>)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = counts.lock().unwrap();
        if state.active == 0 {
            return Ok((state.rows.clone(), state.errors.clone()));
        }
        ensure!(Instant::now() < deadline, "proxy exchanges did not drain");
        drop(state);
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn read_frame(stream: &mut TcpStream, first: Option<u8>) -> Result<(Vec<u8>, Value)> {
    let mut prefix = [0u8; 4];
    if let Some(first) = first {
        prefix[0] = first;
        stream.read_exact(&mut prefix[1..])?;
    } else {
        stream.read_exact(&mut prefix)?;
    }
    let length = u32::from_be_bytes(prefix) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "invalid cache frame size"
    );
    let mut raw = vec![0; length];
    stream.read_exact(&mut raw)?;
    let value = serde_json::from_slice(&raw)?;
    let mut frame = prefix.to_vec();
    frame.extend(raw);
    Ok((frame, value))
}
fn proxy_connection(
    mut client: TcpStream,
    port: u16,
    counts: Arc<Mutex<Counts>>,
    directory: PathBuf,
) -> Result<()> {
    let connection = {
        let mut c = counts.lock().unwrap();
        c.connections += 1;
        c.connections
    };
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    client.set_nodelay(true)?;
    let mut upstream: Option<TcpStream> = None;
    let mut version = None;
    loop {
        let mut first = [0u8; 1];
        match client.read(&mut first) {
            Ok(0) => return Ok(()),
            Err(err)
                if [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut]
                    .contains(&err.kind()) =>
            {
                return Ok(());
            }
            Err(err) => return Err(err.into()),
            _ => (),
        }
        counts.lock().unwrap().active += 1;
        let start = Instant::now();
        let result = (|| -> Result<()> {
            let (request_wire, envelope) = read_frame(&mut client, Some(first[0]))?;
            let current = envelope["version"].as_u64().context("version")?;
            ensure!(
                [1, 2].contains(&current) && version.is_none_or(|v| v == current),
                "invalid/changing protocol version"
            );
            version = Some(current);
            let request = &envelope["request"];
            if upstream.is_none() {
                let stream = TcpStream::connect_timeout(
                    &format!("127.0.0.1:{port}").parse()?,
                    Duration::from_secs(90),
                )?;
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(Duration::from_secs(90)))?;
                stream.set_write_timeout(Some(Duration::from_secs(90)))?;
                upstream = Some(stream);
            }
            let upstream = upstream.as_mut().unwrap();
            upstream.write_all(&request_wire)?;
            let (response_wire, response) = read_frame(upstream, None)?;
            let length = if response["status"] == "data" {
                response["length"].as_u64().context("Data length")? as usize
            } else {
                0
            };
            ensure!(length <= MAX_FRAME, "oversize Data");
            let mut body = vec![0; length];
            upstream.read_exact(&mut body)?;
            let hash = format!("sha256:{:x}", Sha256::digest(&body));
            if response["status"] == "data" {
                ensure!(response["sha256"] == hash, "Data body hash mismatch");
            }
            // Persist exact frames and complete verified body, never reserialize forwarding.
            let raw = directory.join(format!("{connection}-{}", unique()));
            fs::create_dir(&raw)?;
            fs::write(raw.join("request.frame"), &request_wire)?;
            fs::write(raw.join("response.frame"), &response_wire)?;
            fs::write(raw.join("body"), &body)?;
            client.write_all(&response_wire)?;
            client.write_all(&body)?;
            let row = json!({"connection_id":connection,"version":current,"op":request["op"],"path":request["path"],"request":request,"response":response,
                "content_bytes":if request["op"]=="read"{length}else{0},"metadata_bytes":if request["op"]=="metadata"{length}else{0},
                "response_bytes":response_wire.len()+length,"forwarded_body_sha256":hash,"elapsed_ns":start.elapsed().as_nanos()as u64,"raw":raw});
            counts.lock().unwrap().rows.push(row);
            Ok(())
        })();
        counts.lock().unwrap().active -= 1;
        result?;
        if version == Some(1) {
            return Ok(());
        }
    }
}
pub(super) fn worker(root: &Path) -> Result<()> {
    pivot(root)?;
    set_affinity(&[2, 3])?;
    let config = load(&root.join("remote-config.json"))?;
    let reservation = TcpListener::bind(("127.0.0.1", 0))?;
    let port = reservation.local_addr()?.port();
    drop(reservation);
    let env = Env::from([(
        "PVISOR_CACHE_TOKEN".into(),
        config["token"].as_str().context("cache token")?.into(),
    )]);
    let argv = args(&[
        "/tmp/lazy-binaries/pvisor-daemon",
        "serve",
        "--cache",
        "--cache-listen",
        &format!("tcp://127.0.0.1:{port}"),
        "--cache-image-store",
        &s(&root.join("service")),
    ]);
    let mut service = spawn_logged(
        &argv,
        &env,
        &root.join("cache-service.stdout"),
        &root.join("cache-service.stderr"),
    )?;
    let result = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            ensure!(!service.exited()?, "cache service exited");
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            ensure!(Instant::now() < deadline, "cache service readiness timeout");
            std::thread::sleep(Duration::from_millis(100));
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let proxy_port = listener.local_addr()?.port();
        let counts = Arc::new(Mutex::new(Counts::default()));
        let state = counts.clone();
        let frames = root.join("proxy-frames");
        fs::create_dir(&frames)?;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let counts = state.clone();
                let directory = frames.clone();
                std::thread::spawn(move || {
                    let result = stream
                        .map_err(anyhow::Error::from)
                        .and_then(|s| proxy_connection(s, port, counts.clone(), directory));
                    if let Err(err) = result {
                        counts.lock().unwrap().errors.push(format!("{err:#}"));
                    }
                });
            }
        });
        let mut namespaces = json!({});
        for ns in ["user", "mnt", "pid"] {
            namespaces[ns] = json!(s(&fs::read_link(format!("/proc/self/ns/{ns}"))?));
        }
        println!("{}", json!({"ready":true,"namespaces":namespaces}));
        std::io::stdout().flush()?;
        for line in std::io::stdin().lock().lines() {
            let request: Value = serde_json::from_str(&line?)?;
            if request["stop"] == true {
                return Ok(());
            }
            let directory =
                PathBuf::from(request["directory"].as_str().context("trial directory")?);
            let cache = PathBuf::from(request["cache"].as_str().context("cache")?);
            ensure!(
                directory.starts_with(root) && cache.starts_with(root),
                "worker path escaped evidence"
            );
            let mut environment = env.clone();
            environment.extend([
                (
                    "PVISOR_CACHE_SERVER".into(),
                    format!("tcp://127.0.0.1:{proxy_port}"),
                ),
                ("PVISOR_RUN_HOME".into(), s(&directory.join("runs"))),
                (
                    "PVISOR_IMAGE_STORE".into(),
                    s(&directory.join("unused-local-store")),
                ),
                ("XDG_CACHE_HOME".into(), s(&cache)),
                ("XDG_CONFIG_HOME".into(), s(&directory.join("config"))),
                ("PVISOR_STARTUP_TIMING".into(), "0".into()),
            ]);
            let mut argv = args(&[
                "taskset",
                "-c",
                "0,1",
                "/tmp/lazy-binaries/pvisor",
                "spawn",
                "--vm",
                "--rootfs",
                &format!("image={}", request["image"].as_str().context("image")?),
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--gateway-mode",
                "off",
                "--stdio",
                "inherit",
                "--stage",
                &s(&directory.join("stage")),
                "--cpu",
                "2",
                "--memory",
                "512MiB",
                "--",
            ]);
            argv.extend(
                request["payload"]
                    .as_array()
                    .context("payload")?
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned()),
            );
            let (before, _) = snapshot(&counts)?;
            fs::create_dir(directory.join("workspace"))?;
            let previous = std::env::current_dir()?;
            std::env::set_current_dir(directory.join("workspace"))?;
            let record = capture(&argv, &environment, &directory, 120, true);
            std::env::set_current_dir(previous)?;
            let record = record?;
            let (after, errors) = snapshot(&counts)?;
            let rows = &after[before.len()..];
            save(
                &directory.join("requests.json"),
                &json!({"rows":rows,"errors":errors}),
            )?;
            ensure!(errors.is_empty(), "cache proxy transfer error");
            let bundles = walk(&directory.join("stage"))?
                .into_iter()
                .filter(|p| p.file_name().is_some_and(|p| p == "run-bundle.json"))
                .collect::<Vec<_>>();
            ensure!(bundles.len() == 1, "one VM Bundle required");
            let bundle = load(&bundles[0])?;
            ensure!(
                bundle["run"]["state"] == "completed"
                    && bundle["run"]["exit_code"] == 0
                    && bundle["run"]["executor"]["isolation"] == "virtual_machine",
                "invalid VM Bundle"
            );
            let prepared = rows
                .iter()
                .filter(|r| r["op"] == "prepare")
                .collect::<Vec<_>>();
            ensure!(
                !prepared.is_empty()
                    && prepared
                        .iter()
                        .all(|r| r["response"]["digest"] == request["digest"]),
                "lazy image identity mismatch"
            );
            let sum = |key: &str| rows.iter().map(|r| r[key].as_u64().unwrap()).sum::<u64>();
            let metrics = json!({"ready_ns":record["ready_ns"],"exit_ns":record["exit_ns"],"content_bytes":sum("content_bytes"),"metadata_bytes":sum("metadata_bytes"),"response_bytes":sum("response_bytes"),"requests":rows.len()});
            ensure!(
                (metrics["content_bytes"].as_u64().unwrap() > 0) == (request["state"] == "cold"),
                "invalid cache cold/warm bytes"
            );
            save(&directory.join("metrics.json"), &metrics)?;
            println!("{metrics}");
            std::io::stdout().flush()?;
        }
        Ok(())
    })();
    let cleanup = stop(&mut service);
    result.and(cleanup)
}
pub(super) fn set_affinity(cpus: &[usize]) -> Result<()> {
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::CPU_ZERO(&mut mask);
    }
    for &cpu in cpus {
        ensure!(cpu < libc::CPU_SETSIZE as usize, "CPU out of range");
        unsafe {
            libc::CPU_SET(cpu, &mut mask);
        }
    }
    ensure!(
        unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&mask), &mask) } == 0,
        "required affinity unavailable"
    );
    Ok(())
}
/// Build all mountpoints before binding host trees; no writes through bound trees.
fn pivot(root: &Path) -> Result<()> {
    ensure!(
        root.is_absolute() && !root.starts_with("/tmp"),
        "remote evidence must be outside private /tmp"
    );
    let before = fs::metadata(root)?;
    let identity = (before.dev(), before.ino());
    let mut sources = BTreeSet::new();
    let mut aliases = vec![];
    for name in [
        "/home", "/usr", "/etc", "/dev", "/proc", "/bin", "/sbin", "/lib", "/lib64",
    ] {
        let path = Path::new(name);
        if !path.exists() {
            continue;
        }
        let canonical = fs::canonicalize(path)?;
        ensure!(
            canonical.is_dir() && canonical != Path::new("/") && !canonical.starts_with("/tmp"),
            "unsafe root bind"
        );
        sources.insert(canonical);
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            aliases.push((path.to_owned(), fs::read_link(path)?));
        }
    }
    sources.insert(root.to_owned());
    let mut candidates = sources.into_iter().collect::<Vec<_>>();
    candidates.sort_by_key(|p| p.components().count());
    let mut binds: Vec<PathBuf> = vec![];
    for path in candidates {
        if !binds.iter().any(|p| path.starts_with(p)) {
            binds.push(path);
        }
    }
    mount(
        Some("tmpfs"),
        Path::new("/tmp"),
        Some("tmpfs"),
        libc::MS_NOSUID | libc::MS_NODEV,
        Some("mode=1777"),
    )?;
    let private = Path::new("/tmp").join(format!("lazy-owned-root-{}", unique()));
    fs::create_dir(&private)?;
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700))?;
    fs::create_dir(private.join(".old-root"))?;
    fs::create_dir(private.join("tmp"))?;
    fs::set_permissions(private.join("tmp"), fs::Permissions::from_mode(0o1777))?;
    for source in &binds {
        fs::create_dir_all(private.join(source.strip_prefix("/")?))?;
    }
    for (alias, target) in &aliases {
        let path = private.join(alias.strip_prefix("/")?);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(target, path)?;
    }
    let bin = private.join("tmp/lazy-binaries");
    fs::create_dir(&bin)?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    let mut receipts = json!({});
    for name in ["pvisor", "pvisor-daemon"] {
        let frozen = root.join("frozen/bin").join(name);
        let expected = sha(&frozen)?;
        fs::copy(&frozen, bin.join(name))?;
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o700))?;
        ensure!(sha(&bin.join(name))? == expected, "launch copy mismatch");
        receipts[name] = json!(expected);
    }
    let mut receipt = json!({"host_private_root":private,"binds":binds,"aliases":aliases,"binaries":receipts,"entered":false,"old_root_detached":false});
    save(&root.join("containment-chroot.json"), &receipt)?;
    mount(Some(&s(&private)), &private, None, libc::MS_BIND, None)?;
    for source in &binds {
        mount(
            Some(&s(source)),
            &private.join(source.strip_prefix("/")?),
            None,
            libc::MS_BIND | libc::MS_REC,
            None,
        )?;
    }
    std::env::set_current_dir(&private)?;
    let new = std::ffi::CString::new(s(&private))?;
    let old = std::ffi::CString::new(s(&private.join(".old-root")))?;
    ensure!(
        unsafe { libc::syscall(libc::SYS_pivot_root, new.as_ptr(), old.as_ptr()) } == 0,
        "pivot_root unavailable; no chroot fallback"
    );
    std::env::set_current_dir("/")?;
    ensure!(
        unsafe { libc::umount2(c"/.old-root".as_ptr(), libc::MNT_DETACH) } == 0,
        "cannot detach old root"
    );
    let after = fs::metadata(root)?;
    ensure!(
        (after.dev(), after.ino()) == identity,
        "evidence identity changed"
    );
    for name in ["pvisor", "pvisor-daemon"] {
        let expected = receipts[name].as_str().unwrap();
        ensure!(
            sha(&Path::new("/tmp/lazy-binaries").join(name))? == expected
                && sha(&root.join("frozen/bin").join(name))? == expected,
            "frozen/launch binary changed"
        );
    }
    receipt["entered"] = json!(true);
    receipt["old_root_detached"] = json!(true);
    receipt["mountinfo"] = json!(fs::read_to_string("/proc/self/mountinfo")?);
    save(&root.join("containment-chroot.json"), &receipt)?;
    std::env::set_current_dir(root)?;
    Ok(())
}
fn mount(
    source: Option<&str>,
    target: &Path,
    kind: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<()> {
    let source = source.map(std::ffi::CString::new).transpose()?;
    let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    let kind = kind.map(std::ffi::CString::new).transpose()?;
    let data = data.map(std::ffi::CString::new).transpose()?;
    let rc = unsafe {
        libc::mount(
            source.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            target.as_ptr(),
            kind.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            flags,
            data.as_ref()
                .map_or(std::ptr::null(), |s| s.as_ptr().cast()),
        )
    };
    ensure!(
        rc == 0,
        "private mount failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(value: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(value).unwrap();
        let mut result = (bytes.len() as u32).to_be_bytes().to_vec();
        result.extend(bytes);
        result
    }
    #[test]
    fn proxy_preserves_frames_and_authenticates_body() -> Result<()> {
        for valid in [true, false] {
            let directory = std::env::temp_dir().join(unique());
            fs::create_dir(&directory)?;
            let upstream = TcpListener::bind(("127.0.0.1", 0))?;
            let port = upstream.local_addr()?.port();
            let request =
                frame(&json!({"version":2,"token":"fixture","request":{"op":"read","path":[97]}}));
            let response = frame(
                &json!({"status":"data","length":3,"sha256":if valid{format!("sha256:{:x}",Sha256::digest(b"abc"))}else{"sha256:bad".into()}}),
            );
            let expected = request.clone();
            let reply = response.clone();
            let server = std::thread::spawn(move || -> Result<()> {
                let (mut stream, _) = upstream.accept()?;
                let (raw, _) = read_frame(&mut stream, None)?;
                ensure!(raw == expected, "request changed");
                stream.write_all(&reply)?;
                stream.write_all(b"abc")?;
                Ok(())
            });
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            let address = listener.local_addr()?;
            let counts = Arc::new(Mutex::new(Counts::default()));
            let state = counts.clone();
            let root = directory.clone();
            let proxy = std::thread::spawn(move || -> Result<()> {
                let (client, _) = listener.accept()?;
                proxy_connection(client, port, state, root)
            });
            let mut client = TcpStream::connect(address)?;
            client.set_read_timeout(Some(Duration::from_secs(5)))?;
            client.write_all(&request)?;
            if valid {
                let (raw, _) = read_frame(&mut client, None)?;
                assert_eq!(raw, response);
                let mut body = [0; 3];
                client.read_exact(&mut body)?;
                assert_eq!(&body, b"abc");
                client.shutdown(std::net::Shutdown::Both)?;
                proxy.join().unwrap()?;
                let (rows, errors) = snapshot(&counts)?;
                assert!(errors.is_empty());
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0]["content_bytes"], 3);
                assert_eq!(rows[0]["response_bytes"], response.len() + 3);
                assert_eq!(
                    fs::read(Path::new(rows[0]["raw"].as_str().unwrap()).join("body"))?,
                    b"abc"
                );
            } else {
                assert!(proxy.join().unwrap().is_err());
                assert_eq!(counts.lock().unwrap().active, 0);
                assert!(counts.lock().unwrap().rows.is_empty());
            }
            server.join().unwrap()?;
            fs::remove_dir_all(directory)?;
        }
        Ok(())
    }
    #[test]
    fn failed_worker_readiness_keeps_the_owner_until_cleanup() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        fs::create_dir(&root)?;
        let mut exchange =
            Exchange::new(&args(&["/bin/sh", "-c", "exit 7"]), &root.join("stderr"))?;
        let pid = exchange.child.id();
        assert!(exchange.receive(None, 1).is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !exchange.child.exited()? {
            ensure!(Instant::now() < deadline, "fixture exit deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(fs::read_to_string(format!("/proc/{pid}/stat"))?.contains(") Z "));
        drop(exchange);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn oversized_frame_is_rejected_before_body_allocation() -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let sender = std::thread::spawn(move || -> Result<()> {
            let mut client = TcpStream::connect(address)?;
            client.write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())?;
            Ok(())
        });
        let (mut stream, _) = listener.accept()?;
        assert!(read_frame(&mut stream, None).is_err());
        sender.join().unwrap()?;
        Ok(())
    }
}
