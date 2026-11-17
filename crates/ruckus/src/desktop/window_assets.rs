//! Embedded window-control glyphs used by gpui-component's inline title bar.
//!
//! The component resolves these paths through the application's asset source;
//! relying on files in the working directory would leave packaged controls blank.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub(super) struct WindowAssets;

const ICONS: [(&str, &[u8]); 4] = [
    (
        "icons/window-minimize.svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path d="M3 8h10" fill="none" stroke="black" stroke-width="1.2"/></svg>"#,
    ),
    (
        "icons/window-maximize.svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect x="3" y="3" width="10" height="10" fill="none" stroke="black" stroke-width="1.2"/></svg>"#,
    ),
    (
        "icons/window-restore.svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path d="M5 3h8v8M3 5h8v8H3z" fill="none" stroke="black" stroke-width="1.2"/></svg>"#,
    ),
    (
        "icons/window-close.svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path d="m3 3 10 10M13 3 3 13" fill="none" stroke="black" stroke-width="1.2"/></svg>"#,
    ),
];

impl AssetSource for WindowAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::{IconName, IconNamed};

    #[test]
    fn every_platform_window_glyph_is_embedded_at_the_component_path() {
        for icon in [
            IconName::WindowMinimize,
            IconName::WindowMaximize,
            IconName::WindowRestore,
            IconName::WindowClose,
        ] {
            let path = icon.path();
            let bytes = WindowAssets.load(&path).unwrap().expect("embedded glyph");
            assert!(bytes.starts_with(b"<svg "));
            assert!(bytes.ends_with(b"</svg>"));
        }
        assert_eq!(WindowAssets.list("icons/").unwrap().len(), 4);
        assert!(WindowAssets.load("../unknown.svg").unwrap().is_none());
    }
}
