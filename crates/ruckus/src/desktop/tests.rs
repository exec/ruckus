//! Synthetic native windows, never the user's desktop or a live IRC transport.

use super::*;
use gpui::{EntityInputHandler, Focusable, TestAppContext, VisualTestContext};
use gpui_component::{PixelsExt, Theme, ThemeMode};
use ruckus_core::irc::Message;

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
        (view.expect("desktop entity"), window)
    });
    (view, VisualTestContext::from_window(window.into(), cx))
}

fn draw(cx: &mut VisualTestContext) {
    // Do not drain the executor: Desktop owns a repeating 50 ms poll. Draw a
    // bounded frame directly, including native inputs and Root's overlays.
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

#[gpui::test]
fn case_mapping_merge_invalidates_history_even_when_counters_alias(cx: &mut TestAppContext) {
    let (view, mut window) = desktop(cx, 900., 640.);
    window.update(|_, cx| {
        view.update(cx, |desktop, _| {
            desktop
                .model
                .set_case_mapping(ruckus_core::isupport::CaseMapping::Ascii);
            for index in 0..501 {
                desktop.model.push("Nick[", format!("old {index}"));
            }
            desktop.model.push("Nick{", "merged row");
            desktop.model.select("Nick[");
            desktop.sync_message_list();
            let cache = desktop.list_messages.clone();
            let counters = (desktop.list_revision, desktop.list_edits);
            desktop
                .model
                .set_case_mapping(ruckus_core::isupport::CaseMapping::Rfc1459);
            desktop.sync_message_list();
            assert_eq!(counters, (desktop.list_revision, desktop.list_edits));
            assert!(!Arc::ptr_eq(&cache, &desktop.list_messages));
            assert_eq!(desktop.list_messages.last().unwrap(), "merged row");
            assert_eq!(desktop.list_messages.first().unwrap(), "old 2");
        });
    });
}

fn assert_input_start_visible(
    window: &mut VisualTestContext,
    input: &Entity<InputState>,
    selector: &'static str,
) {
    let bounds = window.debug_bounds(selector).expect("field was painted");
    assert!(
        bounds.size.width > px(100.),
        "{selector} is too narrow: {bounds:?}"
    );
    let indices = window.update(|window, cx| {
        input.update(cx, |input, cx| {
            let mut indices = Vec::new();
            for x in (0..48).step_by(2) {
                for y in (0..bounds.size.height.as_f32() as usize).step_by(2) {
                    let point = bounds.origin + gpui::point(px(x as f32), px(y as f32));
                    if let Some(index) = input.character_index_for_point(point, window, cx) {
                        indices.push(index);
                    }
                }
            }
            indices
        })
    });
    assert!(
        indices.contains(&0),
        "{selector}: short text starts outside {bounds:?}; hit indices: {indices:?}"
    );
}

#[gpui::test]
fn short_host_text_stays_visible_from_the_start_while_typing(cx: &mut TestAppContext) {
    for (width, height) in [(1240., 820.), (900., 640.)] {
        let (view, mut window) = desktop(cx, width, height);
        draw(&mut window);
        let host = window.update(|_, cx| view.read(cx).host.clone());
        for character in "localhost".chars() {
            window.update(|window, cx| {
                host.update(cx, |input, cx| {
                    input.replace_text_in_range(None, &character.to_string(), window, cx);
                });
            });
            draw(&mut window);
            assert_input_start_visible(&mut window, &host, "field:Server address");
        }
    }
}

#[gpui::test]
fn every_input_keeps_a_visible_text_viewport_during_burst_typing(cx: &mut TestAppContext) {
    for width in [900., 1240.] {
        // Extra height exposes all advanced fields at once. Default/minimum
        // heights are covered independently by the host and composer checks.
        let (view, mut window) = desktop(cx, width, 1200.);
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                desktop.advanced_open = true;
                cx.notify();
            })
        });
        draw(&mut window);
        let fields = window.update(|_, cx| {
            let desktop = view.read(cx);
            [
                (desktop.host.clone(), "field:Server address"),
                (desktop.nick.clone(), "field:Nickname"),
                (desktop.channel.clone(), "field:Initial channel · optional"),
                (desktop.port.clone(), "field:Port"),
                (desktop.account.clone(), "field:SASL account · optional"),
                (desktop.password.clone(), "field:SASL password"),
            ]
        });
        for (input, selector) in fields {
            window.update(|window, cx| {
                input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                })
            });
            draw(&mut window);
            window.simulate_input("typed123");
            draw(&mut window);
            window.update(|_, cx| assert_eq!(input.read(cx).value(), "typed123"));
            assert_input_start_visible(&mut window, &input, selector);
        }
    }
}

