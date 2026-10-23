// Adapted from Podman test/e2e/build_test.go and test/system/070-build.bats.
// Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
package integration

import (
	"archive/tar"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func buildFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}

func buildContext(t *testing.T, e *engine, body string) string {
	t.Helper()
	dir := t.TempDir()
	buildFile(t, filepath.Join(dir, "Dockerfile"), "FROM "+e.image+"\n"+body)
	return dir
}

// Source: build_test.go, basic alpine; 070-build.bats, basic build.
// Observe bytes/ID through public CLI instead of inspect or Podman COMMIT logs.
func TestBuildBasicAndRemove(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "RUN echo built-content > /built-file\n")
	_, logs := e.want(0, "", "build", "--network=none", "-t", e.name, dir)
	if strings.Contains(logs, "Pulling") {
		t.Fatalf("cached base pulled again: %s", logs)
	}
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.name, "cat", "/built-file")
	if out != "built-content\n" {
		t.Fatalf("built bytes: %q", out)
	}
	e.want(0, "", "rmi", e.name)
	out, _ = e.want(0, "", "images", "-q", e.name)
	if out != "" {
		t.Fatalf("removed build still listed: %q", out)
	}
}

// Source: build_test.go, print id to external file. Public images/run replace
// inspect; the complete sha256 ID must select the image.
func TestBuildIIDFile(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "CMD [\"/bin/echo\",\"iid-correct\"]\n")
	iid := filepath.Join(t.TempDir(), "iid")
	out, _ := e.want(0, "", "build", "-q", "--iidfile", iid, "-t", e.name, dir)
	bytes, err := os.ReadFile(iid)
	if err != nil {
		t.Fatal(err)
	}
	id := strings.TrimSpace(string(bytes))
	if id != strings.TrimSpace(out) || !strings.HasPrefix(id, "sha256:") || len(id) != 71 {
		t.Fatalf("iid=%q stdout=%q", bytes, out)
	}
	listed, _ := e.want(0, "", "images", "-q", "--no-trunc", e.name)
	if strings.TrimSpace(listed) != id {
		t.Fatalf("listed=%q iid=%q", listed, id)
	}
	out, _ = e.want(0, "", "run", "--rm", "--name", e.name, id)
	if out != "iid-correct\n" {
		t.Fatalf("image ID selected wrong build: %q", out)
	}
}

// Source: build_test.go, missing Containerfile/Dockerfile. Docker-profile errors
// use exit 1 rather than Podman's 125; Containerfile auto-discovery is not claimed.
func TestBuildMissingDockerfile(t *testing.T) {
	e := newEngine(t)
	dir := t.TempDir()
	for _, args := range [][]string{{"build", "-t", e.name, dir}, {"build", "-t", e.name, "-f", filepath.Join(dir, "absent"), dir}} {
		_, stderr := e.want(1, "", args...)
		if !strings.Contains(stderr, "cannot read Dockerfile") {
			t.Fatalf("missing definition diagnostic: %q", stderr)
		}
	}
	out, _ := e.want(0, "", "images", "-q", e.name)
	if out != "" {
		t.Fatalf("failed build published a tag: %q", out)
	}
}

// Source: build_test.go, context directory a file.
func TestBuildContextIsFile(t *testing.T) {
	e := newEngine(t)
	file := filepath.Join(t.TempDir(), "context")
	buildFile(t, file, "not a directory")
	_, stderr := e.want(1, "", "build", "-t", e.name, file)
	if !strings.Contains(stderr, "context is not a directory") {
		t.Fatalf("context diagnostic: %q", stderr)
	}
}

// Source: build_test.go, workdir is a symlink. Use an explicit local context.
func TestBuildWorkdirSymlink(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "RUN mkdir /real && ln -s /real /alias\nWORKDIR /alias\nRUN echo hello > marker\nCMD [\"/bin/cat\",\"marker\"]\n")
	e.want(0, "", "build", "--network=none", "-t", e.name, dir)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.name)
	if out != "hello\n" {
		t.Fatalf("symlink workdir: %q", out)
	}
}

// Source: build_test.go, remote context empty directories/symlinks. Local COPY
// replaces remote ADD; retain directory, link-identity and bytes checks.
func TestBuildContextEmptyDirectoriesAndSymlinks(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "COPY . /test\nRUN test -d /test/emptyDir && test -L /test/dummy-symlink\n")
	buildFile(t, filepath.Join(dir, "dummy"), "dummy")
	if err := os.Mkdir(filepath.Join(dir, "emptyDir"), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("dummy", filepath.Join(dir, "dummy-symlink")); err != nil {
		t.Fatal(err)
	}
	e.want(0, "", "build", "--network=none", "-t", e.name, dir)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.name, "sh", "-c", "test -d /test/emptyDir && test -L /test/dummy-symlink && readlink /test/dummy-symlink && cat /test/dummy-symlink")
	if out != "dummy\ndummy" {
		t.Fatalf("copied context: %q", out)
	}
}

