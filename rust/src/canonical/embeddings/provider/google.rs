use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Deserialize)]
struct Embeddings {
    embeddings: Vec<Embedding>,
}
#[derive(Deserialize)]
struct Embedding {
    values: Vec<f64>,
}

pub(super) fn vectors(
    http: &ureq::Agent,
    base: &url::Url,
    key: &str,
    model: &str,
    texts: &[String],
    query: bool,
) -> Result<Vec<Vec<f64>>> {
    ensure!(
        !model.contains("..") && !model.contains(['?', '&']),
        "invalid model parameter."
    );
    let model = if model.starts_with("models/") || model.starts_with("tunedModels/") {
        model.to_owned()
    } else {
        format!("models/{model}")
    };
    let task = if query {
        "RETRIEVAL_QUERY"
    } else {
        "RETRIEVAL_DOCUMENT"
    };
    let endpoint = format!(
        "{}/v1beta/{model}:batchEmbedContents",
        base.as_str().trim_end_matches('/')
    );
    let mut out = Vec::new();
    for batch in texts.chunks(100) {
        let requests=batch.iter().map(|text|json!({"model":model,"content":{"parts":[{"text":text}],"role":"user"},"taskType":task})).collect::<Vec<_>>();
        for attempt in 0..3 {
            match http
                .post(&endpoint)
                .set("x-goog-api-key", key)
                .set("Accept", "application/json")
                .send_json(json!({"requests":requests}))
            {
                Ok(response) => {
                    let embeddings: Embeddings = response.into_json()?;
                    ensure!(
                        embeddings.embeddings.len() == batch.len(),
                        "Google API returned a different number of vectors than inputs"
                    );
                    out.extend(embeddings.embeddings.into_iter().map(|e| e.values));
                    break;
                }
                Err(ureq::Error::Status(code, response)) => {
                    let raw = response.into_string()?;
                    let details = serde_json::from_str::<Value>(&raw)
                        .unwrap_or_else(|_| json!({"message":raw}));
                    let status = details["status"]
                        .as_str()
                        .or(details["error"]["status"].as_str())
                        .unwrap_or("None");
                    let message =
                        format!("{code} {status}. {}", crate::model::value_repr(&details));
                    if !["429", "500", "503"].iter().any(|s| message.contains(s)) {
                        bail!("{message}");
                    }
                    if attempt == 2 {
                        eprintln!("ERROR: Gemini API request failed after 3 requests.");
                        bail!("{message}");
                    }
                    let wait = 1 << attempt;
                    eprintln!(
                        "WARNING: Gemini API retry {}/3 in {wait}s ({}): {message}",
                        attempt + 1,
                        if code < 500 {
                            "ClientError"
                        } else {
                            "ServerError"
                        }
                    );
                    std::thread::sleep(Duration::from_secs(wait));
                }
                Err(err) => bail!("Gemini API request failed: {err}"),
            }
        }
    }
    Ok(out)
}
