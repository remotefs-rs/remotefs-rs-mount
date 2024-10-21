mod inode;

use std::ffi::OsStr;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyBmap, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyLock, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, TimeOrNow,
};
use libc::c_int;
use remotefs::{File, RemoteResult};

pub use self::inode::{InodeDb, InodeDbError};
use super::Driver;

const BLOCK_SIZE: usize = 512;

/// Get the inode number for a [`Path`]
fn inode(path: &Path) -> u64 {
    let mut hasher = seahash::SeaHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

/// Convert a [`remotefs::fs::FileType`] to a [`FileType`] from [`fuser`]
fn convert_remote_filetype(filetype: remotefs::fs::FileType) -> FileType {
    match filetype {
        remotefs::fs::FileType::Directory => FileType::Directory,
        remotefs::fs::FileType::File => FileType::RegularFile,
        remotefs::fs::FileType::Symlink => FileType::Symlink,
    }
}

/// Convert a [`File`] from [`remotefs`] to a [`FileAttr`] from [`fuser`]
fn convert_file(value: &File) -> FileAttr {
    FileAttr {
        ino: inode(value.path()),
        size: value.metadata().size,
        blocks: (value.metadata().size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
        atime: value.metadata().accessed.unwrap_or(UNIX_EPOCH),
        mtime: value.metadata().modified.unwrap_or(UNIX_EPOCH),
        ctime: value.metadata().created.unwrap_or(UNIX_EPOCH),
        crtime: UNIX_EPOCH,
        kind: convert_remote_filetype(value.metadata().file_type.clone()),
        perm: value
            .metadata()
            .mode
            .map(|mode| (u32::from(mode)) as u16)
            .unwrap_or(0o777),
        nlink: 0,
        uid: value.metadata().uid.unwrap_or(0),
        gid: value.metadata().gid.unwrap_or(0),
        rdev: 0,
        blksize: BLOCK_SIZE as u32,
        flags: 0,
    }
}

/// Convert a [`TimeOrNow`] to a [`SystemTime`]
fn time_or_now(t: TimeOrNow) -> SystemTime {
    match t {
        TimeOrNow::SpecificTime(t) => t,
        TimeOrNow::Now => SystemTime::now(),
    }
}

impl Driver {
    /// Get the inode for a path.
    ///
    /// If the inode is not in the database, it will be fetched from the remote filesystem.
    fn get_inode_from_path(&mut self, path: &Path) -> RemoteResult<(File, FileAttr)> {
        let (file, attrs) = self.remote.stat(path).map(|file| {
            let attrs = convert_file(&file);
            (file, attrs)
        })?;

        // Save the inode to the database
        if !self.database.has(attrs.ino) {
            self.database.put(attrs.ino, path.to_path_buf());
        }

        Ok((file, attrs))
    }

    /// Get the inode from the inode number
    fn get_inode_from_inode(&mut self, inode: u64) -> RemoteResult<(File, FileAttr)> {
        let path = self
            .database
            .get(inode)
            .ok_or_else(|| {
                remotefs::RemoteError::new(remotefs::RemoteErrorType::NoSuchFileOrDirectory)
            })?
            .to_path_buf();

        self.get_inode_from_path(&path)
    }
}

impl Filesystem for Driver {
    /// Initialize filesystem.
    /// Called before any other filesystem method.
    fn init(&mut self, _req: &Request, _config: &mut KernelConfig) -> Result<(), c_int> {
        info!("Initializing filesystem");
        if let Err(err) = self.remote.connect() {
            error!("Failed to connect to remote filesystem: {err}");
            return Err(libc::EIO);
        }
        info!("Connected to remote filesystem");

        Ok(())
    }

    /// Clean up filesystem.
    /// Called on filesystem exit.
    fn destroy(&mut self) {
        info!("Destroying filesystem");
        if let Err(err) = self.remote.disconnect() {
            error!("Failed to disconnect from remote filesystem: {err}");
        } else {
            info!("Disconnected from remote filesystem");
        }
    }

    /// Look up a directory entry by name and get its attributes.
    fn lookup(&mut self, _req: &Request, _parent: u64, name: &OsStr, reply: ReplyEntry) {
        let path = PathBuf::from(name.to_string_lossy().to_string());

        match self.get_inode_from_path(path.as_path()) {
            Err(err) => {
                error!("Failed to get file attributes: {err}");
                reply.error(libc::ENOENT)
            }
            Ok((_, attrs)) => reply.entry(&Duration::new(0, 0), &attrs, 0),
        }
    }

    /// Forget about an inode.
    /// The nlookup parameter indicates the number of lookups previously performed on
    /// this inode. If the filesystem implements inode lifetimes, it is recommended that
    /// inodes acquire a single reference on each lookup, and lose nlookup references on
    /// each forget. The filesystem may ignore forget calls, if the inodes don't need to
    /// have a limited lifetime. On unmount it is not guaranteed, that all referenced
    /// inodes will receive a forget message.
    fn forget(&mut self, _req: &Request, _ino: u64, _nlookup: u64) {}

    /// Get file attributes.
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let attrs = match self.get_inode_from_inode(ino) {
            Err(err) => {
                error!("Failed to get file attributes: {err}");
                reply.error(libc::ENOENT);
                return;
            }
            Ok((_, attrs)) => attrs,
        };

        reply.attr(&Duration::new(0, 0), &attrs);
    }

    /// Set file attributes.
    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let (mut file, _) = match self.get_inode_from_inode(ino) {
            Ok(attrs) => attrs,
            Err(err) => {
                error!("Failed to get file attributes: {err}");
                reply.error(libc::ENOENT);
                return;
            }
        };

        if let Some(mode) = mode {
            file.metadata.mode = Some(mode.into());
        }
        if let Some(uid) = uid {
            file.metadata.uid = Some(uid);
        }
        if let Some(gid) = gid {
            file.metadata.gid = Some(gid);
        }
        if let Some(size) = size {
            file.metadata.size = size;
        }
        if let Some(atime) = atime {
            file.metadata.accessed = Some(time_or_now(atime));
        }
        if let Some(mtime) = mtime {
            file.metadata.modified = Some(time_or_now(mtime));
        }
        if let Some(ctime) = ctime {
            file.metadata.created = Some(ctime);
        }

        // set attributes
        match self.remote.setstat(file.path(), file.metadata().clone()) {
            Ok(_) => {
                let attrs = convert_file(&file);
                reply.attr(&Duration::new(0, 0), &attrs);
            }
            Err(err) => {
                error!("Failed to set file attributes: {err}");
                reply.error(libc::EIO);
            }
        }
    }

    /// Read symbolic link.
    fn readlink(&mut self, req: &Request, ino: u64, reply: ReplyData) {
        todo!()
    }

    /// Create file node.
    /// Create a regular file, character device, block device, fifo or socket node.
    fn mknod(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        todo!()
    }

    /// Create a directory.
    fn mkdir(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        todo!()
    }

    /// Remove a file
    fn unlink(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        todo!()
    }

    /// Remove a directory
    fn rmdir(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        todo!()
    }

    /// Create a symbolic link
    fn symlink(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        link: &std::path::Path,
        reply: ReplyEntry,
    ) {
        todo!();
    }

    /// Rename a file
    fn rename(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        todo!()
    }

    /// Create a hard link
    fn link(
        &mut self,
        req: &Request,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        todo!()
    }

    /// Open a file.
    /// Open flags (with the exception of O_CREAT, O_EXCL, O_NOCTTY and O_TRUNC) are
    /// available in flags. Filesystem may store an arbitrary file handle (pointer, index,
    /// etc) in fh, and use this in other all other file operations (read, write, flush,
    /// release, fsync). Filesystem may also implement stateless file I/O and not store
    /// anything in fh. There are also some flags (direct_io, keep_cache) which the
    /// filesystem may set, to change the way the file is opened. See fuse_file_info
    /// structure in <fuse_common.h> for more details.
    fn open(&mut self, req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        todo!()
    }

    /// Read data.
    /// Read should send exactly the number of bytes requested except on EOF or error,
    /// otherwise the rest of the data will be substituted with zeroes. An exception to
    /// this is when the file has been opened in 'direct_io' mode, in which case the
    /// return value of the read system call will reflect the return value of this
    /// operation. fh will contain the value set by the open method, or will be undefined
    /// if the open method didn't set any value.
    fn read(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        todo!()
    }

    /// Write data.
    /// Write should return exactly the number of bytes requested except on error. An
    /// exception to this is when the file has been opened in 'direct_io' mode, in
    /// which case the return value of the write system call will reflect the return
    /// value of this operation. fh will contain the value set by the open method, or
    /// will be undefined if the open method didn't set any value.
    fn write(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        write_flags: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        todo!()
    }

    /// Flush method.
    /// This is called on each close() of the opened file. Since file descriptors can
    /// be duplicated (dup, dup2, fork), for one open call there may be many flush
    /// calls. Filesystems shouldn't assume that flush will always be called after some
    /// writes, or that if will be called at all. fh will contain the value set by the
    /// open method, or will be undefined if the open method didn't set any value.
    /// NOTE: the name of the method is misleading, since (unlike fsync) the filesystem
    /// is not forced to flush pending writes. One reason to flush data, is if the
    /// filesystem wants to return write errors. If the filesystem supports file locking
    /// operations (setlk, getlk) it should remove all locks belonging to 'lock_owner'.
    fn flush(&mut self, req: &Request, ino: u64, fh: u64, lock_owner: u64, reply: ReplyEmpty) {
        todo!()
    }

    /// Release an open file.
    /// Release is called when there are no more references to an open file: all file
    /// descriptors are closed and all memory mappings are unmapped. For every open
    /// call there will be exactly one release call. The filesystem may reply with an
    /// error, but error values are not returned to close() or munmap() which triggered
    /// the release. fh will contain the value set by the open method, or will be undefined
    /// if the open method didn't set any value. flags will contain the same flags as for
    /// open.
    fn release(
        &mut self,
        req: &Request,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        todo!()
    }

    /// Synchronize file contents.
    /// If the datasync parameter is non-zero, then only the user data should be flushed,
    /// not the meta data.
    fn fsync(&mut self, req: &Request, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        todo!()
    }

    /// Open a directory.
    /// Filesystem may store an arbitrary file handle (pointer, index, etc) in fh, and
    /// use this in other all other directory stream operations (readdir, releasedir,
    /// fsyncdir). Filesystem may also implement stateless directory I/O and not store
    /// anything in fh, though that makes it impossible to implement standard conforming
    /// directory stream operations in case the contents of the directory can change
    /// between opendir and releasedir.
    fn opendir(&mut self, req: &Request, _ino: u64, _flags: i32, reply: ReplyOpen) {
        reply.opened(0, 0);
    }

    /// Read directory.
    /// Send a buffer filled using buffer.fill(), with size not exceeding the
    /// requested size. Send an empty buffer on end of stream. fh will contain the
    /// value set by the opendir method, or will be undefined if the opendir method
    /// didn't set any value.
    fn readdir(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory) {
        todo!()
    }

    /// Release an open directory.
    /// For every opendir call there will be exactly one releasedir call. fh will
    /// contain the value set by the opendir method, or will be undefined if the
    /// opendir method didn't set any value.
    fn releasedir(&mut self, req: &Request, ino: u64, fh: u64, flags: i32, reply: ReplyEmpty) {
        todo!()
    }

    /// Synchronize directory contents.
    /// If the datasync parameter is set, then only the directory contents should
    /// be flushed, not the meta data. fh will contain the value set by the opendir
    /// method, or will be undefined if the opendir method didn't set any value.
    fn fsyncdir(&mut self, req: &Request, ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        todo!()
    }

    /// Get file system statistics.
    fn statfs(&mut self, req: &Request, ino: u64, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 512, 255, 0);
    }

    /// Set an extended attribute.
    fn setxattr(
        &mut self,
        req: &Request,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        todo!()
    }

    /// Get an extended attribute.
    /// If `size` is 0, the size of the value should be sent with `reply.size()`.
    /// If `size` is not 0, and the value fits, send it with `reply.data()`, or
    /// `reply.error(ERANGE)` if it doesn't.
    fn getxattr(&mut self, req: &Request, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        todo!()
    }

    /// List extended attribute names.
    /// If `size` is 0, the size of the value should be sent with `reply.size()`.
    /// If `size` is not 0, and the value fits, send it with `reply.data()`, or
    /// `reply.error(ERANGE)` if it doesn't.
    fn listxattr(&mut self, req: &Request, ino: u64, size: u32, reply: ReplyXattr) {
        todo!()
    }

    /// Remove an extended attribute.
    fn removexattr(&mut self, req: &Request, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        todo!()
    }

    /// Check file access permissions.
    /// This will be called for the access() system call. If the 'default_permissions'
    /// mount option is given, this method is not called. This method is not called
    /// under Linux kernel versions 2.4.x
    fn access(&mut self, req: &Request, ino: u64, mask: i32, reply: ReplyEmpty) {
        todo!()
    }

    /// Create and open a file.
    /// If the file does not exist, first create it with the specified mode, and then
    /// open it. Open flags (with the exception of O_NOCTTY) are available in flags.
    /// Filesystem may store an arbitrary file handle (pointer, index, etc) in fh,
    /// and use this in other all other file operations (read, write, flush, release,
    /// fsync). There are also some flags (direct_io, keep_cache) which the
    /// filesystem may set, to change the way the file is opened. See fuse_file_info
    /// structure in <fuse_common.h> for more details. If this method is not
    /// implemented or under Linux kernel versions earlier than 2.6.15, the mknod()
    /// and open() methods will be called instead.
    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        todo!()
    }

    /// Test for a POSIX file lock.
    fn getlk(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        reply: ReplyLock,
    ) {
        todo!()
    }

    /// Acquire, modify or release a POSIX file lock.
    /// For POSIX threads (NPTL) there's a 1-1 relation between pid and owner, but
    /// otherwise this is not always the case.  For checking lock ownership,
    /// 'fi->owner' must be used. The l_pid field in 'struct flock' should only be
    /// used to fill in this field in getlk(). Note: if the locking methods are not
    /// implemented, the kernel will still allow file locking to work locally.
    fn setlk(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        sleep: bool,
        reply: ReplyEmpty,
    ) {
        todo!();
    }

    fn readdirplus(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        reply: fuser::ReplyDirectoryPlus,
    ) {
        log::debug!(
            "[Not Implemented] readdirplus(ino: {:#x?}, fh: {}, offset: {})",
            ino,
            fh,
            offset
        );
        reply.error(libc::ENOSYS);
    }

    fn ioctl(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        flags: u32,
        cmd: u32,
        in_data: &[u8],
        out_size: u32,
        reply: fuser::ReplyIoctl,
    ) {
        log::debug!(
            "[Not Implemented] ioctl(ino: {:#x?}, fh: {}, flags: {}, cmd: {}, \
            in_data.len(): {}, out_size: {})",
            ino,
            fh,
            flags,
            cmd,
            in_data.len(),
            out_size,
        );
        reply.error(libc::ENOSYS);
    }

    fn fallocate(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        log::debug!(
            "[Not Implemented] fallocate(ino: {:#x?}, fh: {}, offset: {}, \
            length: {}, mode: {})",
            ino,
            fh,
            offset,
            length,
            mode
        );
        reply.error(libc::ENOSYS);
    }

    fn lseek(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: fuser::ReplyLseek,
    ) {
        log::debug!(
            "[Not Implemented] lseek(ino: {:#x?}, fh: {}, offset: {}, whence: {})",
            ino,
            fh,
            offset,
            whence
        );
        reply.error(libc::ENOSYS);
    }

    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: ReplyWrite,
    ) {
        log::debug!(
            "[Not Implemented] copy_file_range(ino_in: {:#x?}, fh_in: {}, \
            offset_in: {}, ino_out: {:#x?}, fh_out: {}, offset_out: {}, \
            len: {}, flags: {})",
            ino_in,
            fh_in,
            offset_in,
            ino_out,
            fh_out,
            offset_out,
            len,
            flags
        );
        reply.error(libc::ENOSYS);
    }

    /// Map block index within file to block index within device.
    /// Note: This makes sense only for block device backed filesystems mounted
    /// with the 'blkdev' option
    fn bmap(&mut self, req: &Request, ino: u64, blocksize: u32, idx: u64, reply: ReplyBmap) {
        todo!()
    }
}
