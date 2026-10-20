//! Linux/KVM engineering probe for one long, ordinary model request.
//! CASE_ROOT and rootfs/firmware environment match vm_idle_handoff.
//! Args: long|timer|network|stream|race|short|sched|psi|psi-late|psi-stats baseline|pause|offload seconds.
#[cfg(feature = "gateway")]
mod probe {
    use anyhow::{Context, ensure};
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use pvisor::{
        CooperativeModelWait, GatewayDriverConfig, MemoryEventSink, ModelStreamBuffer,
        ModelWaitRounds, OverlayHint, PVisor, VmExecutor, VmSettings,
    };
    use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
    use serde_json::{Value, json};
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        sync::{mpsc, watch},
    };
    use tokio_stream::wrappers::ReceiverStream;

    const GUEST: &str = r#"
import hashlib, json, os, socket, sys, threading, time, urllib.request
# Observation only: the Host boot policy sets this before this program starts.
compaction_proactiveness=int(open("/proc/sys/vm/compaction_proactiveness").read())
print("guest-boot-policy "+json.dumps(dict(compaction_proactiveness=compaction_proactiveness,cmdline=open("/proc/cmdline").read().strip(),possible_cpus=open("/sys/devices/system/cpu/possible").read().strip())),flush=True)
scenario, seconds, host, port = sys.argv[1], float(sys.argv[2]), sys.argv[3], int(sys.argv[4])
stream = scenario in ('stream','race')
base=os.environ['OPENAI_BASE_URL'].rstrip('/')
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
size=192*1024*1024
memory=bytearray(size)
block=bytes(range(256))*256
for offset in range(0,size,len(block)): memory[offset:offset+len(block)]=block
expected=hashlib.sha256(memory).hexdigest()
sock=socket.create_connection((host,port),timeout=seconds+30)
sock.settimeout(None)
reader=sock.makefile('rb')
assert reader.readline()==b'R\n'
lock=threading.Lock(); stop=threading.Event(); timer_start=threading.Event()
timer_gaps=[]; timer_spans=[]; network_events=[]
def send(value):
    with lock: sock.sendall((json.dumps(value)+'\n').encode())
def network():
    for line in reader:
        value=line.decode().strip()
        if value=='T': timer_start.set()
        elif value.startswith('W '):
            seq=int(value.split()[1]); network_events.append(seq)
            send(dict(kind='network_ack',seq=seq))
        elif value=='Q': break
worker=threading.Thread(target=network,daemon=True); worker.start()
def timer():
    timer_start.wait()
    period=.001 if scenario=='short' else .25
    while not stop.is_set():
        start=time.monotonic(); time.sleep(period)
        end=time.monotonic()
        timer_gaps.append((end-start)*1000)
        timer_spans.append((start,end))
timer_worker=None
if scenario in ('timer','race','short','sched'):
    timer_worker=threading.Thread(target=timer,daemon=True); timer_worker.start()
scheduler_child=None; scheduler_result=None
if scenario=='sched':
    import subprocess
    # Isolated test guest only: remove RT throttling so fair-server protection,
    # rather than the RT throttle, must let the ordinary Python timer run.
    rt_path='/proc/sys/kernel/sched_rt_runtime_us'
    with open(rt_path) as f: old_rt_runtime=f.read()
    with open(rt_path,'w') as f: f.write('-1')
    scheduler_child=subprocess.Popen([sys.executable,'-c',
        'import os,time,json; time.sleep(2); os.sched_setscheduler(0,os.SCHED_FIFO,os.sched_param(50)); start=time.monotonic(); end=start+4\n'
        'while time.monotonic()<end: pass\n'
        'print(json.dumps(dict(start=start,end=time.monotonic())))'],stdout=subprocess.PIPE,text=True)
