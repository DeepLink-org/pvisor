// Adapted from pinned Podman system/e2e scenarios; per-test sources below.
// Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
package integration

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"
)

// Source: system/030-run.bats (stdin closure and large output).
func TestRunClosedStdin(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(0, "must not arrive\n", "run", "--rm", "--name", e.name, e.image, "sh", "-c", "cat; echo EOF")
	if out != "EOF\n" || stderr != "" {
		t.Fatalf("stdin without -i: %q %q", out, stderr)
	}
}

func TestRunLargeOutput(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(0, "", "run", "--rm", "--name", e.name, e.image, "sh", "-c", `head -c 1048576 /dev/zero | tr '\000' x; echo separate >&2`)
	if out != strings.Repeat("x", 1048576) || stderr != "separate\n" {
		t.Fatalf("large stdout: got %d bytes; stderr=%q", len(out), stderr)
	}
}

// Source: system/030-run.bats (defaultenv) and 075-exec.bats (--tty).
func TestRunTTYAndEnvironment(t *testing.T) {
	e := newEngine(t)
	for _, term := range []string{"", "custom-term"} {
		args := []string{"run", "--rm", "--name", e.name, "-t"}
		expected := "xterm"
		if term != "" {
			args = append(args, "-e", "TERM="+term)
			expected = term
		}
		args = append(args, e.image, "sh", "-c", `test -t 0 && test -t 1 && test -t 2 && printf '%s\n' "$TERM"`)
		out, stderr := e.want(0, "", args...)
		if out != expected+"\r\n" || stderr != "" {
			t.Fatalf("TTY TERM=%q: %q %q", term, out, stderr)
		}
	}
}

func TestExecTTYDoesNotChangeContainerEnvironment(t *testing.T) {
	e := newEngine(t)
	e.detached()
	for _, term := range []string{"", "exec-term"} {
		args := []string{"exec", "-t"}
		expected := "xterm"
		if term != "" {
			args = append(args, "-e", "TERM="+term)
			expected = term
		}
		args = append(args, e.name, "sh", "-c", `test -t 0 && test -t 1 && test -t 2 && printf '%s\n' "$TERM"`)
		out, stderr := e.want(0, "", args...)
		if out != expected+"\r\n" || stderr != "" {
			t.Fatalf("exec TTY TERM=%q: %q %q", term, out, stderr)
		}
	}
	out, _ := e.want(0, "", "exec", e.name, "sh", "-c", `printf '%s' "${TERM-unset}"`)
	if out != "unset" {
		t.Fatalf("TTY changed container TERM: %q", out)
	}
}

// Source: system/075-exec.bats (stdin and invalid exec session cleanup).
func TestExecClosedStdin(t *testing.T) {
	e := newEngine(t)
	e.detached()
	out, stderr := e.want(0, "must not arrive\n", "exec", e.name, "cat")
	if out != "" || stderr != "" {
		t.Fatalf("exec stdin without -i: %q %q", out, stderr)
	}
}

func TestConcurrentExecExitStatusesAfterFailedSpawns(t *testing.T) {
	e := newEngine(t)
	e.detached()
	for i := 0; i < 3; i++ {
		e.want(127, "", "exec", e.name, "/no-such-command")
	}
	var pending sync.WaitGroup
	for i := 0; i < 8; i++ {
		pending.Add(1)
		go func(i int) {
			defer pending.Done()
			script := fmt.Sprintf("sleep 0.%d; printf request-%d; exit %d", i%3+1, i, i+11)
			code, out, stderr := e.call("", "exec", e.name, "sh", "-c", script)
			if code != i+11 || out != fmt.Sprintf("request-%d", i) || stderr != "" {
				t.Errorf("exec %d: exit=%d stdout=%q stderr=%q", i, code, out, stderr)
			}
		}(i)
	}
	pending.Wait()
	e.want(0, "", "exec", e.name, "true")
}

// Source: e2e/stop_test.go (stopped container and --timeout).
func TestStopStoppedContainerIsIdempotent(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "stop", "-t", "1", e.name)
	e.want(0, "", "stop", "-t", "1", e.name)
	code, _, _ := e.call("", "exec", e.name, "true")
	if code == 0 {
		t.Fatal("exec succeeded in a stopped container")
	}
}

// Source: e2e/run_signal_test.go and stop_test.go (signal/timeout semantics).
func TestStopCustomSignal(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, e.image, "sh", "-c", `trap 'echo caught; exit 23' USR1; touch /ready; while :; do sleep 0.1; done`)
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do test -f /ready && exit 0; sleep 0.05; done; exit 1`)
	e.want(0, "", "stop", "-s", "USR1", "-t", "2", e.name)
	out, _ := e.want(0, "", "logs", e.name)
	if out != "caught\n" {
		t.Fatalf("custom stop signal was not handled: %q", out)
	}
	out, _ = e.want(0, "", "ps", "-a")
	if !strings.Contains(out, "Exited (23)") {
		t.Fatalf("signal handler exit was lost: %q", out)
	}
}

func TestStopZeroTimeoutEscalates(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, e.image, "sh", "-c", `trap '' TERM; touch /ready; while :; do sleep 0.1; done`)
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do test -f /ready && exit 0; sleep 0.05; done; exit 1`)
	start := time.Now()
	e.want(0, "", "stop", "-t", "0", e.name)
	if time.Since(start) > 5*time.Second {
		t.Fatal("stop -t 0 ignored its timeout")
	}
	out, _ := e.want(0, "", "ps", "-a")
	if !strings.Contains(out, "Exited (137)") {
		t.Fatalf("SIGKILL exit was lost: %q", out)
	}
}

