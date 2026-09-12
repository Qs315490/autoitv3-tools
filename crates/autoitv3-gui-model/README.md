# autoitv3-gui-model — GUI 控件模型与后端接缝

**GUI 控件模型与后端接缝**（零依赖）：`Window`/`Control`/`GuiModel`、绘制指令与 `GuiBackend` trait。本 crate 不渲染任何东西；离屏（PNG）与真窗口渲染在 [`autoitv3-gui-egui`](../autoitv3-gui-egui/README.md)，GUI 函数语义在 [`autoitv3-platform`](../autoitv3-platform/README.md) 的 winemu 层。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-gui-model/            # 库 crate（零依赖）——GUI 控件模型 + 后端接缝
      src/lib.rs             公共 API（GuiEvent/GuiImage/GuiBackend 导出）
      src/model.rs           Window/Control/GuiModel、绘制指令、$GUI_* 状态位
      src/backend.rs         GuiBackend trait + HeadlessBackend + GuiEvent/GuiImage
```
