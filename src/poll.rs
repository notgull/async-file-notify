//! A backend that repeatedly checks for events.

use crate::{DataKind, Event, EventKind, FileOptions, ItemKind, MetadataKind, ModifyKind};

use async_io::Timer;
use async_lock::{Mutex, RwLock};
use blocking::{unblock, Unblock};
use futures_lite::{future, prelude::*, stream, StreamExt};

use std::collections::hash_map::{HashMap, RandomState};
use std::collections::HashSet;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

/// A backend that repeatedly checks for events.
#[derive(Debug)]
pub(super) struct PollNotify {
    /// The interval at which to check for events.
    interval: StdMutex<Duration>,

    /// The hasher to use for hashing files.
    hasher: RandomState,

    /// Map between paths we are monitoring and their data.
    path_data: RwLock<HashMap<Arc<Path>, PathData>>,
}

#[derive(Debug)]
struct PathData {
    /// The root path.
    root: Arc<Path>,

    /// The options for the path.
    options: FileOptions,

    /// Map between sub-paths of `root` and their metadata.
    file_data: Mutex<HashMap<Arc<Path>, FileData>>,
}

#[derive(Debug)]
struct FileData {
    /// The path of the file.
    path: Arc<Path>,

    /// The file's metadata.
    metadata: fs::Metadata,

    /// The file's hash, if applicable.
    hash: Option<u64>,
}

impl PollNotify {
    /// Create a new `PollNotify` instance.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval: StdMutex::new(interval),
            hasher: RandomState::new(),
            path_data: RwLock::new(HashMap::new()),
        }
    }

    /// Begin watching the given path.
    pub async fn watch(&self, path: &Path, options: &FileOptions) -> io::Result<()> {
        let hasher = if options.hash {
            Some(&self.hasher)
        } else {
            None
        };

        // Collect the path data.
        let file_data = path_data(path, hasher, options.recursive)
            .map(|result| result.map(|data| (Arc::clone(&data.path), data)))
            .try_collect::<_, _, HashMap<_, _>>()
            .await?;

        // Insert the path data.
        {
            let mut path_data = self.path_data.write().await;
            let path_key = {
                let path_buf = path.to_path_buf();
                Arc::from(path_buf.into_boxed_path())
            };

            path_data.insert(
                Arc::clone(&path_key),
                PathData {
                    root: path_key,
                    options: options.clone(),
                    file_data: Mutex::new(file_data),
                },
            );
        }

        Ok(())
    }

    /// Stop watching the given path.
    pub async fn unwatch(&self, path: &Path) -> io::Result<()> {
        self.path_data
            .write()
            .await
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "path not found"))
    }

    /// Read events from the backend.
    pub fn events(&self) -> impl Stream<Item = io::Result<Event>> + '_ {
        // TODO(notgull): This function allocates very gratuitously. It should be
        // rewritten to avoid this.

        // Check for events every `interval`.
        let interval =
            Timer::interval(*self.interval.lock().unwrap_or_else(|err| err.into_inner()));

        // Get the read lock on the path data.
        let get_read_lock = interval.then(move |_| {
            log::trace!("async-file-notify: polling for events");
            self.path_data.read()
        });

        // Preform the diffing.
        let diffs = get_read_lock.then(move |data_lock| async move {
            // Iterate over the path data.
            stream::iter(data_lock.values())
                .then({
                    move |data| async move {
                        let hasher = if data.options.hash {
                            Some(&self.hasher)
                        } else {
                            None
                        };

                        // Get an iterator over the new path data.
                        let new_data = path_data(&data.root, hasher, data.options.recursive);

                        // Compare against the old path data.
                        let mut old_data = data.file_data.lock().await;

                        // Hash set containing paths that we haven't encountered so far.
                        let mut remaining_paths = old_data.keys().cloned().collect::<HashSet<_>>();

                        // Iterate over the new path data.
                        let mut diffs = new_data
                            .filter_map({
                                let encountered_paths = &mut remaining_paths;
                                let old_data = &mut *old_data;
                                move |result| {
                                    result
                                        .map(|new_fd| {
                                            // Insert this path into the encountered paths set.
                                            encountered_paths.remove(&new_fd.path);

                                            let old_fd = old_data.get_mut(&new_fd.path);

                                            // Check if the file has changed.
                                            let diff = diff_data(
                                                new_fd.path.clone(),
                                                old_fd.as_deref(),
                                                Some(&new_fd),
                                            );

                                            // Update the old file data.
                                            if let (Some(old_fd), Some(_)) = (old_fd, &diff) {
                                                *old_fd = new_fd;
                                            }

                                            diff
                                        })
                                        .transpose()
                                }
                            })
                            .collect::<Vec<_>>()
                            .await;

                        // If any files were removed, add them to the diffs.
                        let removed = remaining_paths
                            .into_iter()
                            .filter_map(|path| {
                                let old_fd = old_data.remove(&path).unwrap();
                                diff_data(old_fd.path.clone(), Some(&old_fd), None)
                            })
                            .map(Ok);

                        diffs.extend(removed);
                        diffs
                    }
                })
                .flat_map(stream::iter)
                .collect::<Vec<_>>()
                .await
        });

        diffs.flat_map(stream::iter)
    }
}

