//! Asynchronously watch for file changes.

use futures_lite::prelude::*;

use std::io;
use std::path::{Path};
use std::sync::Arc;
use std::time::Duration;

mod poll;

/// The event that occurred.
#[derive(Debug, Clone)]
pub struct Event {
    /// The paths that changed.
    paths: Vec<Arc<Path>>,

    /// The type of event that occurred.
    kind: EventKind,
}

impl Event {
    fn new(kind: EventKind) -> Self {
        Self {
            kind,
            paths: vec![],
        }
    }

    fn add_path(&mut self, path: Arc<Path>) {
        self.paths.push(path);
    }

    /// Get the kind of event that occurred.
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// Iterate the paths that changed.
    pub fn paths(&self) -> impl Iterator<Item = &Path> + '_ {
        self.paths.iter().map(|path| path.as_ref())
    }
}

/// The type of event that occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum EventKind {
    /// The file was accessed.
    Access(AccessKind),

    /// The file was modified.
    Create(ItemKind),

    /// The file was modified.
    Modify(ModifyKind),

    /// The file was removed.
    Remove(ItemKind),

    /// Something else happened.
    Other,
}

/// The type of access that occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AccessKind {
    /// The file was read.
    Read,

    /// The file was opened for this purpose.
    Open(AccessMode),

    /// The file was closed for this purpose.
    Close(AccessMode),

    /// The file was accessed in some other way.
    Other,
}

/// The mode that a file was opened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AccessMode {
    /// The file was opened for reading.
    Read,

    /// The file was opened for writing.
    Write,

    /// The file was opened for executing.
    Execute,

    /// The file was opened for any purpose.
    Other,
}

/// The types of things that can be created or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ItemKind {
    /// A file was created.
    File,

    /// A directory was created.
    Directory,

    /// Something else was created.
    Other,
}

/// The type of modification that occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ModifyKind {
    /// The file's data changed.
    Data(DataKind),

    /// The file's metadata changed.
    Metadata(MetadataKind),

    /// The file was renamed.
    Rename(RenameKind),

    /// Something else changed.
    Other,
}

/// The kind of data change we had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DataKind {
    /// The file's size changed.
    Size,

    /// The file's hash changed.
    Hash,

    /// Something else changed.
    Other,
}

/// The kind of metadata change we had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum MetadataKind {
    /// The file's access time changed.
    AccessTime,

    /// The file's modification time changed.
    WriteTime,

    /// The file's permissions changed.
    Permissions,

    /// The file's ownership changed.
    Ownership,

    /// Something else changed.
    Extended,

    /// Something else changed.
    Other,
}

/// The kind of renamining this event represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RenameKind {
    To,
    From,
    Both,
    Other,
}

/// The options for watching a file.
#[derive(Debug, Clone, Default)]
pub struct FileOptions {
    /// Whether to compute the file's hash.
    hash: bool,

    /// Whether to watch the file recursively.
    recursive: bool,
}

impl FileOptions {
    /// Create a new `FileOptions` instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether to compute the file's hash.
    pub fn hash(&mut self, hash: bool) -> &Self {
        self.hash = hash;
        self
    }

    /// Set whether to watch the file recursively.
    pub fn recursive(&mut self, recursive: bool) -> &Self {
        self.recursive = recursive;
        self
    }
}

/// A watcher for file changes.
#[derive(Debug)]
pub struct Watcher {
    poll: poll::PollNotify,
}

impl Watcher {
    /// Create a new `Watcher` instance.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            poll: poll::PollNotify::new(Duration::from_secs(30)),
        })
    }

    /// Begin watching the given path.
    pub async fn watch(&self, path: impl AsRef<Path>, options: &FileOptions) -> io::Result<()> {
        self.poll.watch(path.as_ref(), options).await
    }

    /// Stop watching the given path.
    pub async fn unwatch(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.poll.unwatch(path.as_ref()).await
    }

    /// A stream of incoming events.
    pub fn events(&self) -> impl Stream<Item = io::Result<Event>> + '_ {
        self.poll.events()
    }
}
