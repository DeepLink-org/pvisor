// Adapted from pinned Podman exec/run/exit/user/image scenarios.
// Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
package integration

import (
	"context"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"
)

// Source: system/075-exec.bats (missing output / large stdin).
func TestExecLargeOutput(t *testing.T) {
	e := newEngine(t)
	e.detached()
	out, stderr := e.want(0, "", "exec", e.name, "sh", "-c", `head -c 1048576 /dev/zero | tr '\000' z; printf separate >&2`)
	if out != strings.Repeat("z", 1048576) || stderr != "separate" {
		t.Fatalf("exec large output: %d bytes, stderr=%q", len(out), stderr)
	}
}

func TestExecBinaryStdin(t *testing.T) {
	e := newEngine(t)
	e.detached()
	input := strings.Repeat("\x00\xff\x01\r\n", 40000)
	out, stderr := e.want(0, input, "exec", "-i", e.name, "cat")
	if out != input || stderr != "" {
		t.Fatalf("binary stdin: sent %d, received %d bytes, stderr=%q", len(input), len(out), stderr)
	}
}

// Source: e2e/exec_test.go (environment/host imports), e2e/run_env_test.go.
func TestExecEnvironmentUnsetsValue(t *testing.T) {
	t.Setenv("PVISOR_EXEC_UNSET", "")
	if err := os.Unsetenv("PVISOR_EXEC_UNSET"); err != nil {
		t.Fatal(err)
	}
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-e", "PVISOR_EXEC_UNSET=base", e.image, "sleep", "300")
	out, _ := e.want(1, "", "exec", "-e", "PVISOR_EXEC_UNSET", e.name, "printenv", "PVISOR_EXEC_UNSET")
	if out != "" {
		t.Fatalf("explicit unset retained value: %q", out)
	}
	out, _ = e.want(0, "", "exec", e.name, "printenv", "PVISOR_EXEC_UNSET")
	if out != "base\n" {
		t.Fatalf("exec unset changed container defaults: %q", out)
	}
}

func TestExecLiteralArguments(t *testing.T) {
	e := newEngine(t)
	e.detached()
	out, stderr := e.want(0, "", "exec", e.name, "busybox", "printf", "%s\n", "--env=literal", "$HOME; * with spaces", "")
	if out != "--env=literal\n$HOME; * with spaces\n\n" || stderr != "" {
		t.Fatalf("exec expanded or parsed workload args: %q %q", out, stderr)
	}
}

// Source: e2e/exec_test.go (cannot invoke, missing working directory).
func TestExecDirectoryExit126(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(126, "", "exec", e.name, "/etc")
	e.want(0, "", "exec", e.name, "true")
}

func TestExecNonExecutableExit126(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "exec", e.name, "sh", "-c", "printf not-executable > /tmp/program; chmod 644 /tmp/program")
	e.want(126, "", "exec", e.name, "/tmp/program")
	e.want(0, "", "exec", e.name, "true")
}

func TestExecMissingWorkdirDoesNotCreateDirectory(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(127, "", "exec", "-w", "/missing/cwd", e.name, "pwd")
	e.want(0, "", "exec", e.name, "test", "!", "-e", "/missing")
	e.want(0, "", "exec", e.name, "true")
}

// Source: e2e/exec_test.go (user only in container, user inherited from run).
func TestExecNamedUserCreatedInsideContainer(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "exec", e.name, "adduser", "-D", "contractuser")
	out, _ := e.want(0, "", "exec", "-u", "contractuser", e.name, "id", "-un")
	if out != "contractuser\n" {
		t.Fatalf("exec did not resolve current guest passwd: %q", out)
	}
}

func TestExecInheritsRunUser(t *testing.T) {
	e := newEngine(t)
	e.want(0, "", "run", "-d", "--name", e.name, "-u", "nobody", e.image, "sleep", "300")
	for _, user := range []string{"", "root", ""} {
		args := []string{"exec"}
		want := "nobody\n"
		if user != "" {
			args = append(args, "-u", user)
			want = "root\n"
		}
		args = append(args, e.name, "id", "-un")
		out, _ := e.want(0, "", args...)
		if out != want {
			t.Fatalf("exec user %q: %q, want %q", user, out, want)
		}
	}
}

// Source: e2e/exec_test.go (terminal sanity check), --tty without --interactive.
func TestExecTTYRetainsExplicitEmptyTerm(t *testing.T) {
	e := newEngine(t)
	e.detached()
	out, stderr := e.want(0, "", "exec", "-t", "-e", "TERM=", e.name, "sh", "-c", `test -t 0 && test -t 1 && test -t 2 && printf '%s' "${TERM-unset}"`)
	if out != "" || stderr != "" {
		t.Fatalf("TTY overwrote empty TERM: %q %q", out, stderr)
	}
}

// Source: e2e/stop_test.go and exec_test.go (stop with a live exec session).
// No --no-session flag is needed: the generic guest reaper owns all children.
func TestStopTerminatesInFlightExec(t *testing.T) {
	e := newEngine(t)
	e.detached()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, e.binary, "exec", e.name, "sh", "-c", "touch /exec-ready; exec sleep 300")
	cmd.Env = e.env
	done := make(chan error, 1)
	go func() { done <- cmd.Run() }()
	e.want(0, "", "exec", e.name, "sh", "-c", `for i in $(seq 1 80); do test -f /exec-ready && exit 0; sleep 0.05; done; exit 1`)
	e.want(0, "", "stop", "-t", "0", e.name)
	select {
	case err := <-done:
		if err == nil || ctx.Err() != nil {
			t.Fatalf("stopped exec exit: %v; deadline=%v", err, ctx.Err())
		}
	case <-ctx.Done():
		t.Fatal("exec outlived stopped VM")
	}
}

