# 实验五：并发与同步（死锁检测）

## 一、基本信息

- 姓名：王可嘉
- 实验分支：ch8
- 实验日期：2026.11.22

## 二、实验目标

理解进程与线程分离模型（`ProcessControlBlock` 与 `TaskControlBlock`）以及内核中互斥锁、信号量、条件变量的实现机制；基于银行家算法实现进程级死锁检测系统调用 `sys_enable_deadlock_detect` 及资源分配状态跟踪，在发生循环等待死锁时及时拒绝请求并返回 `-0xdead`，在存在安全序列时不发生误报；同时移植 `sys_get_time` 以支持用户态定时等待。

## 三、实验环境

- 操作系统：Windows + WSL Ubuntu
- Rust 工具链：nightly-2024-05-02
- 编译目标：riscv64gc-unknown-none-elf
- 模拟器：QEMU 8.2.x，riscv-virt 平台 + VirtIO 块设备

## 四、实现方法

### 4.1 死锁检测器设计（`os/src/sync/deadlock.rs`）

在每个进程的 `ProcessControlBlockInner` 中新增 `DeadlockDetector`，统一追踪该进程内所有线程对 **互斥锁（Mutex）** 与 **信号量（Semaphore）** 两类资源的持有与请求状态：

- **数据结构**：
  - `enabled: bool`：是否开启死锁检测（默认 `false`）。
  - `mutex_available` / `sem_available`：各资源当前的可用数量（互斥锁创建时初始为 `1`，信号量创建时初始为 `res_count`）。
  - `mutex_allocation[tid][id]` / `sem_allocation[tid][id]`：线程 `tid` 当前已持有的资源数量。
  - `mutex_need[tid][id]` / `sem_need[tid][id]`：线程 `tid` 当前正在请求的资源数量。
- **银行家安全性算法 `detect(&self) -> bool`**：
  1. 复制 `work_mutex = mutex_available`、`work_sem = sem_available`，初始化 `finish = [false; thread_num]`。
  2. 循环寻找满足 `!finish[tid]` 且对所有互斥锁 `mutex_need[tid][m] <= work_mutex[m]`、对所有信号量 `sem_need[tid][s] <= work_sem[s]` 的线程 `tid`。
  3. 找到后将 `finish[tid]` 置为 `true`，并把该线程持有的 `mutex_allocation[tid]` 与 `sem_allocation[tid]` 累加回 `work_mutex`、`work_sem`。
  4. 当不再有线程可被满足时退出循环；若仍存在 `!finish[tid]`，说明系统无安全序列、已发生死锁，返回 `true`，否则返回 `false`。

### 4.2 同步原语状态同步与系统调用接入（`os/src/syscall/sync.rs`）

1. **`sys_enable_deadlock_detect(enabled: usize)`**：参数为 `0` 关闭检测、`1` 开启检测均返回 `0`，其他非法值返回 `-1`。
2. **资源创建**：`sys_mutex_create` 与 `sys_semaphore_create` 在分配资源 ID 后调用 `add_mutex(id)` / `add_sem(id, res_count)` 初始化可用量并扩展各线程行。
3. **资源申请（`sys_mutex_lock` / `sys_semaphore_down`）**：
   - 在调用底层 `mutex.lock()` / `sem.down()` 前，先调用 `try_request_mutex` / `try_request_sem` 将当前线程对应的 `need` 加 1；若开启了死锁检测且 `detect()` 返回 `true`，立刻回滚 `need` 并直接返回 `-0xdead`（不进入阻塞）。
   - 若安全，释放 `process_inner` 锁后执行底层阻塞等待；在成功返回获得资源后，再调用 `on_mutex_acquired` / `on_sem_acquired` 将 `need` 减 1、`allocation` 加 1、`available` 减 1。
4. **资源释放（`sys_mutex_unlock` / `sys_semaphore_up`）**：调用 `on_mutex_released` / `on_sem_released` 更新 `allocation`（使用 `saturating_sub(1)` 兼容生产者-消费者场景下非持有线程调用 `up`）与 `available`（加 1）。
5. **条件变量配合（`sys_condvar_wait`）**：`Condvar::wait` 内部先释放互斥锁再睡眠、唤醒后重新加锁，因此在 `condvar.wait` 前调用 `on_mutex_released`，返回后调用 `on_mutex_reacquired`，确保互斥锁持有表始终准确。

### 4.3 `sys_get_time` 移植

在 `os/src/mm/page_table.rs` 中添加跨页安全拷贝函数 `copy_to_user` 与泛型封装 `copy_to_user_val<T: Copy>`，在 `os/src/syscall/process.rs` 中实现 `sys_get_time`，保证 `ch8_deadlock_sem1` 与 `ch8_deadlock_sem2` 中的 `sleep()` 能够准确计时。

## 五、问题分析

1. **为什么阻塞等待期间必须把请求留在 `need` 中、拿到资源后才转入 `allocation`？**
   当线程 A 因资源不足阻塞在 `sem.down()` 或 `mutex.lock()` 时，它尚未持有该资源，而是正在等待该资源。此时必须保持 `need[A][r] == 1`、`allocation[A][r] == 0`，这样后续线程 B 再请求资源触发银行家算法时，才能准确识别出 A 与 B 之间的等待依赖链；待 A 真正被唤醒并返回后，再把 `need` 转为 `allocation` 并扣减 `available`。
2. **为什么信号量释放时要对 `allocation` 做饱和减法（`saturating_sub`）？**
   在同步型信号量（如 `ch8b_sync_sem`、`ch8b_mpsc_sem`）中，初始计数常为 `0`，由线程 B 调用 `up()` 通知等待在 `down()` 上的线程 A，此时线程 B 从未 `down()` 过该信号量，其 `allocation` 为 `0`。使用 `saturating_sub(1)` 既保证了资源型信号量的准确回收，也避免同步型信号量下溢 panic。

## 六、测试与结果

在仓库根目录执行官方第八章评测：

```bash
make -C ci-user test CHAPTER=8
```

全部 25 项检查通过（`Test passed: 25/25`），包括多线程基础测试（`ch8b_threads`、`ch8b_threads_arg`）、互斥锁/信号量/条件变量测试（`ch8b_race_adder_mutex_spin`、`ch8b_phil_din_mutex`、`ch8b_sync_sem`、`ch8b_mpsc_sem`、`ch8b_test_condvar`）、管道测试（`ch7b_pipetest`），以及全部 3 个死锁检测测例：

- `ch8_deadlock_mutex1`：单线程重复加锁检测到死锁并返回 `-0xdead`，输出 `deadlock test mutex 1 OK!`。
- `ch8_deadlock_sem1`：多线程循环等待信号量检测到死锁，输出 `deadlock test semaphore 1 OK!`。
- `ch8_deadlock_sem2`：存在安全序列的多线程信号量分配未误报死锁，所有子线程返回 `0`，输出 `deadlock test semaphore 2 OK!`。

`lab1` 至 `lab5` 五份实验报告检查全部通过。

## 七、实验收获

1. 理解了死锁检测的核心在于准确刻画“已分配（Allocation）”“当前等待（Need）”与“剩余可用（Available）”三者的状态转换，尤其是阻塞前后、唤醒后以及 `Condvar::wait` 释放/重获锁时的时序一致性。
2. 将互斥锁与信号量纳入同一轮银行家算法联合判定，不仅能处理单类资源死锁，也天然支持互斥锁与信号量交叉等待形成的死锁。
3. 体会到操作系统同步原语中“资源型信号量”与“事件同步型信号量”的使用差异，编写内核通用检测逻辑时需要兼顾两类用法的边界条件。
