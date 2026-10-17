//! Immutable handle handoff over the existing deadline-bounded pool transport.
use super::*;
use crate::ram_backing::sealed::SealedObject;

/// A validated connection reference and independently owned immutable backing.
/// Restore is local; release after all guest pages are restored or discarded.
pub struct SealedReference {
    session: Arc<[u8; 16]>,
    token: u64,
    object: SealedObject,
}

/// Linux compressed-pool client. The caller authenticates the endpoint first.
/// RPCs serialize through mutable access. Publication validates the entire
/// decoded block before returning an owned descriptor. Transport loss disables
/// new publications permanently, while existing references remain recoverable.
pub struct SealedClient {
    client: PoolClient,
    local: BTreeMap<ImageId, (u64, usize)>,
}
impl SealedClient {
    /// Negotiate immutable compressed handoff; legacy GET-only and physical
    /// pools are rejected. Failure never requests guest-memory reclamation.
    pub fn new(mut client: PoolClient) -> io::Result<Self> {
        if client.shared_file.is_some() {
            return Err(invalid("physical mapping already negotiated"));
        }
        client.exchange(|stream| {
            stream.write_all(&[SEALED])?;
            Ok(match status(stream)? {
                Some(error) => Err(error),
                None => Ok(()),
            })
        })?;
        Ok(Self {
            client,
            local: BTreeMap::new(),
        })
    }
    /// Whether the publication connection has not yet failed. This is not a
    /// liveness probe; existing references do not require a live connection.
    pub fn connected(&self) -> bool {
        self.client.stream.is_some()
    }
    /// Publish one stable 64 KiB block. Fully consumed capacity/compressibility
    /// rejection and disabled publication return WouldBlock with no new handle.
    /// Any interrupted/invalid handoff closes the connection before returning.
    pub fn put(&mut self, bytes: &[u8]) -> io::Result<SealedReference> {
        if bytes.len() != BLOCK_BYTES {
            return Err(invalid("sealed pool requires 64 KiB blocks"));
        }
        if !self.connected() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "sealed pool publication disabled",
            ));
        }
        let expected = identity(bytes);
        let session = self.client.session.clone();
        let object = self.client.exchange(|stream| {
            let mut frame = Vec::with_capacity(bytes.len() + 5);
            frame.push(PUT_SEALED);
            frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            frame.extend_from_slice(bytes);
            stream.write_all(&frame)?;
            if let Some(error) = status(stream)? {
                return Ok(Err(io::Error::new(io::ErrorKind::WouldBlock, error)));
            }
            let token = read_u64(stream)?;
            let mut id = [0; 32];
            stream.read_exact(&mut id)?;
            let length = read_u32(stream)? as usize;
            if token == 0 || id != expected || length != bytes.len() {
                return Err(invalid("invalid sealed pool object reply"));
            }
            let file = std::fs::File::from(stream.receive_mapping_fd()?);
            let object = SealedObject::receive(file, id, bytes)?;
            Ok(Ok(SealedReference {
                session,
                token,
                object,
            }))
        })?;
        self.local
            .entry(expected)
            .or_insert((0, object.object.charged_bytes()))
            .0 += 1;
        Ok(object)
    }
    fn owns(&self, object: &SealedReference) -> io::Result<()> {
        if self.client.session != object.session {
            return Err(invalid("sealed reference belongs to another session"));
        }
        Ok(())
    }
    /// Decode and checksum from the retained file, with no IPC or pool liveness
    /// check. Wrong ownership or output length fails before writing output.
    pub fn restore(&self, object: &SealedReference, output: &mut [u8]) -> io::Result<()> {
        self.owns(object)?;
        object.object.restore(output)
    }
    /// Drop local authority and notify the publisher when connected. A failed
    /// notification disables publication; the service keeps disconnected pins
    /// budgeted until this process exits. Already restored RAM remains valid.
    pub fn release(&mut self, object: SealedReference) -> io::Result<()> {
        self.owns(&object)?;
        if self.connected() {
            let result = self.client.exchange(|stream| {
                stream.write_all(&[RELEASE])?;
                stream.write_all(&object.token.to_le_bytes())?;
                Ok(match status(stream)? {
                    Some(error) => Err(error),
                    None => Ok(()),
                })
            });
            if result.is_err() {
                self.client.stream.take();
            }
        }
        let id = object.object.id();
        let (count, _) = self
            .local
            .get_mut(&id)
            .ok_or_else(|| invalid("unknown sealed reference"))?;
        *count -= 1;
        if *count == 0 {
            self.local.remove(&id);
        }
        Ok(())
    }
    /// While connected: pool-wide page-rounded encoded storage and this
    /// session's references. After connection loss: local pinned objects/bytes,
    /// zero cross-session count (sharing is then unknown). No reconnect or stale
    /// remote counters. Use `connected` to distinguish these accounting scopes.
    pub fn stats(&mut self) -> io::Result<PoolStats> {
        if self.connected() {
            match self.client.stats() {
                Ok(stats) => return Ok(stats),
                Err(_) => {
                    self.client.stream.take();
                }
            }
        }
        Ok(PoolStats {
            encoded_bytes: self.local.values().map(|(_, size)| *size as u64).sum(),
            objects: self.local.len() as u64,
            session_references: self.local.values().map(|(count, _)| count).sum(),
            cross_session_objects: 0,
        })
    }
}

impl pvisor_vm::api::ColdRamStore for SealedClient {
    type Object = SealedReference;
    fn put(&mut self, bytes: &[u8]) -> io::Result<Self::Object> {
        self.put(bytes)
    }
    fn restore(&mut self, object: &Self::Object, output: &mut [u8]) -> io::Result<()> {
        Self::restore(self, object, output)
    }
    fn release(&mut self, object: Self::Object) -> io::Result<()> {
        self.release(object)
    }
    fn shared_mapping(&self, _: &Self::Object) -> Option<pvisor_vm::api::SharedRamMapping> {
        None
    }
    fn stats(&mut self) -> io::Result<pvisor_vm::api::ColdRamPoolStats> {
        let stats = self.stats()?;
        Ok(pvisor_vm::api::ColdRamPoolStats {
            encoded_bytes: stats.encoded_bytes,
            objects: stats.objects,
            session_references: stats.session_references,
            cross_session_objects: stats.cross_session_objects,
        })
    }
}