psi_children=[]; psi_events={}; psi_fds=[]; psi_poll_stop=threading.Event()
def start_psi_test():
    global psi_before, psi_poller, pressure
    import ctypes, select, subprocess
    assert seconds>=10
    def pressure():
        line=open('/proc/pressure/cpu').readline()
        return {k:float(v) for k,v in (item.split('=') for item in line.split()[1:])}
    psi_before=pressure()
    # Exercise both aggregators. File credentials, not write-time credentials,
    # select privileged RT polling vs unprivileged averages polling.
    class CapHeader(ctypes.Structure):
        _fields_=[('version',ctypes.c_uint32),('pid',ctypes.c_int)]
    class CapData(ctypes.Structure):
        _fields_=[('effective',ctypes.c_uint32),('permitted',ctypes.c_uint32),('inheritable',ctypes.c_uint32)]
    libc=ctypes.CDLL(None,use_errno=True); hdr=CapHeader(0x20080522,0); caps=(CapData*2)()
    assert libc.capget(ctypes.byref(hdr),caps)==0
    saved=caps[0].effective; assert saved & (1<<24), 'CAP_SYS_RESOURCE required by PSI probe'
    rtfd=os.open('/proc/pressure/cpu',os.O_RDWR|os.O_NONBLOCK)
    caps[0].effective=saved & ~(1<<24)
    assert libc.capset(ctypes.byref(hdr),caps)==0
    try: avgfd=os.open('/proc/pressure/cpu',os.O_RDWR|os.O_NONBLOCK)
    finally:
        caps[0].effective=saved
        assert libc.capset(ctypes.byref(hdr),caps)==0
    for label,fd in [('rtpoll',rtfd),('averages',avgfd)]:
        assert os.write(fd,b'some 10000 2000000\n')==19
        psi_fds.append(fd); psi_events[label]=[]
    def poll_pressure():
        poll=select.poll(); labels={rtfd:'rtpoll',avgfd:'averages'}
        for fd in labels: poll.register(fd,select.POLLPRI)
        while not psi_poll_stop.is_set():
            for fd,events in poll.poll(100):
                if events & select.POLLPRI: psi_events[labels[fd]].append(time.monotonic())
    psi_poller=threading.Thread(target=poll_pressure); psi_poller.start()
    # Two runnable processes contend on one vCPU; delayed start leaves an idle
    # phase first, then pressure must restart aggregation without Agent hooks.
    for _ in range(2):
        psi_children.append(subprocess.Popen([sys.executable,'-c',
            'import time; time.sleep(2); end=time.monotonic()+4\nwhile time.monotonic()<end: pass']))
psi_setup_thread=None
if scenario=='psi':
    start_psi_test()
elif scenario=='psi-late':
    assert seconds>=18
    def late_setup():
        time.sleep(5)
        start_psi_test()
    psi_setup_thread=threading.Thread(target=late_setup)
    psi_setup_thread.start()
elif scenario=='psi-stats':
    import subprocess
    assert seconds>=20
    def pressure_stats():
        result={}
        for resource in ('cpu','memory','io'):
            result[resource]={}
            for line in open('/proc/pressure/'+resource):
                parts=line.split()
                result[resource][parts[0]]={k:float(v) for k,v in (v.split('=') for v in parts[1:])}
        return result
    # Known pressure before the quiet request, no triggers and no polling during
    # the wait. Read only at the two boundaries to check decay and monotonicity.
    children=[subprocess.Popen([sys.executable,'-S','-c',
        'import time; end=time.monotonic()+2\nwhile time.monotonic()<end: pass']) for _ in range(2)]
    for child in children: assert child.wait(timeout=6)==0
    time.sleep(2.2)
    stats_before=pressure_stats()
    assert stats_before['cpu']['some']['avg10']>0, stats_before
    stats_started=time.monotonic()
started=time.monotonic()
received=[]; raw=bytearray(); done=False
if scenario in ('download','tunnel'):
    if scenario=='tunnel':
        import http.client, urllib.parse
        proxy=urllib.parse.urlparse(os.environ['HTTP_PROXY'])
        target=urllib.parse.urlparse(os.environ['PVISOR_PROBE_DOWNLOAD_URL'])
        conn=http.client.HTTPConnection(proxy.hostname,proxy.port,timeout=seconds+30)
        conn.set_tunnel(target.hostname,target.port)
        conn.request('GET','/payload'); raw.extend(conn.getresponse().read()); conn.close()
    else:
        with urllib.request.urlopen(os.environ['PVISOR_PROBE_DOWNLOAD_URL'],timeout=seconds+30) as response:
            raw.extend(response.read())
    assert raw==bytes(range(256))*2048
    send(dict(kind='final_ack',seq=0))
