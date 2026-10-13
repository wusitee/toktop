//! Recursive inotify watcher over the tools' source roots.
//!
//! Only content and directory changes are subscribed to (not opens or reads),
//! so the thread sleeps while no tool is writing. Roots that do not exist yet
//! are picked up when they are created: their nearest existing ancestor is
//! watched for new directories. Single files outside the roots (title
//! indexes) are watched through their parent directory.

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub enum Change {
    /// Files that were written, created, moved or deleted.
    Paths(Vec<PathBuf>),
    /// Events were lost or a watched directory vanished; resync everything.
    Rescan,
}

const TREE_MASK: WatchMask = WatchMask::MODIFY
    .union(WatchMask::CREATE)
    .union(WatchMask::DELETE)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::MOVE_SELF)
    .union(WatchMask::ONLYDIR);

// MASK_ADD: an ancestor and a file's parent can be the same directory.
const ANCESTOR_MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::ONLYDIR)
    .union(WatchMask::MASK_ADD);

const FILE_MASK: WatchMask = WatchMask::MODIFY
    .union(WatchMask::CREATE)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::ONLYDIR)
    .union(WatchMask::MASK_ADD);

struct Watcher {
    inotify: Inotify,
    dirs: HashMap<WatchDescriptor, PathBuf>,
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

/// Starts the watcher thread over `roots` (recursive) and `files`. `emit`
/// is called from that thread.
pub fn spawn(
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
    mut emit: impl FnMut(Change) + Send + 'static,
) -> io::Result<()> {
    let mut watcher = Watcher {
        inotify: Inotify::init()?,
        dirs: HashMap::new(),
        roots,
        files,
    };
    for index in 0..watcher.roots.len() {
        watcher.ensure_root(index);
    }
    for file in watcher.files.clone() {
        if let Some(parent) = file.parent().filter(|dir| dir.is_dir()) {
            watcher.add(parent, FILE_MASK);
        }
    }
    std::thread::Builder::new()
        .name("toktop-watch".into())
        .spawn(move || watcher.run(&mut emit))?;
    Ok(())
}

impl Watcher {
    fn in_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    /// Watches `roots[index]` recursively, or its nearest existing ancestor
    /// until it appears. Returns files found under a newly watched root.
    fn ensure_root(&mut self, index: usize) -> Vec<PathBuf> {
        let root = self.roots[index].clone();
        if root.is_dir() {
            let mut files = Vec::new();
            self.add_tree(&root, &mut files);
            return files;
        }
        let mut ancestor = root.parent();
        while let Some(dir) = ancestor {
            if dir.is_dir() {
                if !self.in_root(dir) {
                    self.add(dir, ANCESTOR_MASK);
                }
                break;
            }
            ancestor = dir.parent();
        }
        Vec::new()
    }

    fn add(&mut self, dir: &Path, mask: WatchMask) {
        if let Ok(wd) = self.inotify.watches().add(dir, mask) {
            self.dirs.insert(wd, dir.to_path_buf());
        }
    }

    fn add_tree(&mut self, dir: &Path, files: &mut Vec<PathBuf>) {
        self.add(dir, TREE_MASK);
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => self.add_tree(&path, files),
                Ok(kind) if kind.is_file() => files.push(path),
                _ => {}
            }
        }
    }

    fn run(mut self, emit: &mut impl FnMut(Change)) {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let events = match self.inotify.read_events_blocking(&mut buffer) {
                Ok(events) => events,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            let mut changed = Vec::new();
            let mut rescan = false;
            let mut lost: Vec<WatchDescriptor> = Vec::new();
            let mut created_dirs = Vec::new();
            for event in events {
                if event.mask.contains(EventMask::Q_OVERFLOW) {
                    rescan = true;
                    continue;
                }
                if event.mask.contains(EventMask::IGNORED) {
                    lost.push(event.wd.clone());
                    continue;
                }
                let Some(dir) = self.dirs.get(&event.wd) else {
                    continue;
                };
                let Some(name) = event.name else { continue };
                let path = dir.join(name);
                if event.mask.contains(EventMask::ISDIR) {
                    if event
                        .mask
                        .intersects(EventMask::CREATE | EventMask::MOVED_TO)
                    {
                        created_dirs.push(path);
                    } else if event.mask.contains(EventMask::MOVED_FROM) {
                        rescan = true;
                    }
                } else if self.in_root(dir)
                    || self.files.contains(&crate::store::source_path(&path))
                {
                    changed.push(path);
                }
            }

            for path in created_dirs {
                if self.in_root(&path) {
                    self.add_tree(&path, &mut changed);
                } else {
                    for index in 0..self.roots.len() {
                        if self.roots[index].starts_with(&path) {
                            changed.extend(self.ensure_root(index));
                        }
                    }
                }
            }
            if !lost.is_empty() {
                for wd in lost {
                    self.dirs.remove(&wd);
                }
                // A watched directory was removed; fall back to its ancestors.
                for index in 0..self.roots.len() {
                    let root = self.roots[index].clone();
                    if !self
                        .dirs
                        .values()
                        .any(|dir| *dir == root || root.starts_with(dir))
                    {
                        changed.extend(self.ensure_root(index));
                    }
                }
                rescan = true;
            }

            if rescan {
                emit(Change::Rescan);
            } else if !changed.is_empty() {
                emit(Change::Paths(changed));
            }
        }
    }
}
