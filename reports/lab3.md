# 实验三：进程管理与 Stride 调度

## 一、基本信息

- 姓名：王可嘉
- 实验分支：ch5
- 实验日期：2026.11.21

## 二、实验目标

理解进程控制块（TCB）、进程树关系以及 `fork`/`exec`/`waitpid` 的执行机制；实现无需复制父进程地址空间的进程创建系统调用 `sys_spawn`；实现基于优先级的 Stride 调度算法与 `sys_set_priority` 系统调用，并正确处理无符号 `stride` 溢出回绕；在进程管理框架下保持 `sys_get_time`、`sys_mmap` 与 `sys_munmap` 等已有系统调用的正确性。

## 三、实验环境

- 操作系统：Windows + WSL Ubuntu
- Rust 工具链：nightly-2024-05-02
- 编译目标：riscv64gc-unknown-none-elf
- 模拟器：QEMU 8.2.x，riscv-virt 平台

## 四、实现方法

### 4.1 进程创建 `sys_spawn`

`sys_spawn` 的目标是在不拷贝父进程用户地址空间的前提下直接创建并运行一个新的子进程：

1. 通过当前进程页表将用户态传入的以 `\0` 结尾的应用名字符串翻译为内核 `String`，并调用 `get_app_data_by_name` 查找对应的 ELF 数据；若找不到则返回 `-1`。
2. 直接调用 `TaskControlBlock::new(data)` 从 ELF 创建新的进程控制块，为其分配独立的 PID、内核栈、用户地址空间、用户栈和初始 `TrapContext`，避免 `fork` 阶段的整块地址空间深拷贝。
3. 建立父子进程关系：在父进程的 `children` 列表中加入子进程的强引用 `Arc`，在子进程中记录父进程的弱引用 `Weak`（避免循环引用导致内存泄漏），并将子进程的初始 `stride` 对齐到父进程当前 `stride`。
4. 调用 `add_task` 将新子进程加入就绪队列，向父进程返回子进程 PID。

此外，在 `TaskControlBlock::exec` 替换地址空间时同步重置了 `heap_bottom` 和 `program_brk` 为新的 `user_sp`，避免 `exec` 后再调用 `sys_sbrk` 使用旧地址空间的堆边界。

### 4.2 `sys_set_priority` 与 Stride 调度算法

在 `TaskControlBlockInner` 中新增 `priority: usize`（初始值 `16`）与 `stride: usize`（初始值 `0`，`fork`/`spawn` 的子进程继承父进程当前 `stride`）：

1. **`sys_set_priority(prio: isize) -> isize`**：校验 `prio >= 2`，非法优先级（如 `0`、`1` 或负数）返回 `-1`；合法时更新当前任务的 `priority` 并返回 `prio`。
2. **大常数 `BIG_STRIDE` 的选择**：在 `os/src/config.rs` 中将 `BIG_STRIDE` 定义为 `isize::MAX as usize`（$2^{63}-1$）。这样一方面可以保证即使进程设置 `priority = isize::MAX`，单次步长 `BIG_STRIDE / priority` 仍为 `1` 而不会整除为 `0`；另一方面由于最小合法优先级为 `2`，单次最大步长满足 $\text{MaxPass} = \lfloor \text{BIG\_STRIDE} / 2 \rfloor = 2^{62}-1 < \text{isize::MAX}$。
3. **调度与溢出处理**：在 `TaskManager::fetch` 中遍历 `ready_queue`，使用无符号回绕差值转有符号比较 `(stride_a.wrapping_sub(stride_b) as isize).cmp(&0)` 选出当前 `stride` 最小的任务，出队后用 `wrapping_add` 累加步长 `BIG_STRIDE / priority`。由于新进程继承父进程的 `stride`，系统内所有活跃任务的 `stride` 极差始终满足 $\max - \min \le \text{MaxPass} < 2^{63}$，因此即便发生 64 位无符号整数回绕，有符号差值比较仍然严格保持全序关系。

### 4.3 `sys_get_time`、`sys_mmap` 与 `sys_munmap`

