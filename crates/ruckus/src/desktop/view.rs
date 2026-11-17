//! Conversation-first desktop composition. Networking stays in the shell.

use super::*;
use crate::{
    presentation::{
        MessageRow, RowKind, author_color, author_initial, composer_guidance,
        empty_conversation_copy, message_row, same_author,
    },
    theme as t,
};
use gpui::{Div, FontWeight, list, rgba};

fn field(title: &str, input: Input) -> Div {
    div()
        .debug_selector(|| format!("field:{title}"))
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(t::label(title))
        // Resolve percentage-based inputs in their own row. The label stack
        // keeps its natural height; callers opt into horizontal flex growth.
        .child(
            div()
                .flex()
                .w_full()
                .min_w_0()
                .child(input.w_full().min_w_0().flex_1()),
        )
}

fn avatar(name: &str, size: f32) -> Div {
    let color = author_color(name);
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .bg(rgba((color << 8) | 0x20))
        .text_color(rgb(color))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .child(author_initial(name))
}

fn chat_row(row: MessageRow, grouped: bool) -> Div {
    chat_row_with_layout(row, grouped).0
}

pub(super) fn chat_row_with_layout(row: MessageRow, grouped: bool) -> (Div, gpui::TextLayout) {
    if row.kind == RowKind::System {
        return (
            div()
                .flex()
                .items_start()
                .gap_3()
                .py_1()
                .pl(px(10.))
                .text_size(px(12.))
                .text_color(rgb(t::MUTED))
                .whitespace_normal()
                .child(div().w(px(26.)).flex_shrink_0().child("·"))
                .child(div().flex_1().min_w_0().child(row.body)),
            gpui::TextLayout::default(),
        );
    }
    let author = row.author.as_deref().unwrap_or("");
    let author_text = gpui::StyledText::new(author.to_owned());
    let author_layout = author_text.layout().clone();
    let element = div()
        .debug_selector(|| "chat-row".into())
        .flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap_3()
        .pt(px(if grouped { 2. } else { 14. }))
        .pb_1()
        .child(if grouped {
            div().w(px(36.)).flex_shrink_0()
        } else {
            avatar(author, 36.)
        })
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .when(!grouped, |view| {
                    view.child(
                        div()
                            .debug_selector(|| "chat-author-heading".into())
                            .flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(author_color(author)))
                            .child(
                                div()
                                    .debug_selector(|| "chat-author".into())
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    // Do not truncate the string during an
                                    // intrinsic measurement pass. Clip only
                                    // genuinely overlong names at final bounds.
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .pr(px(2.))
                                    .child(author_text),
                            )
                            .when(
                                matches!(
                                    row.kind,
                                    RowKind::Queued | RowKind::Sent | RowKind::Unconfirmed
                                ),
                                |view| {
                                    view.child(
                                        div()
                                            .debug_selector(|| "queued-badge".into())
                                            .flex_shrink_0()
                                            .text_size(px(10.))
                                            .font_weight(FontWeight::NORMAL)
                                            .text_color(rgb(match row.kind {
                                                RowKind::Queued | RowKind::Unconfirmed => t::AMBER,
                                                _ => t::MUTED,
                                            }))
                                            .child(match row.kind {
                                                RowKind::Sent => "SENT",
                                                RowKind::Unconfirmed => "UNCONFIRMED",
                                                _ => "QUEUED",
                                            }),
                                    )
                                },
                            ),
                    )
                })
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(rgb(t::TEXT))
                        .whitespace_normal()
                        .child(row.body),
                ),
        );
    (element, author_layout)
}

