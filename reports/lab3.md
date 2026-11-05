# 实验报告 Lab 3：进程创建 `spawn` 与 stride 调度（ch5）

> 姓名：qz999　　学号：（请填写）
> 课程：2026 秋冬季训练营 · 专业阶段 rCore-Tutorial　　仓库：LearningOS/2026a-rcore-qz999-66
> 荣誉准则：本报告与实验代码均为本人独立完成，未抄袭他人代码或报告；参考的资料已在正文中注明。

## 一、本章任务概述

第五章引入了真正的进程管理：`fork`、`exec`、`waitpid`，每个进程拥有独立的 TCB、地址空间与内核栈。本章的编程作业有两项：

1. 实现 `sys_spawn`：等价于 fork + exec，直接创建并运行目标程序的新进程；
2. 实现基于优先级的 stride 调度算法，并新增 `sys_set_priority`。

## 二、`spawn` 的实现

### 2.1 为什么不能简单组合 fork + exec

从用户态看，`fork` 是在**用户态**返回两次的系统调用：父进程拿到子进程 pid，子进程则在 `fork` 返回处继续执行，随后调用 `exec` 覆盖自身。但 `spawn` 是在内核中一次性完成的，内核里根本没有"子进程从 `fork` 返回后调用 `exec`"这条执行流。

如果在内核里直接先 `fork` 再 `exec`，还需要手工伪造子进程的返回地址与栈、让它进入用户态后自己去执行 `exec`，既不自然也很容易出错（题目 tips 也指出了这一点）。更关键的是，`fork` 会完整拷贝父进程的地址空间，而 `spawn` 根本用不到这份拷贝——子进程马上要用 `exec` 换掉整个地址空间。

### 2.2 实现

因此我实现了 `TaskControlBlock::spawn(&self, elf_data)`：直接以目标 ELF 构造全新的地址空间，不拷贝父进程的任何用户内存。

```rust
pub fn spawn(self: &Arc<Self>, elf_data: &[u8]) -> Arc<Self> {
    // 直接以 elf 构造新的 MemorySet（含 trap context 与用户栈）
    let (memory_set, user_sp, entry_point) = MemorySet::from_elf(elf_data);
    let trap_cx_ppn = memory_set.translate(TRAP_CONTEXT_BASE).unwrap().ppn();
    let pid_handle = pid_alloc();
    let kernel_stack = kstack_alloc();
    ...
    // 父进程记录子进程，子进程记录父进程（Weak）
    parent_inner.children.push(task_control_block.clone());
    // 用 entry_point / user_sp 初始化子进程的 TrapContext
    *trap_cx = TrapContext::app_init_context(entry_point, user_sp, KERNEL_SPACE.token(),
                                             kernel_stack_top, trap_handler as usize);
    task_control_block
}
```

系统调用层：

```rust
pub fn sys_spawn(path: *const u8) -> isize {
    let path = translated_str(current_user_token(), path);
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        let new_task = current_task().unwrap().spawn(data);
        let new_pid = new_task.pid.0;
        new_task.inner_exclusive_access().get_trap_cx().x[10] = 0; // 子进程返回 0
        add_task(new_task);
        new_pid as isize
    } else { -1 }
}
```

`x[10]`（即 `a0`）是系统调用返回值寄存器：子进程的 TrapContext 里把它置 0，这样它第一次进入用户态时 `spawn` 的返回值就是 0；父进程则返回子进程 pid。文件名不存在时返回 `-1`。

需要注意的几点：

- **零内存拷贝**：只读 ELF 数据、按段映射新页，与 `fork` 的 `from_existed_user` 完全无关；
- **父子关系**：子进程的 `parent` 用 `Weak` 指向父进程，避免循环引用；父进程的 `children` 持有强引用；
- **优先级继承**：子进程继承父进程的 `priority` 与 `pass`，`stride` 从 0 开始。

## 三、stride 调度算法

### 3.1 数据结构

`TaskControlBlockInner` 新增三个字段：

| 字段 | 含义 |
| --- | --- |
| `priority` | 进程优先级，初始为 16 |
| `stride` | 已经"运行"的累积量，初始为 0 |
| `pass` | 每次被调度后 stride 的增量，`BIG_STRIDE / priority` |

`BIG_STRIDE` 取 `1 << 32`（题目提示 65536 也可以，取大一些能减小整数除法的误差；由于测例只跑几秒、`stride` 是 64 位，不担心溢出）。

### 3.2 调度器

原来的就绪队列是 `VecDeque` + FIFO。改成 stride 后不再需要队列语义，只需每次挑出 `stride` 最小的进程，并把它的 `stride` 加上自己的 `pass`：

```rust
pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
    let mut chosen = None;
    let mut min_stride = usize::MAX;
    for (index, task) in self.ready_queue.iter().enumerate() {
        let stride = task.inner_exclusive_access().stride;
        if stride < min_stride { min_stride = stride; chosen = Some(index); }
    }
    let index = chosen?;
    let task = self.ready_queue.remove(index);
    { let mut inner = task.inner_exclusive_access(); inner.stride += inner.pass; }
    Some(task)
}
```

就绪进程数量很小（十几个），暴力扫描完全够用，也避免引入优先级队列带来的复杂度。

### 3.3 `sys_set_priority`

