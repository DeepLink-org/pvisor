use std::cmp;
use std::io::Write;
use std::iter::zip;
use std::mem::{size_of, size_of_val};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;

use utils::eventfd::EventFd;
use vm_memory::{ByteValued, Bytes, GuestMemoryMmap};

use super::super::{
    ActivateError, ActivateResult, DeviceQueue, DeviceState, QueueConfig, VirtioDevice,
};
use super::{defs, defs::control_event, defs::uapi};
use crate::virtio::console::console_control::{
    ConsoleControl, VirtioConsoleControl, VirtioConsoleResize,
};
use crate::virtio::console::defs::QUEUE_SIZE;
use crate::virtio::console::port::Port;
use crate::virtio::console::port_queue_mapping::{
    num_queues, port_id_to_queue_idx, QueueDirection,
};
use crate::virtio::{InterruptTransport, PortDescription, VmmExitObserver};

pub(crate) const CONTROL_RXQ_INDEX: usize = 2;
pub(crate) const CONTROL_TXQ_INDEX: usize = 3;

pub(crate) const AVAIL_FEATURES: u64 = (1 << uapi::VIRTIO_CONSOLE_F_SIZE as u64)
    | (1 << uapi::VIRTIO_CONSOLE_F_MULTIPORT as u64)
    | (1 << uapi::VIRTIO_F_VERSION_1 as u64);

#[derive(Copy, Clone, Debug, Default)]
#[repr(C, packed)]
pub struct VirtioConsoleConfig {
    cols: u16,
    rows: u16,
    max_nr_ports: u32,
    emerg_wr: u32,
}

// Safe because it only has data and has no implicit padding.
unsafe impl ByteValued for VirtioConsoleConfig {}

