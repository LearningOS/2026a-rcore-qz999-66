# 实验报告 Lab 1：基于地址空间重写 `sys_get_time` 与 `sys_trace`（ch4）

## 一、实验目标

第四章引入了基于 Sv39 的页表，内核与每个应用各自拥有独立的地址空间。用户程序看到的虚拟地址不再等于物理地址，因此第三章中"直接解引用用户指针"的做法彻底失效：

- 用户栈、用户堆等都在用户的虚拟地址空间里，内核必须通过**当前任务页表**做地址翻译才能访问；
- 还要拒绝非法地址（未映射、内核页、只读页），否则内核会读到错误的数据甚至触发内核页错误。

本实验要求把 `sys_get_time` 与 `sys_trace` 重写为基于地址空间的版本，并保持第三章 `sys_trace` 的三种请求语义（读字节 / 写字节 / 查询系统调用次数）。

## 二、关键设计：带检查的用户内存访问

内核访问用户内存的常规做法是 `translated_byte_buffer`：把一段虚拟地址区间翻译成若干物理页切片。但它的实现直接 `unwrap()` 页表项，遇到未映射地址会在内核里 panic，这对 `sys_trace` 是致命的——用户完全可以传一个非法地址来"探测"。

因此我在 `mm/page_table.rs` 中新增了带检查的版本：

```rust
pub fn translated_byte_buffer_checked(
    token: usize, ptr: *const u8, len: usize, write: bool,
) -> Option<Vec<&'static mut [u8]>>
```

它在逐页翻译时检查三件事：

1. 该页在页表中存在（`is_valid`）；
2. 该页带有 `U` 标志，即确实是用户页，防止用户借内核之手读写 TrapContext、内核栈等内核映射；
3. 若要写入，则还要求 `W` 标志，防止写入只读页。

任一检查失败立即返回 `None`；全部通过则返回按物理页切分的字节切片。地址加法用 `checked_add`，避免溢出回绕。

在此之上封装了两个小工具（`syscall/process.rs`）：

```rust
fn read_from_user(ptr: *const u8, len: usize, dst: &mut [u8]) -> bool;
fn write_to_user(ptr: *mut u8, src: &[u8]) -> bool;
```

它们负责把可能跨页的切片与目标缓冲区拼接起来，调用者只关心成功与否。

## 三、`sys_get_time` 的实现

题目提示"如果 `TimeVal` 被两页切开怎么办"，这正是不能用 `*ts = ...` 直接赋值的原因：`TimeVal` 由两个 `usize` 组成，若它的起始地址靠近页尾，第二个字段就落在下一页；而这两页在物理上并不相邻。

实现方式是把它当作**字节流**复制：

```rust
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us();
    let time_val = TimeVal { sec: us / 1_000_000, usec: us % 1_000_000 };
    let bytes = unsafe {
        core::slice::from_raw_parts(&time_val as *const _ as *const u8,
                                    core::mem::size_of::<TimeVal>())
    };
    if write_to_user(ts as *mut u8, bytes) { 0 } else { -1 }
}
```

`write_to_user` 内部按页拆分、逐页拷贝，因此无论 `TimeVal` 落在页内还是跨页都能正确写入；地址非法时返回 `-1` 而不是让内核崩溃。

## 四、`sys_trace` 的实现

统计部分延续第三章的设计：TCB 中增加 `syscall_times: [u32; MAX_SYSCALL_NUM]`，并在系统调用分派入口统一累加，从而保证"`trace` 自身也计入统计"。

三种请求：

```rust
match trace_request {
    TRACE_READ  => 读一个字节，失败返回 -1,
    TRACE_WRITE => 写一个字节，失败返回 -1,
    TRACE_SYSCALL => get_current_syscall_times(id) as isize,
    _ => -1,
}
```

读写都走 `translated_byte_buffer_checked`，因此：

- `trace_read(isize::MAX)`、`trace_read(0x80200000)`（内核地址）→ 该虚拟页在用户页表中不存在 → `None` → 返回 `-1`；
- 对 `mmap` 得到的只读页 `trace_write` → 页表项 `W=0` → 返回 `-1`；
- `munmap` 之后再 `trace_read` 同一地址 → 页表项已被清除 → 返回 `-1`。

测试 `ch4_trace1` 恰好逐条覆盖了这几种情况。

## 五、测试结果

```
cd ci-user && make test CHAPTER=4
```

```
[PASS] found <get_time OK! (\d+)>
...
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

16 项全部通过，其中 `Test 04_2/04_3 fail` 的"未找到"说明 `mmap` 的只读页/只写页确实按预期触发了用户态缺页异常并被内核杀死，而不是被错误地读写成功。

## 六、问题与思考

1. **`translated_byte_buffer` 为什么不直接改**：它在 `sys_write` 等处被使用，语义是"地址一定合法，非法即内核 bug"。另加 `_checked` 版本语义更清晰，也避免给热路径增加分支。
2. **跨页结构体**：这是本章最容易踩的坑。除了 `TimeVal`，后续章节的 `Stat`、`UserBuffer` 等都要走同样的字节流思路。
3. **权限检查必须在软件层再做一遍**：页表本身会在硬件层拦截越权访问，但内核处于 S 态、直接读物理内存，硬件不会替我们检查，所以 `U`/`W` 标志必须由 `translated_byte_buffer_checked` 显式验证。
