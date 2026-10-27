// Adapted from pinned Podman volume, start, logs and networking scenarios.
// Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
package integration

import (
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
)

// Fast workloads can complete and auto-remove between startup observations.
// Their committed generation result must still determine status and streams.
func TestRapidAutoRemovalKeepsExactExitAndStreams(t *testing.T) {
	e := newEngine(t)
	var workers sync.WaitGroup
	for worker := 0; worker < 4; worker++ {
		workers.Add(1)
		go func(worker int) {
			defer workers.Done()
			name := fmt.Sprintf("%s-fast-%d", e.name, worker)
			defer e.cleanup(name)
			for round := 0; round < 64; round++ {
				wantCode := []int{0, 7, 17}[round%3]
				wantOut := fmt.Sprintf("out-%d-%d", worker, round)
				wantErr := fmt.Sprintf("err-%d-%d", worker, round)
				script := fmt.Sprintf("printf %s; printf %s >&2; exit %d", wantOut, wantErr, wantCode)
				code, out, stderr := e.call("", "run", "--rm", "--name", name,
					"--cpus", "1", "--memory", "256m", "--network", "none", e.image, "sh", "-c", script)
				if code != wantCode || out != wantOut || stderr != wantErr {
					t.Errorf("worker %d round %d: code=%d want=%d stdout=%q want=%q stderr=%q want=%q",
						worker, round, code, wantCode, out, wantOut, stderr, wantErr)
					return
				}
			}
		}(worker)
	}
	workers.Wait()
}

// Source: system/160-volumes.bats and e2e/run_volume_test.go (anonymous volumes).
// Observe persistence and isolation through exec, without volume inspect/list.
func TestAnonymousVolumeSurvivesStopStart(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-v", "/data", e.image, "sleep", "300")
	e.want(0, "", "exec", e.name, "sh", "-c", "echo retained > /data/value")
	e.want(0, "", "stop", "-t", "1", e.name)
	e.want(0, "", "start", e.name)
	out, _ := e.want(0, "", "exec", e.name, "cat", "/data/value")
	if out != "retained\n" {
		t.Fatalf("anonymous volume lost on restart: %q", out)
	}
}

func TestAnonymousVolumesAreIsolated(t *testing.T) {
	e := newEngine(t)
	second := e.name + "-second"
	t.Cleanup(func() { e.cleanup(second) })
	for _, name := range []string{e.name, second} {
		e.want(0, "", "run", "-d", "--name", name, "-v", "/data", e.image, "sleep", "300")
		e.want(0, "", "exec", name, "test", "!", "-e", "/data/value")
		e.want(0, "", "exec", name, "sh", "-c", "printf '%s' \"$1\" > /data/value", "sh", name)
	}
	for _, name := range []string{e.name, second} {
		out, _ := e.want(0, "", "exec", name, "cat", "/data/value")
		if out != name {
			t.Fatalf("anonymous volumes overlap: %s contains %q", name, out)
		}
	}
}

