# autoitv3-gui-egui — egui 渲染后端（离屏 / 真窗口）

**egui 渲染后端**：把 [`autoitv3-gui-model`](../autoitv3-gui-model/README.md) 的控件模型画出来——feature `egui` 离屏渲染出 PNG，feature `window` 开真窗口。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-gui-egui/       # 库 crate——egui 渲染后端（feature "egui" 离屏，"window" 真窗口）
      src/widgets.rs         控件 → egui 控件的唯一映射：29 种 ControlKind 全画，
                             返回 Interaction（点击/输入/勾选/列表选择）
      src/render.rs          EguiBackend：把模型布局成帧
      src/raster.rs          egui 三角形 → RGBA 的 CPU 光栅器（无 GPU、确定性）
      src/png.rs             极简 PNG 写出（stored deflate，无依赖）
      src/live.rs            LiveBackend：真窗口（feature "window"），点击/编辑回灌
      tests/controls.rs      29 种控件逐一渲染 + 整窗画廊 + 确定性
      tests/unit/            widgets / raster / live 的单元测试（`#[path]` 回挂）
      examples/live.rs       实时窗口示例：Label + Input + Button
```

### GUI 渲染后端（可选 `egui`）——离屏 + 截图

控件模型与 `GuiBackend` 接缝在零依赖的 `autoitv3-gui-model`；`autoitv3-gui-egui` 用 egui 把模型
布局成帧，再用自带的 **CPU 光栅器**渲染成 RGBA。**不依赖 GPU、不依赖显示服务器**，
因此可复现、可在 CI 里断言像素。

**`widgets.rs` 是控件 → egui 控件的唯一映射**，离屏与实时窗口共用，所以两边画的是同一套：
29 种 `ControlKind` 全部落地——`Label`/`Button`/`Input`/`Edit`/`Checkbox`/`Radio`/`Group` 用原生
组件，`List`/`Combo`/`ListView`/`TreeView` 画成可选中的列表（选择写回 `selection`），
`Progress`/`Slider`/`Updown` 读 `text`/`limit` 显示数值，`Graphic` 重放 `GUICtrlSetGraphic`
的 `DrawCmd`，`Pic`/`Icon`/`Avi`/`Obj`/`Date`/`MonthCal` 画带标题的占位框，
`Menu`/`MenuItem` 组成菜单栏。控件自带的字体、文字色、背景色和 `tip`（悬停提示）也一并生效；
脚本给的 `width`/`height` 会用作控件尺寸。绘制时返回 `Interaction`
（`Clicked`/`Menu`/`Text`/`Checked`/`Selected`），实时后端再把它变成事件或模型更新。

```rust
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_gui_egui::EguiBackend;

// feature "gui-egui" 时也可用 WindowsEmulation::with_egui_backend()
let backend = EguiBackend::new()
    .with_size(800, 600)
    .with_screenshot("/tmp/frame.png");   // GUISetState 时落一张 PNG
