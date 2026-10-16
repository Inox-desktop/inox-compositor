# AGENTS.md — inox-compositor

本仓库遵循根级 [AGENTS.md](https://github.com/Inox-desktop/inox/blob/main/AGENTS.md) 的**全部硬性规则**，本文件只补充仓库特有内容，不放宽任何根级约束。

## 职责

基于 Smithay 的 Wayland 合成器。所属层级：**L2**。M1 广告 `wl_output`、`wl_compositor` 与私有 `inox_ipc` 全局（后者来自 `inox-protocols`）。

## 仓库特有约束

- **`unsafe` 例外（根级 §3.2）**：本 crate 的 `Cargo.toml` 使用 `[lints.rust] unsafe_code = "deny"`（而非 `forbid`），并只允许两处 `#[allow(unsafe_code)]`：
  - `src/render.rs`：`EGLDisplay::new` / `GlesRenderer::new` 的 EGL/GL 胶水；
  - `src/runtime.rs`：从 calloop `Generic` 事件源取得 `Display` 以派发客户端请求（`NoIoDrop::get_mut`）。

  两处都必须是命名函数、包裹在安全 API 内，且每个 `unsafe` 调用附 `// SAFETY:` 理由。**新增第三处 `allow` 必须先在元仓库提交 ADR**（见 ADR 0005）。
- 非测试路径禁止 `unwrap()` / `expect()` / `panic!()` / `todo!()` / `unimplemented!()`。
- 日志统一使用 `tracing`，禁止 `println!` / `eprintln!`；面向 CLI 的输出（如 `--help`）用显式 stdout 写入。
- MSRV：本仓库 `rust-version = "1.86"`（`smithay` 0.7 依赖的 `calloop` 0.14 要求 rustc ≥ 1.86）。其余 L1 仓库仍为 1.85，升级需按根级 §8 说明破坏性影响。
- 跨仓库依赖：`inox-common` 与 `inox-protocols` 均以锁定 `tag` 的 git 依赖引入，并同步登记 `deny.toml` 的 `[sources] allow-git`；禁止未锁定的 `branch = "main"`（根级 §8）。**git 依赖必须同时声明 `version`**，否则其隐式 `*` 版本要求会被 `deny.toml` 的 `[bans] wildcards = "deny"` 判为通配符依赖而失败。`inox-protocols` 是 Wayland 协议（核心 `wl_*` 绑定与私有 `inox-ipc`）的唯一来源。

## 主要依赖

`smithay`（`default-features = false`，仅 `desktop` + `wayland_frontend` + `renderer_gl`，**不启用**任何后端特性）、`calloop`（`signals`）、`anyhow`、`thiserror`、`tracing`、`inox-common` 与 `inox-protocols`（均为 git 依赖，锁定 `tag`）。

**测试依赖**（不进入发布产物）：`proptest`（平铺布局与窗口簿记的算法验证）、`wayland-client` / `wayland-protocols`（集成测试驱动真实客户端）。

## 命令

> 仅作参考，不作为本地验收依据；真正判定在 CI。

| 目的 | 命令 |
|------|------|
| 格式化 | `cargo fmt --all` |
| 格式检查 | `cargo fmt --all -- --check` |
| 静态检查 | `cargo clippy --all-targets --all-features -- -D warnings` |
| 测试 | `cargo test --all-features` |
| 构建 | `cargo build --release --all-features` |
| 文档 | `cargo doc --no-deps --all-features` |
| 依赖检查 | `cargo deny check` |

## 测试要求

- 纯逻辑必须有单元测试（`#[cfg(test)]`）；测试代码不受「禁止 unwrap/expect/panic」约束。
- **核心算法用 `proptest`**：`layout`（master/stack 平铺几何）与 `workspace`（窗口顺序/焦点）的属性测试必须覆盖「每窗口一块、块不越界、块互不重叠、master 列在 stack 左侧」等不变量。
- **headless 集成测试**（`tests/`）必须可在无显示设备、无 DRM 设备的环境中通过：CI 使用软件渲染（`LIBGL_ALWAYS_SOFTWARE=1`）。
- 「渲染确实发生」必须由**离屏像素读回**断言，不能只依赖日志；颜色比较需留 ±1 个 8 位步长的容差（不同驱动的 float→unorm 舍入不同）。
- 窗口/输入集成测试（`tests/window_management.rs`）在**同一进程内**用 `UnixStream::pair()` 连接真实 `wayland-client`，创建 ≥2 个 xdg toplevel，断言合成器可观测状态（托管窗口数、`Space` 中的平铺落点、焦点），**不得 mock 被测核心逻辑**。
- 键盘解析依赖 XKB rules 文件（`xkb-data`），CI 必须安装该系统包。
- 一个测试进程只能持有一个 EGL display，**每个测试二进制中只保留一个创建 EGL 的测试**。
- 进程级测试必须自备私有 `XDG_RUNTIME_DIR`（Wayland 套接字绑定需要绝对路径的运行时目录），并给子进程设定超时，避免 CI 挂死。

## 目录结构

```
src/
  lib.rs          库入口与模块说明
  main.rs         可执行入口（CLI、日志初始化）
  config.rs       命令行解析与配置数据（不依赖 Smithay）
  input.rs        按键 → 合成器动作的纯逻辑（Alt+Tab / Super+Q 等）
  layout.rs       master/stack 平铺几何（纯逻辑 + proptest）
  workspace.rs    窗口簿记与布局顺序（纯逻辑 + proptest）
  runtime.rs      calloop 事件循环：套接字、信号、帧定时器
  render.rs       EGL surfaceless + GLES 离屏渲染（unsafe 例外所在）
  state.rs        Smithay 合成器状态、seat/xdg-shell/shm handler、窗口管理与输入注入
tests/
  headless_render.rs     创建输出并渲染一帧（进程内）
  ipc_global.rs          进程级：广告 inox_ipc 全局并应答 get_version
  signal_shutdown.rs     进程级：终止信号与帧预算都须以退出码 0 结束
  window_management.rs   真实客户端创建 ≥2 窗口，断言平铺与输入（进程内）
.github/        CI 工作流
deny.toml       cargo-deny 依赖与许可证策略
```

## 提交信息格式

Conventional Commits，scope 使用 `compositor`。
