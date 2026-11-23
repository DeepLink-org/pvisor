// SPDX-License-Identifier: MPL-2.0

#![expect(unused_variables)]

use crate::{fs::file::InodeType, prelude::*};

/// A visitor for dir entries.
pub trait DirentVisitor {
    /// Returns the remaining byte capacity, if bounded.
    ///
    /// Filesystems may size batches using this hint. The visitor still decides
    /// whether it can accept each entry.
    fn remaining_bytes(&self) -> Option<usize> {
        None
    }

    /// Visit a dir entry.
    ///
    /// If the visitor succeeds in visiting the given inode, an `Ok(())` is returned;
    /// Otherwise, an error is returned. Different implementations for `DirentVisitor`
    /// may choose to report errors for different reasons. Regardless of the exact
    /// errors and reasons, `readdir`-family methods shall stop feeding the visitor
    /// with the next inode as long as an error is returned by the visitor.
    ///
    /// # Example
    ///
    /// `Vec<String>` is implemented as `DirentVisitor` so that the file names
    /// under a dir can be easily collected, which is convenient for testing purposes.
    ///
    /// ```no_run
    /// let mut all_dirents = Vec::new();
    /// let dir_inode = todo!("create an inode");
    /// dir_inode.readdir_at(0, &mut all_dirents).unwrap();
    /// ```
    fn visit(&mut self, name: &str, ino: u64, type_: InodeType, offset: usize) -> Result<()>;
}

impl DirentVisitor for Vec<String> {
    fn visit(&mut self, name: &str, ino: u64, type_: InodeType, offset: usize) -> Result<()> {
        self.push(name.into());
        Ok(())
    }
}