let _emu = WindowsEmulation::new().with_gui_backend(Box::new(backend));
// … 跑脚本（GUICreate/GUICtrlCreate*/GUISetState…）后即可得到截图
```

也可直接拿 `EguiBackend::snapshot()`（RGBA）或 `screenshot(path)`；`with_screenshot` 只是把
这一步挂到 `present()` 上，省去把仿真层再取回来。

**中文（及其它 CJK）文字**：egui 自带的字体只有拉丁/希腊/西里尔与 emoji，没有汉字，
所以脚本控件里的中文会画成方块。这里**不打包**几 MB 的 CJK 字体，而是用机器上已有的：
先看 `AU3_GUI_FONT`（`.ttc` 合集用 `AU3_GUI_FONT_INDEX` 指定第几个面），否则依次试
`C:\Windows\Fonts\msyh.ttc`（微软雅黑）等 Windows 路径、Linux 的 `NotoSansCJK-*.ttc`/
`wqy-microhei.ttc`、macOS 的 `PingFang.ttc`，最后浅扫 `/usr/share/fonts` 等目录。
字体作为**最低优先级回退**装入，所以拉丁文字仍用 egui 自带字体。找不到时会打印一条提示
（"Chinese text will draw as boxes. Set AU3_GUI_FONT ..."）。实时窗口与离屏渲染器都会装它。

离屏渲染器自己维护字体图集缓存：egui 只在首帧发整张图集、之后用**局部补丁**加新字形，
所以缓存必须把补丁贴回去——否则"前几帧用过的字"能画，"后面才出现的字"会静默丢失。
这段逻辑现在是公开的 `apply_textures()`（与 `rasterize()`/`Texture` 一起导出，写自己的
光栅器时直接复用）。

egui 是**可选依赖**，默认 `cargo test` 不编译它：

```bash
cargo test -p autoitv3-gui-egui --features egui     # 离屏渲染 + PNG 的测试
cargo test -p autoitv3-platform --features gui-egui # 平台接线（脚本建窗→截图）
```

> `~/.cargo` 只读的环境需要把 `CARGO_HOME` 指到可写目录才能拉取 egui（见上文「受限环境」）。

#### 实时窗口（feature `window`）

`LiveBackend` 是交互版后端。winit 要求事件循环必须在**主线程**上创建（放到别的线程上会
直接 panic："Initializing the event loop outside of the main thread is a significant
cross-platform compatibility hazard"），所以 `LiveBackend::run` **占用主线程**跑 eframe，
把解释器放到工作线程——两边靠一个模型镜像 + 两个通道通信：

- **脚本 → 窗口**：`on_window`/`on_control` 更新镜像，窗口每帧读取；
- **窗口 → 脚本**：点击/菜单/列表选择进入 `poll()`（`GUIGetMsg` 消费），输入框/复选框/
  列表的改动通过 `take_updates()` → `GuiUpdate::{SetText,SetChecked,Select}` 回写模型，
  于是 `GUICtrlRead` 能读到用户刚输入/选中的内容。

窗口画什么、怎么交互都由 `widgets.rs` 决定，和离屏渲染完全一致；`LiveBackend::simulate`
可以脱离窗口喂一条交互，便于测试。脚本线程结束时窗口会自己关闭，因此 `run()` 一定返回，
不会留下一个没人更新的空窗口。

示例（需要显示服务器）：

```bash
cargo run -p autoitv3-gui-egui --features window --example live
# 窗口里：Label + Input + Button；点 Greet 打印 "Hello, <输入>!"，关窗结束脚本

cargo run -p autoitv3-gui-egui --features window --example live -- --titlebar
# 最小化时保留标题栏（双击标题栏 / 点标题栏按钮恢复），而不是让窗口离开屏幕

cargo run -p autoitv3-gui-egui --features window --example live -- --states
# 脚本自己驱动窗口的演示：WinMove → @SW_MAXIMIZE → @SW_RESTORE →
# @SW_MINIMIZE → @SW_RESTORE，每步打印 WinGetPos/WinGetState 看到的值；
# 之后停在消息循环里，等你点标题栏按钮或双击标题栏，控制台会打印脚本收到的事件

cargo run -p autoitv3-gui-egui --features window --example live -- --auto
# 不用人操作：脚本建完控件就返回，窗口自动关闭（验证接线用）
```

> `--states` 的第一步会打印**桌面尺寸**：实时窗口的**原生视口就是模拟桌面**，所以
> `@DesktopWidth`/`@DesktopHeight` 报的就是那个父窗口的分辨率（离屏渲染用画布尺寸，
> 无头分析回落到 1024×768 的假定显示模式）。窗口最大化时会把桌面矩形换成自己的几何
> （`WinGetPos` 于是返回桌面尺寸、`WinGetClientSize` 同理），并把原矩形记下来供
> `@SW_RESTORE` 还原——和 Windows 一致；之后你拖动父窗口改变分辨率，已最大化的窗口会跟着变
> 并收到 `$GUI_EVENT_RESIZED`。

示例里的 **Minimise** 按钮会 `WinSetState(@SW_MINIMIZE)`：默认模式下窗口消失、底部出现
恢复按钮；`--titlebar` 模式下窗口收成标题栏、双击恢复。

```rust
use autoitv3_gui_egui::LiveBackend;
use autoitv3_platform::winemu::WindowsEmulation;

