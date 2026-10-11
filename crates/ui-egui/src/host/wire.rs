//! The embed API's JSON shapes (what `EmbeddedEditor` receives from a host page), validated here
//! so the wasm glue stays thin and every rule is tested natively.
//!
//! Page and occurrence numbers are whole numbers on the wire. A fractional, negative or non-finite
//! one is malformed and refused when the JSON is parsed, so the call fails and queues nothing. A
//! whole number of any size (`18446744073709551616`, `1e21`) is merely out of range for any
//! document: it saturates to `u32::MAX` and later answers `jumpNotFound` (or skips a mark), exactly
//! like a page the document lacks.

use egui::Color32;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Number;
use wordcraft_doc::props::Rgb;

use crate::theme::c32;

/// Most marks one `setMarks` call keeps; the rest are ignored.
pub const MAX_MARKS: usize = 2_000;

/// A non-negative whole JSON number of any size, saturated to `u32::MAX`. JavaScript writes large
/// integers in exponent form (`1e+21`), which JSON parsers read as floats, so the value is judged by
/// what it is rather than by how it is spelled.
fn saturating_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let number = Number::deserialize(deserializer)?;
    if let Some(unsigned) = number.as_u64() {
        return Ok(u32::try_from(unsigned).unwrap_or(u32::MAX));
    }
    match number.as_f64() {
        Some(value) if value.is_finite() && value >= 0.0 && value.fract() == 0.0 => {
            Ok(if value >= f64::from(u32::MAX) { u32::MAX } else { value as u32 })
        }
        _ => Err(D::Error::custom(format!("{number} is not a non-negative whole number"))),
    }
}

fn saturating_optional_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    #[derive(Deserialize)]
    struct Whole(#[serde(deserialize_with = "saturating_u32")] u32);
    Ok(Option::<Whole>::deserialize(deserializer)?.map(|whole| whole.0))
}

/// Where to jump: a page (1-based), or a passage of text (`occurrence` is 0-based, among the
/// matches on `page`, or in the whole document when `page` is null).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JumpTarget {
    Page {
        #[serde(deserialize_with = "saturating_u32")]
        page: u32,
    },
    Text {
        text: String,
        #[serde(default, deserialize_with = "saturating_optional_u32")]
        page: Option<u32>,
        #[serde(deserialize_with = "saturating_u32")]
        occurrence: u32,
    },
}

/// A passage the host wants highlighted.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkSpec {
    pub id: String,
    pub text: String,
    /// 1-based; `None` searches every page.
    #[serde(default, deserialize_with = "saturating_optional_u32")]
    pub page: Option<u32>,
    /// `#rrggbbaa` (or `#rrggbb`); checked by [`parse_marks`].
    pub color: String,
}

/// The host's theme: dark or light chrome, and its accent with the text colour on it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSpec {
    pub dark: bool,
    pub accent: String,
    pub accent_content: String,
}

/// `EmbeddedEditor.mount`'s options.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct MountOptions {
    pub mode: String,
    pub theme: ThemeSpec,
}

impl ThemeSpec {
    /// (accent, text on the accent), or `None` unless both are opaque `#rrggbb`.
    pub fn colors(&self) -> Option<(Color32, Color32)> {
        let opaque = |value: &str| if value.len() == 7 { parse_color(value) } else { None };
        Some((opaque(&self.accent)?, opaque(&self.accent_content)?))
    }
}

/// `#rrggbb` or `#rrggbbaa`, either case.
pub fn parse_color(value: &str) -> Option<Color32> {
    let hex = value.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    // The colour digits go through the engine's own parser; only the alpha pair is read here.
    let opaque = c32(Rgb::parse(hex.get(..6)?)?);
    match hex.get(6..) {
        Some("") | None => Some(opaque),
        Some(pair) => Some(Color32::from_rgba_unmultiplied(opaque.r(), opaque.g(), opaque.b(), u8::from_str_radix(pair, 16).ok()?)),
    }
}