```rust
pub fn sys_set_priority(prio: isize) -> isize {
    if prio < 2 { return -1; }          // 优先级必须 >= 2
    if task.set_priority(prio as usize) { prio } else { -1 }
}
```

`set_priority` 同时更新 `pass = max(BIG_STRIDE / priority, 1)`。`max(..., 1)` 是为了处理 `set_priority(isize::MAX)` 这种极端情况：`BIG_STRIDE / isize::MAX` 会截断为 0，若 `pass` 为 0，该进程的 `stride` 将永远不增长，从而永久占据 CPU。

## 四、测试结果

```
cd ci-user && make test CHAPTER=5
```

```
[PASS] found <Test spawn0 OK!>
[PASS] found <Test wait OK!>
[PASS] found <Test waitpid OK!>
[PASS] found <Test set_priority OK!>
[PASS] found <Test 04_1 OK!>  ... (ch2/ch3/ch4 的全部测例)
[PASS] not found <FAIL: T.T>
[PASS] not found <Test sbrk failed!>

Test passed: 15/15
```

`ch5_spawn0` 连续创建 40 个子进程并逐一回收，验证了 `spawn` 不会拷贝父进程地址空间、也不会泄漏；`ch5_spawn1` 验证了通过 `waitpid` 能取到子进程的真实退出码（66778 / -233）。

stride 的公平性由 `ch5_stride` 检验，六个子进程的优先级分别是 5~10，运行 4 秒后各自的计数除以优先级如下：

| priority | exitcode | exitcode / priority |
| --- | --- | --- |
| 5 | 14350000 | 2870000 |
| 6 | 17219200 | 2869866 |
| 7 | 19831200 | 2833028 |
| 8 | 22352000 | 2794000 |
| 9 | 25967200 | 2885244 |
| 10 | 28830400 | 2883040 |

`max(ratio) / min(ratio) ≈ 2885244 / 2794000 ≈ 1.03`，远小于通过标准 1.5，说明 CPU 时间确实与优先级成正比。

## 五、问答作业：stride 算法深入

### 1. 8 位存储时 `p1.stride = 255, p2.stride = 250`，p2 执行一个时间片后轮到谁？

p2 执行后 `p2.stride = 250 + 10 = 260`，8 位无符号数溢出后变成 `260 mod 256 = 4`。此时 4 < 255，于是调度器认为 p2 的 stride 更小，**下一次仍然调度 p2**。这就是"只看数值大小"在溢出时会出错的原因：真实大小关系被回绕破坏。

### 2. 为什么优先级都 >= 2 时，`STRIDE_MAX - STRIDE_MIN <= BIG_STRIDE / 2`？

每个进程的 `pass = BigStride / priority <= BigStride / 2`（因为 priority >= 2）。设当前最大 stride 为 S_max、最小为 S_min，考虑最近一次对 S_max 所属进程的调度：在它被调度前，它的 stride 一定不大于当时的最小值，而它一次只增加 `pass <= BigStride/2`，所以调度后它至多比当时的最小值大 `BigStride/2`。其它进程只会增加自己的 stride，不会使这个差值变大。因此在任意时刻，最大值与最小值之差不超过 `BigStride/2`，也就是**小于 stride 类型量程的一半**。这样即使发生回绕，两个值的"真实"距离仍然小于半个量程，可以用有符号差值正确判断大小。

### 3. 溢出情况下的比较函数

```c
typedef unsigned long long Stride_t;
const Stride_t BIG_STRIDE = 0xffffffffffffffffULL;

int cmp(Stride_t a, Stride_t b) {
    if (a == b) return 0;
    // 回绕后两者真实差值仍小于 BIG_STRIDE / 2，
    // 因此用无符号差值判断"谁在前面"
    return (a - b) < (BIG_STRIDE / 2) ? 1 : -1;
}
```

`a - b` 是模 2⁶⁴ 的无符号差：若 `a` 只比 `b` 大一点（正常情况），差值是个小正数 → 判定 `a > b`；若 `a` 已经回绕而 `b` 还没回绕（`a` 的真实值其实更大），则 `a - b` 接近 2⁶⁴，大于 `BIG_STRIDE/2` → 判定 `a < b`。

用题目里的 8 位例子（`BIG_STRIDE = 255`，半个量程约 127）验证：

- `cmp(125, 255)`：`(125 - 255) mod 256 = 126`，`126 < 127` → 返回 1，即 `a > b`，符合题目给出的期望；
- `cmp(129, 255)`：`(129 - 255) mod 256 = 130`，`130 > 127` → 返回 -1，同样符合期望。

可见"真实差值的绝对值小于半个量程"这一前提，正是 `priority >= 2` 所保证的。

## 六、问题与思考

1. **不要在内核里伪造 fork 的执行流**：直接实现"以 ELF 构造进程"比"fork 后再想办法让子进程 exec"清晰得多，也是本题刻意引导的设计。
2. **`RefCell` 嵌套借用**：`TaskManager::fetch` 中先借出 `stride` 再移除元素，否则会和 `UPSafeCell` 的 `borrow_mut` 冲突而 panic。这类"借用即持有"的约束在同步原语里尤其容易踩。
3. **整数除法的边界**：`BIG_STRIDE / priority` 在极端优先级下会退化为 0，必须夹逼到 1，这说明实现调度算法时"参数合法范围"与"数值安全"要分开考虑。
