use std::borrow::Cow;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::{mem, thread};

use vm_memory::GuestMemoryMmap;

use crate::devices::virtio::console::console_control::ConsoleControl;
use crate::devices::virtio::console::port_io::{PortInput, PortOutput};
use crate::devices::virtio::console::process_rx::process_rx;
use crate::devices::virtio::console::process_tx::process_tx;
use crate::devices::virtio::port_io::PortTerminalProperties;
use crate::devices::virtio::{InterruptTransport, Queue};

pub struct PortDescription {
    pub name: Cow<'static, str>,
    pub input: Option<Box<dyn PortInput + Send>>,
    pub output: Option<Box<dyn PortOutput + Send>>,
    pub terminal: Option<Box<dyn PortTerminalProperties>>,
}

impl PortDescription {
    pub fn console(
        input: Option<Box<dyn PortInput + Send>>,
        output: Option<Box<dyn PortOutput + Send>>,
        terminal: Box<dyn PortTerminalProperties>,
    ) -> Self {
        Self {
            name: "".into(),
            input,
            output,
            terminal: Some(terminal),
        }
    }

    pub fn output_pipe(
        name: impl Into<Cow<'static, str>>,
        output: Box<dyn PortOutput + Send>,
    ) -> Self {
        Self {
            name: name.into(),
            input: None,
            output: Some(output),
            terminal: None,
        }
    }

    pub fn input_pipe(
        name: impl Into<Cow<'static, str>>,
        input: Box<dyn PortInput + Send>,
    ) -> Self {
        Self {
            name: name.into(),
            input: Some(input),
            output: None,
            terminal: None,
        }
    }
}

enum PortState {
    Inactive,
    Active {
        stopfd: crate::utils::eventfd::EventFd,
        stop: Arc<AtomicU8>,
        rx_thread: Option<JoinHandle<(Queue, bool)>>,
        tx_thread: Option<JoinHandle<(Queue, bool)>>,
        rx: Option<(Queue, bool)>,
        tx: Option<(Queue, bool)>,
    },
    Frozen {
        rx: Queue,
        tx: Queue,
        rx_closed: bool,
        tx_closed: bool,
    },
}

pub(crate) struct Port {
    port_id: u32,
    /// Empty if no name given
    name: Cow<'static, str>,
    state: PortState,
    input: Option<Arc<Mutex<Box<dyn PortInput + Send>>>>,
    output: Option<Arc<Mutex<Box<dyn PortOutput + Send>>>>,
    terminal: Option<Box<dyn PortTerminalProperties>>,
}

