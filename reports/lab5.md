# 实验报告 Lab 5：死锁检测（ch8）

> 姓名：qz999　　学号：（请填写）
> 课程：2026 秋冬季训练营 · 专业阶段 rCore-Tutorial　　仓库：LearningOS/2026a-rcore-qz999-66
> 荣誉准则：本报告与实验代码均为本人独立完成，未抄袭他人代码或报告；参考的资料已在正文中注明。

## 一、本章任务概述

第八章把"进程"进一步拆成"进程 + 线程"：`ProcessControlBlock` 管理地址空间、文件描述符表、子进程与各类同步资源，`TaskControlBlock` 管理每个线程的上下文与内核栈；内核提供 `mutex`、`semaphore`、`condvar` 三种同步原语。

本章的编程作业是**死锁检测**：当线程请求一个暂时无法获得的阻塞型资源时，内核要判断"等待是否会导致永久阻塞"，如果是，就立即返回 `-0xdead`，而不是让线程永远睡眠下去。

对应的系统调用是：

```rust
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize
```

同时还需要把前几章的实验（`spawn`、stride 调度、`mmap`、`fstat`/`linkat`/`unlinkat`）移植到本章的进程/线程模型上，保持前向兼容。

## 二、死锁检测的设计

### 2.1 等待图

死锁的本质是**等待环**：线程 A 等 B 占着资源，B 等 C，C 又在等 A。因此内核维护一张"等待图"：

- 每条边表示"某个线程正在等待某个资源"；
- 每个资源记录"哪些线程持有它"；
- 线程 T 请求资源 R 时，检查从 R 的持有者出发、顺着等待边能否走回 T。

如果走回来，说明 T 一旦阻塞就形成环，于是直接返回 `-0xdead`。

### 2.2 关键细节：不是"有环"就是死锁

最初我的实现是"从资源持有者出发，顺着等待边能否走回请求者"的简单环检测，两个测例一次就暴露了它的两个方向上的错误：

- **漏报**（`ch8_deadlock_sem1` 卡死）：请求者本身此刻还是可运行的，它是"即将阻塞"而不是"已经阻塞"。检测时若只统计 `waiting` 表里已有的线程，那么**最后一个闭合环的线程永远看不到自己**，于是三个线程依次睡死，谁也没有报死锁。
- **误报**（`ch8_deadlock_sem2` 报出死锁）：资源图上确实存在环，但环上某些持有者其实只是暂时阻塞在别处（例如阻塞在一个稍后会被主线程 `up` 的信号量上），它们迟早会释放资源。**环只是必要条件，不是充分条件。**

正确的判据是"这个线程是否永远无法推进"，它是一个**最大不动点**：

> 线程 T 是"卡死"的，当且仅当 T 阻塞在资源 R 上，且 R 的所有持有者都是"卡死"的。

于是算法变成：

1. 初始把所有阻塞中的线程都当作候选；
2. 反复扫描：若某个候选所等待的资源没有任何持有者，或它的持有者中存在**不在候选集里**的线程，就把该候选剔除；
3. 直到集合不再变化，剩下的就是"永远无法推进"的线程集合；
4. 请求 T→R 会造成死锁，当且仅当 R 的所有持有者都在这个集合里（此时把 T 也当作已阻塞）。

`stuck_tasks` 就是这个不动点迭代：

```rust
fn stuck_tasks(&self) -> Vec<usize> {
    let mut stuck: Vec<usize> = self.waiting.keys().copied().collect();
    loop {
        let mut changed = false;
        for tid in stuck.clone() {
            let rid = self.waiting.get(&tid);
            // 没有任何持有者 -> 不可能真的在等它 -> 剔除
            // 有持有者不卡死 -> 资源迟早被释放 -> 剔除
            ...
        }
        if !changed { break; }
    }
    stuck
}
```

用这个判据回看两个测例：

- `sem1`：t1 等 sem1（持有者 t2）、t2 等 sem3（持有者 t3）、t3 等 sem2（持有者 t1、t2）——三者互相支撑，不动点就是它们本身，任何一个闭合环的请求都会被判死锁；
- `sem2`：t1 等 sem1（持有者 t2、t3），t2 阻塞在 sem0 上，而 sem0 的持有者是正在 `sleep` 的主线程，主线程不在等待图里（它由定时器唤醒），于是 t2 被剔除，进而 t1 不被判死锁。

主线程 `sleep` 这一细节很关键：定时器阻塞**不进入等待图**，所以它天然被视为"迟早会继续运行"，这是正确的——内核一定会唤醒它。

### 2.3 请求者的虚拟等待边

检测时请求者尚未阻塞，`waiting` 里没有它的记录。为了让它"参与"环，`check_deadlock` 会先临时插入一条 `tid -> rid` 的等待边，检测结束后再恢复：

```rust
let previous = detector.waiting.insert(tid, rid);
let holders = detector.holders.get(&rid).cloned().unwrap_or_default();
let deadlock = if holders.is_empty() { false } else {
    let stuck = detector.stuck_tasks();
    holders.iter().all(|holder| stuck.contains(holder))
};
match previous {
    Some(old_rid) => { detector.waiting.insert(tid, old_rid); }
    None => { detector.waiting.remove(&tid); }
}
```

