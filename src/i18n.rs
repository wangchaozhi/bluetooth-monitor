//! UI translations. Protocol identifiers, user content and file formats stay language independent.
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "en")]
    English,
    #[default]
    #[serde(rename = "zh-CN", other)]
    SimplifiedChinese,
}

type Catalog = BTreeMap<String, String>;

impl Language {
    pub const ALL: [Self; 2] = [Self::SimplifiedChinese, Self::English];

    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }

    fn catalog(self) -> &'static Catalog {
        static ENGLISH: OnceLock<Catalog> = OnceLock::new();
        static CHINESE: OnceLock<Catalog> = OnceLock::new();
        let (cell, json) = match self {
            Self::English => (&ENGLISH, include_str!("../locales/en.json")),
            Self::SimplifiedChinese => (&CHINESE, include_str!("../locales/zh-CN.json")),
        };
        cell.get_or_init(|| serde_json::from_str(json).expect("valid embedded language catalog"))
    }

    pub fn text(self, key: &str) -> &str {
        self.catalog()
            .get(key)
            .or_else(|| Self::English.catalog().get(key))
            .map_or(key, String::as_str)
    }

    /// Substitute numbered arguments in one pass, without interpreting braces in user data.
    pub fn format(self, key: &str, args: &[String]) -> String {
        let template = self.text(key);
        let mut result = String::with_capacity(template.len());
        let mut remaining = template;
        while let Some(start) = remaining.find('{') {
            result.push_str(&remaining[..start]);
            remaining = &remaining[start..];
            if let Some(end) = remaining.find('}')
                && let Ok(index) = remaining[1..end].parse::<usize>()
                && let Some(value) = args.get(index)
            {
                result.push_str(value);
                remaining = &remaining[end + 1..];
            } else {
                result.push('{');
                remaining = &remaining[1..];
            }
        }
        result.push_str(remaining);
        result
    }
}

/// Keep a status message's identity so switching language also updates existing status text.
#[derive(Debug, Clone)]
pub enum LocalizedText {
    Message {
        key: &'static str,
        args: Vec<String>,
    },
    Diagnostic(String),
}

impl LocalizedText {
    pub fn new(key: &'static str, args: &[String]) -> Self {
        Self::Message {
            key,
            args: args.to_vec(),
        }
    }

    pub fn render(&self, language: Language) -> String {
        match self {
            Self::Message { key, args } => language.format(key, args),
            Self::Diagnostic(text) => text.clone(),
        }
    }
}

impl From<String> for LocalizedText {
    fn from(text: String) -> Self {
        Self::Diagnostic(text)
    }
}

/// Add a system CJK font as fallback for both UI text and monospace diagnostics.
/// An explicit font path supports distributions without a preinstalled CJK font.
pub fn install_fonts(ctx: &egui::Context) {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("BLUETOOTH_MONITOR_FONT") {
        paths.push(std::path::PathBuf::from(path));
    }
    if cfg!(target_os = "windows") {
        let windows = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        paths.push(std::path::PathBuf::from(windows).join("Fonts/msyh.ttc"));
    } else if cfg!(target_os = "macos") {
        paths.push("/System/Library/Fonts/PingFang.ttc".into());
        paths.push("/System/Library/Fonts/STHeiti Light.ttc".into());
    } else {
        paths.push("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
        paths.push("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".into());
    }
    for path in paths {
        if let Ok(data) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(data).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    tracing::warn!("No CJK font found; set BLUETOOTH_MONITOR_FONT to a Chinese font file");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_have_matching_keys_and_parameters() {
        let en = Language::English.catalog();
        let zh = Language::SimplifiedChinese.catalog();
        assert_eq!(en.keys().collect::<Vec<_>>(), zh.keys().collect::<Vec<_>>());
        fn parameters(text: &str) -> Vec<usize> {
            let mut indices = text
                .split('{')
                .skip(1)
                .filter_map(|part| part.split('}').next()?.parse().ok())
                .collect::<Vec<_>>();
            indices.sort();
            indices
        }
        for (key, english) in en {
            assert!(!english.is_empty() && !zh[key].is_empty(), "{key}");
            assert_eq!(parameters(english), parameters(&zh[key]), "{key}");
        }
    }

    #[test]
    fn application_translation_keys_exist() {
        for source in [include_str!("app.rs"), include_str!("ble/worker.rs")] {
            for marker in ["language.text(", "language.format(", "LocalizedText::new("] {
                for call in source.split(marker).skip(1) {
                    let argument = call.trim_start();
                    if argument.starts_with('"') {
                        let key = serde_json::Deserializer::from_str(argument)
                            .into_iter::<String>()
                            .next()
                            .unwrap()
                            .unwrap();
                        assert!(
                            Language::English.catalog().contains_key(&key),
                            "Missing translation: {key}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn arguments_are_reordered_and_user_text_is_not_translated() {
        let message = LocalizedText::new(
            "将在 {:.1}s 后进行第 {attempt} 次重连",
            &["1.5".into(), "3".into()],
        );
        assert_eq!(message.render(Language::English), "Reconnect #3 in 1.5s");
        assert_eq!(
            message.render(Language::SimplifiedChinese),
            "将在 1.5 秒后进行第 3 次重连"
        );
        assert_eq!(
            Language::English.format("Service {}", &["{0} 设备".into()]),
            "Service {0} 设备"
        );
        assert_eq!(Language::English.text("unknown-key"), "unknown-key");
    }

    #[test]
    fn language_preferences_round_trip_and_unknown_languages_fall_back() {
        for language in Language::ALL {
            let json = serde_json::to_string(&language).unwrap();
            assert_eq!(serde_json::from_str::<Language>(&json).unwrap(), language);
        }
        assert_eq!(
            serde_json::from_str::<Language>("\"future-locale\"").unwrap(),
            Language::default()
        );
    }
}