// Source: build_test.go, remote .dockerignore. COPY replaces unsupported ADD;
// inspect resulting files rather than implementation-specific step logs.
func TestBuildDockerignoreFiltersContext(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "COPY . /testfilter/\n")
	for _, file := range []string{"dummy1", "dummy2", "subdir/dummy3"} {
		buildFile(t, filepath.Join(dir, file), file)
	}
	buildFile(t, filepath.Join(dir, ".dockerignore"), "dummy1\nsubdir**\n")
	e.want(0, "", "build", "-t", e.name, dir)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.name, "sh", "-c", "test ! -e /testfilter/dummy1 && test ! -e /testfilter/subdir && cat /testfilter/dummy2")
	if out != "dummy2" {
		t.Fatalf("ignore filter: %q", out)
	}
}

// Source: 070-build.bats, COPY hardlinks. Preserve cross-directory inode identity.
func TestBuildCopyHardlinks(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "COPY . /test\n")
	names := []string{"hardlink1", "subdir/hardlink2", "subdir/subsubdir/hardlink3"}
	for _, name := range names {
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.Link(filepath.Join(dir, "Dockerfile"), path); err != nil {
			t.Fatal(err)
		}
	}
	e.want(0, "", "build", "-t", e.name, dir)
	args := []string{"run", "--rm", "--name", e.name, e.name, "stat", "-c", "%i %n", "/test/Dockerfile"}
	for _, name := range names {
		args = append(args, "/test/"+name)
	}
	out, _ := e.want(0, "", args...)
	lines := strings.Split(strings.TrimSpace(out), "\n")
	if len(lines) != 4 {
		t.Fatalf("stat: %q", out)
	}
	inode := strings.Fields(lines[0])[0]
	for i, name := range append([]string{"Dockerfile"}, names...) {
		if lines[i] != inode+" /test/"+name {
			t.Fatalf("hardlink identity: %q", out)
		}
	}
}

// Source: 070-build.bats, context ownership. Retain build-time cmp and runtime stat.
func TestBuildContextOwnership(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "COPY empty-file.txt /empty-file.txt\nRUN echo 0:0 > /expected; stat -c '%u:%g' /empty-file.txt > /actual; cmp /expected /actual\n")
	buildFile(t, filepath.Join(dir, "empty-file.txt"), "")
	e.want(0, "", "build", "--network=none", "-t", e.name, dir)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.name, "stat", "-c", "%u:%g", "/empty-file.txt")
	if out != "0:0\n" {
		t.Fatalf("COPY owner: %q", out)
	}
}

// Source: 070-build.bats, workdir/cmd/env/label and explicit/implicit build args.
// Secret/default-mount/DNS assertions are not claimed. Public OCI output replaces
// label inspect; execute the image to verify defaults without private catalogs.
func TestBuildWorkdirCmdEnvLabelAndArgs(t *testing.T) {
	e := newEngine(t)
	dir := buildContext(t, e, "ARG EXPLICIT\nARG PVISOR_BUILD_IMPLICIT\nLABEL contract.owner=podman\nENV MYENV1=$EXPLICIT MYENV2=$PVISOR_BUILD_IMPLICIT\nWORKDIR /created/by/build\nCMD [\"/bin/sh\",\"-c\",\"printf '%s:%s:%s' \\\"$PWD\\\" \\\"$MYENV1\\\" \\\"$MYENV2\\\"\"]\n")
	e.env = append(e.env, "PVISOR_BUILD_IMPLICIT=implicit-value")
	archive := filepath.Join(t.TempDir(), "image.tar")
	e.want(0, "", "build", "--build-arg", "EXPLICIT=explicit-value", "--build-arg", "PVISOR_BUILD_IMPLICIT", "-t", e.name, "--output", "type=oci,dest="+archive, dir)
	out, _ := e.want(0, "", "run", "--rm", "--name", e.name, e.name)
	if out != "/created/by/build:explicit-value:implicit-value" {
		t.Fatalf("defaults: %q", out)
	}
	file, err := os.Open(archive)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	found := false
	reader := tar.NewReader(file)
	for {
		header, err := reader.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		if header.Size > 1024*1024 || !strings.HasPrefix(header.Name, "blobs/sha256/") {
			continue
		}
		data, err := io.ReadAll(reader)
		if err != nil {
			t.Fatal(err)
		}
		var doc struct {
			Config struct{ Labels map[string]string }
		}
		if json.Unmarshal(data, &doc) == nil && doc.Config.Labels["contract.owner"] == "podman" {
			found = true
		}
	}
	if !found {
		t.Fatalf("OCI export %s lacks configured label", archive)
	}
}
