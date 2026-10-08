import argparse
import sys
import re

from elftools.elf.elffile import ELFFile

PAGE_SIZE_DEFAULT = 65536  # 64k covers 4k/16k/64k Linux kernels
PAGE_SIZE_WINDOWS = 4096   # x86_64 Windows / WHP uses 4k pages
AARCH64_LOAD_ADDR = '0x80000000'

def write_header(ofile, bundle_name, page_size):
    ofile.write('#include <stddef.h>\n')
    ofile.write('__attribute__ ((aligned ({}))) char {}_BUNDLE[] = \n"'.format(page_size, bundle_name))


def write_padding(ofile, padding, col):
    while padding > 0:
        ofile.write('\\x0')

        if col == 15:
            ofile.write('"\n"')
            col = 0
        else:
            col = col + 1
            
        padding = padding - 1
        
        
def write_elf_cbundle(ifile, ofile, page_size) -> int:
    elffile = ELFFile(ifile)
    entry_addr = elffile['e_entry']

    load_segments = [ ]
    for segment in elffile.iter_segments():
        if segment['p_type'] == 'PT_LOAD':
            load_segments.append(segment)
        
    col = 0
    total_size = 0
    prev_paddr = None

    for segment in load_segments:
        if prev_paddr == None:
            load_addr = segment['p_vaddr'] & 0xfffffff
        else:
            padding = segment['p_paddr'] - prev_paddr - prev_filesz
            write_padding(ofile, padding, col)
            total_size = total_size + padding

        assert((segment['p_paddr'] - load_addr) == total_size)
        
        for byte in segment.data():
            ofile.write('\\x{:x}'.format(byte))
                
            if col == 15:
                ofile.write('"\n"')
                col = 0
            else:
                col = col + 1

        prev_paddr = segment['p_paddr']
        prev_filesz = segment['p_filesz']
        total_size = total_size + prev_filesz

    rounded_size = int((total_size + page_size - 1) / page_size) * page_size
    padding = rounded_size - total_size    
    write_padding(ofile, padding, col)

    return load_addr, entry_addr

    
def write_raw_cbundle(ifile, ofile, page_size) -> int:
    col = 0
    total_size = 0
    byte = ifile.read(1)
    while byte:
        ofile.write('\\x{:x}'.format(byte[0]))
            
        if col == 15:
            ofile.write('"\n"')
            col = 0
        else:
            col = col + 1
        
        total_size = total_size + 1
        byte = ifile.read(1)

    rounded_size = int((total_size + page_size - 1) / page_size) * page_size
    padding = rounded_size - total_size    
    write_padding(ofile, padding, col)

    
def write_footer_generic(ofile, bundle_name):
    footer = """
char * krunfw_get_{}(size_t *size)
{{
    *size = sizeof({}_BUNDLE) - 1;
    return &{}_BUNDLE[0];
}}
"""
    ofile.write('";\n')
    ofile.write(footer.format(bundle_name.lower(), bundle_name, bundle_name))

    
def write_footer_kernel(ofile, load_addr, entry_addr):
    footer = """
char * krunfw_get_kernel(size_t *load_addr, size_t *entry_addr, size_t *size)
{{
    *load_addr = {};
    *entry_addr = {};
    *size = sizeof(KERNEL_BUNDLE) - 1;
    return &KERNEL_BUNDLE[0];
}}

int krunfw_get_version()
{{
    return ABI_VERSION;
}}
"""
    ofile.write('";\n')
    ofile.write(footer.format(load_addr, entry_addr))
    


def read_elf_bundle(ifile, page_size):
    elffile = ELFFile(ifile)
    segments = [s for s in elffile.iter_segments() if s['p_type'] == 'PT_LOAD']
    if not segments:
        raise ValueError('ELF kernel has no loadable segments')
    load_addr = segments[0]['p_vaddr'] & 0xfffffff
    data = bytearray()
    for segment in segments:
        offset = segment['p_paddr'] - load_addr
        if offset < len(data):
            raise ValueError('ELF kernel load segments overlap or precede the load address')
        data.extend(bytes(offset - len(data)))
        data.extend(segment.data())
    data.extend(bytes((-len(data)) % page_size))
    return bytes(data), load_addr, elffile['e_entry']


def compact_chunks(data):
    """Represent long constant-byte padding without changing the memory image."""
    payload = bytearray()
    chunks = []
    previous = 0
    for match in re.finditer(rb'(.)\1{255,}', data, flags=re.DOTALL):
        if match.start() > previous:
            literal = data[previous:match.start()]
            chunks.append((len(literal), len(payload), -1))
            payload.extend(literal)
        chunks.append((match.end() - match.start(), 0, match[1][0]))
        previous = match.end()
    if previous < len(data):
        chunks.append((len(data) - previous, len(payload), -1))
        payload.extend(data[previous:])
    return bytes(payload), chunks


