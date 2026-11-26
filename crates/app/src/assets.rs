//! The app's one asset source, [`AppAssets`]: the icons the app bundles
//! beyond the Kit's defaults ([`AppIcons`]), then the Kit's own
//! ([`gpui_kit::assets::Assets`]). `run` registers it; an icon neither serves
//! draws blank.
//!
//! The icons are Lucide's, embedded from the pinned `gpui-kit-assets`
//! catalog, so no copy of an SVG lives in this repository. Their license (ISC)
//! is in `crates/app/licenses/LICENSE-LUCIDE`.
//!
//! To draw another catalog icon outside the Kit's defaults, add its
//! `gpui_kit::assets::IconName` variant to [`AppIcons`].

use std::borrow::Cow;

use gpui_kit::assets::Assets;
use gpui_kit::{AssetSource, SharedString};

gpui_kit::assets::icon_assets!(pub AppIcons, [Lightbulb, ListFilter, Plus, ArrowLeft, Check,
    MessageSquarePlus, Camera, Paperclip, Trash, X]);

/// [`AppIcons`] first, then the Kit's default icons.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    /// A path neither source has is `Ok(None)`, the `AssetSource` answer for
    /// "not here" (gpui-component warns about a blank icon only then). The
    /// Kit's source answers it with an error instead, so it is asked only for
    /// a path it lists.
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = AppIcons.load(path)? {
            return Ok(Some(bytes));
        }
        if Assets
            .list(path)?
            .iter()
            .any(|known| known.as_ref() == path)
        {
            return Assets.load(path);
        }
        Ok(None)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(AppIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
