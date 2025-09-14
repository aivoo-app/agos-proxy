//! Persistence for the domain model.
//!
//! The MVP backs onto a single embedded SQLite file, so there is no external
//! database to run or configure. This module owns the store location and the
//! connection lifecycle so the rest of the crate never has to think about where
//! data actually lives on disk.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Handle to the on-disk store.
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// The default backing-file name inside a given config/home directory.
    pub fn default_path(home: &Path) -> PathBuf {
        home.join("agos.db")
    }

    /// Open the store at `path`, creating the file and schema if needed.
    pub fn open(path: PathBuf) -> Result<Self> {
        // Connecting to the database and running the initial migrations is the
        // storage milestone's first job. Nothing is persisted yet, but the
        // Store owns the path so later commits fill in from here.
        Ok(Store { path })
    }

    /// Base location of the backing database file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
