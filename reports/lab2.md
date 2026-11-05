# 实验报告 Lab 2：地址空间下的系统调用重写与 `mmap`/`munmap`（ch4）

> 姓名：qz999　　学号：（请填写）
> 课程：2026 秋冬季训练营 · 专业阶段 rCore-Tutorial　　仓库：LearningOS/2026a-rcore-qz999-66
> 荣誉准则：本报告与实验代码均为本人独立完成，未抄袭他人代码或报告；参考的资料已在正文中注明。

## 一、本章任务概述

第四章引入了基于 Sv39 的页表，内核与每个应用各自拥有独立的地址空间。用户程序看到的虚拟地址不再等于物理地址，第三章中"直接解引用用户指针"的做法彻底失效。本章完成两项编程作业：

1. **作业一**：基于地址空间重写 `sys_get_time`（`gettimeofday`）与 `sys_trace`，恢复其正常功能；
2. **作业二**：实现匿名映射 `sys_mmap` 与 `sys_munmap`。

## 二、作业一：重写 `sys_get_time` 与 `sys_trace`

### 2.1 带检查的用户内存访问

内核访问用户内存的常规做法是 `translated_byte_buffer`：把一段虚拟地址区间翻译成若干物理页切片。但它的实现直接 `unwrap()` 页表项，遇到未映射地址会在内核里 panic，这对 `sys_trace` 是致命的——用户完全可以传一个非法地址来"探测"。

因此我在 `mm/page_table.rs` 中新增了带检查的版本：

```rust
pub fn translated_byte_buffer_checked(
    token: usize, ptr: *const u8, len: usize, write: bool,
) -> Option<Vec<&'static mut [u8]>>
```

它逐页翻译时检查三件事：

1. 该页在页表中存在（`is_valid`）；
2. 该页带有 `U` 标志，即确实是用户页，防止用户借内核之手读写 TrapContext、内核栈等内核映射；
3. 若要写入，则还要求 `W` 标志，防止写入只读页。

任一检查失败立即返回 `None`；全部通过则返回按物理页切分的字节切片。地址加法用 `checked_add`，避免溢出回绕。在此基础上封装了 `read_from_user` / `write_to_user` 两个小工具，负责把可能跨页的切片与目标缓冲区拼接起来。

### 2.2 `sys_get_time`

题目提示"如果 `TimeVal` 被两页切开怎么办"，这正是不能用 `*ts = ...` 直接赋值的原因：`TimeVal` 由两个 `usize` 组成，若它的起始地址靠近页尾，第二个字段就落在下一页，而这两页在物理上并不相邻。

实现方式是把它当作**字节流**复制：先在栈上构造 `TimeVal`，再取它的字节切片交给 `write_to_user`，后者按页拆分、逐页拷贝，因此无论落在页内还是跨页都能正确写入；地址非法时返回 `-1` 而不是让内核崩溃。

### 2.3 `sys_trace`

统计部分沿用第三章的设计：TCB 中增加 `syscall_times: [u32; MAX_SYSCALL_NUM]`，并在系统调用分派入口统一累加，保证"`trace` 自身也计入统计"。

三种请求：读字节、写字节、查询系统调用次数。读写都走 `translated_byte_buffer_checked`，因此：

- `trace_read(isize::MAX)`、`trace_read(0x80200000)`（内核地址）→ 该虚拟页在用户页表中不存在 → 返回 `-1`；
- 对 `mmap` 得到的只读页 `trace_write` → 页表项 `W=0` → 返回 `-1`；
- `munmap` 之后再 `trace_read` 同一地址 → 页表项已被清除 → 返回 `-1`。

测试 `ch4_trace1` 恰好逐条覆盖了这几种情况。

## 三、作业二：`mmap` 与 `munmap`

### 3.1 接口与权限转换

| 系统调用 | 语义 |
| --- | --- |
| `sys_mmap(start, len, port)` | 在 `start` 处申请 `len` 字节、权限为 `port` 的匿名映射 |
| `sys_munmap(start, len)` | 撤销 `[start, start+len)` 的映射 |

`port` 的第 0/1/2 位分别表示可读、可写、可执行，其他位必须为 0；`start` 必须页对齐，`len` 向上取整到页。

```rust
fn port_to_permission(port: usize) -> Option<MapPermission> {
    if port == 0 || port > 0b111 { return None; }
    let mut permission = MapPermission::U;
    if port & 1 != 0 { permission |= MapPermission::R; }
    if port & 2 != 0 { permission |= MapPermission::W; }
    if port & 4 != 0 { permission |= MapPermission::X; }
    Some(permission)
}
```

测试 `ch4_mmap2` 使用 `port = 2`（只写、不可读）。RISC-V 规定 `R=0 && W=1` 是**保留组合**，因此按位照搬即可——映射建立成功，但任何访问都会触发缺页异常，正好符合"程序被杀死、不输出 error"的预期。

### 3.2 `MemorySet::mmap`

```rust
pub fn mmap(&mut self, start_va: VirtAddr, len: usize, permission: MapPermission) -> bool {
    if len == 0 || start_va.page_offset() != 0 { return false; }
    let start_vpn = start_va.floor();
    let end_vpn: VirtPageNum = VirtAddr::from(start_va.0 + len).ceil();
    if self.areas.iter().any(|area| area.overlaps(start_vpn, end_vpn)) { return false; }
    self.push(MapArea::new(...), None);
    true
}
```

要点是**先检查、再修改**：`push` 会逐个分配物理帧并写页表项，而 `PageTable::map` 内部断言"该虚拟页此前未映射"，一旦重叠就会 panic。所以必须在分配之前把所有页检查一遍，保证失败时地址空间完全不变。

