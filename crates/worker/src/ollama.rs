//! Private Ollama `/api/chat` transport shared by the API intent classifier
//! (`api::assistant_provider::ollama`) and the worker's program-fact
//! extraction (`ai_extraction::provider`). One endpoint policy for both:
//! loopback, the compose service name `ollama` or a private address, port
//! 11434 unless explicitly allowed, no credentials/query/fragment/path, no
//! redirects, no proxy, bounded response body.
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 11434;
/// Upper bound of `options.num_thread`.
pub const MAX_NUM_THREADS: u32 = 64;

/// `options.num_thread` is sent on every request because Ollama sizes its
/// thread pool from the host's cores, not the container's cpuset: on a 2-3
/// core cpuset of a 4-core host that oversubscription made generation ~45x
/// slower. Accepts 1..=[`MAX_NUM_THREADS`].
pub fn valid_num_threads(n: u32) -> bool {
    (1..=MAX_NUM_THREADS).contains(&n)
}

/// CPUs available to this process (cgroup quota and affinity aware), capped
/// at [`MAX_NUM_THREADS`]. Only a fallback: when Ollama runs in another
/// container with a smaller cpuset, configure the thread count explicitly.
pub fn default_num_threads() -> u32 {
    std::thread::available_parallelism()
        .map_or(1, |n| u32::try_from(n.get()).unwrap_or(MAX_NUM_THREADS))
        .clamp(1, MAX_NUM_THREADS)
}

/// Thread count from an optional setting: unset or blank means
/// [`default_num_threads`]; anything else must be an integer in 1..=64.
pub fn parse_num_threads(value: Option<&str>) -> Option<u32> {
    match value.map(str::trim) {
        None | Some("") => Some(default_num_threads()),
        Some(raw) => raw.parse().ok().filter(|n| valid_num_threads(*n)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The endpoint, model name or keep-alive failed the allowlist.
    #[error("invalid ollama configuration")]
    InvalidConfig,
    /// Transport failure, non-success status or timeout.
    #[error("ollama unavailable")]
    Unavailable,
    /// The body exceeded the caller's bound.
    #[error("ollama response too large")]
    TooLarge,
}

/// Ollama `keep_alive`: `-1` (forever) or `<n>s|m|h` with 1-4 digits.
pub fn valid_keep_alive(value: &str) -> bool {
    if value == "-1" {
        return true;
    }
    let Some(unit) = value.chars().last() else {
        return false;
    };
    let number = &value[..value.len() - unit.len_utf8()];
    matches!(unit, 's' | 'm' | 'h')
        && (1..=4).contains(&number.len())
        && number.chars().all(|c| c.is_ascii_digit())
}

/// Ollama model tags: `name:tag`, optional namespace, no spaces or shell text.
pub fn valid_model_name(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | '/'))
}

/// The `/api/chat` URL for `base` when it passes the private-endpoint policy.
pub fn private_chat_endpoint(base: &str, allow_any_port: bool) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(base).ok()?;
    let private = match url.host_str() {
        // The compose service names only, exactly (security review F2).
        Some("localhost" | "ollama" | "planner") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| match ip {
                std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
                std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
            }),
        None => false,
    };
    let port_ok = allow_any_port || url.port_or_known_default() == Some(DEFAULT_PORT);
    if !private
        || !port_ok
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    url.set_path("/api/chat");
    Some(url)
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    model: String,
    keep_alive: String,
    num_threads: u32,
    max_response: usize,
}