这条"虚拟边"正是修复 `sem1` 漏报的关键：**最后闭合环的线程必须把自己算进去**，否则环永远差一条边。

### 2.4 与同步原语的结合

`Mutex` trait 的 `lock` 与 `Semaphore::down` 改为返回 `isize`：

```rust
fn lock(&self) -> isize { ... }        // 0 或 -0xdead
pub fn down(&self) -> isize { ... }    // 0 或 -0xdead
```

在真正阻塞之前做三件事：

```rust
if deadlock::check_deadlock(tid, rid) {
    return deadlock::DEADLOCK_RET;
}
deadlock::set_waiting(tid, rid);      // 记录等待边
block_current_and_run_next();
deadlock::clear_waiting(tid);         // 被唤醒后清除
```

资源的身份用**对象自身地址**（`self as *const _ as usize`）表示：每个互斥量/信号量在进程的资源表里都是唯一的 `Arc`，地址天然唯一，也不需要额外分配 id。

持有关系在 `lock`/`down` 成功时登记，在 `unlock`/`up` 时注销。信号量可以被"非持有者"释放（例如信号量常被另一个线程 up），因此注销时优先移除"本人持有"的那一份；若本人并未持有，则优先移除一个**仍可运行**的持有者——它本来就不会长期占用资源。

### 2.5 线程 id

等待图以线程为单位，因此需要拿到当前线程的 tid。`TaskControlBlockInner.res` 里已经有 `TaskUserRes.tid`，新增一个辅助函数：

```rust
pub fn current_tid() -> usize {
    current_task().unwrap().inner_exclusive_access().res.as_ref().unwrap().tid
}
```

## 三、前向兼容：把前几章的实验移植过来

ch8 的代码结构（进程/线程分离）与前几章差别较大，移植时需要按新结构改写：

| 功能 | ch8 中的落点 |
| --- | --- |
| `spawn` | `ProcessControlBlock::new(elf_data)` 直接构造进程（内部会创建主线程并加入调度器），再把子进程挂到父进程的 `children` 上 |
| stride 调度 | `TaskControlBlockInner` 增加 `priority/stride/pass`，`TaskManager` 改为每次取 stride 最小者 |
| `mmap`/`munmap` | 在 `ProcessControlBlockInner.memory_set` 上操作 |
| `get_time` | 通过 `translated_byte_buffer_checked` 按页写回 |
| `fstat`/`linkat`/`unlinkat` | 复用第六章的 easy-fs 扩展（`nlink`、`dealloc_inode`、`link`、`unlink`），内核侧用 `File::as_any` 取到 `OSInode` |

`spawn` 的关键仍然是**不做任何内存拷贝**：直接以目标 ELF 建立新的地址空间；`ProcessControlBlock::new` 已经把主线程放进了调度器的就绪队列，只需把该线程 `TrapContext` 的 `a0` 置 0（让子进程从 `spawn` 返回 0），并建立父子关系。

## 四、测试结果

```
cd ci-user && make test CHAPTER=8
```

```
[PASS] found <deadlock test mutex 1 OK!>
[PASS] found <deadlock test semaphore 1 OK!>
[PASS] found <deadlock test semaphore 2 OK!>
[PASS] found <threads test passed!>
[PASS] found <threads with arg test passed!>
[PASS] found <mpsc_sem passed!>
[PASS] found <philosopher dining problem with mutex test passed!>
[PASS] found <race adder using spin mutex test passed!>
[PASS] found <sync_sem passed!>
[PASS] found <test_condvar passed!>
[PASS] found <pipetest passed!>
... （前六章全部测例）
Test passed: 42/42
```

三个死锁测例分别验证了：

- `ch8_deadlock_mutex1`：同一线程重复加锁 → 必须报死锁；
- `ch8_deadlock_sem1`：三个线程各占一种资源并环形请求 → 必须检测到死锁，且被"判死"的线程释放资源后其余线程能继续完成；
- `ch8_deadlock_sem2`：看起来像环、但存在可运行的持有者 → **不能**误报。

## 五、问题与思考

1. **"看起来像环"不等于死锁**：资源图上的环只是必要条件，还要求环上的线程全部处于阻塞状态。这是本实验的核心陷阱，也是银行家算法（安全性检查）思想的简化版。
2. **判定时机**：检测放在"即将阻塞"的那一刻，此时请求者的等待边还没有真正建立，所以搜索的目标是"能否回到自己"。若放在阻塞之后再做，就变成"检测已经发生的死锁"，语义不同（那时只能选择杀掉线程）。
3. **资源身份的表达**：用对象地址当 id 简单可靠，但前提是资源在等待图存续期间不会被释放或复用地址。内核中互斥量/信号量都由 `Arc` 持有，只要进程还活着就不会被复用，这一点是有保证的。
4. **返回值的约定**：`-0xdead` 是一个"约定"而非错误码表的一部分，用户程序据此区分"资源暂时不可用（阻塞等待）"与"永远不可用（死锁）"，这样死锁的处理策略就交给了用户态。
5. **移植成本**：从 ch7 到 ch8 不是简单的代码搬运——进程/线程分离后，`memory_set`、`fd_table`、`children` 都搬到了 `ProcessControlBlock`，凡是用到这些字段的 syscall 都要改写。这也说明了"内核数据结构的设计"对后续章节的影响远大于单个功能实现。
