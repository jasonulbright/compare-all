//! A dialog in a native window of its own.
//!
//! A dialog that is used alongside a comparison, such as the options or the
//! settings of a session, is shown in a window the platform owns, so it can be
//! moved beside the main window, put on another monitor, or left open while
//! the comparison is worked on. A backend without native viewports, and a
//! headless test frame, embed the same dialog in the root window instead,
//! where it draws an egui window of its own.
//!
//! The dialog itself knows nothing about where it is drawn. It offers a body
//! for a panel and a window for the root, and the host picks one.

use egui::{Context, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};

/// Where a dialog is drawn on this frame.
pub enum Host<'a> {
    /// The root window: the dialog draws an egui window of its own.
    Embedded(&'a Context),
    /// The panel of a native window: the dialog draws its body into `ui`. A
    /// window the dialog opens over its body is drawn through `ui.ctx()`,
    /// which is the context of the native window.
    Native(&'a mut Ui),
}

/// What one frame of a native window reported.
#[derive(Debug)]
pub struct Frame<R> {
    /// Whatever the dialog produced.
    pub produced: R,
    /// The close box of the native window was pressed on this frame. The
    /// embedded host never reports it; the egui window has a close box of
    /// its own that the dialog handles.
    pub close_requested: bool,
    /// The dialog was drawn in a native window, not embedded.
    pub native: bool,
}

/// A native window that hosts one dialog.
///
/// The window exists while [`NativeWindow::show`] is called every frame. A
/// frame that does not show it closes it, so a dialog that is dropped takes
/// its window with it.
#[derive(Debug, Clone)]
pub struct NativeWindow {
    id: ViewportId,
    title: String,
    size: [f32; 2],
    min_size: [f32; 2],
    focus_requested: bool,
}

impl NativeWindow {
    /// A window named `title`, opening at `size` points and never smaller
    /// than `min_size`. `key` and `instance` make its identity unique among
    /// the windows of one process.
    #[must_use]
    pub fn new(
        key: &'static str,
        instance: u64,
        title: impl Into<String>,
        size: [f32; 2],
        min_size: [f32; 2],
    ) -> Self {
        Self {
            id: ViewportId::from_hash_of((key, instance)),
            title: title.into(),
            size,
            min_size,
            focus_requested: false,
        }
    }

    /// The identity of the viewport the window draws into.
    #[must_use]
    pub fn id(&self) -> ViewportId {
        self.id
    }

    /// Bring the window to the front on the next frame, as when the command
    /// that opens it runs while it is already open.
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// Draw one frame of the dialog through `draw`, which runs exactly once
    /// with the host the platform offers.
    pub fn show<R>(&mut self, ctx: &Context, mut draw: impl FnMut(Host<'_>) -> R) -> Frame<R> {
        let builder = ViewportBuilder::default()
            .with_title(self.title.clone())
            .with_inner_size(self.size)
            .with_min_inner_size(self.min_size);
        let focus_requested = std::mem::take(&mut self.focus_requested);
        ctx.show_viewport_immediate(self.id, builder, |viewport, class| {
            if class == ViewportClass::Embedded {
                return Frame {
                    produced: draw(Host::Embedded(viewport)),
                    close_requested: false,
                    native: false,
                };
            }
            crate::theme::chrome::install(viewport);
            if focus_requested {
                viewport.send_viewport_cmd(ViewportCommand::Focus);
            }
            let close_requested = viewport.input(|input| input.viewport().close_requested());
            let produced = egui::CentralPanel::default()
                .show(viewport, |ui| draw(Host::Native(ui)))
                .inner;
            Frame {
                produced,
                close_requested,
                native: true,
            }
        })
    }

    /// Close the native window now, without waiting for the frame that no
    /// longer shows it. Does nothing for an embedded dialog.
    pub fn close(&self, ctx: &Context, frame_was_native: bool) {
        if frame_was_native {
            ctx.send_viewport_cmd_to(self.id, ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{Frame, Host, NativeWindow};
    use crate::testing::{native_viewports, sized_input, viewport_was_sent};
    use egui::ViewportCommand;
    use std::cell::RefCell;

    thread_local! {
        /// Whether the test renderer presses the close box of the child window.
        static PRESS_CLOSE: RefCell<bool> = const { RefCell::new(false) };
    }

    /// Run each immediate viewport as a nested frame, the way a native
    /// backend does.
    fn install_native_renderer(ctx: &egui::Context) {
        native_viewports(ctx, |_| PRESS_CLOSE.with(|press| *press.borrow()));
    }

    fn run_frame(
        ctx: &egui::Context,
        window: &mut NativeWindow,
        dialog: &mut Vec<&'static str>,
    ) -> (Frame<usize>, egui::FullOutput) {
        let mut frame = None;
        let output = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            frame = Some(window.show(ctx, |host| match host {
                Host::Embedded(_) => {
                    dialog.push("embedded");
                    1
                }
                Host::Native(ui) => {
                    dialog.push("native");
                    ui.label("body");
                    2
                }
            }));
        });
        (frame.unwrap(), output)
    }

    #[test]
    fn a_backend_without_viewports_embeds_the_dialog() {
        let ctx = egui::Context::default();
        let mut window =
            NativeWindow::new("test-window", 1, "Test", [400.0, 300.0], [200.0, 100.0]);
        let mut drawn = Vec::new();
        let (frame, _) = run_frame(&ctx, &mut window, &mut drawn);
        assert_eq!(drawn, vec!["embedded"]);
        assert_eq!(frame.produced, 1);
        assert!(!frame.native);
        assert!(!frame.close_requested);
    }

    #[test]
    fn a_native_backend_draws_the_body_in_its_own_viewport_and_reports_its_close_box() {
        let ctx = egui::Context::default();
        install_native_renderer(&ctx);
        let mut window =
            NativeWindow::new("test-window", 2, "Test", [400.0, 300.0], [200.0, 100.0]);
        let mut drawn = Vec::new();

        window.request_focus();
        let (frame, output) = run_frame(&ctx, &mut window, &mut drawn);
        assert_eq!(drawn, vec!["native"]);
        assert_eq!(frame.produced, 2);
        assert!(frame.native);
        assert!(!frame.close_requested);
        assert!(viewport_was_sent(
            &output,
            window.id(),
            &ViewportCommand::Focus
        ));

        let (frame, output) = run_frame(&ctx, &mut window, &mut drawn);
        assert!(
            !viewport_was_sent(&output, window.id(), &ViewportCommand::Focus),
            "focus is requested once, not on every frame"
        );
        assert!(!frame.close_requested);

        PRESS_CLOSE.with(|press| *press.borrow_mut() = true);
        let (frame, _) = run_frame(&ctx, &mut window, &mut drawn);
        assert!(frame.close_requested);
        assert!(frame.native);
    }

    #[test]
    fn closing_sends_the_command_to_the_child_viewport_only_when_it_was_native() {
        let ctx = egui::Context::default();
        install_native_renderer(&ctx);
        let mut window =
            NativeWindow::new("test-window", 3, "Test", [400.0, 300.0], [200.0, 100.0]);
        let mut drawn = Vec::new();

        // The window is shown on the frame it is told to close, as a dialog
        // that closes itself is: its native window must be told to go.
        let mut native = false;
        let output = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            let frame = window.show(ctx, |host| match host {
                Host::Embedded(_) => {
                    drawn.push("embedded");
                    1
                }
                Host::Native(ui) => {
                    drawn.push("native");
                    ui.label("body");
                    2
                }
            });
            native = frame.native;
            window.close(ctx, frame.native);
        });
        assert!(native);
        assert!(
            viewport_was_sent(&output, window.id(), &ViewportCommand::Close),
            "the native window is told to close"
        );

        // An embedded dialog has no native window to close.
        let output = ctx.run(sized_input(1_280.0, 800.0), |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            let _ = window.show(ctx, |_| 0);
            window.close(ctx, false);
        });
        assert!(!viewport_was_sent(
            &output,
            window.id(),
            &ViewportCommand::Close
        ));
    }
}
