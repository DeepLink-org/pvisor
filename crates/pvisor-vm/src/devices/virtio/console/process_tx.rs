use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::{io, thread};

use vm_memory::{GuestMemory, GuestMemoryError, GuestMemoryMmap, GuestMemoryRegion};

use crate::devices::virtio::console::port_io::PortOutput;
use crate::devices::virtio::{DescriptorChain, InterruptTransport, Queue};

pub(crate) fn process_tx(
    mem: GuestMemoryMmap,
    mut queue: Queue,
    interrupt: InterruptTransport,
    output: Arc<Mutex<Box<dyn PortOutput + Send>>>,
    stop: Arc<AtomicU8>,
) -> (Queue, bool) {
    loop {
        let Some(head) = pop_head_blocking(&mut queue, &mem, &interrupt, &stop) else {
            return (queue, false);
        };

        let head_index = head.index;
        let mut bytes_written = 0;

        for desc in head.into_iter().readable() {
            let desc_len = desc.len as usize;
            match write_desc_to_output(desc, output.lock().unwrap().as_mut(), &interrupt) {
                Ok(0) => return (queue, true),
                Ok(n) => {
                    assert_eq!(n, desc_len);
                    bytes_written += n;
                }
                Err(e) => {
                    log::error!("Failed to write output: {e}");
                    // External writes may already have succeeded. Mark the endpoint failed
                    // rather than presenting this request as safely resumable.
                    return (queue, true);
                }
            }
        }

        if bytes_written == 0 {
            log::trace!("Tx Add used {bytes_written}");
            queue.undo_pop();
        } else {
            log::trace!("Tx add used {bytes_written}");
            if let Err(e) = queue.add_used(&mem, head_index, bytes_written as u32) {
                error!("failed to add used elements to the queue: {e:?}");
            }
        }
    }
}

fn pop_head_blocking<'mem>(
    queue: &mut Queue,
    mem: &'mem GuestMemoryMmap,
    interrupt: &InterruptTransport,
    stop: &AtomicU8,
) -> Option<DescriptorChain<'mem>> {
    loop {
        if stop.load(Ordering::Acquire) == 1 {
            return None;
        }
        match queue.pop(mem) {
            Some(descriptor) => break Some(descriptor),
            None => {
                interrupt.signal_used_queue();
                if stop.load(Ordering::Acquire) != 0 {
                    break None;
                }
                thread::park();
                log::trace!("tx unparked, queue len {}", queue.len(mem))
            }
        }
    }
}

fn write_desc_to_output(
    desc: DescriptorChain,
    output: &mut (dyn PortOutput + Send),
    interrupt: &InterruptTransport,
) -> Result<usize, GuestMemoryError> {
    // try_access tracks partial progress across guest-memory regions.
    #[allow(deprecated)]
    desc.mem
        .try_access(desc.len as usize, desc.addr, |_, len, addr, region| {
            let src = region.get_slice(addr, len).unwrap();
            loop {
                log::trace!("Tx {src:?}, write_volatile {len} bytes");
                match output.write_volatile(&src) {
                    // try_access resumes partial writes at the remaining offset.
                    Ok(n) => break Ok(n),
                    // We can't return an error otherwise we would not know how many bytes were processed before WouldBlock
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        log::trace!("Tx wait for output (would block)");
                        interrupt.signal_used_queue();
                        output.wait_until_writable();
                    }
                    Err(e) => break Err(GuestMemoryError::IOError(e)),
                }
            }
        })
}
