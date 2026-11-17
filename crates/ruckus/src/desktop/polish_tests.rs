//! Synthetic-window polish regressions: no network worker or real desktop.

use super::*;
use gpui::{Focusable, TestAppContext, VisualTestContext};

fn desktop(
    cx: &mut TestAppContext,
    width: f32,
    height: f32,
) -> (Entity<Desktop>, VisualTestContext) {
    let (view, window) = cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::install(cx);
        let mut view = None;
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(0.), px(0.)),
                        size(px(width), px(height)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    let desktop = cx.new(|cx| Desktop::new(window, cx));
                    view = Some(desktop.clone());
                    cx.new(|cx| Root::new(desktop, window, cx))
                },
            )
            .expect("synthetic window");
        (view.expect("desktop"), window)
    });
    (view, VisualTestContext::from_window(window.into(), cx))
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

fn contained(cx: &mut VisualTestContext, inner: &'static str, outer: &'static str) {
    let inner = cx.debug_bounds(inner).expect(inner);
    let outer = cx.debug_bounds(outer).expect(outer);
    assert!(inner.size.width > px(0.) && inner.size.height > px(0.));
    assert!(inner.origin.x >= outer.origin.x && inner.right() <= outer.right());
    assert!(inner.origin.y >= outer.origin.y && inner.bottom() <= outer.bottom());
}

#[gpui::test]
fn empty_workspace_paints_truthful_copy_and_a_working_next_step(cx: &mut TestAppContext) {
    for (width, height) in [(900., 640.), (1240., 820.)] {
        for connected in [false, true] {
            let (view, mut window) = desktop(cx, width, height);
            window.update(|_, cx| {
                view.update(cx, |desktop, cx| {
                    // Synthetic registration only: never create a client.
                    if connected {
                        desktop.model.registered("tester".into());
                        desktop.model.buffers.clear();
                    }
                    desktop.show_conversation();
                    cx.notify();
                })
            });
            draw(&mut window);
            for label in ["empty-title", "empty-detail", "empty-action"] {
                contained(&mut window, label, "empty-conversation");
                contained(&mut window, label, "conversation-messages");
            }
            let action = window.debug_bounds("empty-action").unwrap();
            window.simulate_click(action.center(), gpui::Modifiers::default());
            draw(&mut window);
            window.update(|window, cx| {
                let desktop = view.read(cx);
                assert!(desktop.client.is_none());
                if connected {
                    assert!(!desktop.connection_open);
                    assert_eq!(desktop.composer.read(cx).value(), "/join ");
                    assert!(
                        desktop
                            .composer
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                    );
                } else {
                    assert!(desktop.connection_open);
                    assert!(desktop.host.read(cx).focus_handle(cx).is_focused(window));
                }
            });
        }
    }
}

#[gpui::test]
fn connection_hint_fits_the_card_without_displacing_controls(cx: &mut TestAppContext) {
    for (width, height) in [(900., 640.), (1240., 820.)] {
        let (_, mut window) = desktop(cx, width, height);
        draw(&mut window);
        contained(&mut window, "connection-hint", "connection-actions");
        let hint = window.debug_bounds("connection-hint").unwrap();
        let actions = window.debug_bounds("connection-actions").unwrap();
        // The trailing native Connect button needs its own non-overlapping lane.
        assert!(actions.right() - hint.right() >= px(90.));
    }
}