### 3.3 `MemorySet::munmap`

合法性条件：`start` 页对齐、`len` 是页大小整数倍、区间内每一页都已映射、不能把已有 `MapArea` 从中间切开。先统计"被区间完整覆盖的区域"的页数，与区间页数比较，相等才继续；然后逐个移除：

```rust
let mut area = self.areas.remove(index);
area.unmap(&mut self.page_table);   // 先清页表项
// area 在此处被 drop，FrameTracker 随之释放物理帧
```

顺序很重要：**先清页表项，再释放物理帧**。否则物理帧可能被立刻分配给其它映射并写入数据，而页表项仍指向该帧。

## 四、测试结果

```
cd ci-user && make test CHAPTER=4
```

```
[PASS] found <get_time OK! (\d+)>
[PASS] found <Test sleep OK!>
[PASS] found <Test sleep1 passed!>
[PASS] found <string from task trace test>
[PASS] found <Test trace OK!>
[PASS] found <Test 04_1 OK!>
[PASS] found <Test 04_4 test OK!>
[PASS] found <Test 04_5 ummap OK!>
[PASS] found <Test 04_6 ummap2 OK!>
[PASS] found <Test trace_1 OK!>
[PASS] not found <FAIL: T.T>
[PASS] not found <Test sbrk failed!>
[PASS] not found <Should cause error, Test 04_2 fail!>
[PASS] not found <Should cause error, Test 04_3 fail!>

Test passed: 16/16
```

16 项全部通过。其中"未找到 `Should cause error`"说明只读页/只写页确实按预期触发了用户态缺页异常并被内核杀死。

## 五、问答作业

### 1. SV39 页表项的组成与标志位

Sv39 页表项为 64 位：`[63:54]` 保留，`[53:10]` 为物理页号 PPN（44 位），`[9:0]` 为标志位：

| 位 | 名称 | 作用 |
| --- | --- | --- |
| 0 | V | 有效位，为 0 表示该页未映射，访问触发缺页 |
| 1 | R | 可读 |
| 2 | W | 可写（`R=0,W=1` 为保留组合） |
| 3 | X | 可执行 |
| 4 | U | 用户态可访问，为 0 则只有 S 态能访问 |
| 5 | G | 全局映射，TLB 刷新时不失效 |
| 6 | A | 被访问过（可用于页面置换） |
| 7 | D | 被写过（脏页，可用于回写） |
| 8 | RSW | 保留给操作系统使用 |

标志位的组合决定了页的语义：`R|W|X=0` 而 V=1 是"指向下一级页表"的非叶节点；`U=0` 的页就是内核页，这正是内核与用户共用一张页表时隔离用户访问的手段。A/D 位可以支撑 Clock 之类的置换算法，也可以用来实现 COW。

### 2. 缺页

**哪些异常可能是缺页导致的**：`LoadPageFault`、`StorePageFault`、`InstructionPageFault`（分别对应读、写、取指），另外 `LoadFault`/`StoreFault` 也可能由非法地址引起。`stval` 保存出错的虚拟地址，`scause` 保存异常原因，`sepc` 保存出错指令的地址。

**Lazy 策略的好处**：程序启动时不必把整个可执行文件和数据段都读进内存，只建立映射关系，真正访问时才分配物理页。这样可以缩短启动时间、减少内存占用（很多页可能永远不会被访问），也让"申请大块内存"变得廉价。

**10G 连续内存的页表开销**：10 GiB = 2 620 144 个 4 KiB 页，每 512 个页需要一个末级页表页，即约 5118 个页表页，每个 4 KiB ≈ 20 MiB，加上若干中间级页表页，量级约 **20 MiB**（约为数据本身的 2×10⁻³）。注意这只是**页表本身**的开销，物理数据页仍需要另外 10 GiB。

**如何实现 Lazy**：`mmap` 时只记录 `(start, len, port)`，先不分配物理帧、不写页表项；用户访问触发缺页时，在 `trap_handler` 中用 `stval` 定位出错地址，检查它是否落在某个 lazy 区域内，若是则分配一页、按其权限写入页表项并 `sfence.vma`，再返回用户态重新执行该指令。

**swap 时页如何失效**：把页表项清空（V=0），并在 PTE 的 RSW 位或操作系统维护的映射表中记录该页在磁盘上的位置；访问时缺页，由内核把页换回来并恢复 PTE。

### 3. 双页表与单页表

- **单页表下如何更换页表**：切换进程时把新进程的 `satp` 写入 CSR；内核映射也在同一张页表里，不需要切换专用的内核页表。
- **单页表下如何阻止用户访问内核页**：把内核页 PTE 的 `U` 位置 0。U 模式访问 `U=0` 的页会触发缺页异常，而 S 模式可以访问。
- **单页表的优势**：切换进程/进出内核时不需要切换 `satp` 和刷新 TLB，系统调用与中断的开销更小，实现也更简单。
- **双页表何时换页表**：进入用户态时切到用户页表，陷入内核时切到内核页表。若实现单页表，我会选择"只在进程切换时更换页表"，靠 `U` 位做隔离——这正是 KPTI 出现之前的传统设计。

## 六、问题与思考

1. **失败要"原子"**：`mmap`/`munmap` 都可能在中途失败，所有校验必须放在副作用之前，否则会泄漏物理帧甚至 panic。
2. **跨页结构体**：`TimeVal`、`Stat` 这类结构体都可能跨页，统一走字节流是最稳妥的做法。
3. **权限检查必须在软件层再做一遍**：内核处于 S 态、直接读物理内存，硬件不会替我们检查，`U`/`W` 标志必须由 `translated_byte_buffer_checked` 显式验证。