#[gpui::test]
fn composer_text_and_home_position_remain_visible_after_overflow_and_resize(
    cx: &mut TestAppContext,
) {
    let (view, mut window) = desktop(cx, 1240., 820.);
    let composer = window.update(|window, cx| {
        view.update(cx, |desktop, cx| {
            connected(desktop);
            desktop
                .composer
                .update(cx, |input, cx| input.focus(window, cx));
            desktop.composer.clone()
        })
    });
    draw(&mut window);
    window.simulate_input("shortdraft");
    draw(&mut window);
    assert_input_start_visible(&mut window, &composer, "field:Composer");
    let long = "long draft ".repeat(100);
    window.update(|window, cx| composer.update(cx, |input, cx| input.set_value(&long, window, cx)));
    draw(&mut window);
    window.simulate_keystrokes("home");
    draw(&mut window);
    assert_input_start_visible(&mut window, &composer, "field:Composer");
    window.simulate_resize(size(px(900.), px(640.)));
    draw(&mut window);
    assert_input_start_visible(&mut window, &composer, "field:Composer");
    window.update(|window, cx| {
        composer.update(cx, |input, cx| {
            input.set_value("short again", window, cx);
        })
    });
    draw(&mut window);
    assert_input_start_visible(&mut window, &composer, "field:Composer");
    window.update(|_, cx| assert_eq!(composer.read(cx).value(), "short again"));
}

fn connected(desktop: &mut Desktop) {
    let mut state = State::new("tester");
    for (source, command, params) in [
        ("tester!u@host", "JOIN", vec!["#ruckus"]),
        (
            "server",
            "353",
            vec!["tester", "=", "#ruckus", "@tester friend"],
        ),
        (
            "server",
            "332",
            vec!["tester", "#ruckus", "A synthetic topic"],
        ),
    ] {
        state.apply(&Message {
            source: Some(source.into()),
            command: command.into(),
            params: params.into_iter().map(String::from).collect(),
            tags: Default::default(),
        });
    }
    desktop.model.registered("tester".into());
    desktop.model.sync_snapshot(&state, true, false);
    desktop.model.select("#ruckus");
    desktop.snapshot = Arc::new(state);
    desktop.connection_open = false;
}

#[gpui::test]
fn first_run_and_advanced_settings_render_at_supported_sizes(cx: &mut TestAppContext) {
    for (width, height) in [(1240., 820.), (900., 640.)] {
        let (view, mut window) = desktop(cx, width, height);
        draw(&mut window);
        window.update(|_, cx| {
            let desktop = view.read(cx);
            assert!(desktop.connection_open);
            assert!(!desktop.advanced_open);
            assert!(desktop.tls);
            assert!(desktop.client.is_none());
            assert!(!desktop.model.connected);
            assert!(desktop.snapshot.channels.is_empty());
            assert_eq!(desktop.port.read(cx).value(), "6697");
            assert!(desktop.host.read(cx).value().is_empty());
            assert!(desktop.composer.read(cx).value().is_empty());
        });
        for advanced in [true, false] {
            window.update(|_, cx| {
                view.update(cx, |desktop, cx| {
                    desktop.advanced_open = advanced;
                    cx.notify();
                });
            });
            draw(&mut window);
            window.update(|_, cx| assert_eq!(view.read(cx).advanced_open, advanced));
        }
    }
}