#[gpui::test]
fn offline_history_keeps_an_actionable_connection_path_and_preserves_draft(
    cx: &mut TestAppContext,
) {
    for (width, height) in [(900., 640.), (1240., 820.)] {
        let (view, mut window) = desktop(cx, width, height);
        window.update(|window, cx| {
            view.update(cx, |desktop, cx| {
                desktop.model.push("#rust", "<friend> retained history");
                desktop.model.select("#rust");
                desktop.composer.update(cx, |input, cx| {
                    input.set_value("unsent café 🦀", window, cx)
                });
                desktop.show_conversation();
                cx.notify();
            });
        });
        draw(&mut window);
        for selector in ["composer-state-hint", "composer-connection-action"] {
            contained(&mut window, selector, "composer-connection-state");
            contained(&mut window, selector, "composer-area");
        }
        let hint = window.debug_bounds("composer-state-hint").unwrap();
        let action = window.debug_bounds("composer-connection-action").unwrap();
        assert!(hint.right() <= action.origin.x);
        let heading = window.debug_bounds("conversation-heading").unwrap();
        let messages = window.debug_bounds("conversation-messages").unwrap();
        let composer = window.debug_bounds("composer-area").unwrap();
        assert_eq!(heading.origin.x, messages.origin.x);
        assert_eq!(composer.origin.x, messages.origin.x);
        assert_eq!(composer.right(), messages.right());
        assert!(messages.size.height > px(100.));
        window.simulate_click(action.center(), gpui::Modifiers::default());
        draw(&mut window);
        window.update(|window, cx| {
            let desktop = view.read(cx);
            assert!(desktop.connection_open);
            assert!(desktop.client.is_none());
            assert_eq!(desktop.composer.read(cx).value(), "unsent café 🦀");
            assert_eq!(desktop.model.selected, "#rust");
            assert!(desktop.host.read(cx).focus_handle(cx).is_focused(window));
        });
        // Returning to the workspace must not discard the retained history
        // or hide the recovery action behind the connection form.
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.show_conversation();
                cx.notify();
            });
        });
        draw(&mut window);
        contained(&mut window, "composer-connection-action", "composer-area");
        window.update(|_, cx| {
            assert_eq!(view.read(cx).list_messages.len(), 1);
            assert_eq!(view.read(cx).composer.read(cx).value(), "unsent café 🦀");
        });
    }
}

#[gpui::test]
fn registered_composer_keeps_guidance_and_native_input_in_the_message_column(
    cx: &mut TestAppContext,
) {
    for (width, height) in [(900., 640.), (1240., 820.)] {
        let (view, mut window) = desktop(cx, width, height);
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.model.registered("tester".into());
                desktop.model.push("#rust", "<friend> hello");
                desktop.model.select("#rust");
                desktop.show_conversation();
                cx.notify();
            });
        });
        draw(&mut window);
        contained(&mut window, "composer-guidance", "composer-area");
        contained(&mut window, "field:Composer", "composer-area");
        let messages = window.debug_bounds("conversation-messages").unwrap();
        let composer = window.debug_bounds("composer-area").unwrap();
        assert!(messages.bottom() <= composer.origin.y);
        assert!(messages.size.height > px(100.));
        window.update(|window, cx| {
            assert!(
                view.read(cx)
                    .composer
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
        });
        window.simulate_input("a native draft");
        draw(&mut window);
        window.update(|_, cx| {
            assert_eq!(view.read(cx).composer.read(cx).value(), "a native draft");
        });
    }
}

#[gpui::test]
fn every_outgoing_status_keeps_its_badge_and_full_author_source(cx: &mut TestAppContext) {
    use std::{cell::RefCell, rc::Rc};
    struct Probe {
        line: String,
        layout: Rc<RefCell<Option<gpui::TextLayout>>>,
    }
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let (row, layout) = crate::desktop::view::chat_row_with_layout(
                crate::presentation::message_row(&self.line),
                false,
            );
            *self.layout.borrow_mut() = Some(layout);
            div().size_full().flex().flex_col().child(row)
        }
    }
    let (_, mut window) = desktop(cx, 900., 640.);
    for width in [200., 600.] {
        for status in ["queued", "sent", "unconfirmed"] {
            for author in ["exec".to_owned(), "nickname".repeat(60)] {
                let layout = Rc::new(RefCell::new(None));
                window.draw(
                    gpui::point(px(0.), px(0.)),
                    size(
                        gpui::AvailableSpace::Definite(px(width)),
                        gpui::AvailableSpace::Definite(px(100.)),
                    ),
                    |_, cx| {
                        cx.new(|_| Probe {
                            line: format!("{author} ({status}): hello"),
                            layout: layout.clone(),
                        })
                        .into_any_element()
                    },
                );
                assert_eq!(layout.borrow().as_ref().unwrap().text(), author);
                contained(&mut window, "chat-author", "chat-author-heading");
                contained(&mut window, "queued-badge", "chat-author-heading");
                let author = window.debug_bounds("chat-author").unwrap();
                let badge = window.debug_bounds("queued-badge").unwrap();
                assert!(author.right() <= badge.origin.x);
            }
        }
    }
}