/// The 0-based index of a host's 1-based page number, if it names one of `pages`.
pub fn page_index(page: u32, pages: usize) -> Option<usize> {
    let index = usize::try_from(page).ok()?.checked_sub(1)?;
    (index < pages).then_some(index)
}

/// A host's 0-based occurrence. `u32::MAX` is what an out-of-range number saturated to, so it names
/// no match.
pub fn occurrence_index(occurrence: u32) -> Option<usize> {
    if occurrence == u32::MAX {
        return None;
    }
    usize::try_from(occurrence).ok()
}

fn checked_theme(theme: &ThemeSpec, what: &str) -> Result<(), String> {
    theme.colors().map(|_| ()).ok_or_else(|| format!("{what}: accent and accentContent must be '#rrggbb'"))
}

pub fn parse_mount_options(json: &str) -> Result<MountOptions, String> {
    let options: MountOptions = serde_json::from_str(json).map_err(|error| format!("mount: {error}"))?;
    if options.mode != "view" {
        return Err(format!("mount: mode '{}' is not supported: an editor mounts in view mode; setMode switches to edit mode", options.mode));
    }
    checked_theme(&options.theme, "mount")?;
    Ok(options)
}

pub fn parse_theme(json: &str) -> Result<ThemeSpec, String> {
    let theme: ThemeSpec = serde_json::from_str(json).map_err(|error| format!("setTheme: {error}"))?;
    checked_theme(&theme, "setTheme")?;
    Ok(theme)
}

pub fn parse_jump_target(json: &str) -> Result<JumpTarget, String> {
    serde_json::from_str(json).map_err(|error| format!("jumpTo: {error}"))
}

/// `Mark[]` from the host's JSON. Marks beyond [`MAX_MARKS`] are ignored (the first ones are kept),
/// as the embed API says, so a long list still paints its first part.
pub fn parse_marks(json: &str) -> Result<Vec<MarkSpec>, String> {
    let mut marks: Vec<MarkSpec> = serde_json::from_str(json).map_err(|error| format!("setMarks: {error}"))?;
    marks.truncate(MAX_MARKS);
    if let Some(bad) = marks.iter().find(|mark| parse_color(&mark.color).is_none()) {
        return Err(format!("setMarks: mark {:?} has colour {:?}; expected #rrggbbaa", bad.id, bad.color));
    }
    Ok(marks)
}

/// `setMode`'s request (embed API 0.2).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ModeRequest {
    View,
    Edit {
        author: AuthorSpec,
        #[serde(rename = "revisionDate")]
        revision_date: String,
        permissions: PermissionsSpec,
    },
}

/// The person editing: their display name authors every tracked change of the session. Other
/// fields a host sends are ignored, as the embed API allows.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AuthorSpec {
    pub id: String,
    pub name: String,
}

/// Which tools edit mode offers; this editor reads `accept_reject_changes` only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionsSpec {
    pub accept_reject_changes: bool,
    pub organize_pages: bool,
    pub fill_forms: bool,
    pub redact: bool,
    pub stamps: bool,
    pub drawing: bool,
}

/// Tracked changes and annotations name their author; Word refuses longer names.
const MAX_AUTHOR_CHARS: usize = 255;

/// Most fraction digits a revision date may carry (nanoseconds); Word keeps whole seconds anyway.
const MAX_FRACTION_DIGITS: usize = 9;

/// A host's revision date, `YYYY-MM-DDTHH:MM:SS` on a calendar day with an optional fraction and
/// `Z`, as Word stores it: to the second (`2026-10-10T08:00:00.123Z` is `2026-10-10T08:00:00Z`).
/// `None` when it is not one.
pub fn revision_date_seconds(date: &str) -> Option<String> {
    let whole = format!("{}Z", date.get(..19)?);
    let tail_ok = match date.get(19..)?.as_bytes() {
        [b'Z'] => true,
        [b'.', fraction @ .., b'Z'] => (1..=MAX_FRACTION_DIGITS).contains(&fraction.len()) && fraction.iter().all(u8::is_ascii_digit),
        _ => false,
    };
    (tail_ok && wordcraft_engine::edit_mode::is_revision_date(&whole)).then_some(whole)
}