#[gpui::test]
fn synthetic_channel_renders_members_and_connection_settings(cx: &mut TestAppContext) {
    for (width, height) in [(1240., 820.), (900., 640.)] {
        let (view, mut window) = desktop(cx, width, height);
        window.update(|_, cx| {
            view.update(cx, |desktop, cx| {
                connected(desktop);
                desktop
                    .model
                    .push("#ruckus", "<friend> Hello from a synthetic event");
                desktop
                    .model
                    .push("#ruckus", "<friend> A grouped follow-up");
                desktop
                    .model
                    .push("#ruckus", "tester (queued): A queued message");
                cx.notify();
            });
        });
        for (setup, members) in [(false, true), (false, false), (true, false), (false, true)] {
            window.update(|_, cx| {
                view.update(cx, |desktop, cx| {
                    desktop.connection_open = setup;
                    desktop.show_members = members;
                    cx.notify();
                });
            });
            draw(&mut window);
            window.update(|_, cx| {
                let desktop = view.read(cx);
                assert!(
                    desktop.client.is_none(),
                    "render must not create a transport"
                );
                assert_eq!(
                    desktop.snapshot.channel("#ruckus").unwrap().members.len(),
                    2
                );
                assert_eq!(desktop.model.selected, "#ruckus");
                assert_eq!(desktop.connection_open, setup);
                assert_eq!(desktop.show_members, members);
            });
        }
    }
}

#[gpui::test]
fn join_shortcut_preserves_every_nonempty_draft(cx: &mut TestAppContext) {
    let (view, mut window) = desktop(cx, 1240., 820.);
    for online in [false, true] {
        for draft in [
            "",
            "unfinished message",
            " ",
            "/msg friend unsent",
            "héllo 👋",
        ] {
            window.update(|window, cx| {
                view.update(cx, |desktop, cx| {
                    desktop.model.connected = online;
                    desktop.connection_open = true;
                    desktop.composer_feedback.clear();
                    desktop.composer.update(cx, |input, cx| {
                        input.set_value(draft, window, cx);
                    });
                    desktop.prepare_join(window, cx);
                    assert_eq!(
                        desktop.composer.read(cx).value(),
                        if online && draft.is_empty() {
                            "/join "
                        } else {
                            draft
                        }
                    );
                    assert_eq!(desktop.connection_open, !online);
                    if online {
                        assert_eq!(desktop.pending_focus, Some(PendingFocus::Composer));
                        if !draft.is_empty() {
                            assert!(desktop.composer_feedback.contains("draft is safe"));
                        }
                    }
                });
            });
            draw(&mut window);
        }
    }
}

#[gpui::test]
fn composer_focus_and_native_utf16_editing_survive_render(cx: &mut TestAppContext) {
    let (view, mut window) = desktop(cx, 1240., 820.);
    window.update(|_, cx| {
        view.update(cx, |desktop, cx| {
            connected(desktop);
            desktop.request_visible_focus();
            cx.notify();
        });
    });
    draw(&mut window);
    window.update(|window, cx| {
        let desktop = view.read(cx);
        assert!(
            desktop.pending_focus.is_none(),
            "deferred focus is consumed by render"
        );
        let composer = desktop.composer.clone();
        assert!(composer.read(cx).focus_handle(cx).is_focused(window));
        composer.update(cx, |input, cx| {
            input.replace_text_in_range(None, "hi 👋", window, cx);
            assert_eq!(input.value(), "hi 👋");
            assert_eq!(
                input.selected_text_range(false, window, cx).unwrap().range,
                5..5
            );
            input.replace_text_in_range(Some(3..5), "there", window, cx);
            assert_eq!(input.value(), "hi there");
            assert_eq!(
                input.selected_text_range(false, window, cx).unwrap().range,
                8..8
            );
        });
    });
    window.simulate_keystrokes("enter");
    draw(&mut window);
    window.update(|window, cx| {
        let composer = &view.read(cx).composer;
        assert_eq!(composer.read(cx).value(), "hi there");
        assert!(composer.read(cx).focus_handle(cx).is_focused(window));
    });
}

