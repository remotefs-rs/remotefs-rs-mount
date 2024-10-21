use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type Inode = u64;

type Database = HashMap<Inode, PathBuf>;

/// A database to map inodes to files
///
/// The database is saved to a file when the instance is dropped
#[derive(Debug, Default, Clone)]
pub struct InodeDb {
    database: Database,
}

impl InodeDb {
    /// Load [`InodeDb`] from a file
    pub fn load() -> Self {
        Self {
            database: Database::new(),
        }
    }

    /// Check if the database contains an inode
    pub fn has(&self, inode: Inode) -> bool {
        self.database.contains_key(&inode)
    }

    /// Put a new inode into the database
    pub fn put(&mut self, inode: Inode, path: PathBuf) {
        self.database.insert(inode, path);
    }

    /// Get a path from an inode
    pub fn get(&self, inode: Inode) -> Option<&Path> {
        self.database.get(&inode).map(|x| x.as_path())
    }
}