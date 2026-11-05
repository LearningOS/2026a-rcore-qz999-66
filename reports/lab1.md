# 实验报告 Lab 1：实现 `trace` 系统调用（ch3）

> 姓名：qz999　　学号：（请填写）
> 课程：2026 秋冬季训练营 · 专业阶段 rCore-Tutorial　　仓库：LearningOS/2026a-rcore-qz999-66
> 荣誉准则：本报告与实验代码均为本人独立完成，未抄袭他人代码或报告；参考的资料已在正文中注明。

## 一、实验目标

第三章的内核已经支持多道程序与分时多任务：任务可以在时钟中断下被抢占，也可以通过 `yield` 主动让出 CPU。本次实验在此基础上新增一个调试用的系统调用 `sys_trace`（syscall id = 410），使它能够：

1. **统计系统调用次数**：查询当前任务对某个系统调用一共调用过多少次；
2. **读取用户内存**：读取当前任务地址空间（第三章即物理内存）中某个字节的内容；
3. **写入用户内存**：修改当前任务地址空间中某个字节的内容。

`trace` 是后续章节（尤其是带地址空间的第四章）实现更复杂调试功能的基础，因此本实验把它按"请求类型"设计成三种子功能，而不是三个独立系统调用。

## 二、接口设计

用户库 `user_lib` 中的封装如下：

```rust
pub enum TraceRequest { Read, Write, Syscall }   // 0, 1, 2

pub fn trace(request: TraceRequest, id: usize, data: usize) -> isize;
```

对应三个参数：`trace_request`（请求类型）、`id`（地址或系统调用号）、`data`（写入的数据）。返回值约定为：

| 请求 | 参数含义 | 返回值 |
| --- | --- | --- |
| `Read` (0) | `id` 为地址 | 该地址上的字节值；地址非法返回 `-1` |
| `Write` (1) | `id` 为地址，`data` 为写入值 | 成功返回 `0`，地址非法返回 `-1` |
| `Syscall` (2) | `id` 为系统调用号 | 当前任务调用该系统调用的次数 |

一个容易出错的细节是：**统计自身也必须被统计**。测试程序 `ch3_trace.rs` 在开头连续调用两次 `count_syscall` 后断言 `count_syscall(SYSCALL_TRACE) == 2`，说明这一次 `trace` 调用本身已经计入。因此在系统调用分派的最开始就要累加计数，而不是在具体处理函数里累加。

## 三、实现

### 3.1 每个任务维护一张计数表

统计必须是"按任务"的：不同任务各自的调用次数互不影响。因此把计数表放进 TCB（`os/src/task/task.rs`）：

```rust
pub struct TaskControlBlock {
    pub task_status: TaskStatus,
    pub task_cx: TaskContext,
    /// 以系统调用号为下标，记录本任务对每个系统调用的调用次数
    pub syscall_times: [u32; MAX_SYSCALL_NUM],
}

impl TaskControlBlock {
    pub fn increase_syscall_times(&mut self, syscall_id: usize) {
        if syscall_id < MAX_SYSCALL_NUM {
            self.syscall_times[syscall_id] += 1;
        }
    }
    pub fn get_syscall_times(&self, syscall_id: usize) -> usize { ... }
}
```

`MAX_SYSCALL_NUM = 512`（`os/src/config.rs`），大于课程中用到的最大系统调用号 473。用数组下标直接定位，查询与累加都是 O(1)；比起用 `Vec` 或哈希表，在 `no_std` 且 TCB 需要 `Copy` 的内核里更简单，也没有堆分配。整张表占 16 × 512 × 4 = 32 KiB 的 .bss，代价可以接受。

`TaskManager` 中增加两个方法，分别对"当前任务"做累加与查询：

