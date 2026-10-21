// Adapted from Podman test/e2e/{run,exec,start,stop,rm}_test.go.
// Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
package integration

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
)

type engine struct {
	t           *testing.T
	binary      string
	env         []string
	image, name string
}

func newEngine(t *testing.T) *engine {
	t.Helper()
	e := &engine{t: t, image: os.Getenv("PVISOR_TEST_IMAGE")}
	e.binary = os.Getenv("PVISOR_BINARY")
	if e.binary == "" {
		e.binary = "pvisor"
	}
	if e.image == "" {
		e.image = "docker.io/library/alpine:3.20"
	}
	dir, err := os.MkdirTemp("", "pvisor-contract-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		// --rm supervisors may still finish writing after the foreground exits.
		// Retry only our private directory; never touch the engine's shared store.
		deadline := time.Now().Add(5 * time.Second)
		for {
			err := os.RemoveAll(dir)
			if err == nil {
				return
			}
			if time.Now().After(deadline) {
				t.Errorf("remove private store: %v", err)
				return
			}
			time.Sleep(50 * time.Millisecond)
		}
	})
	e.name = fmt.Sprintf("pv-contract-%d-%s", os.Getpid(), filepath.Base(dir))
	// Replace any inherited store instead of creating duplicate environment keys.
	e.env = nil
	for _, value := range os.Environ() {
		if !strings.HasPrefix(value, "PVISOR_DOCKER_HOME=") {
			e.env = append(e.env, value)
		}
	}
	e.env = append(e.env, "PVISOR_DOCKER_HOME="+filepath.Join(dir, "store"))
	t.Cleanup(func() {
		code, out, stderr := e.call("", "rm", "-f", "-v", e.name)
		// --rm may have removed it; still clean up after partial startup.
		if code != 0 && !strings.Contains(strings.ToLower(out+stderr), "no such container") {
			t.Errorf("cleanup container %s exit %d: %s%s", e.name, code, out, stderr)
		}
	})
	e.want(0, "", "pull", e.image)
	return e
}

func (e *engine) call(input string, args ...string) (int, string, string) {
	e.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, e.binary, args...)
	cmd.Env = e.env
	cmd.Stdin = strings.NewReader(input)
	var out, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &stderr
	cmd.WaitDelay = 2 * time.Second
	err := cmd.Run()
	if ctx.Err() != nil {
		e.t.Fatalf("command timed out: %v", args)
	}
	code := 0
	if err != nil {
		exit, ok := err.(*exec.ExitError)
		if !ok {
			e.t.Fatalf("execute %v: %v", args, err)
		}
		code = exit.ExitCode()
	}
	return code, out.String(), stderr.String()
}

func (e *engine) want(code int, input string, args ...string) (string, string) {
	e.t.Helper()
	got, out, stderr := e.call(input, args...)
	if got != code {
		e.t.Fatalf("%v: exit %d, want %d\nstdout: %s\nstderr: %s", args, got, code, out, stderr)
	}
	return out, stderr
}

func TestRunStreamsAndExit(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(7, "", "run", "--name", e.name, "--rm", e.image, "sh", "-c", "echo out; echo err >&2; exit 7")
	if out != "out\n" || stderr != "err\n" {
		t.Fatalf("streams: stdout=%q stderr=%q", out, stderr)
	}
}

func (e *engine) detached() {
	e.t.Helper()
	e.want(0, "", "run", "-d", "--name", e.name, e.image, "sleep", "300")
	// exec readiness is bounded by the command deadline; detached run must be ready.
	e.want(0, "", "exec", e.name, "true")
}

func TestExecStdinAndExit(t *testing.T) {
	e := newEngine(t)
	e.detached()
	out, stderr := e.want(0, "input\n", "exec", "-i", e.name, "cat")
	if out != "input\n" || stderr != "" {
		t.Fatalf("stdin roundtrip: %q %q", out, stderr)
	}
	e.want(23, "", "exec", e.name, "sh", "-c", "exit 23")
	e.want(127, "", "exec", e.name, "/no-such-command")
}

func TestStopStartPersistsFiles(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "exec", e.name, "sh", "-c", "echo retained > /retained")
	e.want(0, "", "stop", "-t", "1", e.name)
	e.want(0, "", "start", e.name)
	out, _ := e.want(0, "", "exec", e.name, "cat", "/retained")
	if out != "retained\n" {
		t.Fatalf("persistent file: %q", out)
	}
}

func TestAutomaticRemoval(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "--rm", "--name", e.name, e.image, "true")
	// Reusing the same name proves removal without relying on inspect or formatting.
	e.want(0, "", "run", "--rm", "--name", e.name, e.image, "true")
}

func TestDuplicateNameFails(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(125, "", "run", "--name", e.name, e.image, "true")
	e.want(0, "", "exec", e.name, "true")
}

// Source: Podman test/e2e/exec_test.go (environment and working directory).
func TestExecEnvironmentAndWorkdir(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-e", "CONTRACT=base", "-w", "/tmp", e.image, "sleep", "300")
	out, _ := e.want(0, "", "exec", "-e", "CONTRACT=exec", "-w", "/", e.name, "sh", "-c", `printf "%s:%s" "$CONTRACT" "$PWD"`)
	if out != "exec:/" {
		t.Fatalf("exec options: %q", out)
	}
	out, _ = e.want(0, "", "exec", e.name, "sh", "-c", `printf "%s:%s" "$CONTRACT" "$PWD"`)
	if out != "base:/tmp" {
		t.Fatalf("exec changed container defaults: %q", out)
	}
}

