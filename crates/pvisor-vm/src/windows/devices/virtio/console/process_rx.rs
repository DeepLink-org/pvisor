use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::{io, thread};

use vm_memory::{GuestMemory, GuestMemoryError, GuestMemoryMmap, GuestMemoryRegion};

use crate::devices::virtio::console::console_control::ConsoleControl;
use crate::devices::virtio::console::port_io::PortInput;
use crate::devices::virtio::{DescriptorChain, InterruptTransport, Queue};

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_rx(
    mem: GuestMemoryMmap,
    mut queue: Queue,
    interrupt: InterruptTransport,
    input: Arc<Mutex<Box<dyn PortInput + Send>>>,
    control: Arc<ConsoleControl>,
    port_id: u32,
    stopfd: crate::utils::eventfd::EventFd,
    stop: Arc<AtomicBool>,
) {
    let mem = &mem;
    let mut eof = false;

    let mut input = input.lock().unwrap();
    loop {
        let Some(head) = pop_head_blocking(
            &mut queue,
            mem,
            &|| interrupt.signal_used_queue(),
            &stop,
        ) else {
            return;
        };

        let head_index = head.index;
        let mut bytes_read = 0;
        for chain in head.into_iter().writable() {
            match read_to_desc(chain, input.as_mut(), &mut eof) {
                Ok(0) => {
                    break;
                }
                Ok(len) => {
                    bytes_read += len;
                }
                Err(e) => {
                    log::error!("Failed to read: {e:?}")
                }
            }
        }

        if bytes_read != 0 {
            log::trace!("Rx {bytes_read} bytes queue len{}", queue.len(mem));
            if let Err(e) = queue.add_used(mem, head_index, bytes_read as u32) {
                error!("failed to add used elements to the queue: {e:?}");
            }
        }

        // We signal_used_queue only when we get WouldBlock or EOF
        if eof {
            interrupt.signal_used_queue();
            log::trace!("signaling EOF on port {port_id}");
            control.port_open(port_id, false);
            return;
        } else if bytes_read == 0 {
            queue.undo_pop();
            interrupt.signal_used_queue();
            input.wait_until_readable(Some(&stopfd));
        }

        if stop.load(Ordering::Acquire) {
            return;
        }
    }
}

fn pop_head_blocking<'mem>(
    queue: &mut Queue,
    mem: &'mem GuestMemoryMmap,
    notify: &impl Fn(),
    stop: &AtomicBool,
) -> Option<DescriptorChain<'mem>> {
    loop {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        match queue.pop(mem) {
            Some(descriptor) => return Some(descriptor),
            None => {
                notify();
                // shutdown sets stop before unparking. Check both before the
                // pop and before parking so an empty queue cannot retain join.
                if stop.load(Ordering::Acquire) {
                    return None;
                }
                thread::park();
                log::trace!("rx unparked, queue len {}", queue.len(mem))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    use vm_memory::GuestAddress;

    #[test]
    fn shutdown_wakes_an_empty_descriptor_queue() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (waiting, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 65536)]).unwrap();
            let mut queue = Queue::new(8);
            let head = pop_head_blocking(&mut queue, &mem, &|| {
                let _ = waiting.send(());
            }, &worker_stop);
            finished.send(head.is_none()).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        stop.store(true, Ordering::Release);
        worker.thread().unpark();
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap());
        worker.join().unwrap();
    }
}

#[allow(deprecated)]
fn read_to_desc(
    desc: DescriptorChain,
    input: &mut (dyn PortInput + Send),
    eof: &mut bool,
) -> Result<usize, GuestMemoryError> {
    desc.mem
        .try_access(desc.len as usize, desc.addr, |_, len, addr, region| {
            let mut target = region.get_slice(addr, len).unwrap();
            match input.read_volatile(&mut target) {
                Ok(n) => {
                    if n == 0 {
                        *eof = true
                    }
                    Ok(n)
                }
                // We can't return an error otherwise we would not know how many bytes were processed before WouldBlock
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
                Err(e) => Err(GuestMemoryError::IOError(e)),
            }
        })
}
