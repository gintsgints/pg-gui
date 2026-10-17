//! The debug panel that replaces the results while a session runs.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};

use crate::app::PgGuiApp;
use crate::{DebugContinue, DebugStepInto, DebugStepOver, DebugStop};

impl PgGuiApp {
    /// The debug panel that replaces the results table while a session runs:
    /// a toolbar, the stepped source, and a variables/stack sidebar.
    pub(in crate::app) fn render_debug_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some(dbg) = self.debug.as_ref() else {
            return div().into_any_element();
        };
        let running = !dbg.terminated;
        let toolbar = h_flex()
            .gap_2()
            .p_2()
            .items_center()
            .child(
                div()
                    .font_semibold()
                    .child(format!("Debug: {}", dbg.session.target)),
            )
            // Between stops there is nothing else moving in the panel: the
            // previous stop's variables and stack stay on screen while the
            // step runs.
            .when(dbg.busy, |this| this.child(Spinner::new().xsmall()))
            .child(div().flex_1())
            .child(
                Button::new("dbg-step-over")
                    .outline()
                    .small()
                    .label("Step Over")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_step_over(&DebugStepOver, window, cx);
                    })),
            )
            .child(
                Button::new("dbg-step-into")
                    .outline()
                    .small()
                    .label("Step Into")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_step_into(&DebugStepInto, window, cx);
                    })),
            )
            .child(
                Button::new("dbg-continue")
                    .outline()
                    .small()
                    .label("Continue")
                    .disabled(!running)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_continue(&DebugContinue, window, cx);
                    })),
            )
            .child(
                Button::new("dbg-stop")
                    .danger()
                    .small()
                    .label("Stop")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.debug_stop(&DebugStop, window, cx);
                    })),
            );
        // The stepped source lives in the editor itself — the selection follows
        // each stop — so the panel only carries the state around it.
        let body = div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .child(if dbg.stop.is_some() {
                self.render_debug_sidebar(cx)
            } else {
                self.render_debug_status(cx)
            });
        // A long routine's result is one line; keep it from growing the panel.
        let output = dbg.output.as_ref().map(|out| {
            div()
                .id("dbg-output")
                .flex_none()
                .max_h(px(64.))
                .overflow_y_scroll()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("Result: {out}"))
        });
        v_flex()
            .size_full()
            .child(toolbar)
            .child(body)
            .children(output)
            .into_any_element()
    }

    /// What the session is doing before its first stop (connecting, waiting
    /// for the target to trap), plus any output or error, so a run that never
    /// traps is visible rather than silent. Once stopped, the panel shows the
    /// variables/stack instead and the source is read in the editor.
    pub(in crate::app) fn render_debug_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dbg) = self.debug.as_ref() else {
            return div().into_any_element();
        };
        let mut pane = v_flex()
            .p_2()
            .gap_2()
            .text_color(cx.theme().muted_foreground);
        pane = pane.child(if dbg.terminated {
            div()
                .child(format!("Session ended before stopping. {}", dbg.status))
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .when(dbg.busy, |this| this.child(Spinner::new().xsmall()))
                .child(format!("{}…", dbg.status.trim_end_matches('…')))
                .into_any_element()
        });
        if let Some(output) = &dbg.output {
            pane = pane.child(div().child(format!("Target: {output}")));
        }
        pane.into_any_element()
    }

    /// Variables of the active frame (click to change) over the call stack
    /// (click a frame to inspect it).
    pub(in crate::app) fn render_debug_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(stop) = self.debug.as_ref().and_then(|d| d.stop.as_ref()) else {
            return div().into_any_element();
        };
        // A narrow marker column down the left, mirroring the editor's gutter:
        // it carries the arrow on the frame being inspected and nothing else,
        // so every name in both lists starts at the same x.
        let muted = cx.theme().muted_foreground;
        let gutter = move |label: &'static str| {
            div()
                .w(px(14.))
                .flex_none()
                .text_center()
                .text_color(muted)
                .child(label)
        };
        let mut vars = v_flex().gap_0().child(
            h_flex()
                .gap_2()
                .px_1()
                .py_1()
                .child(gutter(""))
                .child(div().font_semibold().child("Variables")),
        );
        for var in &stop.variables {
            let (name, value) = (var.name.clone(), var.value.clone());
            vars = vars.child(
                h_flex()
                    .gap_2()
                    .px_1()
                    .cursor_pointer()
                    .child(gutter(""))
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(var.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .child(var.value.clone()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, window, cx| {
                            Self::open_deposit_dialog(name.clone(), value.clone(), window, cx);
                        }),
                    ),
            );
        }
        let mut stack = v_flex().gap_0().child(
            h_flex()
                .gap_2()
                .px_1()
                .py_1()
                .mt_2()
                .child(gutter(""))
                .child(div().font_semibold().child("Call stack")),
        );
        for frame in &stop.stack {
            let level = frame.level;
            // Mark the frame the panel is showing — the innermost one at every
            // stop, or whichever the user clicked into since.
            let marker = if level == stop.frame { "▸" } else { "" };
            stack = stack.child(
                h_flex()
                    .gap_2()
                    .px_1()
                    .cursor_pointer()
                    .child(gutter(marker))
                    .child(
                        div()
                            .truncate()
                            .child(format!("{} :{}", frame.target_name, frame.line)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, _| {
                            if let Some(dbg) = this.debug.as_ref() {
                                dbg.session.select_frame(level);
                            }
                        }),
                    ),
            );
        }
        div()
            .id("dbg-sidebar")
            .size_full()
            .overflow_scroll()
            // Left gutter, so the names line up off the panel edge rather than
            // against it.
            .pl_3()
            .pr_2()
            .text_size(cx.theme().mono_font_size)
            .child(vars)
            .child(stack)
            .into_any_element()
    }
}