impl OllamaClient {
    pub fn new(
        base: &str,
        model: &str,
        timeout: Duration,
        keep_alive: &str,
        allow_any_port: bool,
        num_threads: u32,
        max_response: usize,
    ) -> Result<Self, TransportError> {
        let endpoint =
            private_chat_endpoint(base, allow_any_port).ok_or(TransportError::InvalidConfig)?;
        if !valid_model_name(model)
            || !valid_keep_alive(keep_alive)
            || !valid_num_threads(num_threads)
            || max_response == 0
        {
            return Err(TransportError::InvalidConfig);
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| TransportError::InvalidConfig)?;
        Ok(Self {
            client,
            endpoint,
            model: model.to_owned(),
            keep_alive: keep_alive.to_owned(),
            num_threads,
            max_response,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn keep_alive(&self) -> &str {
        &self.keep_alive
    }

    /// Ollama `options.num_thread`; every payload must carry it.
    pub fn num_threads(&self) -> u32 {
        self.num_threads
    }

    /// The digest Ollama lists for this client's model (`GET /api/tags`;
    /// a bare name matches `name:latest`), `None` when it is not listed.
    pub async fn model_digest(&self) -> Result<Option<String>, TransportError> {
        let mut url = self.endpoint.clone();
        url.set_path("/api/tags");
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| TransportError::Unavailable)?;
        if !response.status().is_success() {
            return Err(TransportError::Unavailable);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| TransportError::Unavailable)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(TransportError::TooLarge);
        }
        let tags: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransportError::Unavailable)?;
        let wanted = |name: &str| {
            name == self.model
                || (!self.model.contains(':') && name == format!("{}:latest", self.model))
        };
        Ok(tags["models"].as_array().and_then(|models| {
            models.iter().find_map(|m| {
                m["name"]
                    .as_str()
                    .filter(|n| wanted(n))
                    .and_then(|_| m["digest"].as_str().map(str::to_owned))
            })
        }))
    }

