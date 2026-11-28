// SPDX-License-Identifier: MPL-2.0

//! TTY devices.
//!
//! This module implements TTY devices such as `/dev/tty0`, `/dev/tty`, and `/dev/console`.
//!
//! Reference: <https://www.kernel.org/doc/html/latest/admin-guide/devices.html>

use device_id::{DeviceId, MajorId, MinorId};
use spin::Once;

use crate::{
    device::{
        Device, DeviceType,
        registry::char,
        tty::{hvc::hvc0_device, serial::serial0_device, vt},
    },
    fs::{
        devtmpfs::DevtmpfsNodeMeta,
        file::{PerOpenFileOps, mkmod},
    },
    prelude::*,
};

/// Corresponds to `/dev/tty0` in the file system. This device represents the active virtual
/// terminal.
pub(crate) struct Tty0Device;

impl Device for Tty0Device {
    fn type_(&self) -> DeviceType {
        DeviceType::Char
    }

    fn id(&self) -> DeviceId {
        DeviceId::new(MajorId::new(4), MinorId::new(0))
    }

    fn devtmpfs_meta(&self) -> Option<DevtmpfsNodeMeta> {
        Some(DevtmpfsNodeMeta::new("tty0").unwrap())
    }

    fn open(&self) -> Result<Box<dyn PerOpenFileOps>> {
        vt::active_vt().open()
    }
}

/// Corresponds to `/dev/tty` in the file system. This device represents the controlling terminal
/// of the session of the current process.
pub(crate) struct TtyDevice;

impl Device for TtyDevice {
    fn type_(&self) -> DeviceType {
        DeviceType::Char
    }

    fn id(&self) -> DeviceId {
        DeviceId::new(MajorId::new(5), MinorId::new(0))
    }

    fn devtmpfs_meta(&self) -> Option<DevtmpfsNodeMeta> {
        // Reference: <https://elixir.bootlin.com/linux/v6.18/source/drivers/tty/tty_io.c#L3511>.
        Some(DevtmpfsNodeMeta::with_mode("tty", mkmod!(a+rw)).unwrap())
    }

    fn open(&self) -> Result<Box<dyn PerOpenFileOps>> {
        let Some(terminal) = current!().terminal() else {
            return_errno_with_message!(
                Errno::ENOTTY,
                "the process does not have a controlling terminal"
            );
        };

        terminal.open()
    }
}

/// Corresponds to `/dev/console` in the file system. This device represents a console to which
/// system messages will be sent.
pub(crate) struct SystemConsole {
    inner: Arc<dyn Device>,
}

/// The device ID of `/dev/console`.
pub(crate) const CONSOLE_DEVICE_ID: DeviceId = DeviceId::new(MajorId::new(5), MinorId::new(1));

impl Device for SystemConsole {
    fn type_(&self) -> DeviceType {
        DeviceType::Char
    }

    fn id(&self) -> DeviceId {
        CONSOLE_DEVICE_ID
    }

    fn devtmpfs_meta(&self) -> Option<DevtmpfsNodeMeta> {
        Some(DevtmpfsNodeMeta::new("console").unwrap())
    }

    fn open(&self) -> Result<Box<dyn PerOpenFileOps>> {
        self.inner.open()
    }
}

pub(super) fn init_in_first_process() -> Result<()> {
    let name = CONSOLES
        .get()
        .and_then(|consoles| consoles.first())
        .map_or("tty0", String::as_str);
    let inner: Arc<dyn Device> = match name {
        "tty0" => {
            vt::init_in_first_process()?;
            let vt = Arc::new(Tty0Device);
            char::register(vt.clone())?;
            vt
        }
        "hvc0" => hvc0_device().cloned().ok_or(Error::with_message(
            Errno::ENODEV,
            "virtio console is unavailable",
        ))?,
        "ttyS0" => serial0_device().cloned().ok_or(Error::with_message(
            Errno::ENODEV,
            "serial console is unavailable",
        ))?,
        _ => return_errno_with_message!(Errno::ENODEV, "unsupported system console"),
    };
    char::register(Arc::new(TtyDevice))?;
    char::register(Arc::new(SystemConsole { inner }))
}

static CONSOLES: Once<Vec<String>> = Once::new();
aster_cmdline::define_repeatable_kv_param!("console", CONSOLES);