```rust
fn increase_current_syscall_times(&self, syscall_id: usize) {
    let mut inner = self.inner.exclusive_access();
    let current = inner.current_task;
    inner.tasks[current].increase_syscall_times(syscall_id);
}

fn get_current_syscall_times(&self, syscall_id: usize) -> usize {
    let inner = self.inner.exclusive_access();
    inner.tasks[current].get_syscall_times(syscall_id)
}
```

### 3.2 在系统调用入口统一计数

`os/src/syscall/mod.rs` 的分派函数在 `match` 之前先累加：

```rust
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    crate::task::increase_current_syscall_times(syscall_id);
    match syscall_id { ... }
}
```

这样所有系统调用（包括 `trace` 自己）都会被统一统计，新增系统调用时也不需要再单独维护，避免遗漏。

### 3.3 实现 `sys_trace`

`os/src/syscall/process.rs`：

```rust
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    match trace_request {
        TRACE_READ => if is_valid_user_addr(id) {
            unsafe { core::ptr::read_volatile(id as *const u8) as isize }
        } else { -1 },
        TRACE_WRITE => if is_valid_user_addr(id) {
            unsafe { core::ptr::write_volatile(id as *mut u8, data as u8) }; 0
        } else { -1 },
        TRACE_SYSCALL => get_current_syscall_times(id) as isize,
        _ => -1,
    }
}
```

这里用 `read_volatile` / `write_volatile`，是为了保证编译器不会把这次访存优化掉或与用户的其它访问重排——用户可能正是靠 `trace` 去观察自己刚写下的变量。

### 3.4 地址合法性判断

第三章还没有页表，内核与用户程序共享同一段恒等映射的物理内存：内核位于 `0x8020_0000` 附近，各应用被加载在 `0x8040_0000 + i * 0x20000`，而**用户栈其实是内核 .bss 里的静态数组**（`loader.rs` 中的 `USER_STACK`）。因此用户的局部变量地址可能落在 `0x8020_xxxx`，而不是应用区间内。

第一版实现把合法区间写成 `[APP_BASE_ADDRESS, MEMORY_END)`，结果 `trace_read(&var)` 返回 `-1`，测试直接失败（`left: Some(111), right: None`）。改为只要落在物理内存 `[0x8000_0000, MEMORY_END)` 内即可访问，既能读到内核 .bss 中的用户栈，又能拒绝 `isize::MAX` 这类明显非法的地址。

## 四、测试结果

使用官方检查器：

```
cd ci-user && make test CHAPTER=3
```

```
[PASS] found <get_time OK! (\d+)>
[PASS] found <Test sleep OK!>
[PASS] found <current time_msec = (\d+)>
[PASS] found <time_msec = (\d+) after sleeping (\d+) ticks, delta = (\d+)ms!>
[PASS] found <Test sleep1 passed!>
[PASS] found <string from task trace test>
[PASS] found <Test trace OK!>

Test passed: 7/7
```

7 项全部通过。其中 `ch3_trace` 同时还验证了：计数包含自身、`write` 次数为 0、`yield` 次数大于 0、`exit` 次数为 0，以及通过 `trace` 写入的字节确实出现在用户变量里。

## 五、问题与思考

1. **模块划分**：最初把计数逻辑直接写在 `sys_trace` 里，会导致"统计自身"的语义变得别扭，而且 `exit`、`yield` 的计数必须靠每个系统调用自己记得调用累加函数。把累加放到分派入口后，语义统一且不会漏。
2. **`deny(warnings)`**：本模板开启了 `#![deny(warnings)]`，实验过程中出现过未使用的 `import`（`APP_BASE_ADDRESS`）导致编译失败，说明内核代码对整洁度要求较高，不需要的常量应及时清理。
3. **可移植性**：第三章的地址合法性判断依赖"无地址空间、恒等映射"这一前提，第四章引入页表后必须换成基于当前任务页表的翻译（`translated_byte_buffer`），否则会把内核地址暴露给用户，也会读错虚拟地址。这一点在第四章实验中已经重写。