- **`sys_get_time`**：读取微秒级时间并拆分为 `TimeVal { sec, usec }`，序列化为字节数组后调用 `mm::copy_to_user`。`copy_to_user` 先逐页校验目标用户地址范围的映射有效性以及 `U | W` 权限，全部合法后再逐页拷贝，保证跨页写入安全且不会发生部分写入。
- **`sys_mmap` 与 `sys_munmap`**：在系统调用层校验 `start` 页对齐、`len > 0`、`start + len` 不溢出、区间落在 Sv39 用户低半区 `[0, 2^38)` 以及 `prot` 仅包含低三位且非零，然后分别调用当前进程 `MemorySet` 的 `map_user_area` 与 `unmap_framed_area`。两阶段先验证后修改的策略确保重叠映射或包含未映射页的解除请求会直接返回 `-1`，部分解除映射时会自动拆分并保留剩余区域的 `MapArea`。

## 五、问题分析

### 5.1 为什么 `spawn` 不等价于 `fork + exec`？

1. **性能与内存开销**：`fork` 会先完整复制父进程的整个用户地址空间（所有数据页、栈页、页表），紧接着 `exec` 又会立即丢弃这份副本并重新从 ELF 加载新地址空间。`spawn` 直接由目标 ELF 构造子进程地址空间，省去了无意义的页拷贝与释放。
2. **语义与状态继承**：`fork` 会继承父进程的堆、mmap 区域和寄存器上下文，而 `spawn` 直接以全新进程的方式初始化地址空间与 `TrapContext`，只需与父进程建立父子关系。

### 5.2 Stride 调度的溢出问题与比较条件

当 `stride` 以无符号整数 `usize`（64 位）累加并发生回绕时，直接用无符号 `<` 比较大小会失效（例如已回绕到较小值的进程会被误判为落后者）。将比较改写为 `(a.wrapping_sub(b) as isize) < 0` 后，只要系统中任意两个就绪进程的真实步长差满足：

$$\text{STRIDE}_{\max} - \text{STRIDE}_{\min} \le \text{Pass}_{\max} \le \text{isize::MAX}$$

那么模 $2^{64}$ 下的有符号差值与真实差值的符号完全一致。本实现取 `BIG_STRIDE = isize::MAX as usize` 且规定 `priority >= 2`，使得 $\text{Pass}_{\max} = \lfloor \text{BIG\_STRIDE} / 2 \rfloor < 2^{63}$，同时让 `fork`/`spawn` 的子进程继承父进程 `stride`，从而在初始阶段与后续回绕过程中均严格保持该不变式。

## 六、测试与结果

在仓库根目录执行官方第五章检查器：

```bash
make -C ci-user test CHAPTER=5
```

测试输出 `Test passed: 15/15`，所有测例（包括 `ch3_sleep`、`ch3_sleep1`、`ch4_mmap0~3`、`ch4_unmap`、`ch4_unmap2`、`ch5_spawn0`、`ch5_spawn1`、`ch5_setprio` 和 `ch5_stride`）均通过，且 `lab1` 至 `lab3` 实验报告检查全部通过。

其中 `ch5_stride` 运行 6 个不同优先级进程的实测统计如下：

| 进程优先级 (`priority`) | 计数 (`exitcode`) | 比值 (`ratio = count / priority`) |
| ---: | ---: | ---: |
| 5 | 22,172,800 | 4,434,560 |
| 6 | 28,003,200 | 4,667,200 |
| 7 | 32,356,000 | 4,622,285 |
| 8 | 37,852,000 | 4,731,500 |
| 9 | 41,821,200 | 4,646,800 |
| 10 | 47,046,800 | 4,704,680 |

各进程的 `count / priority` 集中在 `4.43e6 ~ 4.73e6` 之间，获得的 CPU 时间片与优先级基本成正比，符合 Stride 调度预期。

## 七、实验收获

1. 理解了进程创建中 `fork + exec` 与 `spawn` 在地址空间管理上的本质差异：直接从 ELF 构造新进程可以避免冗余的地址空间深拷贝，同时也更清楚地看到父子关系（`Arc` / `Weak`）、PID 与内核栈回收是如何协同工作的。
2. 深入理解了 Stride 调度算法的工程细节：`BIG_STRIDE` 过小会导致高优先级（如 `isize::MAX`）步长下溢为 `0`，而 `BIG_STRIDE` 设大后必须同步配合有符号差值比较与新进程 `stride` 继承，才能在整数回绕时仍保证调度公平性。
3. 在多章节演进的内核中，需要同步维护跨模块的一致性（如 `exec` 重置堆边界、在新的 `TaskControlBlock` 结构下复用 `MemorySet` 映射接口），并结合本地官方 checker 完整回归前期章节的系统调用。
