# tablet-input-windows

[English](README.md) | 简体中文

面向自定义绘画、笔记画布的 Rust 笔输入库。Windows Ink 后端通过 `windows` crate 读取桌面 `WM_POINTER` 消息，返回拥有所有权的采样，包含笔画编号、合并历史、压力、倾角和笔状态。

库独立于 winit、WinUI 和渲染框架。窗口生命周期、焦点策略、DPI awareness、画布归属和渲染由应用负责。

## 平台与范围

- `pen` 模块及根模块重导出的事件类型可在各平台构建；原生 `ink` 模块仅在 Windows 上提供。
- 当前后端为 Windows Ink / `WM_POINTER`，尚未实现 WinTab。
- 项目使用 Rust edition 2024、`windows` 0.62 和 `thiserror` 2；winit 0.30 仅作为 Windows 示例依赖。
- 底层 Pointer API 自 Windows 8 提供，但 Rust 工具链、依赖和应用可能要求更新的系统。

## 快速接入

假设调用方项目与本仓库位于同一父目录：

```toml
[dependencies]
tablet-input-windows = { path = "../tablet-input-windows" }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.62.2", features = ["Win32_Foundation"] }
```

每个有效 HWND 使用一个 `WindowsInk`，在窗口所属线程创建、调用，并保存在窗口状态中。以下函数演示如何挂接现有窗口并传入原生消息：

```rust
use tablet_input_windows::ink::{InkError, WindowsInk};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

fn attach(hwnd: HWND) -> Result<WindowsInk, InkError> {
    WindowsInk::new(hwnd)
}

fn on_message(
    ink: &mut WindowsInk,
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> bool {
    let result = ink.handle_message(hwnd, message, wparam, lparam);
    if let Some(error) = result.error {
        eprintln!("Windows Ink: {error}");
    }
    for sample in result.samples {
        // 在此处理画布归属，并将客户区坐标转换为文档坐标。
        println!("{sample:?}");
    }
    result.handled
}
```

必须在下一次 `GetMessage` 或 `PeekMessage` 之前同步调用 `handle_message`。原生笔历史属于当前线程最近取出的消息，读取下一条消息后可能失效。[微软：GetPointerPenInfoHistory](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getpointerpeninfohistory)

原生 WndProc 在 `handled` 为 true 时返回 `LRESULT(0)`，否则继续正常分发；框架 hook 按框架约定映射这个布尔值。同一笔输入序列应持续交给一个后端处理，以免重复采样以及产生不一致的行为。

## 应用生命周期

失焦、应用失活、取消操作、切换工具、关闭画布是否取消笔画，由调用方决定。`handle_message` 忽略 `WM_KILLFOCUS`、`WM_ACTIVATEAPP`、`WM_CANCELMODE`、`WM_DESTROY` 和 `WM_NCDESTROY`，不会因此改变笔画状态。

需要取消时显式调用：

```rust
// ink 是保存在窗口状态中的适配器。
for canceled in ink.cancel_all() {
    println!("{canceled:?}"); // 将事件交给绘图引擎。
}
```

`cancel_all()` 为每个活动的笔画生成一次 `Cancel`，复用最后的位置和属性并更新时间戳，然后清空接触和悬停状态；新笔画开始前重复调用不会产生事件。后续笔画继续分配新编号。

替换或释放后端时，取消剩余笔画并交付返回事件。HWND 销毁后停止原生输入读取和设备刷新，并释放后端。Drop 不会交付事件，也不会销毁窗口。`WindowsInk` 既不是 `Send` 也不是 `Sync`，拥有所有权的 `PenSample` 可以跨线程传递。

库仍会把笔输入本身的中断转换为 `Cancel`：原生取消标志、指针捕获丢失，以及缺失 Up 后到达的 Hover/Leave。这些事件描述笔输入流自身的状态。

## 事件模型

`PenSample`、`PenPhase` 和 `PenTilt` 同时由 `pen` 模块及 crate 根模块导出。

| 字段 | 含义 |
| --- | --- |
| `pointer_id` | 当前指针生命周期内的系统标识，可以被复用，不是物理设备 ID。 |
| `stroke_id` | 本后端分配的接触编号。输出的 Down/Move/Up/Cancel 都带编号，Hover/Leave 不带编号；Enter 可以保留活动笔画编号。 |
| `time_us` | 自后端创建以来的微秒时间，指针被追踪期间不倒退；不同后端的时间基准独立。 |
| `position` | 客户区坐标 `[x, y]`。窗口正确配置 DPI awareness 时单位为物理像素，当前来源是整数像素坐标。 |
| `pressure` | `0..=1` 的可选归一化压力，`None` 表示不可用。 |
| `pressure_level` | 原始压力属性逻辑范围内整数取值的可选数量；标准压力回退时为 `None`。 |
| `tilt_deg` | 分别可选的 X/Y 倾角，单位为度，各轴范围为 `-90..=90`。 |
| `rotation_deg` | 可选的顺时针旋转角，单位为度，范围为 `0..360`。 |
| `eraser` | 驱动报告的橡皮工具或橡皮端状态。 |
| `inverted` | 驱动报告的笔倒置状态，独立于 `eraser`。 |
| `barrel_button` | 驱动报告的侧键状态。 |
| `phase` | 下表所述的指针或接触生命周期阶段。 |

| 阶段 | 行为 |
| --- | --- |
| `Enter` | 进入窗口或感应范围，不创建笔画。 |
| `Hover` | 未接触时的位置更新。 |
| `Down` | 开始新笔画并分配编号。 |
| `Move` | 更新已有笔画，包括捕获期间位于客户区之外的移动。 |
| `Up` | 正常结束笔画。 |
| `Cancel` | 中断笔画；合成事件可能复用最后已知位置。 |
| `Leave` | 离开窗口或感应范围，尚未结束的笔画会先被取消。 |