// Regression: image entrypoints chown managed data before dropping privileges.
func TestManagedVolumePreservesNonRootOwnershipAndPermissions(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("Linux managed filesystem contract")
	}
	e := newEngine(t)
	second := e.name + "-reader"
	t.Cleanup(func() { e.cleanup(second) })
	volume := e.name + "-owned"
	e.want(0, "", "run", "-d", "--name", e.name, "-v", volume+":/data", e.image, "sleep", "300")
	e.want(0, "", "exec", e.name, "sh", "-c", "chown 1234:2345 /data && chmod 2770 /data")
	out, _ := e.want(0, "", "exec", "-u", "1234:2345", e.name, "sh", "-c", "printf retained > /data/value && chmod 640 /data/value && stat -c '%u:%g:%a' /data/value")
	if out != "1234:2345:640\n" {
		t.Fatalf("created metadata: %q", out)
	}
	e.want(0, "", "exec", "-u", "1235:2346", e.name, "sh", "-c", "if cat /data/value; then exit 1; fi")
	e.want(0, "", "stop", "-t", "1", e.name)
	e.want(0, "", "start", e.name)
	out, _ = e.want(0, "", "exec", "-u", "1234:2345", e.name, "sh", "-c", "test $(stat -c %u /data/value) = 1234 && cat /data/value")
	if out != "retained" {
		t.Fatalf("restart lost data/identity: %q", out)
	}
	e.want(0, "", "run", "-d", "--name", second, "-v", volume+":/data:ro", e.image, "sleep", "300")
	out, _ = e.want(0, "", "exec", "-u", "1234:2345", second, "cat", "/data/value")
	if out != "retained" {
		t.Fatalf("read-only peer data: %q", out)
	}
	e.want(0, "", "exec", second, "sh", "-c", "if chown 0:0 /data/value; then exit 1; fi; if touch /data/forbidden; then exit 2; fi")
	e.want(0, "", "exec", e.name, "sh", "-c", "chmod 000 /data/value && printf root-access > /data/value && test $(stat -c %a /data/value) = 0")
	e.want(0, "", "exec", "-u", "1234:2345", e.name, "sh", "-c", "if cat /data/value; then exit 1; fi")
	e.want(0, "", "exec", e.name, "sh", "-c", "chmod 640 /data/value && ln /data/value /data/alias && chown 1234:2345 /data/alias && test $(stat -c %u /data/value) = 1234")
}

// Source: system/160-volumes.bats (shared files and read-only mounting).
func TestReadOnlyNamedVolumeSeesOtherVMWrites(t *testing.T) {
	e := newEngine(t)
	second := e.name + "-reader"
	t.Cleanup(func() { e.cleanup(second) })
	volume := e.name + "-shared"
	e.want(0, "", "run", "-d", "--name", e.name, "-v", volume+":/data", e.image, "sleep", "300")
	e.want(0, "", "exec", e.name, "sh", "-c", "printf before > /data/value")
	e.want(0, "", "run", "-d", "--name", second, "-v", volume+":/data:ro", e.image, "sleep", "300")
	for _, value := range []string{"before", "a-longer-update", "end"} {
		e.want(0, "", "exec", e.name, "sh", "-c", "printf '%s' \"$1\" > /data/value", "sh", value)
		out, _ := e.want(0, "", "exec", second, "cat", "/data/value")
		if out != value {
			t.Fatalf("read-only share returned stale contents/length: %q, want %q", out, value)
		}
	}
	e.want(0, "", "exec", second, "sh", "-c", "if touch /data/forbidden; then exit 1; fi")
	e.want(0, "", "exec", e.name, "test", "!", "-e", "/data/forbidden")
}

// Source: system/160-volumes.bats (persistent named volumes).
func TestNamedVolumeIsSharedAcrossContainers(t *testing.T) {
	e := newEngine(t)
	second := e.name + "-second"
	t.Cleanup(func() { e.cleanup(second) })
	for _, name := range []string{e.name, second} {
		e.want(0, "", "run", "-d", "--name", name, "-v", e.name+"-shared:/data", e.image, "sleep", "300")
	}
	for _, name := range []string{e.name, second} {
		e.want(0, "", "exec", name, "sh", "-c", "printf '%s' \"$1\" > /data/value", "sh", name)
		for _, reader := range []string{e.name, second} {
			out, _ := e.want(0, "", "exec", reader, "cat", "/data/value")
			if out != name {
				t.Fatalf("shared volume: %s reads %q, want %q", reader, out, name)
			}
		}
	}
	e.want(0, "", "exec", e.name, "test", "!", "-e", "/data/new")
	e.want(0, "", "exec", second, "sh", "-c", "printf created > /data/new")
	out, _ := e.want(0, "", "exec", e.name, "cat", "/data/new")
	if out != "created" {
		t.Fatalf("shared volume hid a new directory entry: %q", out)
	}
	e.want(0, "", "exec", second, "mv", "/data/new", "/data/renamed")
	e.want(0, "", "exec", e.name, "sh", "-c", "test ! -e /data/new && test -f /data/renamed")
	e.want(0, "", "exec", second, "rm", "/data/renamed")
	e.want(0, "", "exec", e.name, "test", "!", "-e", "/data/renamed")
}

