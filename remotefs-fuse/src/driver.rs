#[cfg(unix)]
#[cfg_attr(docsrs, doc(cfg(unix)))]
mod unix;
#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
mod windows;

use remotefs::RemoteFs;

use crate::MountOption;

/// Remote Filesystem Driver
///
/// This driver takes a instance which implements the [`RemoteFs`] trait and mounts it to a local directory.
///
/// The driver will use the [`fuser`](https://crates.io/crates/fuser) crate to mount the filesystem, on Unix systems, while
/// it will use [dokan](https://crates.io/crates/dokan) on Windows.
pub struct Driver<T: RemoteFs> {
    /// Inode database
    #[cfg(unix)]
    database: unix::InodeDb,
    /// File handle database
    #[cfg(unix)]
    file_handlers: unix::FileHandlersDb,
    /// Mount options
    pub(crate) options: Vec<MountOption>,
    #[cfg(unix)]
    /// [`RemoteFs`] instance
    remote: T,
    #[cfg(windows)]
    /// [`RemoteFs`] instance usable as `Sync` in immutable references
    remote: std::sync::Arc<std::sync::Mutex<T>>,
}

impl<T> Driver<T>
where
    T: RemoteFs,
{
    /// Create a new instance of the [`Driver`] providing a instance which implements the [`RemoteFs`] trait.
    ///
    /// The [`RemoteFs`] instance must be boxed.
    ///
    /// # Arguments
    ///
    /// * `remote` - The instance which implements the [`RemoteFs`] trait.
    /// * `options` - The mount options.
    pub fn new(remote: T, options: Vec<MountOption>) -> Self {
        Self {
            #[cfg(unix)]
            database: unix::InodeDb::load(),
            #[cfg(unix)]
            file_handlers: unix::FileHandlersDb::default(),
            options,
            #[cfg(unix)]
            remote,
            #[cfg(windows)]
            remote: std::sync::Arc::new(std::sync::Mutex::new(remote)),
        }
    }

    /// Get the specified uid from the mount options.
    #[cfg(unix)]
    fn uid(&self) -> Option<u32> {
        self.options.iter().find_map(|opt| match opt {
            MountOption::Uid(uid) => Some(*uid),
            _ => None,
        })
    }

    /// Get the specified gid from the mount options.
    #[cfg(unix)]
    fn gid(&self) -> Option<u32> {
        self.options.iter().find_map(|opt| match opt {
            MountOption::Gid(gid) => Some(*gid),
            _ => None,
        })
    }

    /// Get the specified default mode from the mount options.
    /// If not set, the default is 0755.
    #[cfg(unix)]
    fn default_mode(&self) -> u32 {
        self.options
            .iter()
            .find_map(|opt| match opt {
                MountOption::DefaultMode(mode) => Some(*mode),
                _ => None,
            })
            .unwrap_or(0o755)
    }
}
