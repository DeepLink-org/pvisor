// Lossless storage of kernel padding. Guest addresses and bytes stay unchanged.

const MAGIC: &[u8; 8] = b"PVKRUN01";
const MAX_KERNEL_SIZE: usize = 256 * 1024 * 1024;
const MIN_FILL: usize = 256;

// This file is shared by the build script and the generated runtime module.
#[allow(dead_code)]
pub fn pack(kernel: &[u8]) -> Vec<u8> {
    assert!(!kernel.is_empty() && kernel.len() <= MAX_KERNEL_SIZE);
    let mut output = MAGIC.to_vec();
    output.extend_from_slice(&(kernel.len() as u64).to_le_bytes());
    let mut literal = 0;
    let mut cursor = 0;
    while cursor < kernel.len() {
        let start = cursor;
        let byte = kernel[start];
        cursor += 1;
        while cursor < kernel.len() && kernel[cursor] == byte {
            cursor += 1;
        }
        if cursor - start >= MIN_FILL {
            if literal < start {
                output.push(1);
                output.extend_from_slice(&((start - literal) as u64).to_le_bytes());
                output.extend_from_slice(&kernel[literal..start]);
            }
            output.push(0);
            output.extend_from_slice(&((cursor - start) as u64).to_le_bytes());
            output.push(byte);
            literal = cursor;
        }
    }
    if literal < kernel.len() {
        output.push(1);
        output.extend_from_slice(&((kernel.len() - literal) as u64).to_le_bytes());
        output.extend_from_slice(&kernel[literal..]);
    }
    output
}

#[allow(dead_code)]
pub fn unpack(mut input: &[u8]) -> Result<Vec<u8>, &'static str> {
    fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], &'static str> {
        if count > input.len() {
            return Err("truncated kernel bundle");
        }
        let (head, tail) = input.split_at(count);
        *input = tail;
        Ok(head)
    }
    fn length(input: &mut &[u8]) -> Result<usize, &'static str> {
        let bytes = take(input, 8)?;
        usize::try_from(u64::from_le_bytes(bytes.try_into().unwrap()))
            .map_err(|_| "kernel length overflow")
    }

    if take(&mut input, MAGIC.len())? != MAGIC {
        return Err("invalid kernel bundle magic");
    }
    let size = length(&mut input)?;
    if size == 0 || size > MAX_KERNEL_SIZE {
        return Err("invalid kernel size");
    }
    let mut kernel = vec![0; size];
    let mut cursor = 0;
    while cursor < size {
        let tag = take(&mut input, 1)?[0];
        let count = length(&mut input)?;
        if count == 0 || count > size - cursor {
            return Err("invalid kernel chunk length");
        }
        let destination = &mut kernel[cursor..cursor + count];
        match tag {
            0 => {
                let byte = take(&mut input, 1)?[0];
                // The allocation is already zeroed. Keep zero padding lazy on
                // allocators backed by fresh anonymous pages instead of touching
                // it a second time while expanding the embedded kernel.
                if byte != 0 {
                    destination.fill(byte);
                }
            }
            1 => destination.copy_from_slice(take(&mut input, count)?),
            _ => return Err("invalid kernel chunk tag"),
        }
        cursor += count;
    }
    if !input.is_empty() {
        return Err("trailing kernel bundle bytes");
    }
    Ok(kernel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_instructions_traps_and_aligned_holes() {
        let mut kernel = (0..=255).collect::<Vec<u8>>();
        kernel.extend_from_slice(&[0xcc; 255]);
        kernel.extend_from_slice(&[0; 65536]);
        kernel.extend_from_slice(&[0xcc; 2 * 1024 * 1024]);
        kernel.extend_from_slice(&[0x90, 0xc3, 0x0f, 0x0b]);
        let packed = pack(&kernel);
        assert!(packed.len() < 1024);
        assert_eq!(unpack(&packed).unwrap(), kernel);
        for kernel in [vec![0; 256], vec![0xff; 257], vec![0xcc; 255], vec![1]] {
            assert_eq!(unpack(&pack(&kernel)).unwrap(), kernel);
        }
    }

    #[test]
    fn rejects_truncated_and_trailing_data() {
        let packed = pack(&[7; 256]);
        for end in 0..packed.len() {
            assert!(unpack(&packed[..end]).is_err());
        }
        let mut trailing = packed.clone();
        trailing.push(0);
        assert!(unpack(&trailing).is_err());
        let literal = pack(&[1, 2, 3]);
        assert!(unpack(&literal[..literal.len() - 1]).is_err());
    }

    #[test]
    fn rejects_invalid_sizes_and_tags_before_expanding() {
        let packed = pack(&[7; 256]);
        for size in [0_u64, (MAX_KERNEL_SIZE + 1) as u64, u64::MAX] {
            let mut invalid = packed.clone();
            invalid[8..16].copy_from_slice(&size.to_le_bytes());
            assert!(unpack(&invalid).is_err());
        }
        for count in [0_u64, 257, u64::MAX] {
            let mut invalid = packed.clone();
            invalid[17..25].copy_from_slice(&count.to_le_bytes());
            assert!(unpack(&invalid).is_err());
        }
        let mut invalid = packed;
        invalid[16] = 2;
        assert!(unpack(&invalid).is_err());
        invalid[0] = 0;
        assert!(unpack(&invalid).is_err());
    }
}