impl Desktop {
    fn connection_color(&self) -> u32 {
        if self.model.connected {
            t::GREEN
        } else if self.client.is_some() {
            t::AMBER
        } else {
            t::MUTED
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(64.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(rgb(t::BORDER))
            .child(
                div()
                    .w(px(t::SIDEBAR_WIDTH))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .bg(rgb(t::ACCENT))
                            .text_color(rgb(t::SIDEBAR))
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(20.))
                            .child("r"),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(20.))
                            .child("ruckus"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(t::CONVERSATION_INSET))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .mr_4()
                            .text_size(px(12.))
                            .text_ellipsis()
                            .text_color(rgb(t::MUTED))
                            .child(if self.connection_open {
                                "Connection settings"
                            } else if self.model.connected {
                                "Workspace"
                            } else if self.client.is_some() {
                                "Connecting…"
                            } else {
                                "Offline workspace"
                            }),
                    )
                    .child(
                        Button::new("connection-settings")
                            .label(if self.connection_open {
                                "Back to workspace"
                            } else {
                                "Connection"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_connection();
                                cx.notify();
                            })),
                    ),
            )
    }

    fn buffer_row(
        &self,
        target: &str,
        symbol: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let selected = target == self.model.selected && !self.connection_open;
        let target = target.to_owned();
        let title = if target == STATUS {
            "Network activity".into()
        } else if symbol == "#" {
            clean(&target.chars().skip(1).collect::<String>())
        } else {
            clean(&target)
        };
        let symbol = if symbol == "#" {
            clean(&target.chars().take(1).collect::<String>())
        } else {
            symbol.to_owned()
        };
        div()
            .id(SharedString::from(target.clone()))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(if selected { 0x4b4269 } else { t::SIDEBAR }))
            .bg(rgb(if selected {
                t::ACCENT_SURFACE
            } else {
                t::SIDEBAR
            }))
            .text_color(rgb(if selected { t::TEXT } else { t::MUTED }))
            .cursor_pointer()
            .hover(|view| view.bg(rgb(t::RAISED)))
            .child(
                div()
                    .w(px(16.))
                    .text_size(px(16.))
                    .text_color(rgb(if selected { t::ACCENT } else { t::MUTED }))
                    .child(symbol),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .text_ellipsis()
                    .child(title),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.select(&target);
                this.show_conversation();
                this.follow_messages = true;
                cx.notify();
            }))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let is_channel = |target: &str| self.model.is_channel(target);
        let channels: Vec<_> = self
            .model
            .buffers
            .keys()
            .filter(|target| target.as_str() != STATUS && is_channel(target))
            .cloned()
            .collect();
        let peers: Vec<_> = self
            .model
            .buffers
            .keys()
            .filter(|target| target.as_str() != STATUS && !is_channel(target))
            .cloned()
            .collect();
        div()
            .w(px(t::SIDEBAR_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(rgb(t::SIDEBAR))
            .border_r_1()
            .border_color(rgb(t::BORDER))
            .child(
                div()
                    .m_3()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(t::PANEL))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(t::label("YOUR NETWORK"))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_ellipsis()
                            .child(if self.client.is_some() {
                                clean(&self.host.read(cx).value())
                            } else {
                                "No network connected".into()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(11.))
                            .text_color(rgb(self.connection_color()))
                            .child(t::dot(self.connection_color()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .child(clean(&self.model.status)),
                            ),
                    ),
            )
            .child(
                div()
                    .id("conversation-sidebar")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.buffer_row(STATUS, "≋", cx))
                    .child(
                        div()
                            .mt_6()
                            .mb_2()
                            .px_3()
                            .flex()
                            .justify_between()
                            .child(t::label("CHANNELS"))
                            .child(t::label(channels.len().to_string())),
                    )
                    .children(
                        channels
                            .iter()
                            .map(|target| self.buffer_row(target, "#", cx)),
                    )
                    .when(channels.is_empty(), |view| {
                        view.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_size(px(12.))
                                .text_color(rgb(t::MUTED))
                                .child(if self.model.connected {
                                    "No channels joined yet."
                                } else {
                                    "Connect before joining a channel."
                                }),
                        )
                    })
                    .child(
                        div().mt_2().child(
                            Button::new("join-channel")
                                .label("+  Join a channel")
                                .disabled(!self.model.connected)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.prepare_join(window, cx)
                                    }),
                                ),
                        ),
                    )
                    .when(!peers.is_empty(), |view| {
                        view.child(
                            div()
                                .mt_6()
                                .mb_2()
                                .px_3()
                                .child(t::label("DIRECT MESSAGES")),
                        )
                    })
                    .children(peers.iter().map(|target| self.buffer_row(target, "@", cx))),
            )
            .child(
                div()
                    .p_4()
                    .debug_selector(|| "sidebar-identity".into())
                    .border_t_1()
                    .border_color(rgb(t::BORDER))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(avatar(
                        if self.model.nick.is_empty() {
                            "?"
                        } else {
                            &self.model.nick
                        },
                        32.,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .debug_selector(|| "sidebar-identity-column".into())
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .debug_selector(|| "sidebar-nickname".into())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_ellipsis()
                                    .child(if self.model.nick.is_empty() {
                                        "Make yourself at home".into()
                                    } else {
                                        clean(&self.model.nick)
                                    }),
                            )
                            .child(div().text_size(px(11.)).text_color(rgb(t::MUTED)).child(
                                if self.model.connected {
                                    "Connected as you"
                                } else if self.client.is_some() {
                                    "Connecting…"
                                } else {
                                    "You're offline"
                                },
                            )),
                    ),
            )
    }

    fn connection_card(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let busy = self.client.is_some();
        div().id("connection-scroll").w_full().flex_1().min_h_0().overflow_y_scroll()
            .child(
                div().w_full().max_w(px(560.)).mx_auto().px_6().py_8().flex().flex_col().gap_6()
                    .child(div().flex().flex_col().gap_3()
                        .child(t::label(if busy { "YOUR CONNECTION" } else { "WELCOME TO RUCKUS" }))
                        .child(div().text_size(px(30.)).font_weight(FontWeight::SEMIBOLD)
                            .child(if busy { "A place to stay in touch." } else { "Good conversations start here." }))
                        .child(div().text_size(px(14.)).text_color(rgb(t::MUTED))
                            .child("Your people. Your channels. Your corner of the internet.")))
                    .child(
                        div().p_6().rounded_xl().border_1().border_color(rgb(t::BORDER))
                            .bg(rgb(t::PANEL)).flex().flex_col().gap_5()
                            .child(div().flex().items_center().justify_between()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(if busy { "Connection settings" } else { "Connect to a network" }))
                                .child(t::badge(if self.tls { "TLS configured" } else { "Not encrypted" }, if self.tls { t::ACCENT } else { t::AMBER })))
                            .child(field("Server address", Input::new(&self.host).disabled(busy)))
                            .child(div().flex().gap_4()
                                .child(field("Nickname", Input::new(&self.nick).disabled(busy)).flex_1())
                                .child(field("Initial channel · optional", Input::new(&self.channel).disabled(busy)).flex_1()))
                            .child(Button::new("advanced-connection")
                                .label(if self.advanced_open { "−  Hide advanced settings" } else { "+  Authentication & advanced" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.advanced_open = !this.advanced_open;
                                    if !this.advanced_open {
                                        this.request_visible_focus();
                                    }
                                    cx.notify();
                                })))
                            .when(self.advanced_open, |view| view.child(
                                div().flex().flex_col().gap_4().pt_4().border_t_1().border_color(rgb(t::BORDER))
                                    .child(div().flex().items_end().gap_4()
                                        .child(field("Port", Input::new(&self.port).disabled(busy)).flex_1())
                                        .child(Button::new("tls").label(if self.tls { "TLS on" } else { "TLS off · insecure" })
                                            .disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                                                this.tls = !this.tls;
                                                cx.notify();
                                            }))))
                                    .child(div().flex().gap_4()
                                        .child(field("SASL account · optional", Input::new(&self.account).disabled(busy)).flex_1())
                                        .child(field("SASL password", Input::new(&self.password).disabled(busy)).flex_1()))
                                    .child(div().text_size(px(11.)).text_color(rgb(t::MUTED))
                                        .child("SASL PLAIN requires verified TLS. Your password is never logged."))))
                            .when(!self.connection_feedback.is_empty(), |view| view.child(self.feedback_view(&self.connection_feedback, "connection-feedback")))
                            .child(div().debug_selector(|| "connection-actions".into()).flex().items_center().justify_between().gap_3()
                                .child(div().debug_selector(|| "connection-hint".into()).flex_1().min_w_0().whitespace_normal().text_size(px(11.)).text_color(rgb(t::MUTED))
                                    .child(if busy { "Settings are locked during this session." } else { "Enter or Connect to connect. Nothing connects automatically." }))
                                .child(if busy {
                                    Button::new("cancel-connect").label(if self.model.connected { "Disconnect" } else { "Cancel" })
                                        .on_click(cx.listener(|this, _, _, cx| { this.cancel(); cx.notify(); }))
                                } else {
                                    Button::new("connect").primary().label("Connect  →")
                                        .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx)))
                                })),
                    )
                    .child(div().flex().items_start().gap_3().px_1().text_size(px(12.)).text_color(rgb(t::MUTED))
                        .child(div().text_color(rgb(t::ACCENT)).child("#"))
                        .child("Already know your way around? Join channels with /join, or start a private conversation with /msg.")),
            )
    }

    fn feedback_view(&self, message: &str, selector: &'static str) -> Div {
        let color = if self.client.is_some() && !self.model.connected {
            t::MUTED
        } else {
            t::AMBER
        };
        div()
            .debug_selector(move || selector.into())
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(t::BORDER))
            .text_size(px(12.))
            .text_color(rgb(color))
            .child(clean(message))
    }

    fn conversation(&self, cx: &mut Context<Self>) -> Div {
        let is_network = self.model.selected == STATUS;
        let channel = self.snapshot.channel(&self.model.selected);
        let lines = self.list_messages.clone();
        let messages_empty = lines.is_empty();
        let (empty_title, empty_detail) =
            empty_conversation_copy(is_network, self.model.connected, self.client.is_some());
        let topic = channel
            .map(|channel| clean(&channel.topic.text))
            .unwrap_or_default();
        let selected = if is_network {
            "Network activity".into()
        } else {
            clean(&self.model.selected)
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .debug_selector(|| "conversation-heading".into())
                    .px(px(t::CONVERSATION_INSET))
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(t::BORDER))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_ellipsis()
                                    .child(selected),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(t::MUTED))
                                    .text_ellipsis()
                                    .child(if !topic.is_empty() {
                                        topic
                                    } else if is_network {
                                        "Registration, connection notices and command feedback"
                                            .into()
                                    } else if channel.is_some() {
                                        "No current topic available".into()
                                    } else if self.model.is_channel(&self.model.selected) {
                                        "Channel history · no current topic available".into()
                                    } else {
                                        "Direct conversation".into()
                                    }),
                            ),
                    )
                    .when(channel.is_some(), |view| {
                        view.child(
                            Button::new("toggle-members")
                                .label(format!(
                                    "{} members",
                                    channel.map(|c| c.members.len()).unwrap_or(0)
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_members = !this.show_members;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("messages")
                            .debug_selector(|| "conversation-messages".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .when(messages_empty, |view| {
                                view.child(
                                    div()
                                        .debug_selector(|| "empty-conversation".into())
                                        .px_6()
                                        .py_8()
                                        .flex()
                                        .flex_col()
                                        .items_start()
                                        .min_w_0()
                                        .gap_3()
                                        .child(
                                            div()
                                                .size(px(44.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded_xl()
                                                .bg(rgb(t::ACCENT_SURFACE))
                                                .text_color(rgb(t::ACCENT))
                                                .text_size(px(24.))
                                                .child(if is_network { "≋" } else { "#" }),
                                        )
                                        .child(
                                            div()
                                                .debug_selector(|| "empty-title".into())
                                                .text_size(px(22.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(empty_title),
                                        )
                                        .child(
                                            div()
                                                .debug_selector(|| "empty-detail".into())
                                                .text_size(px(13.))
                                                .text_color(rgb(t::MUTED))
                                                .child(empty_detail),
                                        )
                                        .when(
                                            self.client.is_none() && !self.model.connected,
                                            |view| {
                                                view.child(
                                                    div()
                                                        .debug_selector(|| "empty-action".into())
                                                        .flex()
                                                        .child(
                                                            Button::new("empty-open-connection")
                                                                .label("Open Connection")
                                                                .on_click(cx.listener(
                                                                    |this, _, _, cx| {
                                                                        this.show_connection();
                                                                        cx.notify();
                                                                    },
                                                                )),
                                                        ),
                                                )
                                            },
                                        )
                                        .when(is_network && self.model.connected, |view| {
                                            view.child(
                                                div()
                                                    .debug_selector(|| "empty-action".into())
                                                    .flex()
                                                    .child(
                                                        Button::new("empty-join-channel")
                                                            .label("Join a channel")
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.prepare_join(window, cx)
                                                                },
                                                            )),
                                                    ),
                                            )
                                        }),
                                )
                            })
                            .when(!messages_empty, |view| {
                                view.child(
                                    list(self.message_list.clone(), move |index, _, _| {
                                        let row = message_row(&lines[index]);
                                        let grouped =
                                            index.checked_sub(1).is_some_and(|previous| {
                                                same_author(&message_row(&lines[previous]), &row)
                                            });
                                        chat_row(row, grouped).into_any_element()
                                    })
                                    .flex_1()
                                    .min_h_0()
                                    .px(px(t::CONVERSATION_INSET))
                                    .py_4(),
                                )
                            }),
                    )
                    .when(self.show_members && channel.is_some(), |view| {
                        view.child(self.members(cx))
                    }),
            )
            .child(self.composer_view(cx))
    }

    fn members(&self, cx: &mut Context<Self>) -> Div {
        let channel = self.snapshot.channel(&self.model.selected);
        let count = channel.map(|channel| channel.members.len()).unwrap_or(0);
        // Never eagerly format a hostile server's entire 65k-member channel.
        let members: Vec<_> = channel
            .into_iter()
            .flat_map(|channel| channel.members.values())
            .take(1000)
            .map(|member| (clean(&member.nick), clean(&member.prefixes), member.away))
            .collect();
        div()
            .w(px(184.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .min_h_0()
            .bg(rgb(t::SIDEBAR))
            .border_l_1()
            .border_color(rgb(t::BORDER))
            .p_4()
            .child(div().mb_4().child(t::label(format!("MEMBERS · {count}"))))
            .child(
                uniform_list(
                    "member-list",
                    members.len(),
                    cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                        range
                            .map(|index| {
                                let (nick, prefixes, away) = &members[index];
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .py_2()
                                    .child(avatar(nick, 26.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(12.))
                                            .text_ellipsis()
                                            .text_color(rgb(if *away { t::MUTED } else { t::TEXT }))
                                            .child(nick.clone()),
                                    )
                                    .when(!prefixes.is_empty(), |view| {
                                        view.child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(rgb(t::ACCENT))
                                                .child(prefixes.clone()),
                                        )
                                    })
                            })
                            .collect()
                    }),
                )
                .flex_1(),
            )
            .when(count > 1000, |view| {
                view.child(
                    div()
                        .pt_3()
                        .text_size(px(11.))
                        .text_color(rgb(t::MUTED))
                        .child("Showing the first 1,000 members"),
                )
            })
    }

    fn composer_view(&self, cx: &mut Context<Self>) -> Div {
        let (guidance, connection_action) = composer_guidance(
            self.model.connected,
            self.client.is_some(),
            self.model.selected == STATUS,
        );
        div()
            .debug_selector(|| "composer-area".into())
            .w_full()
            .flex_shrink_0()
            .px(px(t::CONVERSATION_INSET))
            .pt_3()
            .pb_4()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(connection_action, |view, action| {
                view.child(
                    div()
                        .debug_selector(|| "composer-connection-state".into())
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .debug_selector(|| "composer-state-hint".into())
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .text_color(rgb(t::MUTED))
                                .child(guidance),
                        )
                        .child(
                            div()
                                .debug_selector(|| "composer-connection-action".into())
                                .flex()
                                .flex_shrink_0()
                                .child(
                                    Button::new("composer-open-connection")
                                        .label(action)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_connection();
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .when(!self.composer_feedback.is_empty(), |view| {
                view.child(self.feedback_view(&self.composer_feedback, "composer-feedback"))
            })
            .child(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(t::BORDER))
                    .bg(rgb(t::PANEL))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .debug_selector(|| "field:Composer".into())
                            .child(
                                Input::new(&self.composer)
                                    .w_full()
                                    .min_w_0()
                                    .flex_1()
                                    .appearance(false)
                                    .disabled(!self.model.connected),
                            ),
                    )
                    .child(
                        Button::new("send")
                            .primary()
                            .label("Send")
                            .disabled(!self.model.connected)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "composer-guidance".into())
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_3()
                    .text_size(px(11.))
                    .text_color(rgb(t::MUTED))
                    .when(self.model.connected, |view| view.child(guidance))
                    .child("Queued ≠ sent · Sent = written to connection, not proof of delivery"),
            )
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_message_list();
        if self.pending_focus.take().is_some() {
            // Recompute availability if registration/cancellation raced this frame.
            self.request_visible_focus();
            match self.pending_focus.take() {
                Some(PendingFocus::Host) => {
                    self.host.update(cx, |input, cx| input.focus(window, cx));
                }
                Some(PendingFocus::Composer) => {
                    self.composer
                        .update(cx, |input, cx| input.focus(window, cx));
                }
                _ => self.workspace_focus.focus(window),
            }
        }
        div()
            .track_focus(&self.workspace_focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(t::CANVAS))
            .text_color(rgb(t::TEXT))
            .text_size(px(14.))
            // Single-line gpui-component inputs emit PressEnter, then propagate
            // the action for enclosing forms. Our subscriptions submit it;
            // consume the bubble here so Enter cannot become literal text.
            .on_action(cx.listener(|_, _: &gpui_component::input::Enter, _, _| {}))
            .child(self.header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(cx))
                    .child(if self.connection_open {
                        self.connection_card(cx).into_any_element()
                    } else {
                        self.conversation(cx).into_any_element()
                    }),
            )
            .child(
                div()
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_4()
                    .border_t_1()
                    .border_color(rgb(t::BORDER))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(10.))
                    .text_color(rgb(t::MUTED))
                    .child("RUCKUS  /  IRCv3")
                    .child(if self.model.connected && self.tls {
                        "Verified TLS connection"
                    } else if self.client.is_some() && !self.model.connected && !self.tls {
                        "Connecting over plaintext · not encrypted"
                    } else if self.client.is_some() && !self.model.connected {
                        "Connecting with TLS…"
                    } else if self.model.connected {
                        "Plaintext connection · not encrypted"
                    } else {
                        "Offline · no connection made automatically"
                    }),
            )
    }
}
