"""Execute the actual patched flush path against observable kernel-call stubs.

The patch retains the complete function as context, so this test does not carry
a second implementation or depend on an existing firmware build directory.
Real guest negotiation, native close errors and lifecycle remain integration
coverage; this harness checks local-check ordering and fallback/error behavior.
"""

import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


PATCH = (Path(__file__).resolve().parents[1] / "patches" /
         "0046-fuse-negotiate-local-readonly-flush.patch")


def candidate_section(relative):
    text = PATCH.read_text()
    start = text.index(f"diff --git a/{relative} b/{relative}\n")
    end = text.find("\ndiff --git ", start + 1)
    section = text[start:end if end >= 0 else len(text)]
    in_hunk = False
    lines = []
    for line in section.splitlines():
        if line.startswith("@@"):
            in_hunk = True
        elif in_hunk and line.startswith((" ", "+")):
            lines.append(line[1:])
    return "\n".join(lines)


def function(source, signature):
    start = source.index(signature)
    opening = source.index("{", start)
    depth = 1
    for position in range(opening + 1, len(source)):
        if source[position] == "{":
            depth += 1
        elif source[position] == "}":
            depth -= 1
            if depth == 0:
                return source[start:position + 1]
    raise ValueError(f"Incomplete function context: {signature}")


class LocalFlushContractTests(unittest.TestCase):
    def test_local_checks_errors_and_protocol_fallback(self):
        compiler = shutil.which("cc")
        if compiler is None:
            self.skipTest("C compiler required for flush contract harness")
        source = candidate_section("fs/fuse/file.c")
        actual = "\n".join([
            function(source, "static bool fuse_pvisor_local_flush_enabled("),
            function(source, "static int fuse_flush("),
        ])
        header = candidate_section("include/uapi/linux/fuse.h")
        definitions = "\n".join(re.findall(
            r"^#define F(?:OPEN|USE)_PVISOR_LOCAL_FLUSH\s+.*$", header, re.M
        ))
        harness = PRELUDE + "\n" + definitions + "\n" + actual + "\n" + CASES
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            source_file = directory / "flush-contract.c"
            executable = directory / "flush-contract"
            source_file.write_text(harness)
            subprocess.run([
                compiler, "-std=c11", "-Wall", "-Wextra", "-Werror",
                "-Wno-unused-parameter", str(source_file), "-o", str(executable),
            ], check=True)
            subprocess.run([str(executable)], check=True)


PRELUDE = r"""
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <sys/stat.h>

#define FMODE_WRITE 2
#define FOPEN_NOFLUSH (1U << 5)
#define FUSE_FLUSH 25
#define STATX_BLOCKS 0x400
#define READ_ONCE(value) (value)
#define FUSE_ARGS(name) struct fuse_args name = {0}
#define FUSE_IS_DAX(inode) ((inode)->dax)
typedef uint64_t fl_owner_t;
struct fuse_conn {
    bool pvisor_local_flush, writeback_cache, no_lock, no_flock, no_flush;
    int pvisor_wb_error;
};
struct fuse_mount { struct fuse_conn *fc; };
struct inode {
    unsigned int i_mode;
    bool bad, dax;
    struct fuse_mount *mount;
};
struct fuse_file {
    struct fuse_mount *fm;
    uint32_t open_flags;
    uint64_t fh;
    bool pvisor_unbound;
};
struct file {
    struct inode *inode;
    struct fuse_file *private_data;
    unsigned int f_mode;
    void *f_mapping;
};
struct fuse_flush_in { uint64_t fh, lock_owner; };
struct fuse_args {
    unsigned int opcode, nodeid, in_numargs;
    struct { unsigned int size; const void *value; } in_args[1];
    bool force;
};
static char events[32];
static size_t event_count;
static int write_error, mapping_error, rpc_error;
static void event(char value) {
    assert(event_count + 1 < sizeof(events));
    events[event_count++] = value;
    events[event_count] = '\0';
}
static struct inode *file_inode(struct file *file) { return file->inode; }
static struct fuse_mount *get_fuse_mount(struct inode *inode) { return inode->mount; }
static bool fuse_is_bad(struct inode *inode) { return inode->bad; }
static bool fuse_pvisor_wb_enabled(struct inode *inode, struct fuse_file *ff) { return false; }
static int write_inode_now(struct inode *inode, int sync) { event('W'); return write_error; }
static void inode_lock(struct inode *inode) { event('L'); }
static void fuse_sync_writes(struct inode *inode) { event('S'); }
static void inode_unlock(struct inode *inode) { event('U'); }
static int filemap_check_errors(void *mapping) { event('E'); return mapping_error; }
static uint64_t fuse_lock_owner_id(struct fuse_conn *fc, fl_owner_t owner) { return owner; }
static unsigned int get_node_id(struct inode *inode) { return 42; }
static int fuse_simple_request(struct fuse_mount *mount, struct fuse_args *args) {
    const struct fuse_flush_in *input = args->in_args[0].value;
    assert(args->opcode == FUSE_FLUSH && args->nodeid == 42 && args->force);
    assert(input->fh == 123 && input->lock_owner == 456);
    event('R');
    return rpc_error;
}
static void fuse_invalidate_attr_mask(struct inode *inode, unsigned int mask) {
    assert(mask == STATX_BLOCKS);
    event('I');
}
"""


