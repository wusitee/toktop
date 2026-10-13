//! Directory lifetime and external title-index coverage.

use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use toktop::watch::{self, Change};

fn receive(rx: &Receiver<Change>, matches: impl Fn(&Change) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let change = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("watcher did not report the change");
        if matches(&change) {
            return;
        }
    }
}

fn coverage(rx: &Receiver<Change>) {
    receive(rx, |change| matches!(change, Change::Coverage(true)));
}

fn changed(rx: &Receiver<Change>, path: &Path) {
    receive(rx, |change| match change {
        Change::Paths(paths) => paths.iter().any(|item| item == path),
        _ => false,
    });
}

fn unchanged(rx: &Receiver<Change>, path: &Path) {
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Change::Paths(paths)) => assert!(!paths.iter().any(|item| item == path)),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return,
            Err(error) => panic!("watcher stopped: {error}"),
        }
    }
}

#[test]
fn late_root_moved_aside_and_recreated_keeps_watching() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("parent/sessions");
    fs::create_dir(root.parent().unwrap()).unwrap();
    let (tx, rx) = mpsc::channel();
    let _watcher = watch::spawn(vec![root.clone()], vec![], move |change| {
        let _ = tx.send(change);
    })
    .unwrap();
    coverage(&rx);

    fs::create_dir(&root).unwrap();
    let file = root.join("session.jsonl");
    fs::write(&file, "first").unwrap();
    receive(&rx, |change| match change {
        Change::Rescan => true,
        Change::Paths(paths) => paths.contains(&file),
        _ => false,
    });
    fs::write(&file, "updated").unwrap();
    changed(&rx, &file);

    fs::rename(&root, dir.path().join("old-sessions")).unwrap();
    receive(&rx, |change| matches!(change, Change::Rescan));
    fs::create_dir(&root).unwrap();
    receive(&rx, |change| matches!(change, Change::Rescan));
    fs::write(&file, "replacement").unwrap();
    changed(&rx, &file);
    // Writes to the moved inode must no longer be reported under the old path.
    fs::write(dir.path().join("old-sessions/outside.jsonl"), "outside").unwrap();
    unchanged(&rx, &root.join("outside.jsonl"));
}

#[test]
fn moved_subtree_reports_its_new_path_and_leaving_root_drops_watches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    fs::create_dir_all(root.join("old/nested")).unwrap();
    let (tx, rx) = mpsc::channel();
    let _watcher = watch::spawn(vec![root.clone()], vec![], move |change| {
        let _ = tx.send(change);
    })
    .unwrap();
    coverage(&rx);
    fs::rename(root.join("old"), root.join("new")).unwrap();
    receive(&rx, |change| matches!(change, Change::Rescan));
    let file = root.join("new/nested/session.jsonl");
    fs::write(&file, "new location").unwrap();
    changed(&rx, &file);

    fs::rename(root.join("new"), dir.path().join("outside")).unwrap();
    receive(&rx, |change| matches!(change, Change::Rescan));
    fs::write(dir.path().join("outside/nested/outside.jsonl"), "outside").unwrap();
    unchanged(&rx, &root.join("new/nested/outside.jsonl"));
}

#[test]
fn title_parent_created_later_and_title_removed_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("gemini/antigravity");
    let file = parent.join("conversation_summaries.db");
    let (tx, rx) = mpsc::channel();
    let _watcher = watch::spawn(vec![], vec![file.clone()], move |change| {
        let _ = tx.send(change);
    })
    .unwrap();
    coverage(&rx);
    fs::create_dir_all(&parent).unwrap();
    receive(&rx, |change| matches!(change, Change::Rescan));
    fs::write(&file, "title").unwrap();
    changed(&rx, &file);
    fs::rename(&file, parent.join("saved.db")).unwrap();
    changed(&rx, &file);
    fs::write(&file, "replacement").unwrap();
    changed(&rx, &file);
    fs::remove_file(&file).unwrap();
    changed(&rx, &file);
}

#[test]
fn incomplete_coverage_is_reported_and_registration_retries() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("blocked");
    fs::write(&parent, "not a directory").unwrap();
    let root = parent.join("sessions");
    let (tx, rx) = mpsc::channel();
    let watcher = watch::spawn(vec![root.clone()], vec![], move |change| {
        let _ = tx.send(change);
    })
    .unwrap();
    receive(&rx, |change| matches!(change, Change::Coverage(false)));
    fs::remove_file(&parent).unwrap();
    // No CREATE could be delivered: this tree was never watched.
    fs::create_dir_all(root.join("nested")).unwrap();
    watcher.reconcile();
    coverage(&rx);
    let file = root.join("nested/session.jsonl");
    fs::write(&file, "recovered").unwrap();
    changed(&rx, &file);
}