// Source: e2e/run_exit_test.go (user exit 125 vs CLI failure, signal exit).
func TestRunWorkloadExit125PreservesStreams(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(125, "", "run", "--rm", "--name", e.name, e.image, "sh", "-c", "printf workload; printf error >&2; exit 125")
	if out != "workload" || stderr != "error" {
		t.Fatalf("workload exit mistaken for engine failure: %q %q", out, stderr)
	}
}

func TestRunSignalExitCode(t *testing.T) {
	e := newEngine(t)
	e.want(143, "", "run", "--rm", "--name", e.name, e.image, "sh", "-c", "kill -TERM $$")
}

// Source: system/075-exec.bats (default umask; explicit --umask unsupported).
func TestRunDefaultUmask(t *testing.T) {
	e := newEngine(t)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.image, "sh", "-c", "umask; touch /tmp/mode; stat -c %a /tmp/mode")
	if out != "0022\n644\n" {
		t.Fatalf("default umask: %q", out)
	}
}

func TestExecUmaskDoesNotLeakBetweenSessions(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "exec", e.name, "sh", "-c", "umask 077; touch /tmp/private; test \"$(stat -c %a /tmp/private)\" = 600")
	out, _ := e.want(0, "", "exec", e.name, "sh", "-c", "umask; touch /tmp/public; stat -c %a /tmp/public")
	if out != "0022\n644\n" {
		t.Fatalf("exec inherited previous session umask: %q", out)
	}
}

// Source: e2e/images_test.go and run_test.go (image ID selection).
func TestRunByFullImageID(t *testing.T) {
	e := newEngine(t)
	id, _ := e.want(0, "", "images", "-q", "--no-trunc", e.image)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, strings.TrimSpace(id), "echo", "full-id")
	if out != "full-id\n" {
		t.Fatalf("run by full image ID: %q", out)
	}
}

func TestRunByShortImageID(t *testing.T) {
	e := newEngine(t)
	id, _ := e.want(0, "", "images", "-q", e.image)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, strings.TrimSpace(id), "echo", "short-id")
	if out != "short-id\n" {
		t.Fatalf("run by short image ID: %q", out)
	}
}

func TestImagesUnknownReferenceIsEmpty(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(0, "", "images", "-q", "contract-no-such-repository:absent")
	if out != "" || stderr != "" {
		t.Fatalf("unknown image filter: %q %q", out, stderr)
	}
}

// Source: e2e/rmi_test.go (batch failures and force with a running container).
func TestRmiBatchContinuesAfterMissingImage(t *testing.T) {
	e := newEngine(t)
	e.want(1, "", "rmi", "contract-no-such-repository:absent", e.image)
	out, _ := e.want(0, "", "images", "-q")
	if out != "" {
		t.Fatalf("valid image after missing argument was not removed: %q", out)
	}
}

func TestRmiForceKeepsPinnedContainerUsable(t *testing.T) {
	e := newEngine(t)
	e.detached()
	e.want(0, "", "rmi", "-f", e.image)
	out, _ := e.want(0, "", "images", "-q")
	if out != "" {
		t.Fatalf("forced image removal retained reference: %q", out)
	}
	e.want(0, "", "exec", e.name, "true")
	e.want(0, "", "stop", "-t", "1", e.name)
	e.want(0, "", "start", e.name)
	e.want(0, "", "exec", e.name, "true")
}

// Source: e2e/run_passwd_test.go (named/numeric user and primary group).
// Only identity is ported; Podman's passwd/group auto-injection is not claimed.
func TestRunUserAndGroupResolution(t *testing.T) {
	e := newEngine(t)
	for _, test := range []struct{ user, identity string }{
		{"root:bin", "0:1"},
		{"nobody:nogroup", "65534:65533"},
		{"65534", "65534:65534"},
		{"12345", "12345:0"},
		{"root:2345", "0:2345"},
	} {
		out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-u", test.user, e.image, "sh", "-c", `printf '%s:%s' "$(id -u)" "$(id -g)"`)
		if out != test.identity {
			t.Fatalf("user %q: %q, want %q", test.user, out, test.identity)
		}
	}
}

// Source: e2e/run_working_dir_test.go (symlinked workdir).
// Alpine's existing /var/run symlink replaces an upstream image-build fixture.
func TestRunSymlinkedWorkdir(t *testing.T) {
	e := newEngine(t)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "-w", "/var/run", e.image, "pwd", "-P")
	if out != "/run\n" {
		t.Fatalf("symlinked workdir: %q", out)
	}
}

// Source: e2e/run_entrypoint_test.go (CMD and entrypoint overrides).
func TestRunImageDefaultCommand(t *testing.T) {
	e := newEngine(t)
	out, stderr := e.want(0, "", "run", "--rm", "--name", e.name, e.image)
	if out != "" || stderr != "" {
		t.Fatalf("Alpine default shell with closed stdin: %q %q", out, stderr)
	}
}

func TestRunEmptyEntrypointWithExplicitCommand(t *testing.T) {
	e := newEngine(t)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, "--entrypoint", "", e.image, "echo", "explicit-command")
	if out != "explicit-command\n" {
		t.Fatalf("empty entrypoint: %q", out)
	}
}

// Source: system/030-run.bats (--name) and e2e/run_test.go name validation.
func TestRunInvalidNameDoesNotPublishInstance(t *testing.T) {
	e := newEngine(t)
	e.want(125, "", "run", "--name", "invalid/name", e.image, "true")
	out, _ := e.want(0, "", "ps", "-aq")
	if out != "" {
		t.Fatalf("invalid name published container: %q", out)
	}
}