// Source: Podman test/e2e/exec_test.go (user override and separate streams).
// Regression: DNS setup must preserve guest directory access after setuid.
func TestExecUserAndStreams(t *testing.T) {
	e := newEngine(t)
	e.detached()
	out, stderr := e.want(0, "", "exec", "-u", "1234:2345", e.name, "sh", "-c", `printf "%s:%s" "$(id -u)" "$(id -g)"; echo error >&2`)
	if out != "1234:2345" || stderr != "error\n" {
		t.Fatalf("exec user/streams: %q %q", out, stderr)
	}
	out, _ = e.want(0, "", "exec", e.name, "id", "-u")
	if out != "0\n" {
		t.Fatalf("exec user override persisted: %q", out)
	}
}

// Source: Podman test/system/075-exec.bats (#5046 large stdin regression).
func TestExecLargeStdin(t *testing.T) {
	e := newEngine(t)
	e.detached()
	input := strings.Repeat("0123456789abcdef\n", 100000)
	out, stderr := e.want(0, input, "exec", "-i", e.name, "cat")
	if out != input || stderr != "" {
		t.Fatalf("large stdin: sent %d, received %d bytes; stderr=%q", len(input), len(out), stderr)
	}
}

// Source: Podman test/system/040-ps.bats and test/e2e/ps_test.go.
func TestPsLifecycleAndIDSelection(t *testing.T) {
	e := newEngine(t)
	out, _ := e.want(0, "", "run", "-d", "--name", e.name, e.image, "sleep", "300")
	id := strings.TrimSpace(out)
	if !regexp.MustCompile(`^[0-9a-f]{64}$`).MatchString(id) {
		t.Fatalf("container ID: %q", id)
	}
	containsID := func(want bool, target string, args ...string) {
		t.Helper()
		out, _ := e.want(0, "", args...)
		found := false
		for _, line := range strings.Fields(out) {
			if line == target {
				found = true
			}
		}
		if found != want {
			t.Fatalf("%v: ID %s membership=%v, want %v; output=%q", args, target, found, want, out)
		}
	}
	containsID(true, id, "ps", "-q", "--no-trunc")
	containsID(true, id[:12], "ps", "-q")
	e.want(0, "", "stop", "-t", "1", id[:12])
	containsID(false, id, "ps", "-q", "--no-trunc")
	containsID(true, id, "ps", "-a", "-q", "--no-trunc")
	e.want(0, "", "rm", id)
	containsID(false, id, "ps", "-a", "-q", "--no-trunc")
}

// Source: Podman test/system/035-logs.bats and test/e2e/logs_test.go.
func TestLogsPreserveStreamsAfterExit(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "--name", e.name, e.image, "sh", "-c", "echo first; echo second; echo error >&2")
	for i := 0; i < 2; i++ {
		out, stderr := e.want(0, "", "logs", e.name)
		if out != "first\nsecond\n" || stderr != "error\n" {
			t.Fatalf("logs call %d: %q %q", i, out, stderr)
		}
	}
}

// Source: Podman test/e2e/images_test.go and rmi_test.go.
func TestImagesAndRmi(t *testing.T) {
	e := newEngine(t)
	ref := e.image
	out, _ := e.want(0, "", "images", "-q", "--no-trunc", ref)
	id := strings.TrimSpace(out)
	if !regexp.MustCompile(`^sha256:[0-9a-f]{64}$`).MatchString(id) {
		t.Fatalf("image ID: %q", out)
	}
	short, _ := e.want(0, "", "images", "-q", ref)
	if strings.TrimSpace(short) != id[7:19] {
		t.Fatalf("short image ID: %q, full=%q", short, id)
	}
	e.want(0, "", "rmi", ref)
	out, _ = e.want(0, "", "images", "-q", ref)
	if out != "" {
		t.Fatalf("removed reference remains listed: %q", out)
	}
}

// Source: Podman test/system/160-volumes.bats (implicit named volume creation).
func TestNamedVolumeSurvivesContainerRmV(t *testing.T) {
	e := newEngine(t)
	volume := e.name + "-volume"
	e.want(0, "", "run", "--name", e.name, "-v", volume+":/data", e.image, "sh", "-c", "echo retained > /data/value")
	e.want(0, "", "rm", "-v", e.name)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", volume+":/data", e.image, "cat", "/data/value")
	if out != "retained\n" {
		t.Fatalf("named volume lost after rm -v: %q", out)
	}
}

// Source: Podman test/e2e/run_volume_test.go (image content copy-up).
func TestNamedVolumeCopiesImageContent(t *testing.T) {
	e := newEngine(t)
	volume := e.name + "-seed"
	expected, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.image, "cat", "/etc/alpine-release")
	if strings.TrimSpace(expected) == "" {
		t.Fatal("image release fixture is empty")
	}
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-v", volume+":/etc", e.image, "cat", "/etc/alpine-release")
	if out != expected {
		t.Fatalf("volume image initialization: %q, want %q", out, expected)
	}
	out, _ = e.want(0, "", "run", "--rm", "--name", e.name, "-v", volume+":/etc:ro", e.image, "cat", "/etc/alpine-release")
	if out != expected {
		t.Fatalf("initialized read-only volume: %q, want %q", out, expected)
	}
}

// Source: Podman test/system/055-rm.bats and test/e2e/rm_test.go.
func TestRmRunningContainerRequiresForce(t *testing.T) {
	e := newEngine(t)
	e.detached()
	code, _, _ := e.call("", "rm", e.name)
	if code == 0 {
		t.Fatal("rm accepted a running container without --force")
	}
	e.want(0, "", "exec", e.name, "true")
	e.want(0, "", "rm", "-f", e.name)
	// Removal releases the name; successful reuse is observable without inspect.
	e.want(0, "", "run", "--rm", "--name", e.name, e.image, "true")
}
