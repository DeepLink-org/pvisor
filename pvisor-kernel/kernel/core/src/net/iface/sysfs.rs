// SPDX-License-Identifier: MPL-2.0

use aster_device::{
    class::{self, Class, ClassDevice},
    common::Attr,
};

use super::{Iface, iter_all_ifaces};
use crate::prelude::*;

struct NetClass;

impl Class for NetClass {
    const NAME: &'static str = "net";
    type Device = Arc<Iface>;

    fn dev_attrs(&self) -> &'static [Attr<ClassDevice<Self>>] {
        &[]
    }
}

pub(super) fn register_interfaces() -> Result<()> {
    let class = class::register(NetClass)?;
    for iface in iter_all_ifaces() {
        let name = iface
            .name()
            .as_str()
            .map_err(|_| Error::new(Errno::EINVAL))?;
        let dev = ClassDevice::builder(&class, name, iface.clone()).build();
        aster_device::add_device(&dev)?;
    }
    Ok(())
}