elif scenario=='image':
    with open('/workspace/payload.bin','rb') as source: raw.extend(source.read())
    assert raw==bytes([42])*(3*1024*1024)
    send(dict(kind='final_ack',seq=0))
else:
    req=urllib.request.Request(base+'/chat/completions',data=json.dumps(dict(model='fixture',messages=[dict(role='user',content='reason')],stream=stream)).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer '+os.environ['OPENAI_API_KEY']})
    received=[]; raw=bytearray(); done=False; finished=False
    with opener.open(req,timeout=seconds+30) as response:
        if stream:
            for line in response:
                raw.extend(line)
                if not line.startswith(b'data: '): continue
                if line.strip()==b'data: [DONE]': done=True; continue
                value=json.loads(line[6:]); content=value['choices'][0]['delta']['content']
                finished = finished or value['choices'][0].get('finish_reason')=='stop'
                seq=int(content.split(':',1)[0]); assert seq==len(received), (seq,len(received))
                assert content==('%04d:'%seq)+'x'*512
                received.append(seq); send(dict(kind='chunk_ack',seq=seq))
            assert done
            assert finished, 'missing model protocol terminal'
            assert len(received)==int(seconds/.35), len(received)
        else:
            result=json.load(response); assert result['choices'][0]['message']['content']=='reasoning-ok'
            send(dict(kind='final_ack',seq=0))
response_finished=time.monotonic()
response_ms=(response_finished-started)*1000
stop.set()
if timer_worker:
    timer_worker.join(timeout=2); assert not timer_worker.is_alive()
    timer_gaps=[(b-a)*1000 for a,b in timer_spans if b<=response_finished]
if scheduler_child:
    child_output=scheduler_child.communicate(timeout=8)[0]
    assert scheduler_child.returncode==0, scheduler_child.returncode
    scheduler_result=json.loads(child_output)
    with open(rt_path,'w') as f: f.write(old_rt_runtime)
    progress=[(a,b) for a,b in timer_spans if scheduler_result['start']<b<scheduler_result['end']]
    assert len(progress)>=3, (scheduler_result,progress)
    assert max((b-a)*1000 for a,b in progress)<1500, progress
    scheduler_result['fair_timer_progress']=len(progress)
    scheduler_result['max_fair_timer_gap_ms']=max((b-a)*1000 for a,b in progress)
    print('scheduler-stress '+json.dumps(scheduler_result),flush=True)
if scenario in ('psi','psi-late'):
    if psi_setup_thread:
        psi_setup_thread.join(timeout=2); assert not psi_setup_thread.is_alive()
    for child in psi_children:
        assert child.wait(timeout=2)==0
    psi_poll_stop.set(); psi_poller.join(timeout=2); assert not psi_poller.is_alive()
    psi_after=pressure()
    for fd in psi_fds: os.close(fd)
    assert psi_after['total']>psi_before['total']+100000, (psi_before,psi_after)
    assert psi_after['avg10']>0, psi_after
    assert all(psi_events.values()), psi_events
    print('psi-semantics '+json.dumps(dict(before=psi_before,after=psi_after,events=psi_events)),flush=True)
if scenario=='psi-stats':
    stats_after=pressure_stats()
    for resource in stats_before:
        for state in stats_before[resource]:
            assert stats_after[resource][state]['total']>=stats_before[resource][state]['total']
    assert stats_after['cpu']['some']['avg10']<stats_before['cpu']['some']['avg10'], (stats_before,stats_after)
    print('psi-statistics '+json.dumps(dict(before=stats_before,after=stats_after,elapsed_s=time.monotonic()-stats_started)),flush=True)
restore_start=time.monotonic(); assert hashlib.sha256(memory).hexdigest()==expected
scan_ms=(time.monotonic()-restore_start)*1000
send(dict(kind='guest_done',seq=0))
print(json.dumps(dict(scenario=scenario,kernel=os.uname().release,request_count=int(scenario not in ('download','tunnel','image')),stream=stream,response_ms=response_ms,stream_chunks=len(received),stream_sha256=hashlib.sha256(raw).hexdigest(),payload_bytes=len(raw),timer_gaps_ms=timer_gaps,network_events=network_events,memory_bytes=size,memory_sha256=expected,post_response_scan_ms=scan_ms)),flush=True)
print('reasoning-rounds-guest-ok',flush=True)
"#;

    #[derive(Clone)]
    struct Trace {
        started: Instant,
        unix_ms: u128,
        rows: Arc<Mutex<Vec<Value>>>,
    }
    impl Trace {
        fn record(&self, kind: &str, seq: usize) {
            self.rows.lock().unwrap().push(
                json!({"kind":kind,"seq":seq,"ms":self.started.elapsed().as_secs_f64()*1000.0,"unix_ms":self.unix_ms as f64+self.started.elapsed().as_secs_f64()*1000.0}),
            );
        }
    }
    #[derive(Clone)]
    struct Fixture {
        scenario: String,
        seconds: f64,
        stream_initial_delay: Duration,
        trace: Trace,
        network: mpsc::Sender<String>,
        paused: watch::Receiver<usize>,
    }
    async fn model(State(f): State<Fixture>, Json(body): Json<Value>) -> Response {
        f.trace.record("model_request", 0);
        let _ = f.network.send("T\n".into()).await;
        let wake = if matches!(f.scenario.as_str(), "network" | "race") {
            let mut w = f.clone();
            Some(tokio::spawn(async move {
                for seq in 0..(w.seconds / 1.7) as usize {
                    tokio::time::sleep(Duration::from_millis(1700)).await;
                    let prior = *w.paused.borrow_and_update();
                    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
                        while *w.paused.borrow_and_update() <= prior {
                            w.paused.changed().await?;
                        }
                        Ok::<_, anyhow::Error>(())
                    })
                    .await;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    w.trace.record("network_send", seq);
                    if w.network.send(format!("W {seq}\n")).await.is_err() {
                        break;
                    }
                }
            }))
        } else {
            None
        };
        if body["stream"] == true {
            let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(8);
            tokio::spawn(async move {
                tokio::time::sleep(f.stream_initial_delay).await;
                for seq in 0..(f.seconds / 0.35) as usize {
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    let terminal = seq + 1 == (f.seconds / 0.35) as usize;
                    let value = json!({"id":"fixture","object":"chat.completion.chunk","created":0,"model":"fixture","choices":[{"index":0,"delta":{"content":format!("{seq:04}:{}","x".repeat(512))},"finish_reason":if terminal {Some("stop")} else {None}}]});
                    f.trace.record("provider_chunk", seq);
                    if tx
                        .send(Ok(axum::body::Bytes::from(format!("data: {value}\n\n"))))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                f.trace.record("provider_eof", 0);
                let _ = tx
                    .send(Ok(axum::body::Bytes::from_static(b"data: [DONE]\n\n")))
                    .await;
                if let Some(wake) = wake {
                    wake.abort();
                }
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(ReceiverStream::new(rx)))
                .unwrap()
        } else {
            tokio::time::sleep(Duration::from_secs_f64(f.seconds)).await;
            if let Some(wake) = wake {
                wake.abort();
            }
            f.trace.record("provider_final", 0);
            Json(json!({"id":"fixture","object":"chat.completion","created":0,"model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"reasoning-ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})).into_response()
        }
    }

    async fn download(State(f): State<Fixture>) -> Response {
        f.trace.record("download_request", 0);
        tokio::time::sleep(Duration::from_secs_f64(f.seconds / 2.0)).await;
        let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
        tokio::spawn(async move {
            for seq in 0..8 {
                tokio::time::sleep(Duration::from_secs_f64(f.seconds / 16.0)).await;
                f.trace.record("download_chunk", seq);
                let bytes: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
                if tx.send(Ok(bytes.into())).await.is_err() {
                    break;
                }
            }
            f.trace.record("provider_final", 0);
        });
        Response::builder()
            .header("content-length", 524288)
            .body(Body::from_stream(ReceiverStream::new(rx)))
            .unwrap()
    }

    pub async fn run() -> anyhow::Result<()> {
        let scenario = std::env::args().nth(1).context("scenario required")?;
        let policy = std::env::args()
            .nth(2)
            .context("baseline|pause|offload required")?;
        let seconds: f64 = std::env::args().nth(3).unwrap_or("12".into()).parse()?;
        ensure!(
            [
                "long",
                "timer",
                "network",
                "stream",
                "race",
                "short",
                "download",
                "tunnel",
                "image",
                "sched",
                "psi",
                "psi-late",
                "psi-stats"
            ]
            .contains(&scenario.as_str()),
            "unknown scenario"
        );
        ensure!(
            ["baseline", "pause", "offload"].contains(&policy.as_str()),
            "unknown policy"
        );
        ensure!((3.0..=180.0).contains(&seconds), "seconds 3..180");
        let root = PathBuf::from(std::env::var_os("CASE_ROOT").context("CASE_ROOT required")?);
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let host = std::env::var("IDLE_FIXTURE_HOST").context("IDLE_FIXTURE_HOST required")?;
        let trace = Trace {
            started: Instant::now(),
            unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
            rows: Arc::new(Mutex::new(Vec::new())),
        };
        let network = tokio::net::TcpListener::bind((host.as_str(), 0)).await?;
        let port = network.local_addr()?.port();
        let (network_tx, mut network_rx) = mpsc::channel::<String>(16);
        let net_trace = trace.clone();
        let net_task = tokio::spawn(async move {
            let (socket, _) = network.accept().await?;
            let (read, mut write) = socket.into_split();
            write.write_all(b"R\n").await?;
            let mut lines = BufReader::new(read).lines();
            loop {
                tokio::select! {
                    line=lines.next_line()=>{
                        let Some(line)=line? else {break;};
                        let value:Value=serde_json::from_str(&line)?;
                        net_trace.record(value["kind"].as_str().context("ack kind")?,value["seq"].as_u64().unwrap_or(0) as usize);
                    }
                    command=network_rx.recv()=>{
                        let Some(command)=command else {break;};
                        write.write_all(command.as_bytes()).await?;
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        });
        let model_listener = tokio::net::TcpListener::bind((host.as_str(), 0)).await?;
        let model_port = model_listener.local_addr()?.port();
        let (paused, paused_rx) = watch::channel(0);
        let stream_initial_delay_ms: u64 =
            std::env::var("PVISOR_PROBE_STREAM_INITIAL_DELAY_MS").map_or(Ok(0), |v| v.parse())?;
        ensure!(
            stream_initial_delay_ms <= 60_000,
            "initial stream delay <=60000 ms"
        );
        let fixture = Fixture {
            stream_initial_delay: Duration::from_millis(stream_initial_delay_ms),
            scenario: scenario.clone(),
            seconds,
            trace: trace.clone(),
            network: network_tx,
            paused: paused_rx,
        };
        let model_task = tokio::spawn(async move {
            axum::serve(
                model_listener,
                Router::new()
                    .route("/v1/chat/completions", post(model))
                    .route("/payload", get(download))
                    .with_state(fixture),
            )
            .await
        });
        let proxy = pvisor_gateway::config::ProxyConfig::from_toml_str(&format!(
            r#"
listen="127.0.0.1:0"
admin_listen="127.0.0.1:0"
[[models]]
name="fixture"
upstream="http://{host}:{model_port}/v1"
provider="openai"
api_key="local-fixture"
"#
        ))?;
        // Host-only fixture knob; model requests and guest checks stay identical.
        let buffer_wait_ms: u64 =
            std::env::var("PVISOR_PROBE_BUFFER_WAIT_MS").map_or(Ok(250), |v| v.parse())?;
        ensure!(
            (1..=60_000).contains(&buffer_wait_ms),
            "buffer wait 1..60000 ms"
        );
        let mut gateway = GatewayDriverConfig::new(proxy);
        if policy != "baseline" {
            gateway = gateway.cooperative_model_wait(CooperativeModelWait {
                require_guest_idle: true,
                downloads: matches!(scenario.as_str(), "download" | "tunnel"),
                filesystem_reads: scenario == "image",
                pause_after: Duration::from_millis(20),
                offload_after: (policy == "offload").then(|| Duration::from_millis(50)),
                stream_buffer: Some(ModelStreamBuffer {
                    max_bytes: 64 * 1024,
                    max_wait: Duration::from_millis(buffer_wait_ms),
                }),
                rounds: Some(ModelWaitRounds {
                    recheck_after: Duration::from_millis(100),
                    offload_cooldown: Duration::from_secs(5),
                }),
            });
        }
        let sink = Arc::new(MemoryEventSink::default());
        let settings = VmSettings {
            rootfs: Some(
                std::env::var_os("PVISOR_CASE_ROOTFS")
                    .context("rootfs required")?
                    .into(),
            ),
            library_dir: std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(Into::into),
            cpus: 1,
            memory_mib: 512,
            ram_backing: Some(
                std::env::var_os("PVISOR_PROBE_RAM_BACKING")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| root.join("live.ram")),
            ),
            ..Default::default()
        };
        let disable_compaction =
            std::env::var("PVISOR_PROBE_DISABLE_COMPACTION").map_or(Ok(true), |v| {
                match v.as_str() {
                    "1" => Ok(true),
                    "0" => Ok(false),
                    _ => anyhow::bail!("compaction selector must be 0 or 1"),
                }
            })?;
        let minimum_ms: u64 = std::env::var("PVISOR_PROBE_OFFLOAD_MINIMUM_MS")
            .unwrap_or_else(|_| "1000".into())
            .parse()?;
        let vm = VmExecutor::new(settings)?.with_guest_idle_handoff_policy(
            disable_compaction,
            Duration::from_millis(minimum_ms),
        )?;
        let lazy = if scenario == "image" {
            let socket = std::env::var("PVISOR_PROBE_LAZY_SOCKET")?;
            let config = pvisor::cache::CacheConfig {
                backend: pvisor::cache::CacheBackend::Server,
                location: format!("unix://{socket}"),
                read_only: false,
                image_store: Some(root.join("image-store")),
            };
            let handle = format!(
                "pvisor-v1:{}:linux-amd64:{}",
                "a".repeat(64),
                "b".repeat(64)
            );
            Some(
                tokio::task::spawn_blocking(move || {
                    pvisor::cache::open_image_handle_for_vm(config, &handle)
                })
                .await??,
            )
        } else {
            None
        };
        let mut lowers = vec![workspace.clone()];
        if let Some(lazy) = &lazy {
            lowers.push(lazy.rootfs().to_owned());
        }
        let runtime = PVisor::builder()
            .executors(vec![Arc::new(vm)])
            .storage(root.join("run"))
            .event_sink(sink.clone())
            .overlay(OverlayHint {
                lower_dirs: lowers,
                stage_dir: Some(root.join("stage")),
                ..Default::default()
            })
            .network(pvisor::NetworkDriverConfig::new(
                pvisor::OverlayNetMode::Auto,
                pvisor_core::NetworkConfig {
                    mode: pvisor_core::NetworkMode::Allowlist,
                    allowed_hosts: vec![host.clone()],
                    ..Default::default()
                },
            ))
            .gateway(gateway)
            .build();
        let mut spec = RunSpec::process(
            "reasoning-rounds",
            &scenario,
            std::env::var("PVISOR_CASE_VM_PYTHON").unwrap_or("/opt/miniconda3/bin/python".into()),
        );
        spec.runtime.timeout_ms = Some((seconds * 1000.0) as u64 + 90_000);
        let RunInvocation::Process(p) = &mut spec.invocation;
        p.args = vec![
            "-B".into(),
            "-u".into(),
            "-c".into(),
            GUEST.into(),
            scenario.clone(),
            seconds.to_string(),
            host.clone(),
            port.to_string(),
        ];
        p.env.insert(
            "PVISOR_PROBE_DOWNLOAD_URL".into(),
            format!("http://{host}:{model_port}/payload"),
        );
        p.cwd = Some("/workspace".into());
        p.stdout = StdioMode::Capture;
        p.stderr = StdioMode::Capture;
        spec.metadata
            .insert("pvisor.workspace".into(), json!(workspace));
        spec.metadata
            .insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
        spec.metadata
            .insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
        let handle = runtime.run(spec).await?;
        let monitor_sink = sink.clone();
        let monitor = tokio::spawn(async move {
            loop {
                let count = monitor_sink
                    .events()
                    .iter()
                    .filter(|e| {
                        e.name() == "vm.control_completed"
                            && e.observation_payload().is_some_and(|v| {
                                v["value"]["vm"]["state"] == "paused"
                                    && v["operation"]["kind"]["op"] == "run.pause"
                            })
                    })
                    .count();
                if count > *paused.borrow() {
                    paused.send_replace(count);
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        let result = handle.wait().await;
        monitor.abort();
        net_task.abort();
        model_task.abort();
        let events = sink.events();
        std::fs::write(
            root.join("events.json"),
            serde_json::to_vec_pretty(&events)?,
        )?;
        std::fs::write(
            root.join("delivery.json"),
            serde_json::to_vec_pretty(&*trace.rows.lock().unwrap())?,
        )?;
        let result = result?;
        std::fs::write(
            root.join("result.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
        let stdout = result.output.stdout.as_deref().unwrap_or_default();
        println!("{stdout}");
        let stderr = result.output.stderr.as_deref().unwrap_or_default();
        std::fs::write(root.join("runner-stderr.txt"), stderr)?;
        let controls: Vec<_> = events
            .iter()
            .filter(|e| e.name() == "vm.control_completed")
            .filter_map(|e| e.observation_payload())
            .collect();
        let pauses = controls
            .iter()
            .filter(|v| {
                v["value"]["vm"]["state"] == "paused" && v["operation"]["kind"]["op"] == "run.pause"
            })
            .count();
        let resumes = controls
            .iter()
            .filter(|v| {
                v["value"]["vm"]["state"] == "running"
                    && v["operation"]["kind"]["op"] == "run.resume"
            })
            .count();
        let offloads = controls
            .iter()
            .filter(|v| v["value"]["vm"]["state"] == "offloaded")
            .count();
        let deadline_skips = events
            .iter()
            .filter(|e| e.name() == "vm.guest_idle")
            .filter_map(|e| e.observation_payload())
            .filter_map(|p| p["offload_deadline_skips"].as_u64())
            .max()
            .unwrap_or(0);
        let boot_policy = stdout
            .lines()
            .find_map(|l| l.strip_prefix("guest-boot-policy "))
            .context("missing guest boot policy")?;
        let boot_policy: Value = serde_json::from_str(boot_policy)?;
        if disable_compaction {
            ensure!(
                boot_policy["compaction_proactiveness"] == 0,
                "Host compaction policy not applied"
            );
        }
        let report = json!({"scenario":scenario,"policy":policy,"seconds":seconds,"pauses":pauses,"resumes":resumes,"offloads":offloads,"deadline_skips":deadline_skips,"disable_proactive_compaction":disable_compaction,"offload_minimum_ms":minimum_ms,"guest_boot_policy":boot_policy,"buffer_bytes":65536,"buffer_wait_ms":buffer_wait_ms,"stream_initial_delay_ms":stream_initial_delay_ms,"recheck_ms":100,"offload_cooldown_ms":5000});
        std::fs::write(
            root.join("summary.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("{report}");
        ensure!(
            result.state == RunState::Completed && result.exit_code == Some(0),
            "guest failed: {result:?}"
        );
        ensure!(
            stdout.contains("reasoning-rounds-guest-ok"),
            "missing guest completion"
        );
        ensure!(pauses == resumes, "unbalanced pause/resume");
        if matches!(scenario.as_str(), "stream" | "race") {
            ensure!(
                events
                    .iter()
                    .filter(|e| e.name() == "llm.response.stream")
                    .count()
                    == 1,
                "stream must have exactly one successful Gateway completion"
            );
            ensure!(
                !events.iter().any(|e| e.name() == "llm.call.cancelled"),
                "guest completion disagrees with Gateway cancellation"
            );
        }
        if matches!(scenario.as_str(), "network" | "race") {
            let delivery = trace.rows.lock().unwrap();
            let sent: Vec<_> = delivery
                .iter()
                .filter(|r| r["kind"] == "network_send")
                .map(|r| r["seq"].clone())
                .collect();
            let acked: Vec<_> = delivery
                .iter()
                .filter(|r| r["kind"] == "network_ack")
                .map(|r| r["seq"].clone())
                .collect();
            ensure!(
                !sent.is_empty() && sent == acked,
                "network delivery incomplete: {sent:?} vs {acked:?}"
            );
        }
        if policy != "baseline" && scenario != "short" {
            ensure!(pauses >= 1, "idle path not exercised");
        }
        if policy == "offload" && scenario != "short" {
            ensure!(
                offloads > 0 || deadline_skips > 0,
                "no offload or deadline admission decision"
            );
        }
        Ok(())
    }
}
fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    #[cfg(feature = "gateway")]
    {
        tokio::runtime::Runtime::new()?.block_on(probe::run())
    }
    #[cfg(not(feature = "gateway"))]
    {
        anyhow::bail!("requires --features gateway")
    }
}