只有 Down 能创建笔画。取消之后或从另一窗口移入时，零散的接触更新、Up 不会中途启动绘制。笔画编号从 1 开始，在同一个后端实例中不复用。

只在 Down 属于画布时接受该笔画，再通过 `stroke_id` 持续保有其归属直到 Up 或 Cancel。捕获期间坐标可以为负数，也可以超出客户区；绘图引擎负责裁剪和文档变换。目前没有进行 HIMETRIC 亚像素映射。

时间戳优先使用性能计数器，缺失时回退到 Windows 毫秒时间戳，处理 32 位回绕，并将早于后端创建的历史时间截为零；它不是墙上时钟时间。

## 压力与设备缓存

初始化时枚举笔设备，缓存各设备的 `TIP_PRESSURE` 描述符及逻辑范围倒数。设备缓存使用 `POINTER_INFO.sourceDevice` 的句柄身份，与 HWND 和指针编号无关。首次收到尚未缓存的设备输入时，按需查询属性。

存在可用描述符时，每批输入调用一次 [`GetRawPointerDeviceData`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawpointerdevicedata) 读取压力，并复用原始值缓冲区。每个历史点使用对应压力，再按时间顺序输出；原始历史排列的处理也可参考 [WPF 实现](https://github.com/dotnet/wpf/blob/main/src/Microsoft.DotNet.Wpf/src/PresentationCore/System/Windows/InterOp/HwndPointerInputProvider.cs)。

```text
pressure       = clamp((raw - logical_min) / (logical_max - logical_min), 0, 1)
pressure_level = logical_max - logical_min + 1
```

例如 `0..=16383` 包含 16384 个整数值，归一化分母是 16383。`pressure_level` 表示逻辑范围容量，不是当前原始压力值，也不是实测硬件精度。

原始属性、范围或读取不可用时，使用有效的 `GetPointerPenInfo` 标准压力除以 1024；标准压力也不可用时，`pressure` 为 `None`。Windows 标准压力范围为 `0..=1024`，不能据此判断设备原生压力容量，所以回退采样保留 `pressure_level = None`。[微软：POINTER_PEN_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_pen_info)

缺失属性、无效范围及属性查询失败都会缓存，直到缓存失效。设备首次使用时尝试报告已缓存的描述符错误；原始读取错误在当前批次报告，后续输入继续重试。多项错误同时发生时，`MessageResult.error` 保留第一个错误。

后端为窗口注册设备变化通知；收到 `WM_POINTERDEVICECHANGE` 或 `WM_DEVICECHANGE` 时使描述符缓存失效，消息仍保持未消费。框架不转发这些消息时，在设备变化后调用 `ink.refresh_devices()`；它清空并重建缓存，保留活动笔画和时间基准。枚举错误直接返回，单设备属性错误延迟到该设备输入时报告。

缓存避免了重复查询描述符；读取原始压力增加的开销尚未进行基准测试。

## 返回结果与错误

`handle_message()` 返回三个互相独立的字段：

| 字段 | 约定 |
| --- | --- |
| `handled` | 后端是否消费了这条笔消息。已识别的笔消息即使读取失败，也保持 true。 |
| `samples` | 拥有所有权的事件，同一指针内按历史从旧到新排列，不重复输出当前历史点。 |
| `error` | 第一个读取或描述符错误，可能与可用采样同时返回。 |

历史读取失败时回退到当前点；原始压力读取失败时保留标准压力；当前批次读取或坐标转换失败时，用已保存位置取消活动笔画。无法识别未知指针类型时报告错误，并保持消息未消费。

[`InkError`](src/ink/error.rs) 使用 `thiserror`，为无效窗口、线程错误、坐标转换、历史或设备数量、压力范围、历史匹配及 Win32 调用失败提供结构化变体。`InkError::Win32 { operation, source }` 保留原生错误码，并通过 `std::error::Error::source()` 暴露底层原因。

## 示例

在 Windows 上从本仓库目录运行：

| 命令 | 行为 |
| --- | --- |
| `cargo run --example ink_events` | 使用 Per-Monitor V2 DPI awareness 创建原生 Win32 窗口，输出笔事件；应用在 WndProc 中处理生命周期取消。 |
| `cargo run --example winit_ink` | 通过 winit `ApplicationHandler` 创建窗口，在终端输出 Down/Move/Up/Cancel。 |
| `cargo run --example pointer_device_properties` | 枚举设备，输出 HID usage、逻辑/物理范围、单位和 `TIP_PRESSURE` 描述符。 |

两个窗口示例只显示背景并输出事件，不渲染笔画。属性示例读取描述符，不能验证实时压力精度。

[winit 示例](examples/winit_ink.rs) 通过 `with_msg_hook` 转发原生消息，并在同一线程通过 `Rc<RefCell<Option<WindowsInk>>>` 共享后端。它在 `Focused(false)` 时取消笔画，`CloseRequested` 时请求退出，销毁或退出时取消剩余笔画并释放后端；设备 Added/Removed 时刷新描述符。这些生命周期决策及 DPI 配置属于应用代码。

## 代码结构

| 模块 | 职责 |
| --- | --- |
| [`pen`](src/pen.rs) | 可移植事件类型。 |
| [`ink`](src/ink/mod.rs) | Windows 公共适配器、消息结果和错误类型。 |
| [`ink::native`](src/ink/native.rs)，私有 | 原生调用、历史读取、坐标和时间转换。 |
| [`ink::pressure`](src/ink/pressure.rs)，私有 | 设备描述符、缓存、原始压力批次及归一化。 |
| [`state`](src/state.rs)，私有 | 不依赖原生调用的指针/接触归属及笔画生命周期。 |