let backend = LiveBackend::new("AutoIt GUI PoC");
backend.run(move |backend| {                    // 主线程；阻塞到窗口关闭
    let emu = WindowsEmulation::new().with_gui_backend(Box::new(backend));
    // … 在 emu 上跑脚本；GUISetState() 之后窗口出现
})?;
```

**模拟桌面**：实时窗口的原生视口（`AutoIt GUI PoC` 那层）就是被仿真机器的桌面，
`@DesktopWidth`/`@DesktopHeight` 读它，`@DesktopDepth`/`@DesktopRefresh` 给假定显示模式的
32/60；离屏渲染后端把画布当桌面，无头运行回落到 1024×768。`GuiBackend::desktop_size()`
就是后端报桌面的接口（不实现即用回落值），自定义后端记得转发它。

**窗口尺寸与状态**：每个模拟窗口就是脚本 `GUICreate` 的**客户区大小**（第一次画时按它
铺开，不再缩成内容大小），四个边和右下角的拖拽都能用，横竖都能拉。两个方向都通：

- **用户拖拽 → 脚本**：新尺寸回写模型，`WinGetPos`/`WinGetClientSize` 立刻反映，`GUIGetMsg`
  收到 `$GUI_EVENT_RESIZED`（-12）；
- **脚本 → 屏幕**：`WinMove`（`WinMove($h, "", x, y [, w [, h]])`，`-1` 表示那一维不动）
  真的会移动/缩放窗口，`WinSetState`/`GUISetState` 的 `@SW_HIDE`/`@SW_MINIMIZE`/
  `@SW_MAXIMIZE`/`@SW_RESTORE` 也照做——最大化的窗口铺满视口。

**用户拖动 = 拥有几何**：拖动边框会改大小、拖动标题栏会移动，两者都**回写模型**
（`GuiUpdate::Resize`/`Move`），所以 `WinGetPos`/`WinGetClientSize` 与屏幕一致；AutoIt 对"移动"
没有消息，脚本靠轮询。**拖动一个最大化的窗口（边框或标题栏）会先退出最大化**——和 Windows
一样先回到正常摆放位置，再让指针接管；为此实时窗口会记住"用户要求的那个状态"，
在脚本应用之前不回弹（否则中间几帧会被最大化规则拉回去）。

> **用户拖的结果当场进模型**：实时后端把上报的 `Move`/`Resize` 立刻折进镜像
> （`fold_user_update`），不等脚本下一次轮询——脚本 `Sleep(900)` 时也不会有"模型还停在原地"
> 的空档。

> **摆放窗口看的是"上一帧真正画出来的位置/尺寸"**，不是"模型里的几何变没变"：两者相同就说明
> 模型里那点变化只是我们自己刚上报的回声，不该动窗口。**拖动期间**再加一层保险：指针按在哪个
> 窗口上（`DrawnWindow::pointer_owns`，归属记到松开），那个窗口的位置就归指针——所以窗口严格
> 跟手、不抖，脚本在用户拖 A 时移动 B 照样有效。这也修掉了**"最大化还原后停在 0,0"**：用户
> 点击（双击标题栏 / 点还原按钮）那一帧指针还压在窗口上，那一帧不会去摆窗口，但记下的是
> **窗口真实所在**，于是下一帧照样会把它摆回 `Window::restore` 里记的原位置（此前记的是"我本来
> 想摆到哪"，于是下一帧误以为已经到位，窗口就永远留在 0,0）。

> **脚本 → 屏幕不走轮询**：后端每次 `on_window`/`on_control`/`GUISetState` 都会唤醒一帧
> （`ctx.request_repaint()`），所以 `WinMove`、控件文本、以及拖动回声都是当帧上屏；50 ms 的
> 周期重绘只是兜底。

**拖动最大化窗口的标题栏 = 立即还原**（Windows 的手势）：指针一拖标题栏，窗口就取
`Window::restore` 里的尺寸，并按光标在最大化矩形里的**相对位置**摆好——光标抓住标题栏的那一点
仍然在光标下面——随后跟着指针 1:1 移动；松开后停在原地，`WinGetPos` 与屏幕一致。这几帧的拖动由
我们接管：egui 会把拖拽中的窗口夹回视口，而"和视口一样大"的最大化窗口根本推不动，所以摆放用
`current_pos` + 关掉该帧的 `constrain`，并把 egui 记录"拖拽起点"的临时数据一起改写（这是 egui
的内部约定，由测试守着；换 egui 版本时它会先失败）。
`dragging_a_maximised_title_bar_restores_the_window_under_the_pointer` 守这条行为。

**标题栏控件**：窗口标题栏右侧有 **最小化** 和 **最大化/还原** 两个按钮（最大化后同一个按钮变成还原），
点一下即可操作，脚本相应收到 `$GUI_EVENT_MINIMIZE`/`RESTORE`/`MAXIMIZE`。egui 只自带关闭按钮，
所以这两个是我们自己画的，但**样式与 egui 关闭按钮完全一致**：同样的 `spacing.icon_width` 方形尺寸、
同样的 `item_spacing.x` 间距、同样的 `fg_stroke` **线条画法**（无底色），悬停时同样按 `visuals.expansion`
略微放大——所以标题栏看起来还是一整条。实现用 `ui.interact` + 自绘（不进布局流，
因此不会影响 egui 对窗口尺寸的测量）。

**标题栏双击 = 最大化/还原**（Windows 的手势）。egui 默认是"双击折叠"（保留标题、藏掉内容），
这里特意关掉折叠；再双击一次（窗口已最大化或最小化）则还原。点在标题栏按钮上不算双击，
所以连点最小化不会"最小化又马上还原"。

**最小化的呈现可以选**（`LiveBackend::with_minimize_style`）：

| 取值 | 行为 |
| ---- | ---- |
| `MinimizeStyle::Hidden`（默认，**仿真**） | 窗口离开屏幕，和 Windows 一样；viewpor 底部出现一条 "Minimised: <标题>" 的**任务栏**，点一下恢复（脚本收到 `$GUI_EVENT_RESTORE`） |
| `MinimizeStyle::TitleBar` | 保留标题栏、只藏内容（egui 折叠的观感），窗口仍在屏幕上，**双击标题栏恢复** |

用户的其他状态操作也会告诉脚本：双击标题栏最大化 → `$GUI_EVENT_MAXIMIZE`(-6)，还原 →
`$GUI_EVENT_RESTORE`(-5)，任务栏恢复 → 同样是 -5；`WinGetState` 的位标志照旧跟着变
（它和 `@SW_*` 是两套值）。

`@SW_*`（0…11）现在是真正的宏，`WinSetState` 按 AutoIt 的 `@SW_*` 解释（注意它**不是**
`WinGetState` 的位标志：`@SW_MINIMIZE` 是 6，而 `WIN_MINIMIZED` 位是 16）。

> egui 的 `Window` 是按**内容**决定尺寸的（`Resize::end` 对窗口回退到内容尺寸），
> 所以内容不填满窗口时，拖动那一维会在下一帧弹回去——这正是"能左右拉、不能拉高"的
> 原因。`widgets.rs` 用"首帧按脚本尺寸、之后按上一帧画出的尺寸设下限、指针按下时放开
> 下限"来解决（见 `show_autoit_window` 的注释与 `tests/window_resize.rs`）。

**其它已知近似**（不是 bug，是模型里没有的信息）：控件按创建顺序纵向排列而不是按脚本的
绝对 `x`/`y`；模型没有保留菜单→菜单项的父子关系，所以每个菜单列出本窗口所有菜单项；
`Tab` 只画成一行标签而不是真正的分层面板；`ListView` 没有列宽/表头点击，`TreeView`
用前导缩进表现层级。`Win*`/`Control*`（`WinMove`/`ControlClick` 等）只改内存模型，
不会真的搬动或缩放这个窗口；托盘、像素与输入注入也还没接。

```bash
cargo test -p autoitv3-gui-egui --features window   # 后端管线 + 交互映射测试
```
