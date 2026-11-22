// SPDX-License-Identifier: MPL-2.0

use alloc::format;
use aster_device::{
    class::{self, Class, ClassDevice},
    common::{AnyDevice, Attr, DevNum},
};
use aster_virtio::device::console::{NamedPort, named_ports};
use device_id::{DeviceId, MinorId};

use super::{Device, DeviceType, registry::char};
use crate::{
    events::IoEvents,
    fs::{
        devtmpfs::DevtmpfsNodeMeta,
        file::{PerOpenFileOps, StatusFlags},
        vfs::inode::FileOps,
    },
    prelude::*,
    process::signal::{PollHandle, Pollable, Pollee},
};

struct PortClass(char::MajorIdOwner);
type PortDevice = ClassDevice<PortClass>;

impl Class for PortClass {
    const NAME: &'static str = "virtio-ports";
    type Device = PortFile;

    fn dev_attrs(&self) -> &'static [Attr<PortDevice>] {
        const ATTRS: &[Attr<PortDevice>] = &[Attr::ro("name", |dev, out| {
            writeln!(out, "{}", dev.payload().port.name())?;
            Ok(())
        })];
        ATTRS
    }
}

pub(super) fn init() -> Result<()> {
    if named_ports().is_empty() {
        return Ok(());
    }
    let owner = PortClass(char::allocate_major()?);
    let major = owner.0.get();
    let class = class::register(owner)?;
    for port in named_ports() {
        let pollee = Arc::new(Pollee::new());
        let wake = pollee.clone();
        port.on_event(Box::new(move || {
            wake.notify(IoEvents::IN | IoEvents::OUT | IoEvents::HUP)
        }));
        let id = DeviceId::new(major, MinorId::new(u32::from(port.id())));
        let dev = PortDevice::builder(
            &class,
            format!("vport0p{}", port.id()),
            PortFile {
                port: port.clone(),
                pollee,
            },
        )
        .devnum(DevNum::char(id))
        .build();
        aster_device::add_device(&dev)?;
        if let Err(err) = char::register(dev.clone()) {
            let _ = aster_device::remove_device(&dev);
            return Err(err);
        }
    }
    Ok(())
}

impl Device for PortDevice {
    fn type_(&self) -> DeviceType {
        DeviceType::Char
    }
    fn id(&self) -> DeviceId {
        self.devnum().unwrap().id()
    }
    fn devtmpfs_meta(&self) -> Option<DevtmpfsNodeMeta> {
        None
    }
    fn open(&self) -> Result<Box<dyn PerOpenFileOps>> {
        Ok(Box::new(self.payload().clone()))
    }
}

#[derive(Clone)]
struct PortFile {
    port: Arc<NamedPort>,
    pollee: Arc<Pollee>,
}

impl Pollable for PortFile {
    fn poll(&self, mask: IoEvents, poller: Option<&mut PollHandle>) -> IoEvents {
        self.pollee.poll_with(mask, poller, || {
            let mut events = IoEvents::empty();
            if self.port.closed() {
                events |= IoEvents::HUP;
            } else if self.port.name() != "krun-stdin" && self.port.writable() {
                events |= IoEvents::OUT;
            }
            if self.port.readable() {
                events |= IoEvents::IN;
            }
            events
        })
    }
}

impl FileOps for PortFile {
    fn read_at(&self, _: usize, writer: &mut VmWriter, flags: StatusFlags) -> Result<usize> {
        if writer.avail() == 0 {
            return Ok(0);
        }
        if matches!(self.port.name(), "krun-stdout" | "krun-stderr") {
            return_errno!(Errno::EBADF);
        }
        let mut buf = vec![0; writer.avail().min(PAGE_SIZE)];
        let mut read = || {
            self.port
                .try_read(&mut buf)
                .ok_or_else(|| Error::new(Errno::EAGAIN))
        };
        let n = if flags.contains(StatusFlags::O_NONBLOCK) {
            read()?
        } else {
            self.wait_events(IoEvents::IN, None, read)?
        };
        self.pollee.invalidate();
        Ok(writer.write_fallible(&mut VmReader::from(&buf[..n]))?)
    }

    fn write_at(&self, _: usize, reader: &mut VmReader, flags: StatusFlags) -> Result<usize> {
        if reader.remain() == 0 {
            return Ok(0);
        }
        if self.port.name() == "krun-stdin" {
            return_errno!(Errno::EBADF);
        }
        let mut buf = vec![0; reader.remain().min(PAGE_SIZE)];
        let n = reader.read_fallible(&mut VmWriter::from(buf.as_mut_slice()))?;
        let submit = || {
            self.port.try_write(&buf[..n]).ok_or_else(|| {
                Error::new(if self.port.closed() {
                    Errno::EPIPE
                } else {
                    Errno::EAGAIN
                })
            })
        };
        if flags.contains(StatusFlags::O_NONBLOCK) {
            submit()?;
        } else {
            let ticket = self.wait_events(IoEvents::OUT, None, submit)?;
            if !self.port.wait_written(ticket) {
                return_errno!(Errno::EPIPE);
            }
        }
        self.pollee.invalidate();
        Ok(n)
    }
}

impl PerOpenFileOps for PortFile {
    fn check_seekable(&self) -> Result<()> {
        return_errno!(Errno::ESPIPE);
    }
    fn is_offset_aware(&self) -> bool {
        false
    }
}
