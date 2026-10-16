# inox-compositor

基于 Smithay 的 Wayland 合成器。属于 Inox 桌面环境的 **L2** 层。

## 状态

M1：headless 合成器已实现——创建 `wl_output`、以 EGL surfaceless（`EGL_MESA_platform_surfaceless`）+ GLES 渲染纯色帧、收到终止信号或渲染完指定帧数后干净退出（退出码 0）。渲染路径与 `unsafe` 策略见元仓库 [ADR 0005](https://github.com/Inox-desktop/inox/blob/main/docs/adr/0005-headless-compositor-runtime.md)。

M2（PRD §13）：在 M1 的 headless 路径上扩展——通过 `wl_seat` 处理键盘/指针事件（确定性 keymap、修饰键跟踪、焦点投递），以 `xdg-shell` 客户端 toplevel 作为窗口并经 Smithay `desktop::Space` 管理 **≥2 个窗口**，按 master/stack 平铺布局（`proptest` 验证块不重叠、不越界、覆盖率完整）。键位绑定为纯逻辑（`Alt+Tab`/`Alt+Shift+Tab` 切换焦点、`Super+Q` 干净退出）；输入事件通过进程内注入 API 驱动，集成测试用**真实 Wayland 客户端**断言窗口数与平铺落点。M2 沿用 headless 路径（**不启用** libinput/udev/DRM 后端），决策见元仓库 [ADR 0006](https://github.com/Inox-desktop/inox/blob/main/docs/adr/0006-m2-input-windows-tiling.md)。

尚未实现（后续里程碑）：真实输入设备链路（libinput/udev/seat）、DRM/KMS 真实输出、XWayland、剪贴板与拖放。

### 运行

```console
# 无显示设备的环境（CI 用的软件渲染）
XDG_RUNTIME_DIR=/tmp/inox-runtime LIBGL_ALWAYS_SOFTWARE=1 \
  cargo run -- --headless --exit-after-frames 120

# 查看选项
cargo run -- --help
```

`XDG_RUNTIME_DIR` 必须指向已存在的绝对路径目录，Wayland 套接字在该目录下创建。

## 开发约定

- 本项目遵循根级 [AGENTS.md](https://github.com/Inox-desktop/inox/blob/main/AGENTS.md) 与仓库级 [`AGENTS.md`](./AGENTS.md)。
- **CI（GitHub Actions）是唯一的构建与测试验证途径**，不在本地进行编译验证。
- 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## 许可证

BSD 3-Clause，见 [`LICENSE`](./LICENSE)。
