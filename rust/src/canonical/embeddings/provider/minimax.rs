use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;

fn request(
    http: &ureq::Request,
    model: &str,
    texts: &[String],
    query: bool,
) -> Result<Vec<Vec<f64>>> {
    let body = json!({"model":model,"texts":texts,"type":if query{"query"}else{"db"}});
    let response = match http.clone().send_json(&body) {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, resp)) => bail!("HTTP Error {code}: {}", resp.status_text()),
        Err(err) => return Err(err.into()),
    };
    let reply: Value = response.into_json()?;
    let status = &reply["base_resp"];
    ensure!(
        status["status_code"].as_i64().unwrap_or(0) == 0,
        "MiniMax API error: {}",
        status["status_msg"].as_str().unwrap_or("unknown")
    );
    let vectors: Vec<Vec<f64>> = serde_json::from_value(reply["vectors"].clone())?;
    ensure!(
        vectors.len() == texts.len(),
        "MiniMax API returned a different number of vectors than inputs"
    );
    Ok(vectors)
}

pub(super) fn vectors(
    http: &ureq::Request,
    model: &str,
    texts: &[String],
    query: bool,
) -> Result<Vec<Vec<f64>>> {
    let mut out = Vec::new();
    for batch in texts.chunks(100) {
        for attempt in 0..3 {
            match request(http, model, batch, query) {
                Ok(v) => {
                    out.extend(v);
                    break;
                }
                Err(e) => {
                    if attempt == 2
                        || !["429", "500", "503"]
                            .iter()
                            .any(|s| e.to_string().contains(s))
                    {
                        return Err(e);
                    }
                    let wait = 1 << attempt;
                    eprintln!(
                        "WARNING: MiniMax API error (attempt {}/3), retrying in {wait}s: {e}",
                        attempt + 1
                    );
                    std::thread::sleep(Duration::from_secs(wait));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::Provider;
    use anyhow::Result;
    use serde_json::json;
    #[test]
    #[ignore = "run make minimax-parity to start the paired loopback provider fixture"]
    fn paired_minimax_protocol() -> Result<()> {
        let port = std::env::var("CRG_TEST_MINIMAX_PORT")?.parse::<u16>()?;
        let path = std::env::var("CRG_TEST_MINIMAX_RESULT")?;
        let mut provider = Provider::select(Some("minimax"), None)?.expect("built-in MiniMax");
        assert_eq!(provider.key, "local-fixture");
        provider.base = url::Url::parse(&format!("http://127.0.0.1:{port}/v1"))?;
        let texts = (0..103).map(|i| format!("doc-{i}")).collect::<Vec<_>>();
        let docs = provider.vectors(&texts, false)?;
        let query = provider.vectors(&["search".into()], true)?;
        let mut errors = Vec::new();
        for text in ["service-error", "unauthorized"] {
            errors.push(
                provider
                    .vectors(&[text.to_owned()], false)
                    .unwrap_err()
                    .to_string(),
            );
        }
        let retry = provider.vectors(&["retry".into()], false)?;
        std::fs::write(
            path,
            serde_json::to_vec(
                &json!({"identity":provider.identity,"documents":docs,"query":query[0],"errors":errors,"retry":retry}),
            )?,
        )?;
        Ok(())
    }
}
