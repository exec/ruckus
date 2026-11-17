//! Hermetic focus, feedback and label-containment regressions.

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

fn registered(desktop: &mut Desktop) {
    desktop.model.registered("tester".into());
    desktop.model.push("#test", "<peer> Hello");
    desktop.model.select("#test");
    desktop.show_conversation();
}

#[gpui::test]
fn view_toggles_handoff_native_typing_and_preserve_drafts(cx: &mut TestAppContext) {
    for (width, height) in [(1240., 820.), (900., 640.)] {
        let (view, mut window) = desktop(cx, width, height);
        draw(&mut window);
        window.simulate_input("localhost");
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                registered(desktop);
                cx.notify();
            })
        });
        draw(&mut window);
        window.update(|window, cx| {
            assert!(
                view.read(cx)
                    .composer
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
        });
        window.simulate_input("draft");
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.toggle_connection();
                cx.notify();
            })
        });
        draw(&mut window);
        window.update(|window, cx| {
            assert!(
                view.read(cx)
                    .host
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
        });
        window.simulate_input("x");
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                assert_eq!(desktop.host.read(cx).value(), "localhostx");
                assert_eq!(desktop.composer.read(cx).value(), "draft");
                desktop.toggle_connection();
                cx.notify();
            })
        });
        draw(&mut window);
        window.simulate_input("again");
        window.update(|window, cx| {
            view.update(cx, |desktop, cx| {
                assert_eq!(desktop.composer.read(cx).value(), "draftagain");
                desktop.show_connection();
                desktop.prepare_join(window, cx);
                cx.notify();
            })
        });
        draw(&mut window);
        window.update(|window, cx| {
            let desktop = view.read(cx);
            assert!(!desktop.connection_open);
            assert!(
                desktop
                    .composer
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            assert_eq!(desktop.composer.read(cx).value(), "draftagain");
        });
    }
}

#[gpui::test]
fn offline_workspace_and_cancel_never_focus_hidden_inputs(cx: &mut TestAppContext) {
    for (width, height) in [(1240., 820.), (900., 640.)] {
        let (view, mut window) = desktop(cx, width, height);
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.show_conversation();
                cx.notify();
            })
        });
        draw(&mut window);
        window.update(|window, cx| {
            assert!(view.read(cx).workspace_focus.is_focused(window));
        });
        window.simulate_input("ignored");
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                assert!(desktop.host.read(cx).value().is_empty());
                assert!(desktop.composer.read(cx).value().is_empty());
                registered(desktop);
                desktop.cancel();
                cx.notify();
            })
        });
        draw(&mut window);
        window.update(|window, cx| {
            assert!(view.read(cx).workspace_focus.is_focused(window));
        });
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.show_connection();
                desktop.cancel();
                cx.notify();
            })
        });
        draw(&mut window);
        window.simulate_input("host");
        window.update(|window, cx| {
            let desktop = view.read(cx);
            assert!(desktop.host.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(desktop.host.read(cx).value(), "host");
            assert!(desktop.client.is_none());
        });
    }
}

#[gpui::test]
fn validation_feedback_stays_on_its_own_surface(cx: &mut TestAppContext) {
    let (view, mut window) = desktop(cx, 900., 640.);
    window.update(|window, cx| {
        view.update(cx, |desktop, cx| {
            desktop
                .account
                .update(cx, |input, cx| input.set_value("account", window, cx));
            desktop.connect(window, cx);
            assert!(
                desktop.advanced_open,
                "SASL validation reveals the invalid fields"
            );
            assert!(!desktop.connection_feedback.is_empty());
            assert!(desktop.composer_feedback.is_empty());
            assert!(desktop.client.is_none());
            desktop.show_conversation();
            cx.notify();
        })
    });
    draw(&mut window);
    assert!(window.debug_bounds("connection-feedback").is_none());
    assert!(window.debug_bounds("composer-feedback").is_none());
    window.update(|window, cx| {
        view.update(cx, |desktop, cx| {
            registered(desktop);
            desktop.advanced_open = false;
            desktop
                .composer
                .update(cx, |input, cx| input.set_value("/nick :bad", window, cx));
            desktop.submit(window, cx);
            assert!(
                !desktop.advanced_open,
                "composer errors must not open connection fields"
            );
            assert!(!desktop.composer_feedback.is_empty());
            desktop.connection_feedback.clear();
            desktop.show_connection();
            cx.notify();
        })
    });
    draw(&mut window);
    assert!(window.debug_bounds("connection-feedback").is_none());
    assert!(window.debug_bounds("composer-feedback").is_none());
    window.update(|_, cx| {
        view.update(cx, |desktop, cx| {
            desktop.model.disconnected("Explicit terminal reason");
            desktop.terminal_feedback("Explicit terminal reason");
            assert_eq!(desktop.connection_feedback, "Explicit terminal reason");
            assert!(desktop.composer_feedback.is_empty());
            cx.notify();
        })
    });
    draw(&mut window);
    assert!(window.debug_bounds("connection-feedback").is_some());

    // GPUI retains debug selectors from older frames, so use a fresh window
    // when checking which feedback surface is actually painted.
    let (view, mut window) = desktop(cx, 900., 640.);
    window.update(|window, cx| {
        view.update(cx, |desktop, cx| {
            registered(desktop);
            desktop
                .composer
                .update(cx, |input, cx| input.set_value("/nick :bad", window, cx));
            desktop.submit(window, cx);
            cx.notify();
        })
    });
    draw(&mut window);
    assert!(window.debug_bounds("connection-feedback").is_none());
    assert!(window.debug_bounds("composer-feedback").is_some());
}

