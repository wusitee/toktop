//! Recursive inotify watcher over source roots and external title indexes.
//! Missing directories are watched through their nearest existing ancestor.

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

pub enum Change {
    Paths(Vec<PathBuf>),
    /// Events were lost or directory paths changed; resync everything.
    Rescan,
    /// Whether every required directory could be watched.
    Coverage(bool),
}

const TREE_MASK: WatchMask = WatchMask::MODIFY
    .union(WatchMask::CREATE)
    .union(WatchMask::DELETE)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::MOVE_SELF)
    .union(WatchMask::ONLYDIR)
    .union(WatchMask::MASK_ADD);

const ANCESTOR_MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::MOVE_SELF)
    .union(WatchMask::ONLYDIR)
    .union(WatchMask::MASK_ADD);

const FILE_MASK: WatchMask = TREE_MASK;

/// Requests registration retries without blocking the worker on traversal.
pub struct Handle {
    control: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// Re-walks roots, as on queue overflow, adding any missing watches.
    pub fn reconcile(&self) {
        let _ = (&self.control).write_all(b"R");
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = (&self.control).write_all(b"S");
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Watcher {
    inotify: Inotify,
    dirs: HashMap<WatchDescriptor, PathBuf>,
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
    failure: Option<String>,
    warned: bool,
}

/// Registers watches before returning; callers must run this off the UI thread.
pub fn spawn(
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
    mut emit: impl FnMut(Change) + Send + 'static,
) -> io::Result<Handle> {
    let mut watcher = Watcher {
        inotify: Inotify::init()?,
        dirs: HashMap::new(),
        roots,
        files,
        failure: None,
        warned: false,
    };
    // Do not report complete coverage until the thread starts successfully.
    watcher.reconcile(&mut |_| {});
    let (control, reader) = UnixStream::pair()?;
    let thread = std::thread::Builder::new()
        .name("toktop-watch".into())
        .spawn(move || {
            watcher.coverage(&mut emit);
            watcher.run(reader, &mut emit);
        })?;
    Ok(Handle {
        control,
        thread: Some(thread),
    })
}

impl Watcher {
    fn in_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    fn failed(&mut self, path: &Path, error: io::Error) {
        if self.failure.is_none() {
            let hint = if error.raw_os_error() == Some(28) {
                "; check fs.inotify.max_user_watches"
            } else {
                ""
            };
            self.failure = Some(format!("{}: {error}{hint}", path.display()));
        }
    }

    fn coverage(&mut self, emit: &mut impl FnMut(Change)) {
        if let Some(reason) = &self.failure
            && !self.warned
        {
            eprintln!("toktop: file watching degraded ({reason}); polling every 10s");
            self.warned = true;
        }
        emit(Change::Coverage(self.failure.is_none()));
    }

    fn add(&mut self, dir: &Path, mask: WatchMask) {
        match self.inotify.watches().add(dir, mask) {
            Ok(wd) => {
                self.dirs.insert(wd, dir.to_path_buf());
            }
            Err(error) => self.failed(dir, error),
        }
    }

    fn add_tree(&mut self, dir: &Path) {
        self.add(dir, TREE_MASK);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.failed(dir, error);
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.failed(dir, error);
                    continue;
                }
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => self.add_tree(&entry.path()),
                Err(error) => self.failed(&entry.path(), error),
                _ => {}
            }
        }
    }

    /// Watches `path`, or its nearest existing ancestor until it appears.
    fn ensure_dir(&mut self, path: &Path, tree: bool) {
        // A directory created after the existence check but before its
        // parent's watch is never reported, so check again once watched.
        // Each retry watches a deeper directory, unless one keeps vanishing.
        for _ in path.ancestors() {
            let Some(dir) = self.watch_nearest(path, tree) else {
                return;
            };
            if dir == path
                || !path
                    .ancestors()
                    .find(|child| child.parent() == Some(dir.as_path()))
                    .is_some_and(Path::exists)
            {
                return;
            }
        }
    }

    /// Watches the nearest existing directory of `path` and its ancestors
    /// and returns it; `None` when none could be checked. A failed watch is
    /// recorded in `failure`, not returned.
    fn watch_nearest(&mut self, path: &Path, tree: bool) -> Option<PathBuf> {
        let mut candidate = Some(path);
        while let Some(dir) = candidate {
            match fs::metadata(dir) {
                Ok(meta) if meta.is_dir() => {
                    if dir == path && tree {
                        self.add_tree(dir);
                    } else {
                        self.add(
                            dir,
                            if dir == path {
                                FILE_MASK
                            } else {
                                ANCESTOR_MASK
                            },
                        );
                    }
                    return Some(dir.to_path_buf());
                }
                Ok(_) => {
                    self.failed(dir, io::Error::from(io::ErrorKind::NotADirectory));
                    return None;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.failed(dir, error);
                    return None;
                }
            }
            candidate = dir.parent();
        }
        None
    }

    fn reconcile(&mut self, emit: &mut impl FnMut(Change)) {
        self.failure = None;
        for root in self.roots.clone() {
            self.ensure_dir(&root, true);
        }
        for file in self.files.clone() {
            if let Some(parent) = file.parent() {
                self.ensure_dir(parent, false);
            }
        }
        self.coverage(emit);
    }

    /// Inotify descriptors follow moved inodes; discard their old path mapping.
    fn rebuild(&mut self, emit: &mut impl FnMut(Change)) {
        for (wd, _) in self.dirs.drain() {
            let _ = self.inotify.watches().remove(wd);
        }
        self.reconcile(emit);
    }

    fn run(mut self, mut control: UnixStream, emit: &mut impl FnMut(Change)) {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match wait_readable(self.inotify.as_raw_fd(), control.as_raw_fd()) {
                Ok(true) => {
                    let mut commands = [0; 64];
                    match control.read(&mut commands) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => {
                            if commands[..count].contains(&b'S') {
                                return;
                            }
                            self.reconcile(emit);
                        }
                    }
                    continue;
                }
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.failed(Path::new("inotify"), error);
                    self.coverage(emit);
                    return;
                }
            }
            let events = match self.inotify.read_events(&mut buffer) {
                Ok(events) => events,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.failed(Path::new("inotify"), error);
                    self.coverage(emit);
                    return;
                }
            };
            let mut changed = Vec::new();
            let mut rescan = false;
            let mut rebuild = false;
            for event in events {
                if event.mask.contains(EventMask::Q_OVERFLOW) {
                    rescan = true;
                    continue;
                }
                let Some(dir) = self.dirs.get(&event.wd) else {
                    continue;
                };
                // Self-events have no name and must be handled first.
                if event
                    .mask
                    .intersects(EventMask::MOVE_SELF | EventMask::DELETE_SELF | EventMask::IGNORED)
                {
                    rebuild = true;
                    continue;
                }
                let Some(name) = event.name else { continue };
                let path = dir.join(name);
                if event.mask.contains(EventMask::ISDIR) {
                    // Ancestor watches also see unrelated sibling directories.
                    if !self.in_root(&path)
                        && !self.roots.iter().any(|root| root.starts_with(&path))
                        && !self.files.iter().any(|file| file.starts_with(&path))
                    {
                        continue;
                    }
                    if event
                        .mask
                        .intersects(EventMask::MOVED_FROM | EventMask::MOVED_TO | EventMask::DELETE)
                    {
                        rebuild = true;
                    } else if event.mask.contains(EventMask::CREATE) {
                        rescan = true;
                    }
                } else if self.in_root(dir)
                    || self.files.contains(&crate::store::source_path(&path))
                {
                    changed.push(path);
                }
            }
            if rebuild {
                self.rebuild(emit);
            } else if rescan {
                // Includes overflow: recover directories whose CREATE was lost.
                self.reconcile(emit);
            }
            if rebuild || rescan {
                emit(Change::Rescan);
            } else if !changed.is_empty() {
                emit(Change::Paths(changed));
            }
        }
    }
}

/// Blocks on inotify or worker commands, with no periodic wakeups while idle.
fn wait_readable(inotify: i32, control: i32) -> io::Result<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    }
    let mut fds = [
        PollFd {
            fd: inotify,
            events: 1,
            revents: 0,
        },
        PollFd {
            fd: control,
            events: 1,
            revents: 0,
        },
    ];
    // SAFETY: fds contains nfds valid pollfd values for the duration of the call.
    if unsafe { poll(fds.as_mut_ptr(), fds.len(), -1) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fds[1].revents != 0)
    }
}
