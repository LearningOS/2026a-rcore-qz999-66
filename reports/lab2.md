# 实验报告 Lab 2：实现 `mmap` 与 `munmap`（ch4）

## 一、实验目标

第四章的地址空间由若干 `MapArea` 组成，每个 `MapArea` 是一段连续的虚拟页，背后是逐个分配的物理帧。实验要求在此基础上实现两个系统调用：

| 系统调用 | 语义 |
| --- | --- |
| `sys_mmap(start, len, port)` | 在 `start` 处申请 `len` 字节、权限为 `port` 的匿名映射，成功返回 `0`，失败返回 `-1` |
| `sys_munmap(start, len)` | 撤销 `[start, start+len)` 的映射，成功返回 `0`，失败返回 `-1` |

`port` 的三个低位分别表示可读（1）、可写（2）、可执行（4），与 Linux 的 `PROT_READ/WRITE/EXEC` 一致。

## 二、实现

### 2.1 地址对齐与权限转换

`mmap` 要求 `start` 页对齐（否则无法确定映射起点），`len` 允许不对齐，向上取整到页；`port` 为 0 或大于 7 都是非法请求。权限直接映射到 `MapPermission`：

```rust
fn port_to_permission(port: usize) -> Option<MapPermission> {
    if port == 0 || port > 0b111 { return None; }
    let mut permission = MapPermission::U;      // 用户页
    if port & 1 != 0 { permission |= MapPermission::R; }
    if port & 2 != 0 { permission |= MapPermission::W; }
    if port & 4 != 0 { permission |= MapPermission::X; }
    Some(permission)
}
```

这里有一个测试上的细节：`ch4_mmap2` 用 `port = 2`（只写、不可读）。RISC-V 规定 `R=0 && W=1` 是**保留组合**，因此按位照搬即可——映射建立成功，但任何访问都会触发缺页异常，正好符合"程序被杀死、不输出 error"的预期；不需要额外做特殊处理。

### 2.2 `MemorySet::mmap`

```rust
pub fn mmap(&mut self, start_va: VirtAddr, len: usize, permission: MapPermission) -> bool {
    if len == 0 || start_va.page_offset() != 0 { return false; }
    let start_vpn = start_va.floor();
    let end_vpn: VirtPageNum = VirtAddr::from(start_va.0 + len).ceil();
    if self.areas.iter().any(|area| area.overlaps(start_vpn, end_vpn)) { return false; }
    self.push(MapArea::new(start_va, VirtAddr::from(start_va.0 + len),
                           MapType::Framed, permission), None);
    true
}
```

要点：

- **先检查、再修改**。`push` 会逐个 `map_one` 分配物理帧并写页表项，而 `PageTable::map` 内部断言"该虚拟页此前未映射"，一旦重叠就会 panic。所以必须在分配之前把所有页都检查一遍，保证失败时地址空间完全不变。
- 为此给 `MapArea` 增加了 `overlaps` 与 `vpn_range_bound` 两个小接口，避免把 `vpn_range` 暴露出去。

### 2.3 `MemorySet::munmap`

`munmap` 的合法性条件更严格：

1. `start` 页对齐，`len` 必须是页大小的整数倍；
2. 区间内每一页都必须已经映射；
3. 不能把一个已有的 `MapArea` 从中间切开（本实现不支持区域分割）。

于是先统计"被区间完整覆盖的区域"的页数，与区间页数比较：相等才说明区间被完整覆盖；否则返回 `false`。确认之后逐个移除这些区域：

```rust
let mut area = self.areas.remove(index);
area.unmap(&mut self.page_table);   // 先清页表项
// area 在此处被 drop，FrameTracker 随之释放物理帧
```

顺序很重要：**先清页表项，再释放物理帧**。若反过来，物理帧可能被立刻分配给其它映射并写入数据，而页表项仍指向该帧，就会出现"已释放的页仍可访问"的错误。

### 2.4 系统调用层

```rust
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    if start % PAGE_SIZE != 0 || len == 0 { return -1; }
    match port_to_permission(port) {
        Some(permission) => if mmap_current(start, len, permission) { 0 } else { -1 },
        None => -1,
    }
}

pub fn sys_munmap(start: usize, len: usize) -> isize {
    if munmap_current(start, len) { 0 } else { -1 }
}
```

## 三、测试逐条分析

| 测例 | 行为 | 预期 | 结果 |
| --- | --- | --- | --- |
| `ch4_mmap0` | mmap 一页可读写，逐字节写后读回 | 数据一致 | 通过 |
| `ch4_mmap1` | mmap 只读页，再写入 | 触发 StorePageFault，进程被杀，不打印 error | 通过 |
| `ch4_mmap2` | mmap 只写页，再读取 | 触发 LoadPageFault，进程被杀 | 通过 |
| `ch4_mmap3` | 重叠映射、非对齐 start、port=0、port 含非法位 | 全部返回 -1 | 通过 |
| `ch4_unmap` | mmap 三页，unmap 首段，再重叠区域重新 mmap | 全部返回 0，读写一致 | 通过 |
| `ch4_unmap2` | unmap 长度非整页、start 非对齐 | 返回 -1 | 通过 |

其中 `ch4_unmap` 最能说明"mmap 先查重叠"的必要性：`mmap(start - len, len + 1, prot)` 的结束地址是 `start + 1`，按页取整后恰好会覆盖已映射的第一页。在 `munmap(start, len)` 之前它必须失败，之后则必须成功——测试里两种情况都出现了。

## 四、测试结果

```
cd ci-user && make test CHAPTER=4
...
Test passed: 16/16
```

## 五、问题与思考

1. **失败要"原子"**：`mmap`/`munmap` 都可能在中途失败。如果先分配后检查，失败时已分配的帧和页表项会泄漏甚至 panic。所有校验都放在副作用之前，是这里唯一稳妥的写法。
2. **不支持部分 unmap 的取舍**：实现不支持把一个大区域从中间切开（需要拆分 `MapArea`），测试用例也不需要。若要做完整支持，可以在 `munmap` 时把区域拆成「左段 + 右段」，这需要 `MapArea` 支持构造子区域。
3. **权限位与硬件语义**：RISC-V 的 `R=0,W=1` 保留组合被直接用来实现"只写页"，这依赖硬件规范，比在软件里额外模拟更简洁，也说明了页表权限位就是要"照着硬件语义去用"。