fn contained(window: &mut VisualTestContext, inner: &'static str, outer: &'static str) {
    let inner = window.debug_bounds(inner).expect(inner);
    let outer = window.debug_bounds(outer).expect(outer);
    assert!(inner.size.width > px(0.));
    assert!(
        inner.origin.x >= outer.origin.x,
        "{inner:?} outside {outer:?}"
    );
    assert!(
        inner.origin.y >= outer.origin.y,
        "{inner:?} outside {outer:?}"
    );
    assert!(
        inner.right() <= outer.right(),
        "{inner:?} outside {outer:?}"
    );
    assert!(
        inner.bottom() <= outer.bottom(),
        "{inner:?} outside {outer:?}"
    );
}

#[gpui::test]
fn exec_is_fully_visible_and_send_confirmation_refreshes_the_existing_row(cx: &mut TestAppContext) {
    use ruckus_core::client::SendId;
    for (width, height) in [(1240., 820.), (900., 640.)] {
        let (view, mut window) = desktop(cx, width, height);
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                registered(desktop);
                desktop.model.nick = "exec".into();
                desktop.model.queued(SendId(7), "#test", "hello");
                cx.notify();
            })
        });
        draw(&mut window);
        let author = window.debug_bounds("chat-author").unwrap();
        let required = window.update(|window, cx| {
            let mut font = window.text_style().font();
            font.family = gpui_component::Theme::global(cx).font_family.clone();
            font.weight = gpui::FontWeight::SEMIBOLD;
            window
                .text_system()
                .shape_line(
                    "exec".into(),
                    px(13.),
                    &[gpui::TextRun {
                        len: 4,
                        font,
                        color: rgb(0xffffff).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width
        });
        assert!(
            author.size.width >= required + px(2.),
            "exec is clipped: {author:?}; text needs {required:?}"
        );
        contained(&mut window, "chat-author", "chat-author-heading");
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.model.sent(SendId(7));
                cx.notify();
            })
        });
        draw(&mut window);
        window.update(|_, cx| {
            let desktop = view.read(cx);
            assert_eq!(desktop.list_messages.last().unwrap(), "exec (sent): hello");
            assert_eq!(desktop.message_list.item_count(), 2);
        });
        let author = window.debug_bounds("chat-author").unwrap();
        assert!(author.size.width >= required + px(2.));
    }
}

#[gpui::test]
fn painted_exec_author_text_is_not_truncated_during_intrinsic_measurement(cx: &mut TestAppContext) {
    use std::{cell::RefCell, rc::Rc};
    struct Probe {
        line: String,
        layout: Rc<RefCell<Option<gpui::TextLayout>>>,
    }
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let (row, layout) = super::view::chat_row_with_layout(
                crate::presentation::message_row(&self.line),
                false,
            );
            *self.layout.borrow_mut() = Some(layout);
            div().size_full().flex().flex_col().child(row)
        }
    }
    let (_, mut window) = desktop(cx, 900., 640.);
    for width in [200., 600.] {
        for label in ["queued", "sent"] {
            let layout = Rc::new(RefCell::new(None));
            window.draw(
                gpui::point(px(0.), px(0.)),
                size(
                    gpui::AvailableSpace::Definite(px(width)),
                    gpui::AvailableSpace::Definite(px(100.)),
                ),
                |_, cx| {
                    cx.new(|_| Probe {
                        line: format!("exec ({label}): hello"),
                        layout: layout.clone(),
                    })
                    .into_any_element()
                },
            );
            let painted = layout.borrow();
            assert_eq!(painted.as_ref().unwrap().text(), "exec");
        }
    }
}

#[gpui::test]
fn long_nicknames_and_queued_badges_stay_inside_their_rows(cx: &mut TestAppContext) {
    for (width, height) in [(1240., 820.), (900., 640.)] {
        for length in [30, 500] {
            let (view, mut window) = desktop(cx, width, height);
            window.update(|_, cx| {
                view.update(cx, |desktop, cx| {
                    registered(desktop);
                    let nickname = "n".repeat(length);
                    desktop.model.nick = nickname.clone();
                    desktop
                        .model
                        .push("#test", format!("{nickname} (queued): text"));
                    cx.notify();
                })
            });
            draw(&mut window);
            contained(&mut window, "sidebar-identity-column", "sidebar-identity");
            contained(&mut window, "sidebar-nickname", "sidebar-identity-column");
            contained(&mut window, "chat-row", "conversation-messages");
            contained(&mut window, "chat-author-heading", "chat-row");
            contained(&mut window, "chat-author", "chat-author-heading");
            contained(&mut window, "queued-badge", "chat-author-heading");
            let author = window.debug_bounds("chat-author").unwrap();
            let badge = window.debug_bounds("queued-badge").unwrap();
            assert!(
                author.right() <= badge.origin.x,
                "author overlaps queued badge"
            );
        }
    }
}
