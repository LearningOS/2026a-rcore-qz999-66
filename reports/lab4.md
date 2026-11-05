# 实验报告 Lab 4：文件系统相关的 `fstat`、`linkat` 与 `unlinkat`（ch6）

> 姓名：qz999　　学号：（请填写）
> 课程：2026 秋冬季训练营 · 专业阶段 rCore-Tutorial　　仓库：LearningOS/2026a-rcore-qz999-66
> 荣誉准则：本报告与实验代码均为本人独立完成，未抄袭他人代码或报告；参考的资料已在正文中注明。

## 一、本章任务概述

第六章把用户程序从内核镜像搬到了 easy-fs 文件系统上，进程通过文件描述符表访问文件。本章的编程作业是补全三个与文件元数据有关的系统调用：

| 系统调用 | 功能 |
| --- | --- |
| `fstat(fd, *Stat)` | 获取已打开文件的元数据（设备号、inode 号、类型、硬链接数） |
| `linkat(old, new)` | 为已有文件创建硬链接 |
| `unlinkat(name)` | 删除一个硬链接，链接数归零时回收 inode 与其数据块 |

难点在于：**easy-fs 原本并不支持硬链接**——它的 `DiskInode` 里没有链接计数字段，`Inode` 也没有备份 `link`/`unlink` 的接口，甚至没有回收 inode 的位图操作。因此本章既要改内核，也要改文件系统本身。

## 二、文件系统层（easy-fs）的改动

### 2.1 为 inode 增加链接计数

在 `DiskInode` 中新增 `nlink: u32`，并在 `initialize` 时置 1：

```rust
pub struct DiskInode {
    pub size: u32,
    pub direct: [u32; INODE_DIRECT_COUNT],
    pub indirect1: u32,
    pub indirect2: u32,
    type_: DiskInodeType,
    /// number of hard links pointing to this inode
    pub nlink: u32,
}
```

`DiskInode` 是 `#[repr(C)]` 且直接落盘的结构，所以改变字段意味着磁盘布局变化。由于 `easy-fs-fuse` 与内核使用的是同一份 `easy-fs`，镜像每次评测都会重新生成，两边布局始终一致。

### 2.2 回收 inode：`dealloc_inode`

`Bitmap` 本身有 `dealloc`，但 `EasyFileSystem` 只暴露了 `alloc_inode`。补上：

```rust
pub fn dealloc_inode(&mut self, inode_id: u32) {
    self.inode_bitmap.dealloc(&self.block_device, inode_id as usize);
}
```

没有它，反复创建/删除文件（`ch6_file3` 要循环 10 次）会把 inode 位图耗尽。

### 2.3 `inode_id` 的反向换算

`fstat` 需要一个稳定的 inode 编号，而 `Inode` 结构里只保存了磁盘位置 `(block_id, block_offset)`。我在 `EasyFileSystem` 里补了正向的逆运算：

```rust
pub fn get_disk_inode_id(&self, block_id: u32, block_offset: usize) -> u32 {
    let inode_size = core::mem::size_of::<DiskInode>();
    let inodes_per_block = (BLOCK_SZ / inode_size) as u32;
    (block_id - self.inode_area_start_block) * inodes_per_block
        + (block_offset / inode_size) as u32
}
```

第一版我用 `block_id * inodes_per_block + offset / size` 直接当编号，结果 `link` 时 `get_disk_inode_pos` 拿到的是错误的位置——**编号必须是"相对于 inode 区起始块"的序号**，否则增删的是别的 inode。这个 bug 直接表现为 `ch6_file2` 在 `assert_eq!(stat.nlink, 2)` 处失败。

### 2.4 `link` 与 `unlink`

```rust
pub fn link(&self, name: &str, inode_id: u32) -> isize {
    if self.read_disk_inode(|d| self.find_inode_id(name, d)).is_some() { return -1; }
    let mut fs = self.fs.lock();
    // 目标 inode 的 nlink += 1
    // 在当前目录末尾追加一个 DirEntry
}
```

`unlink` 的流程是：

