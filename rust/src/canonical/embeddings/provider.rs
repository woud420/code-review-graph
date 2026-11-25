use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::env;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

#[derive(Clone, Copy, PartialEq)]
enum Api {
    OpenAi,
    Voyage,
    MiniMax,
}
pub(super) struct Provider {
    api: Api,
    base: Url,
    key: String,
    model: String,
    pub identity: String,
    dimension: Option<i64>,
    dtype: String,
    batch: usize,
    interval: Duration,
    last: Mutex<Option<Instant>>,
    http: ureq::Agent,
}
fn setting(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.is_empty())
}
fn number(key: &str) -> Result<Option<i64>> {
    setting(key)
        .map(|v| {
            v.trim()
                .parse()
                .map_err(|_| anyhow!("invalid literal for int() with base 10: '{v}'"))
        })
        .transpose()
}
fn host_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let host = match url.port() {
        Some(p) if host.contains(':') => format!("[{host}]:{p}"),
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };
    let path = url.path().trim_end_matches('/');
    let path = path
        .strip_suffix("/embeddings")
        .unwrap_or(path)
        .trim_end_matches('/');
    format!("{}://{host}{path}", url.scheme())
}
fn cloud_warning(name: &str, base: &Url) {
    if setting("CRG_ACCEPT_CLOUD_EMBEDDINGS").as_deref() == Some("1")
        || matches!(
            base.host_str(),
            Some("127.0.0.1" | "localhost" | "0.0.0.0" | "[::1]")
        )
    {
        return;
    }
    eprintln!(
        "\n⚠️  code-review-graph: about to embed code via the '{name}' cloud provider.\n    Your source code (function names, docstrings, file paths) will be sent to an external API.\n    This is necessary for semantic search with the cloud provider you selected.\n    To skip this warning in future runs, set CRG_ACCEPT_CLOUD_EMBEDDINGS=1 in your environment.\n    To stay fully offline, use the default 'local' provider instead (no API key needed).\n"
    );
}
impl Provider {
    pub fn select(name: Option<&str>, model: Option<&str>) -> Result<Option<Self>> {
        let resolved = name.map(|s| s.trim().to_lowercase()).unwrap_or_else(|| {
            if setting("CRG_OPENAI_API_KEY").is_some() && setting("CRG_OPENAI_BASE_URL").is_some() {
                "openai".into()
            } else {
                "local".into()
            }
        });
        let model = model.filter(|m| !m.is_empty());
        let (mut dim, mut dtype, mut batch, mut interval) =
            (None, String::new(), 100i64, Duration::ZERO);
        let (api, base, key, model) = match resolved.as_str() {
            "openai" => {
                let fields = [
                    ("CRG_OPENAI_API_KEY", setting("CRG_OPENAI_API_KEY")),
                    ("CRG_OPENAI_BASE_URL", setting("CRG_OPENAI_BASE_URL")),
                    (
                        "CRG_OPENAI_MODEL",
                        model
                            .map(str::to_owned)
                            .or_else(|| setting("CRG_OPENAI_MODEL")),
                    ),
                ];
                let missing: Vec<_> = fields
                    .iter()
                    .filter_map(|(name, value)| value.is_none().then_some(*name))
                    .collect();
                ensure!(
                    missing.is_empty(),
                    "Missing required environment variable(s) for the OpenAI embedding provider: {}.",
                    missing.join(", ")
                );
                let [(_, Some(key)), (_, Some(base)), (_, Some(model))] = fields else {
                    unreachable!()
                };
                dim = number("CRG_OPENAI_DIMENSION")?;
                batch = number("CRG_OPENAI_BATCH_SIZE")?
                    .filter(|v| *v != 0)
                    .unwrap_or(100);
                (Api::OpenAi, base, key, model)
            }
            "voyage" => {
                let key=setting("VOYAGE_API_KEY").ok_or_else(||anyhow!("VOYAGE_API_KEY environment variable is required for the Voyage embedding provider."))?;
                dim = Some(
                    number("CRG_VOYAGE_OUTPUT_DIMENSION")?
                        .filter(|v| *v != 0)
                        .unwrap_or(1024),
                );
                dtype = setting("CRG_VOYAGE_OUTPUT_DTYPE").unwrap_or_else(|| "float".into());
                batch = number("CRG_VOYAGE_BATCH_SIZE")?
                    .filter(|v| *v != 0)
                    .unwrap_or(100);
                if let Some(v) = setting("CRG_VOYAGE_MIN_INTERVAL_SEC") {
                    let seconds = v
                        .parse::<f64>()
                        .map_err(|_| anyhow!("could not convert string to float: '{v}'"))?;
                    interval = Duration::try_from_secs_f64(seconds.max(0.))
                        .map_err(|_| anyhow!("invalid request interval"))?;
                }
                let base = setting("CRG_VOYAGE_BASE_URL")
                    .unwrap_or_else(|| "https://api.voyageai.com/v1".into());
                let model = model
                    .map(str::to_owned)
                    .or_else(|| setting("CRG_VOYAGE_MODEL"))
                    .unwrap_or_else(|| "voyage-code-3".into());
                (Api::Voyage, base, key, model)
            }
            "minimax" => {
                let key=setting("MINIMAX_API_KEY").ok_or_else(||anyhow!("MINIMAX_API_KEY environment variable is required for the MiniMax embedding provider."))?;
                (
                    Api::MiniMax,
                    "https://api.minimax.io/v1".into(),
                    key,
                    "embo-01".into(),
                )
            }
            "google" => {
                ensure!(
                    setting("GOOGLE_API_KEY").is_some(),
                    "GOOGLE_API_KEY environment variable is required for the Google embedding provider."
                );
                return Ok(None);
            }
            "local" | "" => return Ok(None),
            other => bail!(
                "Unknown embedding provider '{other}'. Valid: local, openai, google, minimax, voyage"
            ),
        };
        let base = Url::parse(&base)?;
        ensure!(
            matches!(base.scheme(), "http" | "https"),
            "embedding endpoint requires HTTP or HTTPS"
        );
        cloud_warning(&resolved, &base);
        let host = host_key(&base);
        let identity = match api {
            Api::OpenAi => format!("openai:{model}@{host}"),
            Api::Voyage => format!("voyage:{model}:dim{}:{dtype}@{host}", dim.unwrap_or(1024)),
            Api::MiniMax => "minimax:embo-01".into(),
        };
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(if api == Api::MiniMax {
                60
            } else {
                120
            }))
            .redirects(0)
            .user_agent("code-review-graph/2.4.0 (+https://github.com/tirth8205/code-review-graph)")
            .build();
        Ok(Some(Self {
            api,
            base,
            key,
            model,
            identity,
            dimension: dim,
            dtype,
            batch: batch.max(1) as usize,
            interval,
            last: Mutex::default(),
            http,
        }))
    }
    pub fn vectors(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f64>>> {
        let mut out = Vec::new();
        for batch in texts.chunks(self.batch) {
            let mut body = json!({"model":self.model,"input":batch});
            let task = if query { "query" } else { "document" };
            let label = match self.api {
                Api::OpenAi => {
                    if let Some(dim) = self.dimension {
                        body["dimensions"] = dim.into();
                    }
                    "OpenAI"
                }
                Api::Voyage => {
                    body["input_type"] = task.into();
                    body["output_dimension"] = json!(self.dimension);
                    body["output_dtype"] = json!(self.dtype);
                    "Voyage"
                }
                Api::MiniMax => {
                    body = json!({"model":self.model,"texts":batch,"type":if query{"query"}else{"db"}});
                    "MiniMax"
                }
            };
            let endpoint = format!("{}/embeddings", self.base.as_str().trim_end_matches('/'));
            for attempt in 0..3 {
                if !self.interval.is_zero() {
                    let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(previous) = *last {
                        std::thread::sleep(self.interval.saturating_sub(previous.elapsed()));
                    }
                    *last = Some(Instant::now());
                }
                let request = self
                    .http
                    .post(&endpoint)
                    .set("Authorization", &format!("Bearer {}", self.key))
                    .set("Accept", "application/json");
                match request.send_json(&body) {
                    Ok(resp) => {
                        let value: Value = resp.into_json()?;
                        if self.api == Api::MiniMax {
                            ensure!(
                                value["base_resp"]["status_code"].as_i64().unwrap_or(0) == 0,
                                "MiniMax API error: {}",
                                value["base_resp"]["status_msg"]
                                    .as_str()
                                    .unwrap_or("unknown")
                            );
                            let vectors: Vec<Vec<f64>> =
                                serde_json::from_value(value["vectors"].clone())?;
                            ensure!(
                                vectors.len() == batch.len(),
                                "MiniMax API returned a different number of vectors than inputs"
                            );
                            out.extend(vectors);
                        } else {
                            out.extend(decode(label, value, batch.len())?);
                        }
                        break;
                    }
                    Err(ureq::Error::Status(code, resp)) => {
                        let text = resp.into_string().unwrap_or_default();
                        let err: Value = serde_json::from_str(&text).unwrap_or_default();
                        let message = err["error"]["message"]
                            .as_str()
                            .or(err["error"].as_str())
                            .unwrap_or(&text);
                        if attempt == 2 || !(code == 429 || (500..600).contains(&code)) {
                            bail!("{label} API HTTP {code}: {message}");
                        }
                    }
                    Err(ureq::Error::Transport(err)) if attempt == 2 => {
                        bail!("{label} API request failed: {}", err.kind())
                    }
                    Err(_) => (),
                }
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
        }
        Ok(out)
    }
}
fn decode(label: &str, value: Value, len: usize) -> Result<Vec<Vec<f64>>> {
    if let Some(error) = value.get("error") {
        bail!(
            "{label} API error: {}",
            error["message"]
                .as_str()
                .or(error.as_str())
                .unwrap_or("unknown")
        );
    }
    let mut rows = value["data"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("{label} API returned empty data"))?
        .iter()
        .collect::<Vec<_>>();
    let indices = rows
        .iter()
        .filter_map(|v| v["index"].as_i64())
        .collect::<Vec<_>>();
    if indices.len() == rows.len() {
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        ensure!(
            sorted.iter().copied().eq(0..len as i64),
            "{label} API returned malformed indices (got {indices:?}, expected permutation of 0..{}) — refusing to misalign vectors.",
            len - 1
        );
        rows.sort_by_key(|v| v["index"].as_i64());
    } else if rows.iter().any(|v| v.get("index").is_some()) {
        bail!("{label} API returned mixed indexed/unindexed data — refusing to misalign vectors.");
    } else {
        ensure!(
            len == rows.len(),
            "{label} API returned {} embeddings for {len} inputs with no index field — refusing to misalign vectors.",
            rows.len()
        );
    }
    Ok(rows
        .into_iter()
        .map(|v| serde_json::from_value(v["embedding"].clone()))
        .collect::<serde_json::Result<_>>()?)
}
