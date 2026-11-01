use crate::api::{RamDeltaCapture, RamDeltaSpec, RamDeltaState};
impl RamDeltaState for RamDeltaSpec {
    fn validate(&self) -> Result<(), String> {
        if self.length == 0
            || self.length > 64 * 1024 * 1024 * 1024
            || !self.block_bytes.is_power_of_two()
            || !(4096..=1024 * 1024).contains(&self.block_bytes)
            || self.length.div_ceil(u64::from(self.block_bytes)) > 1 << 20
            || self.base_sha256.len() != 64
            || !self
                .base_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid incremental RAM baseline".into());
        }
        Ok(())
    }
}
impl RamDeltaState for RamDeltaCapture {
    fn validate(&self) -> Result<(), String> {
        RamDeltaSpec {
            device: 0,
            inode: 0,
            length: self.length,
            block_bytes: self.block_bytes,
            base_sha256: self.base_sha256.clone(),
        }
        .validate()?;
        if self.version != 1
            || self.changed_blocks.len() as u64 > self.length.div_ceil(u64::from(self.block_bytes))
            || self
                .changed_blocks
                .iter()
                .any(|index| *index >= self.length.div_ceil(u64::from(self.block_bytes)))
            || self
                .changed_blocks
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err("invalid incremental RAM capture inventory".into());
        }
        Ok(())
    }
}
