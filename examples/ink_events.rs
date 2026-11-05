//! Run `cargo run --example ink_events` on Windows 10 or later, then use a pen
//! over the blank window. Events are printed to the launching terminal.

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows.");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    demo::run()
}

#[cfg(windows)]
mod demo {
    use std::io::Write;
    use tablet_input_windows::ink::WindowsInk;
    use windows::{
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            Graphics::Gdi::{COLOR_WINDOW, GetSysColorBrush},
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
                },
                WindowsAndMessaging::*,
            },
        },
        core::{Error, Result, w},
    };

    #[derive(Default)]
    struct App {
        ink: Option<WindowsInk>,
    }

    impl App {
        fn cancel_strokes(&mut self) {
            if let Some(ink) = &mut self.ink {
                for sample in ink.cancel_all() {
                    let _ = writeln!(std::io::stdout(), "{sample:?}");
                }
            }
        }

        fn detach_ink(&mut self) {
            self.cancel_strokes();
            self.ink.take();
        }
    }

    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: The HWND belongs to this thread and the App allocation is still alive.
            unsafe {
                if IsWindow(Some(self.0)).as_bool() {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
    }

    pub fn run() -> Result<()> {
        // The application chooses DPI awareness, before creating its window.
        // SAFETY: Process setup and native window APIs are called on this thread;
        // the boxed App remains allocated until after the window is destroyed.
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
            let instance = GetModuleHandleW(None)?.into();
            let name = w!("TabletInputWindowsInkExample");
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: name,
                hCursor: LoadCursorW(None, IDC_CROSS)?,
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(Error::from_thread());
            }
            let mut app = Box::<App>::default();
            let _window = Window(CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                name,
                w!("Windows Ink events - use a pen; see terminal output"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                960,
                640,
                None,
                None,
                Some(instance),
                Some((&mut *app as *mut App).cast()),
            )?);
            let _ = writeln!(
                std::io::stdout(),
                "Use a pen in the window. Close it to exit."
            );
            let mut message = MSG::default();
            loop {
                match GetMessageW(&mut message, None, 0, 0).0 {
                    -1 => return Err(Error::from_thread()),
                    0 => break,
                    _ => {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
        }
        Ok(())
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: WM_NCCREATE's CREATESTRUCTW and lpCreateParams are supplied by
        // our CreateWindowExW call. App outlives the window and stays on this thread.
        unsafe {
            if message == WM_NCCREATE {
                let create = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if !app.is_null() {
                if message == WM_CREATE {
                    match WindowsInk::new(hwnd) {
                        Ok(ink) => (*app).ink = Some(ink),
                        Err(error) => {
                            let _ = writeln!(std::io::stderr(), "{error}");
                            return LRESULT(-1);
                        }
                    }
                }
                // The application chooses which lifecycle events end its strokes.
                match message {
                    WM_KILLFOCUS | WM_CANCELMODE => (*app).cancel_strokes(),
                    WM_ACTIVATEAPP if wparam.0 == 0 => (*app).cancel_strokes(),
                    WM_NCDESTROY => (*app).detach_ink(),
                    _ => {}
                }
                // End this borrow before calling DefWindowProc, which may reenter.
                if let Some(ink) = &mut (*app).ink {
                    let result = ink.handle_message(hwnd, message, wparam, lparam);
                    if let Some(error) = result.error {
                        let _ = writeln!(std::io::stderr(), "Ink read: {error}");
                    }
                    for sample in result.samples {
                        let _ = writeln!(std::io::stdout(), "{sample:?}");
                    }
                    if result.handled {
                        return LRESULT(0);
                    }
                }
            }
            if message == WM_NCDESTROY {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            if message == WM_DESTROY {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }
}