// Source: e2e/run_volume_test.go (named volume image copy-up).
func TestNamedVolumeInitializationDoesNotOverwriteData(t *testing.T) {
	e := newEngine(t)
	volume := e.name + "-seed"
	e.want(0, "", "run", "--rm", "--name", e.name, "-v", volume+":/etc", e.image, "sh", "-c", "test -s /etc/alpine-release && printf modified > /etc/alpine-release")
	for _, destination := range []string{"/etc", "/data"} {
		out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", volume+":"+destination, e.image, "cat", destination+"/alpine-release")
		if out != "modified" {
			t.Fatalf("existing volume overwritten at %s: %q", destination, out)
		}
	}
}

// Source: e2e/run_volume_test.go (same volume in multiple places does not deadlock).
func TestSameVolumeAtMultipleDestinations(t *testing.T) {
	e := newEngine(t)
	volume := e.name + "-shared"
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", volume+":/first", "-v", volume+":/second", e.image, "sh", "-c", "printf shared > /first/value; cat /second/value; printf updated > /second/value; cat /first/value")
	if out != "sharedupdated" {
		t.Fatalf("same-volume aliases: %q", out)
	}
}

// Source: e2e/run_volume_test.go (multiple named volumes).
func TestMultipleVolumesKeepContentsSeparate(t *testing.T) {
	e := newEngine(t)
	first, second := e.name+"-first", e.name+"-second"
	e.want(0, "", "run", "--rm", "--name", e.name, "-v", first+":/first", "-v", second+":/second", e.image, "sh", "-c", "printf first > /first/value; printf second > /second/value")
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", first+":/second", "-v", second+":/first", e.image, "sh", "-c", "cat /first/value; cat /second/value")
	if out != "secondfirst" {
		t.Fatalf("volume identities followed mount order: %q", out)
	}
}

// Source: system/160-volumes.bats (host/container file visibility), extended to restart.
func TestBindMountPersistsAcrossRestart(t *testing.T) {
	e := newEngine(t)
	dir := t.TempDir()
	file := filepath.Join(dir, "value")
	if err := os.WriteFile(file, []byte("before"), 0600); err != nil {
		t.Fatal(err)
	}
	e.want(0, "", "run", "-d", "--name", e.name, "-v", dir+":/data", e.image, "sleep", "300")
	e.want(0, "", "exec", e.name, "sh", "-c", "test \"$(cat /data/value)\" = before")
	e.want(0, "", "stop", "-t", "1", e.name)
	if err := os.WriteFile(file, []byte("host-update"), 0600); err != nil {
		t.Fatal(err)
	}
	e.want(0, "", "start", e.name)
	out, _ := e.want(0, "", "exec", e.name, "sh", "-c", "cat /data/value; printf guest-update > /data/value")
	if out != "host-update" {
		t.Fatalf("restarted bind lost host changes: %q", out)
	}
	actual, err := os.ReadFile(file)
	if err != nil || string(actual) != "guest-update" {
		t.Fatalf("restarted bind lost guest writes: %q, %v", actual, err)
	}
}