impl Port {
    pub(crate) fn new(port_id: u32, description: PortDescription) -> Self {
        Self {
            port_id,
            name: description.name,
            state: PortState::Inactive,
            input: description.input.map(|input| Arc::new(Mutex::new(input))),
            output: description
                .output
                .map(|output| Arc::new(Mutex::new(output))),
            terminal: description.terminal,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn terminal(&self) -> Option<&dyn PortTerminalProperties> {
        self.terminal.as_deref()
    }

    pub fn notify_rx(&self) {
        if let PortState::Active {
            rx_thread: Some(handle),
            ..
        } = &self.state
        {
            handle.thread().unpark()
        }
    }

    pub fn notify_tx(&self) {
        if let PortState::Active {
            tx_thread: Some(handle),
            ..
        } = &self.state
        {
            handle.thread().unpark()
        }
    }

    pub fn start(
        &mut self,
        mem: GuestMemoryMmap,
        rx_queue: Queue,
        tx_queue: Queue,
        interrupt: InterruptTransport,
        control: Arc<ConsoleControl>,
    ) {
        self.start_inner(mem, (rx_queue, false), (tx_queue, false), interrupt, control);
    }

    fn start_inner(
        &mut self,
        mem: GuestMemoryMmap,
        rx: (Queue, bool),
        tx: (Queue, bool),
        interrupt: InterruptTransport,
        control: Arc<ConsoleControl>,
    ) {
        let (rx_queue, rx_closed) = rx;
        let (tx_queue, tx_closed) = tx;
        if let PortState::Active { .. } = &mut self.state {
            self.shutdown();
        };

        let input = self.input.as_ref().cloned();
        let output = self.output.as_ref().cloned();

        let stopfd = crate::utils::eventfd::EventFd::new(crate::utils::eventfd::EFD_NONBLOCK)
            .expect("Failed to create EventFd for interrupt_evt");
        let stop = Arc::new(AtomicU8::new(0));
        let mut rx = Some((rx_queue, rx_closed));
        let mut tx = Some((tx_queue, tx_closed));

        let rx_thread = input.filter(|_| !rx_closed).map(|input| {
            let (rx_queue, _) = rx.take().unwrap();
            let mem = mem.clone();
            let interrupt = interrupt.clone();
            let port_id = self.port_id;
            let stopfd = stopfd.try_clone().unwrap();
            let stop = stop.clone();
            thread::Builder::new()
                .name("console port".into())
                .spawn(move || {
                    process_rx(
                        mem, rx_queue, interrupt, input, control, port_id,
                        super::process_rx::RxStop { event: stopfd, state: stop },
                    )
                })
                .unwrap()
        });

        let tx_thread = output.filter(|_| !tx_closed).map(|output| {
            let (tx_queue, _) = tx.take().unwrap();
            let stop = stop.clone();
            thread::spawn(move || process_tx(mem, tx_queue, interrupt, output, stop))
        });

        self.state = PortState::Active {
            stopfd,
            stop,
            rx_thread,
            tx_thread,
            rx,
            tx,
        }
    }

    pub fn is_started(&self) -> bool {
        !matches!(self.state, PortState::Inactive)
    }

    pub fn freeze(&mut self) -> Result<bool, String> {
        let PortState::Active {
            stopfd,
            stop,
            rx_thread,
            tx_thread,
            rx,
            tx,
        } = &mut self.state
        else {
            return Ok(true);
        };
        if stop.load(Ordering::Acquire) != 1 {
            stop.store(1, Ordering::Release);
            stopfd.write(1).map_err(|e| e.to_string())?;
            if let Some(worker) = rx_thread.as_ref() {
                worker.thread().unpark();
            }
            if let Some(worker) = tx_thread.as_ref() {
                worker.thread().unpark();
            }
        }
        for (worker, queue) in [(rx_thread, &mut *rx), (tx_thread, &mut *tx)] {
            if worker.as_ref().is_some_and(|worker| worker.is_finished()) {
                *queue = Some(
                    worker
                        .take()
                        .unwrap()
                        .join()
                        .map_err(|_| "console worker panicked")?,
                );
            }
        }
        let (Some(_), Some(_)) = (&rx, &tx) else {
            return Ok(false);
        };
        let (rx, rx_closed) = rx.take().unwrap();
        let (tx, tx_closed) = tx.take().unwrap();
        self.state = PortState::Frozen {
            rx,
            tx,
            rx_closed,
            tx_closed,
        };
        Ok(true)
    }

    pub fn thaw(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: InterruptTransport,
        control: Arc<ConsoleControl>,
    ) -> Result<(), String> {
        if matches!(self.state, PortState::Active { .. }) {
            return Err("console freeze still pending".into());
        }
        let old = mem::replace(&mut self.state, PortState::Inactive);
        if let PortState::Frozen {
            rx,
            tx,
            rx_closed,
            tx_closed,
        } = old
        {
            self.start_inner(mem, (rx, rx_closed), (tx, tx_closed), interrupt, control);
        }
        Ok(())
    }

    pub fn frozen_queues(&self) -> Option<(&Queue, &Queue)> {
        match &self.state {
            PortState::Frozen { rx, tx, .. } => Some((rx, tx)),
            _ => None,
        }
    }

    pub fn capture_state(&self) -> Result<super::device::PortSnapshot, String> {
        let (started, rx_closed, tx_closed) = match self.state {
            PortState::Inactive => (false, false, false),
            PortState::Frozen {
                rx_closed,
                tx_closed,
                ..
            } => (true, rx_closed, tx_closed),
            PortState::Active { .. } => return Err("console port must be frozen".into()),
        };
        if tx_closed {
            return Err(
                "console output failed during a request; cannot snapshot partial external effects"
                    .into(),
            );
        }
        let input = self
            .input
            .as_ref()
            .map(|io| {
                io.try_lock()
                    .map_err(|_| "console input is busy".to_string())?
                    .capture_state()
            })
            .transpose()?;
        let output = self
            .output
            .as_ref()
            .map(|io| {
                io.try_lock()
                    .map_err(|_| "console output is busy".to_string())?
                    .capture_state()
            })
            .transpose()?;
        Ok(super::device::PortSnapshot {
            name: self.name.to_string(),
            input,
            output,
            terminal: self.terminal.is_some(),
            started,
            rx_closed,
            tx_closed,
        })
    }

    pub fn validate_state(&self, saved: &super::device::PortSnapshot) -> Result<(), String> {
        if saved.name != self.name
            || saved.input.is_some() != self.input.is_some()
            || saved.output.is_some() != self.output.is_some()
            || saved.terminal != self.terminal.is_some()
            || self.is_started()
            || saved.tx_closed
            || (!saved.started && saved.rx_closed)
        {
            return Err("console port topology or lifecycle mismatch".into());
        }
        Ok(())
    }

    pub fn restore_io(&mut self, saved: &super::device::PortSnapshot) -> Result<(), String> {
        if let (Some(io), Some(state)) = (&self.input, &saved.input) {
            io.try_lock()
                .map_err(|_| "console input is busy")?
                .restore_state(state)?;
        }
        if let (Some(io), Some(state)) = (&self.output, &saved.output) {
            io.try_lock()
                .map_err(|_| "console output is busy")?
                .restore_state(state)?;
        }
        Ok(())
    }

    pub fn restore_state(&mut self, saved: &super::device::PortSnapshot, rx: Queue, tx: Queue) {
        self.state = PortState::Frozen {
            rx,
            tx,
            rx_closed: saved.rx_closed,
            tx_closed: saved.tx_closed,
        };
    }

    pub fn shutdown(&mut self) {
        if let PortState::Active {
            stopfd,
            stop,
            tx_thread,
            rx_thread,
            ..
        } = &mut self.state
        {
            stop.store(2, Ordering::Release);
            if let Some(tx_thread) = mem::take(tx_thread) {
                tx_thread.thread().unpark();
                if let Err(e) = tx_thread.join() {
                    log::error!(
                        "Failed to flush tx for port {port_id}, thread panicked: {e:?}",
                        port_id = self.port_id
                    )
                }
            }
            stopfd.write(1).unwrap();
            if let Some(rx_thread) = mem::take(rx_thread) {
                rx_thread.thread().unpark();
                if let Err(e) = rx_thread.join() {
                    log::error!(
                        "Failed to flush tx for port {port_id}, thread panicked: {e:?}",
                        port_id = self.port_id
                    )
                }
            }
        };
        self.state = PortState::Inactive;
    }
}
