use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ, NAME_LENGTH_LIMIT,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    inode_id: u32,
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        inode_id: u32,
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            inode_id,
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        if name.is_empty() {
            return None;
        }
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if !dirent.name().is_empty() && dirent.name() == name {
                return Some(dirent.inode_id());
            }
        }
        None
    }
    /// Insert a directory entry, reusing an empty slot if available
    fn insert_dirent(
        &self,
        name: &str,
        inode_id: u32,
        dir_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        assert!(dir_inode.is_dir());
        let file_count = (dir_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        let mut target_idx = None;
        for i in 0..file_count {
            assert_eq!(
                dir_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device),
                DIRENT_SZ,
            );
            if dirent.name().is_empty() {
                target_idx = Some(i);
                break;
            }
        }
        let write_idx = match target_idx {
            Some(idx) => idx,
            None => {
                let new_size = (file_count + 1) * DIRENT_SZ;
                self.increase_size(new_size as u32, dir_inode, fs);
                file_count
            }
        };
        let new_dirent = DirEntry::new(name, inode_id);
        dir_inode.write_at(
            write_idx * DIRENT_SZ,
            new_dirent.as_bytes(),
            &self.block_device,
        );
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    inode_id,
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        if name.is_empty() || name.len() > NAME_LENGTH_LIMIT {
            return None;
        }
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            self.insert_dirent(name, new_inode_id, root_inode, &mut fs);
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            new_inode_id,
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// Create a hard link `new_name` pointing to the same inode as `old_name`
    pub fn link(&self, old_name: &str, new_name: &str) -> bool {
        if old_name.is_empty()
            || new_name.is_empty()
            || new_name.len() > NAME_LENGTH_LIMIT
            || old_name == new_name
        {
            return false;
        }
        let mut fs = self.fs.lock();
        let linked = self.modify_disk_inode(|root_inode| {
            let Some(old_inode_id) = self.find_inode_id(old_name, root_inode) else {
                return false;
            };
            if self.find_inode_id(new_name, root_inode).is_some() {
                return false;
            }
            self.insert_dirent(new_name, old_inode_id, root_inode, &mut fs);
            true
        });
        if linked {
            block_cache_sync_all();
        }
        linked
    }
    /// Remove a directory entry by name and reclaim blocks/inode when link count reaches 0
    pub fn unlink(&self, name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let mut fs = self.fs.lock();
        // Step 1: update root directory entry and count remaining links while holding only root's block cache lock
        let removed = self.modify_disk_inode(|root_inode| {
            assert!(root_inode.is_dir());
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            let mut target = None;
            for i in 0..file_count {
                assert_eq!(
                    root_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device),
                    DIRENT_SZ,
                );
                if !dirent.name().is_empty() && dirent.name() == name {
                    target = Some((i, dirent.inode_id()));
                    break;
                }
            }
            let (target_idx, target_inode_id) = target?;
            let empty = DirEntry::empty();
            root_inode.write_at(
                target_idx * DIRENT_SZ,
                empty.as_bytes(),
                &self.block_device,
            );
            let mut remaining_links = 0u32;
            for i in 0..file_count {
                assert_eq!(
                    root_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device),
                    DIRENT_SZ,
                );
                if !dirent.name().is_empty() && dirent.inode_id() == target_inode_id {
                    remaining_links += 1;
                }
            }
            Some((target_inode_id, remaining_links))
        });

        let Some((target_inode_id, remaining_links)) = removed else {
            return false;
        };

        // Step 2: root block cache lock is now dropped, safe to lock target inode's block cache
        if remaining_links == 0 {
            let (target_block_id, target_block_offset) = fs.get_disk_inode_pos(target_inode_id);
            let data_blocks_dealloc =
                get_block_cache(target_block_id as usize, Arc::clone(&self.block_device))
                    .lock()
                    .modify(target_block_offset, |target_inode: &mut DiskInode| {
                        let size = target_inode.size;
                        let blocks = target_inode.clear_size(&self.block_device);
                        assert_eq!(blocks.len(), DiskInode::total_blocks(size) as usize);
                        blocks
                    });
            for data_block in data_blocks_dealloc {
                fs.dealloc_data(data_block);
            }
            fs.dealloc_inode(target_inode_id);
        }

        block_cache_sync_all();
        true
    }
    /// Return (inode_id, is_dir, nlink) for this inode
    pub fn stat_info(&self) -> (u32, bool, u32) {
        let fs = self.fs.lock();
        // Step 1: read this inode's type and release its block cache lock first
        let is_dir = self.read_disk_inode(|disk_inode| disk_inode.is_dir());
        // Step 2: scan root directory to count hard links without nested block locks
        let nlink = if self.inode_id == 0 {
            1
        } else {
            let (root_block_id, root_block_offset) = fs.get_disk_inode_pos(0);
            get_block_cache(root_block_id as usize, Arc::clone(&self.block_device))
                .lock()
                .read(root_block_offset, |root_inode: &DiskInode| {
                    assert!(root_inode.is_dir());
                    let file_count = (root_inode.size as usize) / DIRENT_SZ;
                    let mut dirent = DirEntry::empty();
                    let mut count = 0u32;
                    for i in 0..file_count {
                        assert_eq!(
                            root_inode.read_at(
                                DIRENT_SZ * i,
                                dirent.as_bytes_mut(),
                                &self.block_device,
                            ),
                            DIRENT_SZ,
                        );
                        if !dirent.name().is_empty() && dirent.inode_id() == self.inode_id {
                            count += 1;
                        }
                    }
                    count
                })
        };
        (self.inode_id, is_dir, nlink)
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if !dirent.name().is_empty() {
                    v.push(String::from(dirent.name()));
                }
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
