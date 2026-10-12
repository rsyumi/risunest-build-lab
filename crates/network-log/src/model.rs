use serde::{Deserialize, Serialize};

pub const MIB: u64 = 1024 * 1024;
pub const PAGE_BYTES: usize = 256 * 1024;
pub const BODY_BYTES: u64 = 512 * MIB;
pub const METADATA_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub enabled: bool,
    pub retain: bool,
    pub hide_auth: bool,
    pub recent_count: u32,
    pub recent_mib: u64,
    pub retained_mib: u64,
    #[serde(default = "default_buffer_mib")]
    pub buffer_mib: u64,
    #[serde(default)]
    pub capture_binary: bool,
}
fn default_buffer_mib() -> u64 {
    32
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            retain: false,
            hide_auth: true,
            recent_count: 200,
            recent_mib: 1024,
            retained_mib: 1024,
            buffer_mib: default_buffer_mib(),
            capture_binary: false,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        let budgets = [128, 256, 512, 1024, 2048, 4096, 8192];
        if ![100, 200, 500, 1000].contains(&self.recent_count)
            || !budgets.contains(&self.recent_mib)
            || !budgets.contains(&self.retained_mib)
            || ![4, 8, 16, 32, 64, 128].contains(&self.buffer_mib)
        {
            return Err("invalid-network-log-settings".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
    pub encoding: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Request,
    Response,
}
impl Side {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Request => 0,
            Self::Response => 1,
        }
    }
    pub(crate) fn file(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    pub observed_bytes: u64,
    pub captured_bytes: u64,
    pub binary: bool,
    pub truncated: bool,
    pub representation: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub method: Option<String>,
    pub url: String,
    pub status: Option<u16>,
    pub headers: Vec<Header>,
    pub body: Body,
}
impl Message {
    pub(crate) fn queued_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.url.capacity()
            + self.method.as_ref().map_or(0, String::capacity)
            + self.headers.capacity() * std::mem::size_of::<Header>()
            + self
                .headers
                .iter()
                .map(|h| {
                    h.name.capacity()
                        + h.value.capacity()
                        + h.encoding.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub source: String,
    pub started_at: u64,
    pub elapsed_ms: u64,
    pub retained: bool,
    pub header_masking: bool,
    pub state: String,
    pub capture_error: Option<String>,
    pub request: Message,
    pub response: Message,
    pub stream_kind: Option<String>,
    pub event_bytes: u64,
}
impl Record {
    pub(crate) fn body(&self, side: Side) -> &Body {
        match side {
            Side::Request => &self.request.body,
            Side::Response => &self.response.body,
        }
    }
    pub(crate) fn body_mut(&mut self, side: Side) -> &mut Body {
        match side {
            Side::Request => &mut self.request.body,
            Side::Response => &mut self.response.body,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    pub id: String,
    pub source: String,
    pub started_at: u64,
    pub elapsed_ms: u64,
    pub retained: bool,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub state: String,
    pub captured_bytes: u64,
    pub incomplete: bool,
    pub stream_kind: Option<String>,
}
impl From<&Record> for ListItem {
    fn from(r: &Record) -> Self {
        Self {
            id: r.id.clone(),
            source: r.source.clone(),
            started_at: r.started_at,
            elapsed_ms: r.elapsed_ms,
            retained: r.retained,
            method: r.request.method.clone().unwrap_or_default(),
            url: r.request.url.clone(),
            status: r.response.status,
            state: r.state.clone(),
            captured_bytes: r.request.body.captured_bytes + r.response.body.captured_bytes,
            // Binary bodies left out by the binary-capture setting are not a recording loss.
            incomplete: match r.capture_error.as_deref() {
                Some("binary-skipped") => false,
                Some(_) => true,
                None => r.request.body.truncated || r.response.body.truncated,
            },
            stream_kind: r.stream_kind.clone(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub retained: bool,
    pub search: String,
    pub status: String,
    pub offset: u32,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub items: Vec<ListItem>,
    pub has_more: bool,
    pub recent_bytes: u64,
    pub retained_bytes: u64,
    pub warning: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyPage {
    pub text: String,
    pub offset: u64,
    pub next_offset: u64,
    pub total: u64,
    pub binary: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Arrivals {
    pub times: Vec<Option<u64>>,
    pub next: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub elapsed_ms: u64,
    pub side: Side,
    pub offset: u64,
    pub length: u64,
    pub kind: String,
}