// Source: system/160-volumes.bats (read-only writes), extended to mutation families.
func TestReadOnlyBindRejectsMutations(t *testing.T) {
	e := newEngine(t)
	dir := t.TempDir()
	file := filepath.Join(dir, "value")
	if err := os.WriteFile(file, []byte("retained"), 0644); err != nil {
		t.Fatal(err)
	}
	script := `for operation in 'printf changed > /data/value' 'touch /data/new' 'mkdir /data/dir' 'mv /data/value /data/moved' 'chmod 777 /data/value' 'rm /data/value'; do
  if sh -c "$operation"; then echo "unexpected success: $operation"; exit 1; fi
done
cat /data/value`
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", dir+":/data:ro", e.image, "sh", "-c", script)
	if out != "retained" {
		t.Fatalf("read-only mutation: %q", out)
	}
	actual, err := os.ReadFile(file)
	if err != nil || string(actual) != "retained" {
		t.Fatalf("read-only host contents: %q, %v", actual, err)
	}
	info, err := os.Stat(file)
	if err != nil || info.Mode().Perm() != 0644 {
		t.Fatalf("read-only host permissions: %v, %v", info, err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) != 1 || entries[0].Name() != "value" {
		t.Fatalf("read-only host directory changed: %v, %v", entries, err)
	}
}

// Source: e2e/run_volume_test.go (symlink preservation), adapted to a directory bind.
func TestBindMountPreservesRelativeSymlinks(t *testing.T) {
	e := newEngine(t)
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "value"), []byte("target"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("value", filepath.Join(dir, "link")); err != nil {
		t.Fatal(err)
	}
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", dir+":/data", e.image, "sh", "-c", "test -L /data/link && readlink /data/link && cat /data/link && printf updated > /data/link")
	if out != "value\ntarget" {
		t.Fatalf("guest symlink: %q", out)
	}
	actual, err := os.ReadFile(filepath.Join(dir, "value"))
	if err != nil || string(actual) != "updated" {
		t.Fatalf("symlink write lost: %q, %v", actual, err)
	}
	link, err := os.Readlink(filepath.Join(dir, "link"))
	if err != nil || link != "value" {
		t.Fatalf("host symlink replaced: %q, %v", link, err)
	}
}

// Source: e2e/logs_test.go (partial log lines, exact length and separate streams).
func TestLogsPreserveBinaryAndPartialLines(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(0, "", "run", "--name", e.name, e.image, "sh", "-c", `printf 'out\000\377partial'; printf 'err\000\376partial' >&2`)
	for i := 0; i < 3; i++ {
		if i > 0 {
			out, stderr = e.want(0, "", "logs", e.name)
		}
		if out != "out\x00\xffpartial" || stderr != "err\x00\xfepartial" {
			t.Fatalf("binary/partial streams call %d: %q %q", i, out, stderr)
		}
	}
}

// Source: system/035-logs.bats (logs after restarting) and e2e/start_test.go.
func TestLogsAppendAcrossStarts(t *testing.T) {
	e := newEngine(t)
	script := `count=$(cat /count 2>/dev/null || echo 0); count=$((count + 1)); echo "$count" > /count; printf 'out-%s\n' "$count"; printf 'err-%s\n' "$count" >&2; exit 7`
	e.want(7, "", "run", "--name", e.name, e.image, "sh", "-c", script)
	e.want(0, "", "start", e.name)
	deadline := time.Now().Add(5 * time.Second)
	for {
		out, _ := e.want(0, "", "ps", "-q")
		if out == "" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("restarted short workload did not exit")
		}
		time.Sleep(20 * time.Millisecond)
	}
	out, stderr := e.want(0, "", "logs", e.name)
	if out != "out-1\nout-2\n" || stderr != "err-1\nerr-2\n" {
		t.Fatalf("restart lost or duplicated logs: %q %q", out, stderr)
	}
	status, _ := e.want(0, "", "ps", "-a")
	if !strings.Contains(status, "Exited (7)") {
		t.Fatalf("restarted workload exit lost: %q", status)
	}
}

