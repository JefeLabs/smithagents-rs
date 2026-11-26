//! The app's asset source: the icons it bundles beyond the Kit's defaults,
//! then the Kit's own.

use gpui_kit::assets::IconName;
use gpui_kit::AssetSource as _;
use smithagents_app::assets::AppAssets;

#[test]
fn the_app_source_serves_every_bundled_icon() {
    for icon in [
        IconName::Lightbulb,
        IconName::ListFilter,
        IconName::Plus,
        IconName::ArrowLeft,
        IconName::Check,
        IconName::MessageSquarePlus,
        IconName::Camera,
        IconName::Paperclip,
        IconName::Trash,
        IconName::X,
    ] {
        let path = icon.path();
        let bytes = AppAssets
            .load(&path)
            .unwrap()
            .unwrap_or_else(|| panic!("{path} is not served"));
        assert!(String::from_utf8_lossy(&bytes).contains("<svg"), "{path}");
    }
    // A default icon still comes from the Kit.
    let settings = IconName::Settings.path();
    let bytes = AppAssets.load(&settings).unwrap().expect("a default icon");
    assert!(String::from_utf8_lossy(&bytes).contains("<svg"));
    // An unknown path is "not here", not an error.
    assert!(matches!(AppAssets.load("icons/no-such-icon.svg"), Ok(None)));
}
