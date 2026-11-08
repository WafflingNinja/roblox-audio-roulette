// Roblox Creator Store audio queries + cookie-authenticated download.
// All endpoints are public except the asset download, which needs the cookie.

use rand::seq::SliceRandom;
use serde::Deserialize;
use std::time::Duration;

const MARKETPLACE: &str = "https://apis.roblox.com/toolbox-service/v1/marketplace/3";
const DETAILS: &str = "https://apis.roblox.com/toolbox-service/v1/items/details";
const DELIVERY: &str = "https://assetdelivery.roblox.com/v1/asset/";
const PAGE_SIZE: u32 = 100;
const MAX_PAGE: u32 = 10; // totalResults caps at 1000 => 10 pages of 100
const CYRILLIC_MAX_PAGES: usize = 4; // keyword pages are dense Cyrillic; 4 is enough

// Common Russian words; a random one per roll surfaces different Cyrillic sounds.
const CYRILLIC_KEYWORDS: &[&str] = &[
    "музыка", "песня", "звук", "бой", "грусть", "любовь", "ночь", "русская",
    "шансон", "рэп", "сигнал", "голос", "смех", "крик", "мем", "фонк",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Music,
    Sfx,
    Both,
}

impl Filter {
    fn audio_types(self) -> &'static str {
        match self {
            Filter::Music => "Music",
            Filter::Sfx => "SoundEffect",
            Filter::Both => "",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Filter::Music => "Music",
            Filter::Sfx => "SFX",
            Filter::Both => "Both",
        }
    }
}

#[derive(Clone)]
pub struct Sound {
    pub id: u64,
    pub name: String,
    pub creator: String,
    pub duration: u32,
    pub kind: String, // "Music" / "SoundEffect"
}

#[derive(Deserialize)]
struct IdResult {
    id: u64,
}
#[derive(Deserialize)]
struct MarketResp {
    data: Vec<IdResult>,
}

#[derive(Deserialize)]
struct AudioDetails {
    #[serde(rename = "audioType")]
    audio_type: Option<String>,
    artist: Option<String>,
}
#[derive(Deserialize)]
struct AssetBody {
    id: u64,
    name: String,
    duration: Option<u32>,
    #[serde(rename = "audioDetails")]
    audio_details: Option<AudioDetails>,
}
#[derive(Deserialize)]
struct Creator {
    name: String,
}
#[derive(Deserialize)]
struct DetailItem {
    asset: AssetBody,
    creator: Creator,
}
#[derive(Deserialize)]
struct DetailResp {
    data: Vec<DetailItem>,
}

#[derive(Clone)]
pub struct Api {
    client: reqwest::blocking::Client,
    cookie: Option<String>,
}

/// A name counts as Cyrillic if it contains at least one Cyrillic letter.
pub fn is_cyrillic(name: &str) -> bool {
    name.chars().any(|c| matches!(c, '\u{0400}'..='\u{04FF}' | '\u{0500}'..='\u{052F}'))
}

impl Api {
    pub fn new(cookie: Option<String>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Api { client, cookie }
    }

    fn get_retry(&self, req: impl Fn() -> reqwest::blocking::RequestBuilder) -> Result<reqwest::blocking::Response, String> {
        let mut last = String::new();
        for attempt in 0..3 {
            match req().send() {
                Ok(r) if r.status() == 429 => {
                    last = "rate limited (429)".into();
                    std::thread::sleep(Duration::from_millis(600 * (attempt + 1)));
                }
                Ok(r) if r.status().is_success() => return Ok(r),
                Ok(r) => return Err(format!("HTTP {}", r.status())),
                Err(e) => {
                    last = e.to_string();
                    std::thread::sleep(Duration::from_millis(400));
                }
            }
        }
        Err(last)
    }

    fn page_ids(&self, filter: Filter, page: u32, keyword: &str) -> Result<Vec<u64>, String> {
        let url = MARKETPLACE.to_string();
        let at = filter.audio_types();
        let resp = self.get_retry(|| {
            let mut r = self
                .client
                .get(&url)
                .query(&[("limit", PAGE_SIZE.to_string()), ("pageNumber", page.to_string())])
                .query(&[("keyword", keyword)]);
            // Empty audioTypes 400s — only send it when filtering.
            if !at.is_empty() {
                r = r.query(&[("audioTypes", at)]);
            }
            r
        })?;
        let parsed: MarketResp = resp.json().map_err(|e| e.to_string())?;
        Ok(parsed.data.into_iter().map(|r| r.id).collect())
    }