// Source: system/045-start.bats (start running container).
func TestStartRunningContainerDoesNotRerunEntrypoint(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, e.image, "sh", "-c", `echo started >> /starts; exec sleep 300`)
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do test -s /starts && exit 0; sleep 0.05; done; exit 1`)
	e.want(0, "", "start", e.name)
	out, _ := e.want(0, "", "exec", e.name, "cat", "/starts")
	if out != "started\n" {
		t.Fatalf("start reran a running entrypoint: %q", out)
	}
}

// Regression from lightweight chaos: simultaneous starts must share one launch.
func TestConcurrentStartSharesOneGeneration(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "--cpus", "1", "-m", "256m", e.image,
		"sh", "-c", `n=$(cat /count 2>/dev/null || echo 0); echo $((n+1)) > /count; exec sleep 600`)
	for generation := 1; generation <= 4; generation++ {
		e.want(0, "", "exec", e.name, "sh", "-c", fmt.Sprintf(`for i in $(seq 1 80); do test "$(cat /count)" = %d && exit 0; sleep .05; done; exit 1`, generation))
		e.want(0, "", "stop", "-t", "0", e.name)
		var pending sync.WaitGroup
		barrier := make(chan struct{})
		for i := 0; i < 2; i++ {
			pending.Add(1)
			go func() {
				defer pending.Done()
				<-barrier
				code, out, stderr := e.call("", "start", e.name)
				if code != 0 || out != e.name+"\n" || stderr != "" {
					t.Errorf("concurrent start: exit=%d out=%q err=%q", code, out, stderr)
				}
			}()
		}
		close(barrier)
		pending.Wait()
	}
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do test "$(cat /count)" = 5 && exit 0; sleep .05; done; exit 1`)
}

// Regression: observation must never win a detached owner's lease admission.
func TestConcurrentStartRemainsReliableDuringLogObservation(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "--cpus", "1", "-m", "256m", e.image,
		"sh", "-c", `n=$(cat /count 2>/dev/null || echo 0); echo $((n+1)) > /count; exec sleep 600`)
	stop := make(chan struct{})
	var observers sync.WaitGroup
	for i := 0; i < 4; i++ {
		observers.Add(1)
		go func() {
			defer observers.Done()
			for {
				select {
				case <-stop:
					return
				default:
				}
				code, _, stderr := e.call("", "logs", e.name)
				if code != 0 || stderr != "" {
					t.Errorf("log observation: exit=%d err=%q", code, stderr)
					return
				}
			}
		}()
	}
	defer func() { close(stop); observers.Wait() }()
	for generation := 1; generation <= 16; generation++ {
		e.want(0, "", "exec", e.name, "sh", "-c", fmt.Sprintf(`for i in $(seq 1 80); do test "$(cat /count)" = %d && exit 0; sleep .05; done; exit 1`, generation))
		e.want(0, "", "stop", "-t", "0", e.name)
		var pending sync.WaitGroup
		barrier := make(chan struct{})
		for i := 0; i < 4; i++ {
			pending.Add(1)
			go func() {
				defer pending.Done()
				<-barrier
				code, out, stderr := e.call("", "start", e.name)
				if code != 0 || out != e.name+"\n" || stderr != "" {
					t.Errorf("observed start: exit=%d out=%q err=%q", code, out, stderr)
				}
			}()
		}
		close(barrier)
		pending.Wait()
	}
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do test "$(cat /count)" = 17 && exit 0; sleep .05; done; exit 1`)
}

// Source: e2e/rm_test.go and run_test.go (--rm), extended to cleanup races.
func TestForceRemoveRacingAutomaticRemoval(t *testing.T) {
	e := newEngine(t)
	for i := 0; i < 8; i++ {
		e.want(7, "", "run", "--rm", "--name", e.name, e.image, "sh", "-c", "exit 7")
		// Either removal succeeds or auto-removal already removed the name.
		// The existing cleanup assertion rejects all other errors without retrying.
		e.cleanup(e.name)
	}
	out, _ := e.want(0, "", "ps", "-aq")
	if out != "" {
		t.Fatalf("automatic/forced removal left containers: %q", out)
	}
}

func (e *engine) publishedAddress() string {
	e.t.Helper()
	out, _ := e.want(0, "", "ps")
	match := regexp.MustCompile(`127\.0\.0\.1:([1-9][0-9]*)->8080/tcp`).FindStringSubmatch(out)
	if match == nil {
		e.t.Fatalf("published endpoint absent: %q", out)
	}
	return "127.0.0.1:" + match[1]
}

func (e *engine) echoReady() {
	e.t.Helper()
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do grep -q ':1F90 ' /proc/net/tcp && exit 0; sleep 0.05; done; exit 1`)
}

