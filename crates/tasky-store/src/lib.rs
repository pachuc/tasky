//! Local snapshot storage. All cooperating writers lock the same directory.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use tasky_core::Graph;

pub struct Store {
    directory: PathBuf,
}

impl Store {
    /// Select a store directory without reading or creating files.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("graph.json")
    }

    fn lock(&self) -> Result<File> {
        fs::create_dir_all(&self.directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("graph.lock"))?;
        lock.lock_exclusive()?;
        Ok(lock) // OS unlocks when the handle is dropped, including on error.
    }

    /// Create an empty graph without overwriting an existing snapshot.
    ///
    /// # Errors
    /// Returns an error if a snapshot exists or locking or saving fails.
    pub fn init(&self) -> Result<Graph> {
        let _lock = self.lock()?;
        ensure!(!self.path().exists(), "graph already exists");
        let graph = Graph::default();
        self.save(&graph)?;
        Ok(graph)
    }

    /// Read and validate the current snapshot.
    ///
    /// # Errors
    /// Returns an error if the snapshot cannot be read, parsed, or validated.
    pub fn load(&self) -> Result<Graph> {
        let file = File::open(self.path()).context("cannot open graph; run `tasky init` first")?;
        let graph: Graph = serde_json::from_reader(file).context("invalid graph JSON")?;
        graph.validate()?;
        Ok(graph)
    }

    /// Apply an action under the writer lock and save the validated graph.
    ///
    /// # Errors
    /// Returns an error if locking, loading, the action, validation, or saving fails.
    pub fn update(&self, action: impl FnOnce(&mut Graph) -> Result<()>) -> Result<Graph> {
        let _lock = self.lock()?;
        let mut graph = self.load()?;
        action(&mut graph)?;
        graph.validate()?;
        self.save(&graph)?;
        Ok(graph)
    }

    fn save(&self, graph: &Graph) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer_pretty(&mut file, graph)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(self.path())?;
        sync_directory(&self.directory)?;
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_update_preserves_snapshot_and_init_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.init().unwrap();
        assert!(store.init().is_err());
        assert!(
            store
                .update(|g| {
                    g.add("a".into(), "A".into())?;
                    anyhow::bail!("abort")
                })
                .is_err()
        );
        assert_eq!(store.load().unwrap().tasks().count(), 0);
    }

    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        Store::new(dir.path()).init().unwrap();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = dir.path();
                scope.spawn(move || {
                    Store::new(path)
                        .update(|g| {
                            g.add(i.to_string(), "Task".into())?;
                            Ok(())
                        })
                        .unwrap()
                });
            }
        });
        assert_eq!(Store::new(dir.path()).load().unwrap().tasks().count(), 8);
    }

    #[test]
    fn rejects_unknown_schema_and_dangling_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::write(store.path(), r#"{"schema_version":2,"tasks":{}}"#).unwrap();
        assert!(store.load().is_err());
        fs::write(store.path(), r#"{"schema_version":1,"tasks":{"a":{"id":"a","title":"A","dependencies":["missing"],"status":{"state":"pending"}}}}"#).unwrap();
        assert!(store.load().is_err());
    }
}
