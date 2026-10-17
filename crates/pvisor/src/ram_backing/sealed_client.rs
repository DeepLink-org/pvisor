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
    pending: Vec<u64>,
    disabled_reported: bool,
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
            pending: Vec::new(),
            disabled_reported: false,
        })
    }
    /// Whether the publication connection has not yet failed. This is not a
    /// liveness probe; existing references do not require a live connection.
    pub fn connected(&self) -> bool {
        self.client.stream.is_some()
    }
    fn disable(&mut self) {
        self.client.stream.take();
        self.pending.clear();
        if !self.disabled_reported {
            eprintln!("pvisor-sealed-pool publication_disabled=true accounting_scope=local-pins");
            self.disabled_reported = true;
        }
    }
    // Never called from restore/release: those operations perform no publisher
    // RPC, though they may wait for another operation holding the store lock. Tokens remain covered by the server's reference limit
    // until PUT/STATS flushes them, or process exit releases disconnected pins.
    fn flush_releases(&mut self) {
        while self.connected() && !self.pending.is_empty() {
            let count = self.pending.len().min(256);
            let tokens = &self.pending[..count];
            let result = self.client.exchange(|stream| {
                let mut frame = Vec::with_capacity(count * 9);
                for token in tokens {
                    frame.push(RELEASE);
                    frame.extend_from_slice(&token.to_le_bytes());
                }
                stream.write_all(&frame)?;
                for _ in tokens {
                    if let Some(error) = status(stream)? {
                        return Err(error);
                    }
                }
                Ok(Ok(()))
            });
            if result.is_err() {
                self.disable();
            } else {
                self.pending.drain(..count);
            }
        }
    }
    /// Publish one stable 64 KiB block. Fully consumed capacity/compressibility
    /// rejection and disabled publication return WouldBlock with no new handle.
    /// Any interrupted/invalid handoff closes the connection before returning.
    pub fn put(&mut self, bytes: &[u8]) -> io::Result<SealedReference> {
        if bytes.len() != BLOCK_BYTES {
            return Err(invalid("sealed pool requires 64 KiB blocks"));
        }
        self.flush_releases();
        if !self.connected() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "sealed pool publication disabled",
            ));
        }
        let expected = identity(bytes);
        let session = self.client.session.clone();
        let result = self.client.exchange(|stream| {
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
        });
        if !self.connected() {
            self.disable();
        }
        let object = result?;
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
    /// Drop local authority without IPC and queue a bounded release token.
    /// The next put/stats flushes notifications outside fault recovery. A failed
    /// flush disables publication; disconnected pins stay budgeted at the
    /// publisher until this process exits. Already restored RAM remains valid.
    pub fn release(&mut self, object: SealedReference) -> io::Result<()> {
        self.owns(&object)?;
        if self.connected() {
            self.pending.push(object.token);
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
        self.flush_releases();
        if self.connected() {
            match self.client.stats() {
                Ok(stats) => return Ok(stats),
                Err(_) => {
                    self.disable();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn client(
        pool: Arc<Mutex<crate::ram_backing::sealed::SealedPool>>,
        refs: usize,
    ) -> (SealedClient, std::thread::JoinHandle<io::Result<()>>) {
        let (client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || serve_sealed(server, pool, refs));
        let mut client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
        // This is also the runtime's safe negotiation order.
        assert!(client.enable_shared_mapping().is_err());
        (SealedClient::new(client).unwrap(), worker)
    }

    #[test]
    fn sealed_sessions_share_one_compressed_inode_and_release_independently() {
        use std::os::unix::fs::MetadataExt;
        let pool = Arc::new(Mutex::new(crate::ram_backing::sealed::SealedPool::new(
            4096, 1,
        )));
        let (mut a, ta) = client(pool.clone(), 2);
        let (mut b, tb) = client(pool, 2);
        let bytes = vec![0x61; BLOCK_BYTES];
        let ar = a.put(&bytes).unwrap();
        let br = b.put(&bytes).unwrap();
        assert_eq!(
            ar.object.file().metadata().unwrap().ino(),
            br.object.file().metadata().unwrap().ino()
        );
        let stats = a.stats().unwrap();
        assert_eq!(
            (
                stats.encoded_bytes,
                stats.objects,
                stats.cross_session_objects
            ),
            (4096, 1, 1)
        );
        let mut out = vec![0; BLOCK_BYTES];
        assert!(b.restore(&ar, &mut out).is_err());
        assert!(a.restore(&ar, &mut out[..4096]).is_err());
        a.release(ar).unwrap();
        assert_eq!(a.stats().unwrap().session_references, 0);
        b.restore(&br, &mut out).unwrap();
        assert_eq!(out, bytes);
        b.release(br).unwrap();
        assert_eq!(b.stats().unwrap().objects, 0);
        drop(a);
        drop(b);
        ta.join().unwrap().unwrap();
        tb.join().unwrap().unwrap();
    }

    #[test]
    fn sealed_reference_budget_and_deferred_release_preserve_connection() {
        let pool = Arc::new(Mutex::new(crate::ram_backing::sealed::SealedPool::new(
            4096, 1,
        )));
        let (mut a, worker) = client(pool, 1);
        assert!(a.put(&[0; 4096]).is_err());
        let ar = a.put(&vec![7; BLOCK_BYTES]).unwrap();
        assert_eq!(
            a.put(&vec![7; BLOCK_BYTES]).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            a.put(&vec![8; BLOCK_BYTES]).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(a.connected());
        a.release(ar).unwrap();
        assert_eq!(a.pending.len(), 1);
        let next = a.put(&vec![8; BLOCK_BYTES]).unwrap();
        assert!(a.pending.is_empty());
        a.release(next).unwrap();
        assert_eq!(a.stats().unwrap().objects, 0);
        drop(a);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn legacy_get_pool_cannot_negotiate_sealed_ownership() {
        let pool = Arc::new(Mutex::new(CompressedPool::new(4096, 1)));
        let (client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || serve(server, pool, 1));
        let client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
        assert!(SealedClient::new(client).is_err());
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn sealed_restore_and_release_never_contact_the_publisher() {
        let pool = Arc::new(Mutex::new(crate::ram_backing::sealed::SealedPool::new(
            4096, 1,
        )));
        let (mut a, worker) = client(pool, 1);
        let ar = a.put(&vec![7; BLOCK_BYTES]).unwrap();
        // Remove the transport while retaining a separate connection solely to
        // flush cleanup. Recovery itself must work with no socket in the client.
        let stream = a.client.stream.take().unwrap();
        let mut out = vec![0; BLOCK_BYTES];
        a.restore(&ar, &mut out).unwrap();
        assert_eq!(out, vec![7; BLOCK_BYTES]);
        let token = ar.token;
        a.release(ar).unwrap();
        assert_eq!(a.stats().unwrap().session_references, 0);
        assert!(a.put(&vec![8; BLOCK_BYTES]).is_err());
        // Explicitly finish the server's reference so its same-process pidfd
        // does not wait for this test process to exit.
        a.client.stream = Some(stream);
        a.pending.push(token);
        a.stats().unwrap();
        drop(a);
        worker.join().unwrap().unwrap();
    }
}