func (e *engine) echo(address, payload string) {
	e.t.Helper()
	client, err := net.DialTimeout("tcp4", address, time.Second)
	if err != nil {
		e.t.Fatal(err)
	}
	defer client.Close()
	if err := client.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		e.t.Fatal(err)
	}
	if _, err := io.WriteString(client, payload); err != nil {
		e.t.Fatal(err)
	}
	if err := client.(*net.TCPConn).CloseWrite(); err != nil {
		e.t.Fatal(err)
	}
	actual, err := io.ReadAll(client)
	if err != nil || string(actual) != payload {
		e.t.Fatalf("echo at %s: %q, want %q; err=%v", address, actual, payload, err)
	}
}

// Source: system/500-networking.bats (network after restart).
// Explicit stop/start replaces unsupported restart/policy commands.
func TestPublishedPortSurvivesRestartAndIsReleased(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-p", "127.0.0.1:0:8080", e.image, "busybox", "nc", "-lk", "-s", "127.0.0.1", "-p", "8080", "-e", "cat")
	e.echoReady()
	address := e.publishedAddress()
	e.echo(address, "before restart")
	e.want(0, "", "stop", "-t", "1", e.name)
	listener, err := net.Listen("tcp4", address)
	if err != nil {
		t.Fatalf("stopped container retained port: %v", err)
	}
	listener.Close()
	e.want(0, "", "start", e.name)
	e.echoReady()
	if actual := e.publishedAddress(); actual != address {
		t.Fatalf("published endpoint changed on restart: %s -> %s", address, actual)
	}
	e.echo(address, "after restart")
	e.want(0, "", "rm", "-f", e.name)
	listener, err = net.Listen("tcp4", address)
	if err != nil {
		t.Fatalf("removed container retained port: %v", err)
	}
	listener.Close()
}

// Source: system/500-networking.bats (localhost port conflict preserves first owner).
func TestPublishedPortConflictPreservesOwner(t *testing.T) {
	e := newEngine(t)
	second := e.name + "-conflict"
	t.Cleanup(func() { e.cleanup(second) })
	e.want(0, "", "run", "-d", "--name", e.name, "-p", "127.0.0.1:0:8080", e.image, "busybox", "nc", "-lk", "-s", "127.0.0.1", "-p", "8080", "-e", "cat")
	e.echoReady()
	address := e.publishedAddress()
	// Docker-style engine startup failures use 125; Podman's source expects 126.
	_, stderr := e.want(125, "", "run", "-d", "--name", second, "-p", address+":8080", e.image, "sleep", "300")
	if message := strings.ToLower(stderr); !strings.Contains(message, "address already in use") && !strings.Contains(message, "address in use") {
		t.Fatalf("port collision did not report its cause: %q", stderr)
	}
	e.echo(address, "original owner still works")
	e.want(0, "", "exec", e.name, "true")
}

// Source: system/500-networking.bats (multiple published ports).
func TestMultiplePublishedPortsReachSameGuestService(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-p", "127.0.0.1:0:8080", "-p", "127.0.0.1:0:8080", e.image, "busybox", "nc", "-lk", "-s", "127.0.0.1", "-p", "8080", "-e", "cat")
	e.echoReady()
	out, _ := e.want(0, "", "ps")
	matches := regexp.MustCompile(`127\.0\.0\.1:([1-9][0-9]*)->8080/tcp`).FindAllStringSubmatch(out, -1)
	if len(matches) != 2 || matches[0][1] == matches[1][1] {
		t.Fatalf("distinct published ports: %q", out)
	}
	for i, match := range matches {
		e.echo("127.0.0.1:"+match[1], fmt.Sprintf("endpoint-%d", i))
	}
}