CASES = r"""
_Static_assert(FOPEN_PVISOR_LOCAL_FLUSH == (1U << 29), "OPEN ABI");
_Static_assert(FUSE_PVISOR_LOCAL_FLUSH == (1ULL << 43), "INIT ABI");
static struct fuse_conn connection;
static struct fuse_mount mount;
static struct inode inode;
static struct fuse_file handle;
static struct file file;
static void reset(void) {
    connection = (struct fuse_conn){
        .pvisor_local_flush = true, .no_lock = true, .no_flock = true,
    };
    mount = (struct fuse_mount){ .fc = &connection };
    inode = (struct inode){ .i_mode = 0100000 | 0644, .mount = &mount };
    handle = (struct fuse_file){
        .fm = &mount, .open_flags = FOPEN_PVISOR_LOCAL_FLUSH, .fh = 123,
    };
    file = (struct file){ .inode = &inode, .private_data = &handle };
    events[0] = '\0'; event_count = 0;
    write_error = mapping_error = rpc_error = 0;
}
static void expect(int error, const char *sequence) {
    int result = fuse_flush(&file, 456);
    if (result != error || strcmp(events, sequence)) {
        fprintf(stderr, "flush result=%d/%d events=%s/%s\n",
                result, error, events, sequence);
        assert(false);
    }
}
int main(void) {
    reset(); expect(0, "WLSUE"); /* Only the backend crossing is omitted. */
    reset(); write_error = -EIO; expect(-EIO, "W");
    reset(); mapping_error = -ENOSPC; expect(-ENOSPC, "WLSUE");
    reset(); connection.pvisor_local_flush = false; expect(0, "WLSUER");
    reset(); handle.open_flags = 0; expect(0, "WLSUER");
    reset(); file.f_mode = FMODE_WRITE; expect(0, "WLSUER");
    reset(); inode.i_mode = 0040000 | 0755; expect(0, "WLSUER");
    reset(); inode.dax = true; expect(0, "WLSUER");
    reset(); connection.no_lock = false; expect(0, "WLSUER");
    reset(); connection.no_flock = false; expect(0, "WLSUER");
    reset(); connection.writeback_cache = true; expect(0, "WLSUERI");
    reset(); handle.open_flags = 0; rpc_error = -EIO; expect(-EIO, "WLSUER");
    reset(); connection.pvisor_local_flush = false; rpc_error = -ENOSYS;
    expect(0, "WLSUER"); assert(connection.no_flush);
    reset(); connection.no_flush = true; expect(0, "WLSUE");
    reset(); inode.bad = true; expect(-EIO, "");
    reset(); handle.pvisor_unbound = true; expect(0, "");
    return 0;
}
"""


if __name__ == "__main__":
    unittest.main()