def write_compact_elf_cbundle(ifile, ofile, page_size):
    data, load_addr, entry_addr = read_elf_bundle(ifile, page_size)
    if not data:
        raise ValueError('ELF kernel bundle is empty')
    payload, chunks = compact_chunks(data)
    ofile.write('#include <stddef.h>\n#include <stdint.h>\n#include <string.h>\n#include <sys/mman.h>\n')
    ofile.write('static const unsigned char kernel_payload[] =\n')
    for start in range(0, len(payload), 16):
        ofile.write('"' + ''.join('\\x%02x' % byte for byte in payload[start:start+16]) + '"\n')
    if not payload:
        ofile.write('""\n')
    ofile.write(';\nstruct kernel_chunk { size_t length, source; int fill; };\n')
    ofile.write('static const struct kernel_chunk kernel_chunks[] = {\n')
    for length, source, fill in chunks:
        ofile.write('    {%d, %d, %d},\n' % (length, source, fill))
    ofile.write('};\n')
    ofile.write(f'''/* Keep the guest addresses and every padding byte, including SRSO/PTI layout.
 * Only the on-disk representation changes. Initialization completes at dlopen.
 * The mapping stays writable because the VMM exposes it as guest kernel RAM.
 */
static char *kernel_bundle;
#define KERNEL_SIZE {len(data)}UL
#define KERNEL_ALIGNMENT {page_size}UL
__attribute__((constructor)) static void init_kernel_bundle(void)
{{
    size_t reserved_size = KERNEL_SIZE + KERNEL_ALIGNMENT;
    void *reserved = mmap(NULL, reserved_size, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reserved == MAP_FAILED)
        return;
    uintptr_t start = ((uintptr_t)reserved + KERNEL_ALIGNMENT - 1) & ~(KERNEL_ALIGNMENT - 1);
    size_t prefix = start - (uintptr_t)reserved;
    if (prefix)
        munmap(reserved, prefix);
    size_t suffix = reserved_size - prefix - KERNEL_SIZE;
    if (suffix)
        munmap((void *)(start + KERNEL_SIZE), suffix);
    kernel_bundle = (char *)start;
    size_t destination = 0;
    for (size_t i = 0; i < sizeof(kernel_chunks) / sizeof(kernel_chunks[0]); i++) {{
        const struct kernel_chunk *chunk = &kernel_chunks[i];
        if (chunk->fill < 0)
            memcpy(kernel_bundle + destination, kernel_payload + chunk->source, chunk->length);
        else if (chunk->fill != 0)
            memset(kernel_bundle + destination, chunk->fill, chunk->length);
        /* mmap already supplies zero-filled pages. */
        destination += chunk->length;
    }}
}}
__attribute__((destructor)) static void free_kernel_bundle(void)
{{
    if (kernel_bundle)
        munmap(kernel_bundle, KERNEL_SIZE);
}}
char *krunfw_get_kernel(size_t *load_addr, size_t *entry_addr, size_t *size)
{{
    *load_addr = {load_addr}UL;
    *entry_addr = {entry_addr}UL;
    *size = kernel_bundle ? KERNEL_SIZE : 0;
    return kernel_bundle;
}}
int krunfw_get_version(void)
{{
    return ABI_VERSION;
}}
''')
    print(f'Compact kernel: {len(data)} -> {len(payload)} payload bytes, {len(chunks)} chunks')


def main() -> int:
    parser = argparse.ArgumentParser(description='Generate C blob from a binary')
    
    parser.add_argument('input_file', type=str,
                        help='Input file')
    parser.add_argument('output_file', type=str,
                        help='Output file')
    parser.add_argument('-t', type=str, help='Bundle type (vmlinux, Image, qboot, initrd)')
    parser.add_argument('--os', type=str, default='Linux',
                        help='Target OS (Linux, Darwin, Windows)')
    
    parser.add_argument('--compact', action='store_true',
                        help='Store constant-byte padding compactly (Linux vmlinux only)')
    args = parser.parse_args()
    if args.compact and (args.os != 'Linux' or args.t != 'vmlinux'):
        parser.error('--compact requires Linux vmlinux')

    page_size = PAGE_SIZE_WINDOWS if args.os == 'Windows' else PAGE_SIZE_DEFAULT

    bundle_name = None
    ifmt = None
    if args.t == 'vmlinux':
        bundle_name = 'KERNEL'
        ifmt = 'elf'
    elif args.t == 'Image':
        bundle_name = 'KERNEL'
        ifmt = 'raw'
    elif args.t == 'qboot':
        bundle_name = 'QBOOT'
        ifmt = 'raw'
    elif args.t == 'initrd':
        bundle_name = 'INITRD'
        ifmt = 'raw'
    else:
        print('Invalid bundle type')
        return -1

    ifile = open(args.input_file, 'rb')
    ofile = open(args.output_file, 'w')

    if args.compact:
        write_compact_elf_cbundle(ifile, ofile, page_size)
        ofile.close()
        ifile.close()
        return 0

    write_header(ofile, bundle_name, page_size)

    if ifmt == 'elf':
        load_addr, entry_addr = write_elf_cbundle(ifile, ofile, page_size)
    elif ifmt == 'raw':
        write_raw_cbundle(ifile, ofile, page_size)

    if bundle_name == 'KERNEL':
        if ifmt == 'raw':
            load_addr = AARCH64_LOAD_ADDR;
            entry_addr = AARCH64_LOAD_ADDR;
        write_footer_kernel(ofile, load_addr, entry_addr)
    else:
        write_footer_generic(ofile, bundle_name)

    return 0


if __name__ == '__main__':
    sys.exit(main())