impl VirtioConsoleConfig {
    pub fn new(cols: u16, rows: u16, max_nr_ports: u32) -> Self {
        VirtioConsoleConfig {
            cols,
            rows,
            max_nr_ports,
            emerg_wr: 0u32,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSnapshot {
    pub(crate) name: String,
    pub(crate) input: Option<super::port_io::PortIoSnapshot>,
    pub(crate) output: Option<super::port_io::PortIoSnapshot>,
    pub(crate) terminal: bool,
    pub(crate) started: bool,
    pub(crate) rx_closed: bool,
    pub(crate) tx_closed: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleSnapshot {
    config: [u32; 4],
    ports: Vec<PortSnapshot>,
    control: Vec<Vec<u8>>,
}

pub struct Console {
    pub(crate) device_state: DeviceState,
    pub(crate) control: Arc<ConsoleControl>,
    pub(crate) ports: Vec<Port>,

    queue_config: Vec<QueueConfig>,
    // Queues are stored as Option so individual queues can be taken when ports start.
    pub(crate) queues: Vec<Option<DeviceQueue>>,
    // TODO: move the queue event handling to the correct threads!
    pub(crate) queue_events: Vec<Arc<EventFd>>,

    pub(crate) avail_features: u64,
    pub(crate) acked_features: u64,

    pub(crate) activate_evt: EventFd,
    pub(crate) sigwinch_evt: EventFd,

    config: VirtioConsoleConfig,
    pub(crate) frozen: bool,
    restore: Option<Vec<PortSnapshot>>,
}

impl Console {
    pub fn new(ports: Vec<PortDescription>) -> super::Result<Console> {
        assert!(!ports.is_empty(), "Expected at least 1 port");

        let num_queues = num_queues(ports.len());
        let queue_config: Vec<QueueConfig> = (0..num_queues)
            .map(|_| QueueConfig::new(QUEUE_SIZE))
            .collect();

        let ports: Vec<Port> = zip(0u32.., ports)
            .map(|(port_id, description)| Port::new(port_id, description))
            .collect();

        let (cols, rows) = ports[0]
            .terminal()
            .map(|t| t.get_win_size())
            .unwrap_or((0, 0));
        let config = VirtioConsoleConfig::new(cols, rows, ports.len() as u32);

        Ok(Console {
            control: ConsoleControl::new(),
            ports,
            queue_config,
            queues: Vec::new(),
            queue_events: Vec::new(),
            avail_features: AVAIL_FEATURES,
            acked_features: 0,
            activate_evt: EventFd::new(utils::eventfd::EFD_NONBLOCK)
                .map_err(super::ConsoleError::EventFd)?,
            sigwinch_evt: EventFd::new(utils::eventfd::EFD_NONBLOCK)
                .map_err(super::ConsoleError::EventFd)?,
            device_state: DeviceState::Inactive,
            config,
            frozen: false,
            restore: None,
        })
    }

    pub fn id(&self) -> &str {
        defs::CONSOLE_DEV_ID
    }

    pub fn get_sigwinch_fd(&self) -> RawFd {
        self.sigwinch_evt.as_raw_fd()
    }

    pub fn update_console_size(&mut self, port_id: u32, cols: u16, rows: u16) {
        log::debug!("update_console_size {port_id}: {cols} {rows}");
        self.control
            .console_resize(port_id, VirtioConsoleResize { rows, cols });
    }

    pub(crate) fn process_control_rx(&mut self) -> bool {
        if self.frozen {
            return false;
        }
        log::trace!("process_control_rx");
        let DeviceState::Activated(ref mem, _) = self.device_state else {
            unreachable!()
        };
        let mut raise_irq = false;

        let control_rx = self.queues[CONTROL_RXQ_INDEX]
            .as_mut()
            .expect("control rx queue should exist");

        while let Some(head) = control_rx.queue.pop(mem) {
            if let Some(buf) = self.control.queue_pop() {
                match mem.write(&buf, head.addr) {
                    Ok(n) => {
                        if n != buf.len() {
                            log::error!("process_control_rx: partial write");
                        }
                        raise_irq = true;
                        log::trace!("process_control_rx wrote {n}");
                        if let Err(e) = control_rx.queue.add_used(mem, head.index, n as u32) {
                            error!("failed to add used elements to the queue: {e:?}");
                        }
                    }
                    Err(e) => {
                        log::error!("process_control_rx failed to write: {e}");
                    }
                }
            } else {
                control_rx.queue.undo_pop();
                break;
            }
        }
        raise_irq
    }

    pub(crate) fn process_control_tx(&mut self) -> bool {
        if self.frozen {
            return false;
        }
        log::trace!("process_control_tx");
        let DeviceState::Activated(ref mem, ref interrupt) = self.device_state else {
            unreachable!()
        };

        let control_tx = self.queues[CONTROL_TXQ_INDEX]
            .as_mut()
            .expect("control tx queue should exist");
        let mut raise_irq = false;

        let mut ports_to_start = Vec::new();

        while let Some(head) = control_tx.queue.pop(mem) {
            raise_irq = true;

            let cmd: VirtioConsoleControl = match mem.read_obj(head.addr) {
                Ok(cmd) => cmd,
                Err(e) => {
                    log::error!(
                    "Failed to read VirtioConsoleControl struct: {e:?}, struct len = {len}, head.len = {head_len}",
                    len = size_of::<VirtioConsoleControl>(),
                    head_len = head.len,
                );
                    continue;
                }
            };
            if let Err(e) = control_tx
                .queue
                .add_used(mem, head.index, size_of_val(&cmd) as u32)
            {
                error!("failed to add used elements to the queue: {e:?}");
            }

            log::trace!("VirtioConsoleControl cmd: {cmd:?}");
            if cmd.event != control_event::VIRTIO_CONSOLE_DEVICE_READY
                && cmd.id as usize >= self.ports.len()
            {
                log::warn!("Invalid console port {}", cmd.id);
                continue;
            }
            match cmd.event {
                control_event::VIRTIO_CONSOLE_DEVICE_READY => {
                    log::debug!(
                        "Device is ready: initialization {}",
                        if cmd.value == 1 { "ok" } else { "failed" }
                    );
                    for port_id in 0..self.ports.len() {
                        self.control.port_add(port_id as u32);
                    }
                }
                control_event::VIRTIO_CONSOLE_PORT_READY => {
                    if cmd.value != 1 {
                        log::error!("Port initialization failed: {cmd:?}");
                        continue;
                    }

                    if let Some(term) = self.ports[cmd.id as usize].terminal() {
                        self.control.mark_console_port(mem, cmd.id);
                        self.control.port_open(cmd.id, true);
                        let (cols, rows) = term.get_win_size();
                        self.control
                            .console_resize(cmd.id, VirtioConsoleResize { cols, rows });
                    } else {
                        // We start with all ports open, this makes sense for now,
                        // because underlying file descriptors STDIN, STDOUT, STDERR are always open too
                        self.control.port_open(cmd.id, true)
                    }

                    let name = self.ports[cmd.id as usize].name();
                    log::trace!("Port ready {id}: {name}", id = cmd.id);
                    if !name.is_empty() {
                        self.control.port_name(cmd.id, name)
                    }
                }
                control_event::VIRTIO_CONSOLE_PORT_OPEN => {
                    let opened = match cmd.value {
                        0 => false,
                        1 => true,
                        _ => {
                            log::error!(
                                "Invalid value ({}) for VIRTIO_CONSOLE_PORT_OPEN on port {}",
                                cmd.value,
                                cmd.id
                            );
                            continue;
                        }
                    };

                    if !opened {
                        log::debug!("Guest closed port {}", cmd.id);
                        continue;
                    }

                    let id = cmd.id as usize;
                    if !self.ports[id].is_started() && !ports_to_start.contains(&id) {
                        ports_to_start.push(id);
                    }
                }
                _ => log::warn!("Unknown console control event {:x}", cmd.event),
            }
        }

        for port_id in ports_to_start {
            log::trace!("Starting port io for port {port_id}");
            let rx_idx = port_id_to_queue_idx(QueueDirection::Rx, port_id);
            let tx_idx = port_id_to_queue_idx(QueueDirection::Tx, port_id);

            // Take ownership of port queues - they are moved to the port.
            let rx_queue = self.queues[rx_idx]
                .take()
                .expect("port rx queue should exist")
                .queue;
            let tx_queue = self.queues[tx_idx]
                .take()
                .expect("port tx queue should exist")
                .queue;

            self.ports[port_id].start(
                mem.clone(),
                rx_queue,
                tx_queue,
                interrupt.clone(),
                self.control.clone(),
            );
        }

        raise_irq
    }
}

impl VirtioDevice for Console {
    fn freeze(&mut self) -> Result<bool, String> {
        self.frozen = true;
        let mut complete = true;
        for port in &mut self.ports {
            complete &= port.freeze()?;
        }
        Ok(complete)
    }

    fn thaw(&mut self) -> Result<(), String> {
        if !self.frozen {
            return Ok(());
        }
        if let DeviceState::Activated(mem, interrupt) = &self.device_state {
            // Check every worker before restarting any of them.
            if self
                .ports
                .iter()
                .any(|port| port.is_started() && port.frozen_queues().is_none())
            {
                return Err("console freeze still pending".into());
            }
            for port in &mut self.ports {
                port.thaw(mem.clone(), interrupt.clone(), self.control.clone())?;
            }
            for event in &self.queue_events {
                event.write(1).map_err(|e| e.to_string())?;
            }
            self.control
                .queue_evt()
                .write(1)
                .map_err(|e| e.to_string())?;
        }
        self.frozen = false;
        Ok(())
    }

    fn capture_state(&self) -> Result<super::super::DeviceSnapshot, String> {
        if self.is_activated() && !self.frozen {
            return Err("console must be frozen before capture".into());
        }
        let ports = self
            .ports
            .iter()
            .map(Port::capture_state)
            .collect::<Result<Vec<_>, _>>()?;
        let mut queues = self
            .queues
            .iter()
            .map(|q| q.as_ref().map(|q| q.queue.capture_state()))
            .collect::<Vec<_>>();
        for (id, port) in self.ports.iter().enumerate() {
            if let Some((rx, tx)) = port.frozen_queues() {
                queues[port_id_to_queue_idx(QueueDirection::Rx, id)] = Some(rx.capture_state());
                queues[port_id_to_queue_idx(QueueDirection::Tx, id)] = Some(tx.capture_state());
            }
        }
        let queues = if self.is_activated() {
            Some(
                queues
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or("console queues missing")?,
            )
        } else {
            None
        };
        Ok(super::super::DeviceSnapshot {
            queues,
            state: super::super::DeviceSnapshotState::Console(ConsoleSnapshot {
                config: [
                    u32::from(self.config.cols),
                    u32::from(self.config.rows),
                    self.config.max_nr_ports,
                    self.config.emerg_wr,
                ],
                ports,
                control: self.control.capture_state(),
            }),
        })
    }

    fn restore_state(&mut self, state: &super::super::DeviceSnapshotState) -> Result<(), String> {
        let super::super::DeviceSnapshotState::Console(state) = state else {
            return Err("console state type mismatch".into());
        };
        if self.is_activated()
            || state.ports.len() != self.ports.len()
            || state.config[2] as usize != self.ports.len()
            || state.config[0] > u16::MAX as u32
            || state.config[1] > u16::MAX as u32
        {
            return Err("console topology or configuration mismatch".into());
        }
        for (port, saved) in self.ports.iter().zip(&state.ports) {
            port.validate_state(saved)?;
        }
        self.control
            .restore_state(&state.control, self.ports.len())?;
        for (port, saved) in self.ports.iter_mut().zip(&state.ports) {
            port.restore_io(saved)?;
        }
        self.config = VirtioConsoleConfig {
            cols: state.config[0] as u16,
            rows: state.config[1] as u16,
            max_nr_ports: state.config[2],
            emerg_wr: state.config[3],
        };
        self.restore = Some(state.ports.clone());
        self.frozen = true;
        Ok(())
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features
    }

    fn device_type(&self) -> u32 {
        uapi::VIRTIO_ID_CONSOLE
    }

    fn device_name(&self) -> &str {
        "console"
    }

    fn queue_config(&self) -> &[QueueConfig] {
        &self.queue_config
    }

    fn read_config(&self, offset: u64, mut data: &mut [u8]) {
        let config_slice = self.config.as_slice();
        let config_len = config_slice.len() as u64;
        if offset >= config_len {
            error!("Failed to read config space");
            return;
        }
        if let Some(end) = offset.checked_add(data.len() as u64) {
            // This write can't fail, offset and end are checked against config_len.
            data.write_all(&config_slice[offset as usize..cmp::min(end, config_len) as usize])
                .unwrap();
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "console: guest driver attempted to write device config (offset={:x}, len={:x})",
            offset,
            data.len()
        );
    }

    fn activate(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: InterruptTransport,
        queues: Vec<DeviceQueue>,
    ) -> ActivateResult {
        if self.activate_evt.write(1).is_err() {
            error!("Cannot write to activate_evt");
            return Err(ActivateError::BadActivate);
        }

        self.queue_events = queues.iter().map(|dq| dq.event.clone()).collect();
        self.queues = queues.into_iter().map(Some).collect();
        self.device_state = DeviceState::Activated(mem, interrupt);
        if let Some(ports) = self.restore.take() {
            for (id, (port, saved)) in self.ports.iter_mut().zip(&ports).enumerate() {
                if saved.started {
                    let rx = self.queues[port_id_to_queue_idx(QueueDirection::Rx, id)]
                        .take()
                        .unwrap()
                        .queue;
                    let tx = self.queues[port_id_to_queue_idx(QueueDirection::Tx, id)]
                        .take()
                        .unwrap()
                        .queue;
                    port.restore_state(saved, rx, tx);
                }
            }
        }

        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.device_state.is_activated()
    }

    fn reset(&mut self) -> bool {
        // Shutdown ports and clear queues.
        for port in &mut self.ports {
            port.shutdown();
        }
        self.frozen = false;
        self.restore = None;
        self.control.clear();
        self.queues.clear();
        self.queue_events.clear();
        self.device_state = DeviceState::Inactive;
        true
    }
}

impl VmmExitObserver for Console {
    fn on_vmm_exit(&mut self) {
        self.reset();
        log::trace!("Console on_vmm_exit finished");
    }
}
