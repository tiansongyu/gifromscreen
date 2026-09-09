# Contributing / 参与贡献

感谢帮助改进 GifFromScreen。欢迎可复现的问题报告、文档修正和范围明确的补丁。
Thanks for helping improve GifFromScreen. Reproducible bug reports, documentation fixes and focused patches are welcome.

## 报告问题 / Report a bug

请先查看 [Issues](https://github.com/tiansongyu/gifromscreen/issues) 和 [Linux 已知边界](docs/LINUX-STATUS.md)，再使用缺陷报告表单。请提供：

- Release 标签或源码提交，以及安装来源；仅写 `0.1.0` 可能无法区分预览包和正式包。
- 发行版、桌面/窗口管理器、架构，以及 X11 或 Wayland。
- 应用缩放、显示器缩放、预览缩放和界面语言，尤其是布局或坐标问题。
- 最短复现步骤、预期结果、实际结果及出现频率。

Check existing issues and known limitations first. Include the release tag/commit and package source, distribution/desktop/architecture, X11 or Wayland, UI/display/preview scale, language, minimal steps, expected versus actual results, and frequency.

日志、截图、GIF 和 `.gfsproj` 都可能包含私人内容；开启按键/点击采集的工程还可能保留敏感输入。请优先使用合成内容复现，提交前检查并移除密码、令牌、个人路径和敏感画面。不要为排查问题上传完整的私人录制。

Logs, screenshots, GIFs and projects may contain private data. Projects can also retain opt-in key/click metadata. Prefer synthetic examples and remove secrets, personal paths and sensitive content before attaching evidence. A full private recording is not required.

## 构建与检查 / Build and check

开发目标优先为 Linux；最低 Rust 版本为 1.88。运行说明见 [README](README.md#从源码运行)，Linux 开发库列表以 [CI](.github/workflows/ci.yml) 和[便携打包工作流](.github/workflows/portable.yml)为准。视频导入测试可能需要系统 FFmpeg。

Linux is the primary development target; the minimum Rust version is 1.88. See the [English README](README.en.md) for source builds and the linked workflows for development libraries. Video-import tests may require system FFmpeg.

在仓库根目录执行适用检查，并在 PR 中写明实际工具链和结果：
Run applicable checks from the repository root and report the actual toolchain and results:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo +1.88.0 check --locked --workspace --all-targets --all-features
```

原生录制、输入模拟、设备和忽略测试有额外前提；先阅读对应脚本或验收文档。输入模拟应使用自有隔离桌面，不能借用他人 DISPLAY、鼠标或键盘。打包检查见 [PACKAGING](docs/PACKAGING.md)，桌面冒烟可使用脚本的 `--owned-xvfb` 模式。

Native capture, input injection, device and ignored tests have additional prerequisites. Read their instructions first and use an owned isolated desktop for input simulation, never another person's display or input devices. Packaging checks and owned-Xvfb smoke tests are separate from ordinary unit tests.

## 提交补丁 / Submit a patch

- 一个 PR 聚焦一件事，说明动机、影响范围、复现或验证步骤；避免无关格式化、依赖更新和用户数据改写。Keep changes focused and explain their purpose, scope and verification; avoid unrelated formatting, dependency churn or user-data changes.
- Bug 修复尽量加入回归测试。区分单元、无界面、原生和真实发行包验证；未运行或被忽略的检查不能写成通过。Add regression coverage and distinguish unit/headless/native/package evidence; skipped checks are not passes.
- 保留旧工程和撤销/重做语义。改变已保存内容的渲染契约需显式版本及兼容性检查，不能静默改画面。Preserve saved projects and undo/redo; version persistent rendering changes explicitly.
- 不从待测实现生成“参考像素”，不放宽容差掩盖差异。独立 WPF 参考工具及其来源校验有各自说明。Do not generate expected reference pixels from the implementation under test or relax tolerances to hide differences.
- 翻译使用稳定消息 ID，保留具名参数，不翻译用户正文、路径或机器标识。Use stable catalog IDs and preserve named parameters; user content, paths and machine identities are not translation keys.
- 保留项目及第三方许可声明，注明移植代码的来源与许可。Retain project and third-party notices and identify the source/license of ported material. See [NOTICE](packaging/licenses/NOTICE.txt).

本指南不承诺响应、合并或修复时限。This guide does not promise response, merge or fix deadlines.