pub fn parse_mode_request(json: &str) -> Result<ModeRequest, String> {
    let request: ModeRequest = serde_json::from_str(json).map_err(|error| format!("setMode: {error}"))?;
    if let ModeRequest::Edit { author, revision_date, .. } = &request {
        if author.name.trim().is_empty() || author.name.chars().count() > MAX_AUTHOR_CHARS {
            return Err(format!("setMode: author.name must be 1 to {MAX_AUTHOR_CHARS} characters"));
        }
        if revision_date_seconds(revision_date).is_none() {
            return Err("setMode: revisionDate must be an ISO 8601 UTC timestamp on a calendar day".into());
        }
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use egui::Color32;

    use super::*;

    #[test]
    fn page_and_occurrence_numbers_must_be_in_range() {
        assert_eq!(page_index(1, 3), Some(0));
        assert_eq!(page_index(3, 3), Some(2));
        for page in [0, 4, u32::MAX] {
            assert_eq!(page_index(page, 3), None, "{page}");
        }
        assert_eq!(occurrence_index(0), Some(0));
        assert_eq!(occurrence_index(7), Some(7));
        assert_eq!(occurrence_index(u32::MAX), None, "the saturated value names no match");
    }

    #[test]
    fn colours_are_hex_with_optional_alpha() {
        assert_eq!(parse_color("#0f766e"), Some(Color32::from_rgb(0x0f, 0x76, 0x6e)));
        assert_eq!(parse_color("#FFD60080"), Some(Color32::from_rgba_unmultiplied(0xff, 0xd6, 0x00, 0x80)));
        for bad in ["0f766e", "#0f766", "#0f766e8", "#gg0000", "", "#", "#0f766e80ff", "# 0f766e", "#0f766é", "##0f766e"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn mount_options_accept_view_mode_and_hex_theme_colours() {
        let ok = parse_mount_options(r##"{"mode":"view","theme":{"dark":false,"accent":"#0f766e","accentContent":"#ffffff"}}"##).unwrap();
        assert_eq!(ok.theme.colors(), Some((Color32::from_rgb(0x0f, 0x76, 0x6e), Color32::WHITE)));
        let edit = parse_mount_options(r##"{"mode":"edit","theme":{"dark":false,"accent":"#0f766e","accentContent":"#ffffff"}}"##);
        assert!(edit.unwrap_err().contains("mounts in view mode"));
        assert!(parse_mount_options(r##"{"mode":"view","theme":{"dark":false,"accent":"teal","accentContent":"#ffffff"}}"##).is_err());
        assert!(parse_theme(r##"{"dark":true,"accent":"#0f766e80","accentContent":"#ffffff"}"##).is_err(), "accent must be opaque");
        assert!(parse_theme(r##"{"dark":true,"accent":"#0f766e","accentContent":"#fff"}"##).is_err());
        assert!(parse_theme(r##"{"dark":true,"accent":"#0f766e","accentContent":"#000000"}"##).unwrap().dark);
    }

    #[test]
    fn jump_targets_follow_the_contract_shapes() {
        assert_eq!(parse_jump_target(r#"{"kind":"page","page":3}"#).unwrap(), JumpTarget::Page { page: 3 });
        assert_eq!(
            parse_jump_target(r#"{"kind":"text","text":"Clause 5","page":null,"occurrence":0}"#).unwrap(),
            JumpTarget::Text { text: "Clause 5".into(), page: None, occurrence: 0 }
        );
        assert_eq!(
            parse_jump_target(r#"{"kind":"text","text":"x","page":2,"occurrence":1}"#).unwrap(),
            JumpTarget::Text { text: "x".into(), page: Some(2), occurrence: 1 }
        );
        assert!(parse_jump_target(r#"{"page":3}"#).is_err());
        assert!(parse_jump_target(r#"{"kind":"bookmark","name":"x"}"#).is_err());
        assert!(parse_jump_target(r#"{"kind":"text","text":"x","page":null}"#).is_err(), "occurrence is required");
        assert_eq!(
            parse_jump_target(r#"{"kind":"text","text":"x","occurrence":0}"#).unwrap(),
            JumpTarget::Text { text: "x".into(), page: None, occurrence: 0 },
            "an omitted page means the whole document"
        );
        assert!(parse_jump_target(r#""page 1""#).is_err());
    }

    #[test]
    fn fractional_negative_and_non_finite_numbers_are_malformed() {
        for bad in [
            r#"{"kind":"page","page":-1}"#,
            r#"{"kind":"page","page":1.5}"#,
            r#"{"kind":"page","page":"2"}"#,
            r#"{"kind":"page","page":null}"#,
            r#"{"kind":"page","page":NaN}"#,
            r#"{"kind":"page","page":Infinity}"#,
            r#"{"kind":"text","text":"x","page":null,"occurrence":-1}"#,
            r#"{"kind":"text","text":"x","page":null,"occurrence":0.5}"#,
            r#"{"kind":"text","text":"x","page":1.5,"occurrence":0}"#,
            r#"{"kind":"text","text":"x","page":-3,"occurrence":0}"#,
            r#"{"kind":"page","page":-0.5}"#,
            r#"{"kind":"page","page":-1e21}"#,
            r#"{"kind":"page","page":1e-1}"#,
            r#"{"kind":"page","page":4294967295.5}"#,
            r#"{"kind":"text","text":"x","page":null,"occurrence":-1e300}"#,
        ] {
            assert!(parse_jump_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn whole_numbers_beyond_u32_saturate_instead_of_failing() {
        assert_eq!(parse_jump_target(r#"{"kind":"page","page":5000000000}"#).unwrap(), JumpTarget::Page { page: u32::MAX });
        assert_eq!(
            parse_jump_target(r#"{"kind":"text","text":"x","page":5000000000,"occurrence":9007199254740991}"#).unwrap(),
            JumpTarget::Text { text: "x".into(), page: Some(u32::MAX), occurrence: u32::MAX }
        );
        for huge in ["4294967295", "4294967296", "18446744073709551615", "18446744073709551616", "100000000000000000000", "1e+21", "1e300", "1.5e10"]
        {
            let page = format!(r#"{{"kind":"page","page":{huge}}}"#);
            assert_eq!(parse_jump_target(&page).unwrap(), JumpTarget::Page { page: u32::MAX }, "{huge}");
            let occurrence = format!(r#"{{"kind":"text","text":"x","page":{huge},"occurrence":{huge}}}"#);
            assert_eq!(
                parse_jump_target(&occurrence).unwrap(),
                JumpTarget::Text { text: "x".into(), page: Some(u32::MAX), occurrence: u32::MAX },
                "{huge}"
            );
            let mark = format!(r##"[{{"id":"m","text":"x","page":{huge},"color":"#000000"}}]"##);
            assert_eq!(parse_marks(&mark).unwrap()[0].page, Some(u32::MAX), "{huge}");
        }
        assert_eq!(parse_jump_target(r#"{"kind":"page","page":3.0}"#).unwrap(), JumpTarget::Page { page: 3 });
        assert_eq!(parse_jump_target(r#"{"kind":"page","page":4294967294}"#).unwrap(), JumpTarget::Page { page: 4_294_967_294 });
        assert_eq!(page_index(u32::MAX, 3), None, "no document has that page");
        let far = parse_marks(r##"[{"id":"far","text":"x","page":9007199254740991,"color":"#000000"}]"##).unwrap();
        assert_eq!(far[0].page, Some(u32::MAX));
    }

    #[test]
    fn error_messages_carry_the_api_method_names() {
        assert!(parse_jump_target("{}").unwrap_err().starts_with("jumpTo: "));
        assert!(parse_marks("{}").unwrap_err().starts_with("setMarks: "));
        assert!(parse_marks(r#"[{"id":"a","text":"x","page":null,"color":"red"}]"#).unwrap_err().starts_with("setMarks: "));
        assert!(parse_theme("{}").unwrap_err().starts_with("setTheme: "));
        assert!(parse_mount_options("{}").unwrap_err().starts_with("mount: "));
        assert!(
            parse_mount_options(r##"{"mode":"edit","theme":{"dark":false,"accent":"#000000","accentContent":"#ffffff"}}"##)
                .unwrap_err()
                .starts_with("mount: ")
        );
    }

    #[test]
    fn marks_need_hex_colours_and_whole_pages() {
        let marks = parse_marks(r##"[{"id":"t1","text":"Clause 1","page":null,"color":"#ffd60080"}]"##).unwrap();
        assert_eq!(marks, vec![MarkSpec { id: "t1".into(), text: "Clause 1".into(), page: None, color: "#ffd60080".into() }]);
        let paged = parse_marks(r##"[{"id":"t2","text":"x","page":2,"color":"#00ff00"}]"##).unwrap();
        assert_eq!(paged[0].page, Some(2));
        assert!(parse_marks(r#"[{"id":"t1","text":"x","page":null,"color":"yellow"}]"#).unwrap_err().contains("t1"));
        assert_eq!(parse_marks("[]").unwrap(), Vec::new());
        for bad_page in ["-1", "1.5", "\"2\""] {
            let json = format!(r##"[{{"id":"t","text":"x","page":{bad_page},"color":"#000000"}}]"##);
            assert!(parse_marks(&json).is_err(), "{bad_page}");
        }
    }

    #[test]
    fn marks_beyond_the_cap_are_ignored_and_the_first_are_kept() {
        let entries: Vec<String> =
            (0..=MAX_MARKS).map(|index| format!(r##"{{"id":"m{index}","text":"x","page":null,"color":"#000000"}}"##)).collect();
        let kept = parse_marks(&format!("[{}]", entries.join(","))).expect("marks beyond the cap are ignored, not refused");
        assert_eq!(kept.len(), MAX_MARKS);
        assert_eq!(kept.first().map(|mark| mark.id.as_str()), Some("m0"));
        let ids: Vec<&str> = kept.iter().map(|mark| mark.id.as_str()).collect();
        let expected: Vec<String> = (0..MAX_MARKS).map(|index| format!("m{index}")).collect();
        assert_eq!(ids, expected, "exactly the first {MAX_MARKS} distinct ids");
    }

    #[test]
    fn a_bad_colour_beyond_the_cap_does_not_matter() {
        let mut entries: Vec<String> =
            (0..MAX_MARKS).map(|index| format!(r##"{{"id":"m{index}","text":"x","page":null,"color":"#000000"}}"##)).collect();
        entries.push(r#"{"id":"late","text":"x","page":null,"color":"red"}"#.into());
        assert_eq!(parse_marks(&format!("[{}]", entries.join(","))).unwrap().len(), MAX_MARKS);
    }

    // W3: a revision date is a real calendar day in UTC; Word keeps it to the second.
    #[test]
    fn revision_dates_are_utc_calendar_days_kept_to_the_second() {
        for (date, seconds) in [
            ("2026-10-10T08:00:00Z", "2026-10-10T08:00:00Z"),
            ("2026-10-10T08:00:00.123Z", "2026-10-10T08:00:00Z"),
            ("2028-02-29T23:59:59.5Z", "2028-02-29T23:59:59Z"),
        ] {
            assert_eq!(revision_date_seconds(date).as_deref(), Some(seconds), "{date}");
        }
        for bad in [
            "2026-02-31T08:00:00Z",
            "2026-02-29T08:00:00Z",
            "2026-04-31T08:00:00.000Z",
            "2026-13-01T08:00:00Z",
            "2026-10-10T24:00:00Z",
            "2026-10-10T08:00:00",
            "2026-10-10T08:00:00.Z",
            "2026-10-10T08:00:00.1234567890Z",
            "2026-10-10T08:00:00+07:00",
            "2026-10-10",
            "yesterday",
            "",
            "２０２６-10-10T08:00:00Z",
        ] {
            assert_eq!(revision_date_seconds(bad), None, "{bad}");
        }
    }
}