// Source: e2e/start_test.go, stop_test.go and rm_test.go (multiple containers).
func TestMultipleContainerLifecyclePreservesIsolation(t *testing.T) {
	e := newEngine(t)
	second := e.name + "-second"
	t.Cleanup(func() { e.cleanup(second) })
	e.detached()
	e.want(0, "", "run", "-d", "--name", second, e.image, "sleep", "300")
	for name, value := range map[string]string{e.name: "first", second: "second"} {
		e.want(0, "", "exec", name, "sh", "-c", "printf "+value+" > /identity")
	}
	e.want(0, "", "stop", "-t", "1", e.name, second)
	e.want(0, "", "start", e.name, second)
	for name, value := range map[string]string{e.name: "first", second: "second"} {
		out, _ := e.want(0, "", "exec", name, "cat", "/identity")
		if out != value {
			t.Fatalf("container %s lost its independent upper: %q", name, out)
		}
	}
	e.want(0, "", "rm", "-f", e.name, second)
	out, _ := e.want(0, "", "ps", "-aq")
	if out != "" {
		t.Fatalf("batch removal left containers: %q", out)
	}
}

func TestBatchLifecycleContinuesAfterMissingContainer(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "stop", "-t", "1", e.name)
	e.want(1, "", "start", e.name+"-missing", e.name)
	e.want(0, "", "exec", e.name, "true")
	e.want(1, "", "stop", "-t", "1", e.name+"-missing", e.name)
	e.want(1, "", "rm", e.name+"-missing", e.name)
	out, _ := e.want(0, "", "ps", "-aq")
	if out != "" {
		t.Fatalf("valid argument after missing container was ignored: %q", out)
	}
}

// Source: e2e/logs_test.go (follow live/stopped containers).
func TestLogsFollowStreamsBeforeExitAndFinishes(t *testing.T) {
	e := newEngine(t)
	gate := t.TempDir()
	e.want(0, "", "run", "-d", "--name", e.name, "-v", gate+":/gate", e.image, "sh", "-c", `echo before; while [ ! -e /gate/go ]; do sleep 0.05; done; echo after; echo error >&2; exit 7`)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, e.binary, "logs", "-f", e.name)
	cmd.Env = e.env
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() { cancel(); cmd.Wait() }()
	reader := bufio.NewReader(stdout)
	first := make(chan string, 1)
	go func() { line, _ := reader.ReadString('\n'); first <- line }()
	select {
	case line := <-first:
		if line != "before\n" {
			t.Fatalf("live log: %q", line)
		}
	case <-ctx.Done():
		t.Fatal("logs -f buffered output until container exit")
	}
	if err := os.WriteFile(filepath.Join(gate, "go"), nil, 0600); err != nil {
		t.Fatal(err)
	}
	rest, err := io.ReadAll(reader)
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Wait(); err != nil {
		t.Fatalf("logs -f: %v", err)
	}
	if string(rest) != "after\n" || stderr.String() != "error\n" {
		t.Fatalf("follow streams: %q %q", rest, stderr.String())
	}
	out, errout := e.want(0, "", "logs", "-f", e.name)
	if out != "before\nafter\n" || errout != "error\n" {
		t.Fatalf("follow stopped container: %q %q", out, errout)
	}
}

// Source: system/500-networking.bats (loopback TCP publication/netcat payload).
func TestTCPPublicationPreservesHalfClose(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-p", "127.0.0.1:0:8080", e.image, "busybox", "nc", "-lk", "-s", "127.0.0.1", "-p", "8080", "-e", "cat")
	e.echoReady()
	e.echo(e.publishedAddress(), strings.Repeat("published\n", 256))
}

// Source: e2e/images_test.go and rmi_test.go (identity, used images, repulls).
func TestRmiRefusesAnImageUsedByAContainer(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(1, "", "rmi", e.image)
	out, _ := e.want(0, "", "images", "-q", e.image)
	if strings.TrimSpace(out) == "" {
		t.Fatal("failed rmi removed the image")
	}
	e.want(0, "", "exec", e.name, "true")
}

func TestRmiAcceptsAShortImageID(t *testing.T) {
	e := newEngine(t)
	out, _ := e.want(0, "", "images", "-q", e.image)
	id := strings.TrimSpace(out)
	if !regexp.MustCompile(`^[0-9a-f]{12}$`).MatchString(id) {
		t.Fatalf("short image ID: %q", id)
	}
	e.want(0, "", "rmi", id)
	out, _ = e.want(0, "", "images", "-q")
	if out != "" {
		t.Fatalf("removed image still listed: %q", out)
	}
}

func TestRepeatedPullKeepsOneStableImageID(t *testing.T) {
	e := newEngine(t)
	before, _ := e.want(0, "", "images", "-q", "--no-trunc")
	e.want(0, "", "pull", e.image)
	after, _ := e.want(0, "", "images", "-q", "--no-trunc")
	if before != after || len(strings.Fields(after)) != 1 {
		t.Fatalf("repeated pull changed or duplicated identity: %q -> %q", before, after)
	}
}
