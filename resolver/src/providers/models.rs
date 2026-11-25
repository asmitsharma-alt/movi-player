use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    FourKHdHub,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderMediaId {
    pub provider: ProviderKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Movie,
    Series,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: ProviderMediaId,
    pub title: String,
    pub media_type: MediaType,
    pub year: Option<String>,
    pub poster_url: Option<String>,
    pub season_count: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionIntent {
    Playback,
    #[cfg_attr(not(test), allow(dead_code))]
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMirror {
    pub label: String,
    pub resolver_url: String,
    pub headers: Vec<(String, String)>,
    pub direct_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub provider: ProviderKind,
    pub filename: String,
    pub quality: Option<String>,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub size_bytes: Option<u64>,
    pub season: Option<usize>,
    pub episode: Option<usize>,
    pub mirrors: Vec<SourceMirror>,
    #[serde(default)]
    pub resource_id: Option<String>,
}
impl Release {
    pub fn resolution_u64(&self) -> u64 {
        self.quality
            .as_deref()
            .and_then(|q| {
                let trimmed = q.trim();
                if trimmed.eq_ignore_ascii_case("4k") || trimmed.eq_ignore_ascii_case("uhd") {
                    return Some(2160);
                }
                trimmed.trim_end_matches(['p', 'P']).parse::<u64>().ok()
            })
            .unwrap_or(1080)
    }
}

#[derive(Debug, Clone)]
pub struct PlaybackSource {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub browser_readable: bool,
}
pub fn extract_4digit_year(raw: &str) -> String {
    raw.as_bytes()
        .windows(4)
        .find(|window| window.iter().all(u8::is_ascii_digit) && matches!(window[0], b'1' | b'2'))
        .and_then(|window| std::str::from_utf8(window).ok())
        .map(str::to_string)
        .unwrap_or_default()
}

pub fn parse_size_bytes(text: &str) -> Option<u64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    for i in 0..parts.len() {
        let clean = parts[i].trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
        if let Ok(num) = clean.parse::<f64>()
            && i + 1 < parts.len()
        {
            let unit = parts[i + 1]
                .trim_matches(|c: char| !c.is_ascii_alphabetic())
                .to_ascii_uppercase();
            match unit.as_str() {
                "TB" | "TIB" | "T" => return Some((num * 1_099_511_627_776.0) as u64),
                "GB" | "GIB" | "G" => return Some((num * 1_073_741_824.0) as u64),
                "MB" | "MIB" | "M" => return Some((num * 1_048_576.0) as u64),
                "KB" | "KIB" | "K" => return Some((num * 1_024.0) as u64),
                _ => {}
            }
        }
        let upper = clean.to_ascii_uppercase();
        for (suffix, multiplier) in [
            ("TIB", 1_099_511_627_776.0),
            ("TB", 1_099_511_627_776.0),
            ("GIB", 1_073_741_824.0),
            ("GB", 1_073_741_824.0),
            ("MIB", 1_048_576.0),
            ("MB", 1_048_576.0),
            ("KIB", 1_024.0),
            ("KB", 1_024.0),
        ] {
            if let Some(num_str) = upper.strip_suffix(suffix)
                && let Ok(num) = num_str.parse::<f64>()
            {
                return Some((num * multiplier) as u64);
            }
        }
    }
    None
}