#[gpui::test]
fn invalid_settings_keep_setup_open_without_starting_network(cx: &mut TestAppContext) {
    let (view, mut window) = desktop(cx, 900., 640.);
    window.update(|window, cx| {
        view.update(cx, |desktop, cx| {
            desktop.connect(window, cx);
            assert!(desktop.client.is_none());
            assert!(desktop.connection_open);
            assert!(desktop.connection_feedback.contains("Enter a server"));
        });
    });
    draw(&mut window);
    window.update(|window, cx| {
        let host = view.read(cx).host.clone();
        host.update(cx, |input, cx| {
            input.focus(window, cx);
            input.replace_text_in_range(None, "irc.example.org", window, cx);
            assert_eq!(input.value(), "irc.example.org");
            assert!(input.focus_handle(cx).is_focused(window));
        });
    });
    draw(&mut window);
}

#[gpui::test]
fn enter_in_the_connection_form_uses_the_real_validated_submit_path(cx: &mut TestAppContext) {
    let (view, mut window) = desktop(cx, 900., 640.);
    draw(&mut window);
    window.update(|window, cx| {
        let host = view.read(cx).host.clone();
        host.update(cx, |input, cx| input.focus(window, cx));
    });
    window.simulate_keystrokes("enter");
    draw(&mut window);
    window.update(|_, cx| {
        let desktop = view.read(cx);
        assert!(desktop.client.is_none());
        assert!(desktop.connection_feedback.contains("Enter a server"));
        assert!(desktop.connection_open);
    });
}

#[gpui::test]
fn native_controls_use_the_desktop_dark_palette(cx: &mut TestAppContext) {
    let (_, mut window) = desktop(cx, 1240., 820.);
    window.update(|_, cx| {
        let theme = Theme::global(cx);
        assert_eq!(theme.mode, ThemeMode::Dark);
        assert_eq!(theme.background, rgb(crate::theme::CANVAS).into());
        assert_eq!(theme.foreground, rgb(crate::theme::TEXT).into());
        assert_eq!(theme.input, rgb(crate::theme::BORDER).into());
        assert_eq!(theme.primary, rgb(crate::theme::ACCENT).into());
        assert_eq!(theme.ring, theme.primary);
    });
    draw(&mut window);
}

#[gpui::test]
fn variable_height_history_is_virtualized_and_ring_updates_keep_the_anchor(
    cx: &mut TestAppContext,
) {
    let (view, mut window) = desktop(cx, 900., 640.);
    window.update(|_, cx| {
        view.update(cx, |desktop, cx| {
            connected(desktop);
            for index in 0..500 {
                desktop.model.push(
                    "#ruckus",
                    format!("<friend> {index} {}", "word ".repeat(800)),
                );
            }
            cx.notify();
        });
    });
    draw(&mut window);
    let cache = window.update(|_, cx| {
        let desktop = view.read(cx);
        assert_eq!(desktop.message_list.item_count(), 500);
        assert!(desktop.message_list.viewport_bounds().size.height > px(100.));
        assert!(desktop.message_list.bounds_for_item(0).is_none());
        desktop.list_messages.clone()
    });
    draw(&mut window);
    window.update(|_, cx| {
        view.update(cx, |desktop, cx| {
            assert!(
                Arc::ptr_eq(&cache, &desktop.list_messages),
                "unchanged frames share the cache"
            );
            desktop.message_list.scroll_to(ListOffset {
                item_ix: 50,
                offset_in_item: px(0.),
            });
            desktop.model.push("#ruckus", "<friend> newer");
            desktop.model.push("#ruckus", "<friend> newest");
            desktop.sync_message_list();
            assert_eq!(desktop.message_list.item_count(), 500);
            assert_eq!(desktop.message_list.logical_scroll_top().item_ix, 48);
            assert!(desktop.list_messages[0].starts_with("<friend> 2 "));
            assert_eq!(desktop.list_messages.last().unwrap(), "<friend> newest");
            cx.notify();
        });
    });
    draw(&mut window);
}