/// Get an iterator over the path data for the given path.
fn path_data<'a>(
    root: &Path,
    hasher: Option<&'a RandomState>,
    recursive: bool,
) -> impl Stream<Item = io::Result<FileData>> + 'a {
    // The inner `walkdir`-based iterator.
    let inner = walkdir::WalkDir::new(root)
        .max_depth(if recursive { std::usize::MAX } else { 1 })
        .follow_links(true)
        .into_iter()
        .map(|entry| {
            entry
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
                .and_then(|entry| {
                    let path = entry.into_path();
                    let metadata = fs::metadata(&path)?;

                    Ok((Arc::from(path.into_boxed_path()), metadata))
                })
        });

    // Wrap it in `Unblock` to transform it into a `Stream`.
    Unblock::with_capacity(1, inner).then(move |result| async move {
        // Map to a `FileData` instance.
        match result {
            Ok((path, metadata)) => {
                // Compute the hash if applicable.
                let hash = match hasher {
                    Some(hasher) => Some(file_hash(hasher, Arc::clone(&path)).await?),
                    None => None,
                };

                Ok(FileData {
                    path,
                    metadata,
                    hash,
                })
            }
            Err(e) => Err(e),
        }
    })
}

/// Get the hash of the given file.
async fn file_hash(state: &RandomState, path: Arc<Path>) -> io::Result<u64> {
    /// Chosen arbitrarily.
    const BUFFER_CAPACITY: usize = 512;

    let mut hasher = state.build_hasher();
    let mut file = {
        let std_file = unblock(move || fs::File::open(&path)).await?;
        Unblock::with_capacity(BUFFER_CAPACITY, std_file)
    };
    let mut buf = [0; BUFFER_CAPACITY];

    'outer: loop {
        for _ in 0..200 {
            let n = match file.read(&mut buf).await {
                Ok(0) => break 'outer,
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };

            hasher.write(&buf[..n]);
        }

        // Yield to give other tasks a chance to run.
        future::yield_now().await;
    }

    Ok(hasher.finish())
}

fn diff_data(path: Arc<Path>, old: Option<&FileData>, new: Option<&FileData>) -> Option<Event> {
    fn diff_data_inner(old: &FileData, new: &FileData) -> Option<Event> {
        // Compare metadata time.
        if let (Ok(old), Ok(new)) = (old.metadata.accessed(), new.metadata.accessed()) {
            if new > old {
                return Some(Event::new(EventKind::Modify(ModifyKind::Metadata(
                    MetadataKind::WriteTime,
                ))));
            }
        }

        // Compare hashes.
        if old.hash != new.hash {
            return Some(Event::new(EventKind::Modify(ModifyKind::Data(
                DataKind::Other,
            ))));
        }

        None
    }

    let mut base_event = match (old, new) {
        (Some(old), Some(new)) => diff_data_inner(old, new),

        (None, Some(_)) => {
            // File was created.
            Some(Event::new(EventKind::Create(ItemKind::Other)))
        }

        (Some(_), None) => {
            // File was deleted.
            Some(Event::new(EventKind::Remove(ItemKind::Other)))
        }

        (None, None) => None,
    };

    if let Some(ref mut base_event) = &mut base_event {
        base_event.add_path(path);
    }

    base_event
}