    /// POST `payload` and read at most `max_response` bytes.
    pub async fn post(&self, payload: &Value) -> Result<Vec<u8>, TransportError> {
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .json(payload)
            .send()
            .await
            .map_err(|_| TransportError::Unavailable)?;
        if !response.status().is_success() {
            return Err(TransportError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|n| n > self.max_response as u64)
        {
            return Err(TransportError::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TransportError::Unavailable)?
        {
            if bytes.len() + chunk.len() > self.max_response {
                return Err(TransportError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

impl OllamaClient {
    /// POST `payload` (with `"stream": true`) and read the newline-delimited
    /// JSON chunks one line at a time, bounded by `max_response` in total.
    pub async fn post_streaming(&self, payload: &Value) -> Result<ChatLines, TransportError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(payload)
            .send()
            .await
            .map_err(|_| TransportError::Unavailable)?;
        if !response.status().is_success() {
            return Err(TransportError::Unavailable);
        }
        Ok(ChatLines {
            response,
            buffer: Vec::new(),
            read: 0,
            max_response: self.max_response,
            done: false,
        })
    }
}

/// Lines of a streamed `/api/chat` response.
pub struct ChatLines {
    response: reqwest::Response,
    buffer: Vec<u8>,
    read: usize,
    max_response: usize,
    done: bool,
}

impl ChatLines {
    /// The next non-empty line, or `None` at the end of the body.
    pub async fn next_line(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                let line = line.trim_ascii();
                if line.is_empty() {
                    continue;
                }
                return Ok(Some(line.to_vec()));
            }
            if self.done {
                let rest = std::mem::take(&mut self.buffer);
                let rest = rest.trim_ascii();
                return Ok((!rest.is_empty()).then(|| rest.to_vec()));
            }
            match self
                .response
                .chunk()
                .await
                .map_err(|_| TransportError::Unavailable)?
            {
                Some(chunk) => {
                    self.read += chunk.len();
                    if self.read > self.max_response {
                        return Err(TransportError::TooLarge);
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                None => self.done = true,
            }
        }
    }
}

/// The assistant message of a complete, non-streamed `/api/chat` response.
/// `None` for a malformed envelope or a generation that stopped for any reason
/// other than `stop` (for example `length`: the JSON would be truncated).
pub fn chat_content(bytes: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }
    #[derive(Deserialize)]
    struct Envelope {
        message: Message,
        done: bool,
        done_reason: Option<String>,
    }
    let envelope: Envelope = serde_json::from_slice(bytes).ok()?;
    if !envelope.done || envelope.done_reason.as_deref().is_some_and(|s| s != "stop") {
        return None;
    }
    Some(envelope.message.content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_policy_rejects_public_metadata_and_decorated_urls() {
        for url in [
            "https://evil.example",
            "http://169.254.169.254:11434",
            "http://localhost@evil.example",
            "http://user:pw@127.0.0.1:11434",
            "http://localhost:11434/path",
            "http://localhost:11434?secret=x",
            "http://localhost:11434#frag",
            "http://127.0.0.1:5432",
            "ftp://127.0.0.1:11434",
            "http://ollama",
            // Only the exact compose service names (security review F2).
            "http://planner2:11434",
            "http://my-planner:11434",
            "http://planner.evil.example:11434",
        ] {
            assert!(private_chat_endpoint(url, false).is_none(), "{url}");
        }
        for url in [
            "http://ollama:11434",
            "http://planner:11434",
            "http://127.0.0.1:11434/",
            "http://10.0.0.5:11434",
            "http://[::1]:11434",
            "http://[fd00::1]:11434",
        ] {
            let endpoint = private_chat_endpoint(url, false).expect(url);
            assert_eq!(endpoint.path(), "/api/chat");
        }
        assert!(private_chat_endpoint("http://127.0.0.1:5432", true).is_some());
        assert!(private_chat_endpoint("http://8.8.8.8:5432", true).is_none());
    }

    #[test]
    fn model_and_keep_alive_are_bounded() {
        for ok in ["qwen3.5:4b", "qwen3.5:9b-q4_K_M", "library/qwen:1"] {
            assert!(valid_model_name(ok), "{ok}");
        }
        for bad in ["", "a b", "x;rm", &"m".repeat(101)] {
            assert!(!valid_model_name(bad), "{bad}");
        }
        for ok in ["30m", "-1", "1h", "600s"] {
            assert!(valid_keep_alive(ok), "{ok}");
        }
        for bad in ["", "m", "30", "30d", "99999m", "1h; rm", "-2"] {
            assert!(!valid_keep_alive(bad), "{bad}");
        }
    }

    #[test]
    fn chat_content_requires_a_finished_stop() {
        assert_eq!(
            chat_content(br#"{"message":{"content":"{}"},"done":true,"done_reason":"stop"}"#)
                .as_deref(),
            Some("{}")
        );
        assert!(chat_content(
            br#"{"message":{"content":"{"},"done":false,"done_reason":"length"}"#
        )
        .is_none());
        assert!(
            chat_content(br#"{"message":{"content":"{"},"done":true,"done_reason":"length"}"#)
                .is_none()
        );
        assert!(chat_content(b"not json").is_none());
    }

    #[test]
    fn client_rejects_invalid_configuration() {
        let t = Duration::from_secs(1);
        assert_eq!(
            OllamaClient::new("http://ollama:11434", "bad model", t, "5m", false, 4, 10).err(),
            Some(TransportError::InvalidConfig)
        );
        assert_eq!(
            OllamaClient::new("http://ollama:11434", "m", t, "5d", false, 4, 10).err(),
            Some(TransportError::InvalidConfig)
        );
        assert_eq!(
            OllamaClient::new("http://1.1.1.1:11434", "m", t, "5m", false, 4, 10).err(),
            Some(TransportError::InvalidConfig)
        );
        for threads in [0, MAX_NUM_THREADS + 1] {
            assert_eq!(
                OllamaClient::new("http://ollama:11434", "m", t, "5m", false, threads, 10).err(),
                Some(TransportError::InvalidConfig),
                "{threads}"
            );
        }
        assert!(OllamaClient::new("http://ollama:11434", "m", t, "5m", false, 4, 10).is_ok());
        assert!(OllamaClient::new(
            "http://ollama:11434",
            "m",
            t,
            "5m",
            false,
            MAX_NUM_THREADS,
            10
        )
        .is_ok());
    }

    #[test]
    fn num_threads_default_to_available_cpus_and_are_bounded() {
        let available = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
        let default = default_num_threads();
        assert!(valid_num_threads(default));
        assert_eq!(default, available.min(MAX_NUM_THREADS));
        assert_eq!(parse_num_threads(None), Some(default));
        assert_eq!(parse_num_threads(Some(" ")), Some(default));
        assert_eq!(parse_num_threads(Some("3")), Some(3));
        assert_eq!(parse_num_threads(Some(" 64 ")), Some(64));
        for bad in ["0", "65", "-1", "x", "2.5", "3;rm"] {
            assert_eq!(parse_num_threads(Some(bad)), None, "{bad}");
        }
    }
}
