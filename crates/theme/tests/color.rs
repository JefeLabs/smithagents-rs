use smithagents_theme::color::{contrast_ratio, Oklch, OutOfGamut, Srgb};

fn ok(l: f64, c: f64, h: f64) -> Srgb {
    Oklch { l, c, h }.to_srgb().unwrap()
}

#[test]
fn white_and_black() {
    assert_eq!(ok(1.0, 0.0, 0.0).hex(), "#ffffff");
    assert_eq!(ok(0.0, 0.0, 0.0).hex(), "#000000");
}

#[test]
fn red_is_close() {
    let s = ok(0.6279, 0.2577, 29.23);
    assert!(s.r.abs_diff(255) <= 2, "{s:?}");
    assert!(s.g <= 2, "{s:?}");
    assert!(s.b <= 2, "{s:?}");
}

#[test]
fn out_of_gamut() {
    let color = Oklch {
        l: 0.7,
        c: 0.4,
        h: 150.0,
    };
    assert_eq!(color.to_srgb(), Err(OutOfGamut { color }));
}

#[test]
fn css_formatting() {
    assert_eq!(
        Oklch {
            l: 0.72,
            c: 0.12,
            h: 190.0
        }
        .css(),
        "oklch(0.72 0.12 190)"
    );
    assert_eq!(
        Oklch {
            l: 0.5,
            c: 0.0,
            h: 0.0
        }
        .css(),
        "oklch(0.5 0 0)"
    );
}

#[test]
fn contrast() {
    let b = Srgb { r: 0, g: 0, b: 0 };
    let w = Srgb {
        r: 255,
        g: 255,
        b: 255,
    };
    assert!((contrast_ratio(b, w) - 21.0).abs() < 0.01);
    assert_eq!(contrast_ratio(b, w), contrast_ratio(w, b));
    assert_eq!(contrast_ratio(w, w), 1.0);
}
