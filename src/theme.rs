// Linen in the colors of the running Omarchy theme (2026-11-24). Omarchy writes the
// palette to ~/.local/state/omarchy/current/theme/colors.toml and bumps
// theme.generation on every change; main.rs watches that folder (inotify, no polling)
// and pushes new colors into the page. Linen's own Dusk and Dawn themes stay available.

use crate::files;
use serde_json::{Map, Value, json};
use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    files::home().join(".local/state/omarchy/current")
}

/// `key = "#rrggbb"` and `mode = "dark|light"` lines; everything else is ignored.
fn parse(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim().trim_matches('"').trim());
        let hex = v.len() == 7 && v.starts_with('#') && v[1..].bytes().all(|b| b.is_ascii_hexdigit());
        if hex || (k == "mode" && (v == "dark" || v == "light")) {
            out.insert(k.to_string(), json!(v.to_ascii_lowercase()));
        }
    }
    out
}

fn rgb(hex: &str) -> (f64, f64, f64) {
    let n = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64;
    (n(1), n(3), n(5))
}

fn hex((r, g, b): (f64, f64, f64)) -> String {
    format!("#{:02x}{:02x}{:02x}", r.round() as u8, g.round() as u8, b.round() as u8)
}

fn mix(a: &str, b: &str, t: f64) -> String {
    let (a, b) = (rgb(a), rgb(b));
    hex((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t))
}

fn alpha(c: &str, a: f64) -> String {
    let (r, g, b) = rgb(c);
    format!("rgba({}, {}, {}, {a})", r as u8, g as u8, b as u8)
}

fn luminance(c: &str) -> f64 {
    let f = |v: f64| {
        let v = v / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = rgb(c);
    0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
}

/// Linen's CSS variables from an Omarchy palette, plus `mode` and the window `bg`.
/// None when there is no usable palette (no Omarchy, or an old/odd colors file).
pub fn from_palette(text: &str) -> Option<Value> {
    let p = parse(text);
    let get = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    let (bg, fg, accent) = (get("background")?, get("foreground")?, get("accent")?);
    let light = get("mode").as_deref() == Some("light") || (get("mode").is_none() && luminance(&bg) > 0.5);
    let raised = get("lighter_background").unwrap_or_else(|| mix(&bg, &fg, 0.06));
    let second = get("magenta").or_else(|| get("blue")).unwrap_or_else(|| mix(&accent, &fg, 0.4));
    let warm = get("orange").or_else(|| get("yellow")).unwrap_or_else(|| mix(&accent, &fg, 0.3));
    let cool = get("cyan").or_else(|| get("green")).unwrap_or_else(|| accent.clone());
    let accent2 = mix(&accent, &second, 0.5);
    // Text on accent buttons: the theme's darkest color on a bright accent, else white.
    let on_accent = if luminance(&accent) > 0.35 { get("darker_background").unwrap_or_else(|| mix(&bg, "#000000", 0.4)) } else { "#ffffff".into() };
    Some(json!({
        "mode": if light { "light" } else { "dark" },
        "bg": bg,
        "vars": {
            "color-scheme": if light { "light" } else { "dark" },
            "--sky-a": bg,
            "--sky-b": mix(&bg, &raised, 0.5),
            "--sky-c": mix(&raised, &accent, 0.06),
            "--glow": alpha(&accent, if light { 0.16 } else { 0.18 }),
            "--page": alpha(if light { &bg } else { &raised }, if light { 0.92 } else { 0.82 }),
            "--page-line": alpha(&accent, 0.18),
            "--ink": fg,
            "--muted": mix(&fg, &bg, 0.35),
            "--pink": accent,
            "--orange": warm,
            "--teal": cool,
            "--accent-grad": format!("linear-gradient(90deg, {accent}, {accent2})"),
            "--select": alpha(&accent, if light { 0.22 } else { 0.28 }),
            "--button": alpha(&fg, if light { 0.05 } else { 0.06 }),
            "--button-hover": alpha(&fg, if light { 0.10 } else { 0.12 }),
            "--panel": if light { mix(&bg, "#ffffff", 0.4) } else { raised.clone() },
            "--shadow": if light { "0 18px 50px rgba(0, 0, 0, 0.16)" } else { "0 18px 60px rgba(0, 0, 0, 0.55)" },
            "--on-accent": on_accent,
        }
    }))
}

/// The running Omarchy theme, ready for the page; None without Omarchy.
pub fn current() -> Option<Value> {
    from_palette(&std::fs::read_to_string(state_dir().join("theme/colors.toml")).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: &str = "mode = \"dark\"\naccent = \"#52E5D5\"\nbackground = \"#061018\"\nlighter_background = \"#10232E\"\nforeground = \"#C7E4E5\"\norange = \"#FF8A3D\"\nmagenta = \"#9D86C8\"\n# a comment\nname = \"not a color\"\n";

    #[test]
    fn a_dark_palette_becomes_linen_colors() {
        let t = from_palette(DARK).unwrap();
        assert_eq!(t["mode"], "dark");
        assert_eq!(t["bg"], "#061018");
        let v = &t["vars"];
        assert_eq!(v["--sky-a"], "#061018");
        assert_eq!(v["--ink"], "#c7e4e5");
        assert_eq!(v["--pink"], "#52e5d5");
        assert_eq!(v["color-scheme"], "dark");
        // A bright accent gets dark text on its buttons.
        assert_ne!(v["--on-accent"], "#ffffff");
    }

    #[test]
    fn light_themes_are_detected_by_mode_or_brightness() {
        let light = "mode = \"light\"\nbackground = \"#fbf1c7\"\nforeground = \"#3c3836\"\naccent = \"#076678\"\n";
        assert_eq!(from_palette(light).unwrap()["vars"]["color-scheme"], "light");
        let unmarked = "background = \"#ffffff\"\nforeground = \"#222222\"\naccent = \"#0055aa\"\n";
        assert_eq!(from_palette(unmarked).unwrap()["mode"], "light");
        assert_eq!(from_palette(unmarked).unwrap()["vars"]["--on-accent"], "#ffffff");
    }

    #[test]
    fn missing_or_odd_palettes_fall_back_to_linen_themes() {
        assert!(from_palette("").is_none());
        assert!(from_palette("background = \"#000000\"\n").is_none(), "needs foreground and accent");
        assert!(from_palette("background = \"red\"\nforeground = \"#fff\"\naccent = \"#00ff00\"\n").is_none());
    }
}
