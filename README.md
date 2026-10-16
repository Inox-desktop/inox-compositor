# inox-compositor

基于 Smithay 的 Wayland 合成器。属于 Inox 桌面环境的 **L2** 层。

## 状态

M1：headless 合成器已实现——创建 `wl_output`、以 EGL surfaceless（`EGL_MESA_platform_surfaceless`）+ GLES 渲染纯色帧、收到终止信号或渲染完指定帧数后干净退出（退出码 0）。渲染路径与 `unsafe` 策略见元仓库 [ADR 0005](https://github.com/Inox-desktop/inox/blob/main/docs/adr/0005-headless-compositor-runtime.md)。

尚未实现（后续里程碑）：输入事件、DRM/KMS 与真实输出链路、窗口管理与平铺布局、XWayland。

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
