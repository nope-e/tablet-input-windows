//! `cargo run --example winit_ink`
//!
//! Write with a pen in the window; stroke events go to the terminal.
//! Winit owns the window, while WindowsInk reads native messages on the same thread.

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows.");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    demo::run()
}

#[cfg(windows)]
mod demo {
    use std::{cell::RefCell, error::Error, io::Write, rc::Rc};

    use tablet_input_windows::{PenPhase, PenSample, ink::WindowsInk};
    use windows::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{COLOR_WINDOW, FillRect, GetDC, GetSysColorBrush, ReleaseDC},
        UI::WindowsAndMessaging::{GetClientRect, MSG},
    };
    use winit::{
        application::ApplicationHandler,
        dpi::LogicalSize,
        event::{DeviceEvent, DeviceId, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::{Window, WindowId},
    };

    type SharedInk = Rc<RefCell<Option<WindowsInk>>>;

    #[derive(Default)]
    struct App {
        ink: SharedInk,
        window: Option<Window>,
        error: Option<Box<dyn Error>>,
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let mut app = App::default();
        let hook_ink = Rc::clone(&app.ink);
        let mut builder = EventLoop::builder();
        builder.with_msg_hook(move |raw_message| {
            // SAFETY: winit passes a valid Win32 MSG for this callback's duration.
            // windows and winit's windows-sys binding use the same native MSG ABI.
            let message = unsafe { &*raw_message.cast::<MSG>() };
            let result = {
                let mut slot = hook_ink.borrow_mut();
                let Some(ink) = slot.as_mut() else {
                    return false;
                };
                ink.handle_message(
                    message.hwnd,
                    message.message,
                    message.wParam,
                    message.lParam,
                )
            };
            if let Some(error) = result.error {
                let _ = writeln!(std::io::stderr(), "Windows Ink: {error}");
            }
            print_strokes(result.samples);
            // Only recognized pen messages are consumed. Other input, focus,
            // resize, paint and close messages continue to winit.
            result.handled
        });

        let event_loop = builder.build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        event_loop.run_app(&mut app)?;
        if let Some(error) = app.error {
            return Err(error);
        }
        Ok(())
    }

    impl App {
        fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
            let window = event_loop.create_window(
                Window::default_attributes()
                    .with_title("Windows Ink / winit - write with a pen; see terminal")
                    .with_inner_size(LogicalSize::new(960.0, 640.0)),
            )?;
            let ink = WindowsInk::new(hwnd(&window)?)?;
            *self.ink.borrow_mut() = Some(ink);
            window.request_redraw();
            self.window = Some(window);
            let _ = writeln!(
                std::io::stdout(),
                "Write with a pen in the window. Down / Move / Up / Cancel are printed below.\n\
                 Coordinates are client physical pixels; close the window to exit."
            );
            Ok(())
        }

        fn cancel_strokes(&self) {
            let samples = self
                .ink
                .borrow_mut()
                .as_mut()
                .map(WindowsInk::cancel_all)
                .unwrap_or_default();
            print_strokes(samples);
        }

        fn detach_ink(&self) {
            let ink = self.ink.borrow_mut().take();
            if let Some(mut ink) = ink {
                print_strokes(ink.cancel_all());
            }
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_none()
                && let Err(error) = self.create_window(event_loop)
            {
                self.error = Some(error);
                event_loop.exit();
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window_id: WindowId,
            event: WindowEvent,
        ) {
            let Some(window) = &self.window else { return };
            if window.id() != window_id {
                return;
            }
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                // Application policy: losing focus ends current strokes.
                // WindowsInk leaves window lifecycle decisions to the caller.
                WindowEvent::Focused(false) => self.cancel_strokes(),
                WindowEvent::Destroyed => {
                    self.detach_ink();
                    event_loop.exit();
                }
                WindowEvent::Resized(_) => window.request_redraw(),
                WindowEvent::RedrawRequested => {
                    if let Err(error) = clear_background(window) {
                        self.error = Some(error);
                        event_loop.exit();
                    }
                }
                _ => {}
            }
        }

        fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
            self.detach_ink();
            self.window.take();
        }

        fn device_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _device_id: DeviceId,
            event: DeviceEvent,
        ) {
            if matches!(event, DeviceEvent::Added | DeviceEvent::Removed)
                && let Some(ink) = self.ink.borrow_mut().as_mut()
                && let Err(error) = ink.refresh_devices()
            {
                let _ = writeln!(std::io::stderr(), "Refresh pen devices: {error}");
            }
        }
    }

    fn hwnd(window: &Window) -> Result<HWND, Box<dyn Error>> {
        match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as *mut _)),
            _ => Err(std::io::Error::other("expected a Win32 window handle").into()),
        }
    }

    fn print_strokes(samples: Vec<PenSample>) {
        let mut output = std::io::stdout().lock();
        for sample in samples {
            if matches!(
                sample.phase,
                PenPhase::Down | PenPhase::Move | PenPhase::Up | PenPhase::Cancel
            ) {
                let _ = writeln!(output, "{sample:?}");
            }
        }
    }

    // A plain background needs no rendering framework. This does not draw strokes.
    fn clear_background(window: &Window) -> Result<(), Box<dyn Error>> {
        let hwnd = hwnd(window)?;
        let mut rect = RECT::default();
        // SAFETY: The window and its DC are used on their owning thread, rect is
        // writable, and the borrowed DC is released before returning.
        unsafe {
            GetClientRect(hwnd, &mut rect)?;
            let dc = GetDC(Some(hwnd));
            if dc.is_invalid() {
                return Err(std::io::Error::other("GetDC failed").into());
            }
            let painted = FillRect(dc, &rect, GetSysColorBrush(COLOR_WINDOW));
            ReleaseDC(Some(hwnd), dc);
            if painted == 0 {
                return Err(std::io::Error::other("FillRect failed").into());
            }
        }
        Ok(())
    }
}
