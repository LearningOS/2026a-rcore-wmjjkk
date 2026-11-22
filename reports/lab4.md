# 实验四：文件系统与 I/O

## 一、基本信息

- 姓名：王可嘉
- 实验分支：ch6
- 实验日期：2026.11.22

## 二、实验目标

理解 `easy-fs` 磁盘布局（超级块、inode 位图与区域、数据块位图与区域、目录项）及内核 VFS 抽象层（`File` trait 与 `OSInode`）；完成 `sys_fstat`、`sys_linkat`、`sys_unlinkat` 三个文件系统调用；并在基于文件系统的进程管理框架下移植与适配 `sys_get_time`、`sys_mmap`、`sys_munmap` 与 `sys_spawn`。

## 三、实验环境

- 操作系统：Windows + WSL Ubuntu
- Rust 工具链：nightly-2024-05-02
- 编译目标：riscv64gc-unknown-none-elf
- 模拟器：QEMU 8.2.x，riscv-virt 平台 + VirtIO 块设备

## 四、实现方法

### 4.1 往期系统调用的移植与重构

1. **安全跨页写回用户空间**：将 `copy_to_user` 移入 `os/src/mm/page_table.rs` 并封装出泛型辅助函数 `copy_to_user_val<T: Copy>`，统一支持 `TimeVal`（16 字节）与 `Stat`（80 字节）两类 `#[repr(C)]` 结构体的跨页安全拷贝与权限校验（先整段校验 `V | U | W`，全部合法后再分页写入）。
2. **`sys_mmap` / `sys_munmap`**：抽取公共区间解析函数 `parse_user_range` 校验页对齐、非零长度、`start + len` 溢出及 Sv39 低半区范围 `[0, 2^38)`，在 `MemorySet` 中复用 `map_user_area` 与 `unmap_framed_area`。
3. **基于文件系统的 `sys_spawn`**：在 `TaskControlBlock` 中封装 `spawn` 方法，在 `sys_spawn` 中改为通过 `open_file(path, OpenFlags::RDONLY)` 从 `easy-fs` 读取 ELF 文件并调用 `read_all()` 构造新进程；同时在 `TaskControlBlock::exec` 中同步重置 `base_size`、`heap_bottom` 和 `program_brk`。

### 4.2 硬链接计数与文件元数据（`easy-fs` 层）

为保持 `DiskInode` 固定 128 字节（每 512 字节块恰好存放 4 个 inode）的磁盘布局不变，采用不修改磁盘结构的设计：

1. **记录 `inode_id`**：在 `easy_fs::Inode` 中保存 `inode_id: u32`（根目录为 `0`，`find`/`create` 时记录对应编号），避免反算或额外查表。
2. **空目录项复用**：删除目录项时将对应槽位覆盖为 `DirEntry::empty()`，以 `dirent.name().is_empty()` 标识空闲槽位；`find_inode_id` 和 `ls` 跳过空槽位，`insert_dirent` 在 `create`/`link` 时优先复用空闲槽位再按需扩容。
3. **`Inode::link(old_name, new_name)`**：校验文件名非空、长度不超过 `NAME_LENGTH_LIMIT`（27 字节）且 `old_name != new_name`，确认 `old_name` 存在且 `new_name` 不存在后，向根目录写入指向同一 `old_inode_id` 的新目录项并同步块缓存。
4. **`Inode::unlink(name)`**：先在根目录块锁内定位并清空目标目录项、统计剩余指向同一 `inode_id` 的有效目录项数 `remaining_links`；**释放根目录块锁后**，若 `remaining_links == 0`，再定位目标 `DiskInode`，调用 `clear_size` 回收全部数据块和间接块，并通过 `EasyFileSystem::dealloc_inode` 释放 inode 位图。
5. **`Inode::stat_info()`**：分两步先读自身 `DiskInode` 类型并释放自身块锁，再扫描根目录统计指向 `self.inode_id` 的有效目录项个数作为 `nlink`。

### 4.3 内核 VFS 抽象与系统调用接入

- **`os/src/fs/mod.rs`**：为 `Stat` 派生 `Clone, Copy` 并提供零初始化填充字段的 `Stat::new`；在 `File` trait 中增加默认返回 `None` 的 `fn stat(&self) -> Option<Stat>`。
- **`os/src/fs/inode.rs`**：在 `OSInode` 中实现 `File::stat`，组装 `Stat::new(0, ino as u64, mode, nlink)`；并导出 `link_file` 与 `unlink_file` 供系统调用层调用。
- **`os/src/syscall/fs.rs`**：实现 `sys_fstat`（查 `fd_table` -> 释放 TCB 锁 -> 调用 `file.stat()` -> `copy_to_user_val` 安全写回用户态）、`sys_linkat` 与 `sys_unlinkat`。

## 五、问题与排查

### 5.1 避免同块 `BlockCache` 死锁

在 `easy-fs` 中每个 512 字节块存放 4 个 `DiskInode`（inode `0, 1, 2, 3` 位于同一磁盘块）。若在持有根目录块缓存锁的闭包内再去锁目标文件的 `DiskInode` 块缓存，当目标文件恰为 inode 1~3 时会导致 `spin::Mutex` 自死锁。因此在 `unlink` 和 `stat_info` 中严格拆分阶段：先退出第一个块缓存闭包释放锁，再获取第二个块缓存锁。

### 5.2 `ch6_file3` 超时排查与优化

初版测试中前 30 项全部通过，但 `ch6_file3`（循环 10 次创建并写入约 145 KB 文件后 `unlink`）运行到第 7 轮时触发 30 秒 QEMU 超时。排查发现 `EasyFileSystem::dealloc_data` 在释放每个数据块前都会将该块读入仅 16 项的块缓存并逐字节清零再刷回 VirtIO 磁盘，单次 `unlink` 约 290 个块就会触发近 600 次冗余磁盘 I/O。由于 `increase_size` 与 `write_at` 均会覆写新分配块的有效范围、`read_at` 受 `size` 约束不会越界读旧数据，移除 `dealloc_data` 中对已释放数据块的清零写盘、只更新 `data_bitmap` 位图后，`ch6_file3` 耗时大幅缩短并顺利通过全部 10 轮迭代。

## 六、测试与结果

在仓库根目录执行官方第六章评测：

```bash
make -C ci-user test CHAPTER=6
```

全部 31 项检查通过（`Test passed: 31/31`），涵盖前序章节的 `sleep`、`sbrk`、`mmap`/`munmap`、`fork`/`spawn` 测例，以及第六章的 `ch6b_filetest_simple`、`ch6_file0`（基本读写）、`ch6_file1`（`fstat`）、`ch6_file2`（`link`/`unlink` 与链接计数）和 `ch6_file3`（大量创建/写入/删除回收），且 `lab1` 至 `lab4` 实验报告检查全部通过。

## 七、实验收获

1. 深入理解了分层文件系统设计：磁盘数据结构、块缓存、VFS `Inode`、内核 `File` trait 以及系统调用各司其职，且硬链接本质上是多个目录项共享同一个 `inode_id`。
2. 体会到块缓存锁粒度和 I/O 放大效应在实际系统中的重要性：既要避免同块缓存嵌套加锁造成的死锁，也要避免释放数据块时的冗余刷盘导致严重的性能退化。
3. 复用 `copy_to_user_val<T>` 统一处理内核结构体到用户态的跨页安全拷贝，让 `sys_get_time` 与 `sys_fstat` 的实现更简洁、健壮。