    /// Pull `count` random audio items matching the filter.
    /// `cyrillic` keeps only sounds whose name contains Cyrillic letters.
    pub fn random_batch(&self, filter: Filter, count: usize, cyrillic: bool) -> Result<Vec<Sound>, String> {
        let mut rng = rand::thread_rng();
        let mut pages: Vec<u32> = (1..=MAX_PAGE).collect();
        pages.shuffle(&mut rng);

        if !cyrillic {
            let mut ids: Vec<u64> = Vec::new();
            for page in pages {
                if ids.len() >= count {
                    break;
                }
                ids.extend(self.page_ids(filter, page, "")?);
            }
            ids.shuffle(&mut rng);
            ids.truncate(count);
            return self.details(&ids);
        }

        // Cyrillic: the generic pool has almost no Cyrillic names, so search with
        // a random Russian keyword instead, then keep only Cyrillic-named results.
        let kw = *CYRILLIC_KEYWORDS.choose(&mut rng).unwrap();
        let mut kept: Vec<Sound> = Vec::new();
        // Keyword results are almost all Cyrillic, so a few pages is plenty.
        for page in pages.into_iter().take(CYRILLIC_MAX_PAGES) {
            if kept.len() >= count {
                break;
            }
            let ids = self.page_ids(filter, page, kw)?;
            if ids.is_empty() {
                break;
            }
            let mut details = self.details(&ids)?;
            details.retain(|s| is_cyrillic(&s.name));
            kept.append(&mut details);
        }
        // Drop duplicate ids (a keyword can repeat across pages).
        kept.sort_by_key(|s| s.id);
        kept.dedup_by_key(|s| s.id);
        kept.shuffle(&mut rng);
        kept.truncate(count);
        Ok(kept)
    }

    /// Fetch full details for a set of ids. IDs that come back missing are deleted/private.
    pub fn details(&self, ids: &[u64]) -> Result<Vec<Sound>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(50) {
            let joined = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            let resp = self.get_retry(|| self.client.get(DETAILS).query(&[("assetIds", &joined)]))?;
            let parsed: DetailResp = resp.json().map_err(|e| e.to_string())?;
            for it in parsed.data {
                let ad = it.asset.audio_details.unwrap_or(AudioDetails {
                    audio_type: None,
                    artist: None,
                });
                out.push(Sound {
                    id: it.asset.id,
                    name: it.asset.name,
                    creator: ad.artist.filter(|s| !s.is_empty()).unwrap_or(it.creator.name),
                    duration: it.asset.duration.unwrap_or(0),
                    kind: ad.audio_type.unwrap_or_else(|| "?".into()),
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(out)
    }

    /// Which of these ids still exist (i.e. are returned by the details endpoint).
    pub fn existing_ids(&self, ids: &[u64]) -> Result<std::collections::HashSet<u64>, String> {
        Ok(self.details(ids)?.into_iter().map(|s| s.id).collect())
    }

    /// Download the audio bytes for one id. Requires the cookie.
    pub fn download(&self, id: u64) -> Result<Vec<u8>, String> {
        let cookie = self.cookie.as_ref().ok_or("not logged in (no Firefox cookie)")?;
        let resp = self.get_retry(|| {
            self.client
                .get(DELIVERY)
                .query(&[("id", id.to_string())])
                .header("Cookie", format!(".ROBLOSECURITY={cookie}"))
        })?;
        let bytes = resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())?;
        if !looks_like_audio(&bytes) {
            return Err("unavailable (private or not usable)".into());
        }
        Ok(bytes)
    }

    pub fn has_cookie(&self) -> bool {
        self.cookie.is_some()
    }
}

/// True if bytes start with an Ogg or MP3 signature.
fn looks_like_audio(b: &[u8]) -> bool {
    if b.len() < 4 {
        return false;
    }
    &b[..4] == b"OggS"
        || &b[..3] == b"ID3"
        || (b[0] == 0xff && (b[1] & 0xe0) == 0xe0) // MP3 frame sync
}
