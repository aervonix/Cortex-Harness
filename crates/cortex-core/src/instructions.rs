//! Reading instruction files for the loop
//! (`docs/architecture/07-sessions-and-context.md`, "Project instruction
//! files"; `docs/security.md`, section 2).
//!
//! The loop asks for a file by its absolute path through its
//! `InstructionFiles` seam, and this is the core's answer. An instruction
//! file is text the model reads, and the text goes to the provider with
//! every request, so what is read is held to what the agent itself could
//! read unasked:
//!
//! - **A file in the workspace** is found through the session's path
//!   resolver: a link is followed only where it stays inside the workspace
//!   (a `CLAUDE.md` that links to `AGENTS.md` is read; one that points at a
//!   private key outside is not), and the permission engine must let the
//!   agent read the file without asking (`Engine::may_store`). A file a
//!   rule denies, or one that looks like a secret, is not read.
//! - **The user's own file** in the configuration directory is the user's,
//!   and is read as it is.
//! - **Only a regular file** is opened, without waiting on a FIFO, and no
//!   more is read than the loop asks for.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the loop calls this on the blocking pool"
)]

use std::io::Read;
use std::sync::Arc;

use cortex_agent::{FileText, InstructionFiles};
use cortex_base::{AbsPath, OpenError, PathResolver, open_regular};
use cortex_session::KeepGate;

/// The core's instruction files.
pub(crate) struct DiskInstructions {
    pub(crate) resolver: Arc<PathResolver>,
    pub(crate) user_dir: Option<AbsPath>,
    pub(crate) gate: Arc<dyn KeepGate>,
}

impl std::fmt::Debug for DiskInstructions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskInstructions")
            .field("root", self.resolver.root())
            .finish_non_exhaustive()
    }
}

impl InstructionFiles for DiskInstructions {
    fn read(&self, path: &AbsPath, limit: usize) -> Result<Option<FileText>, String> {
        let users = self
            .user_dir
            .as_ref()
            .is_some_and(|dir| path.parent().as_ref() == Some(dir));
        let real = if users {
            path.to_fs_path()
        } else {
            let root = self.resolver.root();
            let Some(relative) = path.strip_prefix(root) else {
                return Ok(None);
            };
            let Ok(resolved) = self.resolver.resolve(relative.as_str()) else {
                return Ok(None);
            };
            if !resolved.is_inside_workspace()
                || !resolved.exists()
                || !self.gate.may_keep(resolved.abs())
            {
                return Ok(None);
            }
            resolved.to_fs_path()
        };
        let (file, metadata) = match open_regular(&real) {
            Ok(opened) => opened,
            Err(
                OpenError::NotFound | OpenError::Link | OpenError::Directory | OpenError::NotAFile,
            ) => {
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(limit as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        Ok(Some(FileText {
            bytes,
            len: metadata.len(),
        }))
    }
}
