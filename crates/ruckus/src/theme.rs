//! Ruckus's restrained dark palette, shared by chrome and native controls.

use gpui::{App, Div, div, prelude::*, px, rgb};
use gpui_component::{Theme, ThemeMode};

pub(crate) const CANVAS: u32 = 0x18191d;
pub(crate) const SIDEBAR: u32 = 0x121316;
pub(crate) const PANEL: u32 = 0x202126;
pub(crate) const RAISED: u32 = 0x28292f;
pub(crate) const BORDER: u32 = 0x303138;
pub(crate) const TEXT: u32 = 0xeeedf1;
pub(crate) const MUTED: u32 = 0xa19da9;
pub(crate) const ACCENT: u32 = 0xa99aff;
pub(crate) const ACCENT_SURFACE: u32 = 0x2d2842;
pub(crate) const GREEN: u32 = 0x94d7ad;
pub(crate) const AMBER: u32 = 0xe8c283;

pub(crate) fn install(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    theme.font_size = px(14.);
    theme.radius = px(8.);
    theme.radius_lg = px(12.);
    theme.background = rgb(CANVAS).into();
    theme.foreground = rgb(TEXT).into();
    theme.border = rgb(BORDER).into();
    theme.input = rgb(BORDER).into();
    theme.muted = rgb(PANEL).into();
    theme.muted_foreground = rgb(MUTED).into();
    theme.primary = rgb(ACCENT).into();
    theme.primary_hover = rgb(0xb9adff).into();
    theme.primary_active = rgb(0x9685ed).into();
    theme.primary_foreground = rgb(SIDEBAR).into();
    theme.secondary = rgb(RAISED).into();
    theme.secondary_hover = rgb(0x35353e).into();
    theme.secondary_active = rgb(BORDER).into();
    theme.secondary_foreground = rgb(TEXT).into();
    theme.accent = rgb(ACCENT_SURFACE).into();
    theme.accent_foreground = rgb(TEXT).into();
    theme.ring = rgb(ACCENT).into();
    theme.selection = rgb(0x494064).into();
    theme.scrollbar_thumb = rgb(0x48464f).into();
    theme.scrollbar_thumb_hover = rgb(0x625d70).into();
}

pub(crate) fn label(text: impl Into<String>) -> Div {
    div()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(text.into())
}

pub(crate) fn dot(color: u32) -> Div {
    div()
        .size(px(6.))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(color))
}

pub(crate) fn badge(text: impl Into<String>, color: u32) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(PANEL))
        .text_size(px(11.))
        .text_color(rgb(color))
        .child(dot(color))
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((color >> shift) & 255_u32) / 255.;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn small_text_and_actions_have_readable_contrast() {
        for background in [CANVAS, SIDEBAR, PANEL, RAISED, ACCENT_SURFACE] {
            for foreground in [TEXT, MUTED, ACCENT, GREEN, AMBER] {
                assert!(
                    contrast(foreground, background) >= 4.5,
                    "{foreground:06x} on {background:06x} must meet normal-text contrast"
                );
            }
        }
        assert!(contrast(SIDEBAR, ACCENT) >= 4.5);
        for author in ["exec", "ruckus", "alice", "bob", "猫", "sage", "nick42"] {
            assert!(contrast(crate::presentation::author_color(author), CANVAS) >= 4.5);
        }
    }
}