1. 在目录中查找到目标项，记下它的下标、总数与 inode id；
2. 目标 inode 的 `nlink -= 1`；若归零，则 `clear_size` 回收数据块、`dealloc_inode` 回收 inode；
3. 删除目录项：把**最后一项搬到空洞处**，再把目录大小减去 `DIRENT_SZ`，保持目录紧凑。

第 3 步只减小 `size` 而不释放数据块是有意的：目录通常只有几个块，且后续再增加条目时 `blocks_num_needed` 在同一块内算下来为 0，不会重复分配；若真的跨越块边界，也只是浪费一个块而不影响正确性。

## 三、内核层的改动

### 3.1 让 `File` 能取到元数据

`fd_table` 里存的是 `Arc<dyn File>`，它可能是 stdin/stdout，也可能是普通文件。`fstat` 需要拿到后者的元数据，而 trait object 无法直接 `downcast`。于是给 `File` 增加：

```rust
fn as_any(&self) -> &dyn Any;
```

三个实现（`OSInode`、`Stdin`、`Stdout`）都返回 `self`，`sys_fstat` 里用 `downcast_ref::<OSInode>()` 判断是不是真正的文件：

```rust
let os_inode = match file.as_any().downcast_ref::<crate::fs::OSInode>() {
    Some(os_inode) => os_inode,
    None => return -1,
};
```

### 3.2 `Stat` 的构造与跨页写入

`Stat` 的 `pad` 字段是私有的，外部无法直接构造，因此增加了一个构造函数 `Stat::new(dev, ino, mode, nlink)`。写回用户态时沿用第四章的思路：把它当作字节流，通过 `translated_byte_buffer_checked` 逐页拷贝，避免 `Stat` 恰好跨页时写坏。

### 3.3 `sys_linkat` / `sys_unlinkat`

本章只支持根目录，所以直接在 `ROOT_INODE` 上操作：

```rust
pub fn sys_linkat(old_name: *const u8, new_name: *const u8) -> isize {
    let token = current_user_token();
    let old_name = translated_str(token, old_name);
    let new_name = translated_str(token, new_name);
    match ROOT_INODE.find(old_name.as_str()) {
        Some(inode) => ROOT_INODE.link(new_name.as_str(), inode.inode_id()) as isize,
        None => -1,
    }
}
```

## 四、测试结果

```
cd ci-user && make test CHAPTER=6
```

```
[PASS] found <Test file0 OK!>
[PASS] found <Test fstat OK!>
[PASS] found <Test link OK!>
[PASS] found <Test mass open/unlink OK!>
... （前五章全部测例均通过）
[PASS] not found <FAIL: T.T>
[PASS] not found <Test sbrk failed!>

Test passed: 31/31
```

31 项全部通过。其中：

- `ch6_file1` 验证 `fstat` 的 `mode == FILE`、`nlink == 1`；
- `ch6_file2` 逐次验证 `nlink` 为 2、4、3、1，且同 inode 的 `dev`/`ino` 完全一致——这正好覆盖了 `inode_id` 反向换算的正确性；
- `ch6_file3` 循环创建、写入 150 KB、删除 10 次，验证数据块与 inode 都能被真正回收。

## 五、问题与思考

1. **落盘结构的兼容性**：给 `DiskInode` 加字段会改变 `INODES_PER_BLOCK` 等所有由 `size_of` 推导出的常量。这个设计之所以安全，是因为生成镜像与读镜像用的是同一份代码；若磁盘上已有旧镜像，就必须先格式化。
2. **编号的含义**：inode id 是"inode 区内的序号"，而不是绝对块号。凡是需要"从位置反推编号"的地方都要减去 `inode_area_start_block`，否则会静默地操作到别的 inode——这类 bug 不会 panic，只会表现为断言失败，排查成本很高。
3. **目录项删除策略**：用"末项填充空洞 + 缩小 size"做紧凑化，比维护空闲链表简单得多，代价是目录遍历是线性的。easy-fs 的定位就是教学用的小文件系统，这个取舍是合适的。
4. **引用计数与硬链接**：删除目录项与释放 inode 是两件事，只有当 `nlink` 归零时才回收。真实系统还要考虑"仍被进程打开"的引用，本实验按测例要求在 `nlink == 0` 时直接回收。
