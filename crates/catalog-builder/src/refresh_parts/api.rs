//! Rebrickable parts API: paginated fetch with rate-limit-aware retry, and
//! the typed view of each part row we consume.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// Rebrickable parts listing endpoint. `page_size=1000` is the max the API
/// honors; the full ~63K-part catalog comes back in ~64 pages. Parts
/// responses already carry `external_ids` as flat id arrays, so (unlike the
/// colors endpoint) no `inc_external_ids` query param is needed.
const PARTS_LISTING_URL: &str = "https://rebrickable.com/api/v3/lego/parts/?page_size=1000";

/// Delay between page requests. Rebrickable rate-limits at roughly one
/// request per second; a faster cadence earns `429`s partway through the
/// ~64-page sweep (the retry path below recovers, but pacing avoids the
/// stall). The whole sweep takes ~70s — fine for a rare, explicit tool.
const REQUEST_INTERVAL: Duration = Duration::from_millis(1000);

/// How many times to retry a single page on a transient `429`/`5xx` before
/// giving up.
const MAX_RETRIES: u32 = 5;

/// One part row from the Rebrickable parts API. We keep `part_num` and the
/// full `external_ids` map; everything else (name, category, year) is
/// recovered from the bulk CSVs at build time, so there's no reason to read
/// it here.
#[derive(Deserialize, Debug)]
pub(crate) struct ApiPart {
    pub part_num: String,
    /// External-id systems → raw id arrays, e.g. `{"LDraw": ["3001"],
    /// "BrickLink": ["3001", "3001old"]}`. Values are kept as raw [`Value`]s
    /// because Rebrickable historically mixes JSON strings and integers
    /// across systems; [`ApiPart::ids`] coerces them to strings at the
    /// consumer.
    #[serde(default)]
    pub external_ids: BTreeMap<String, Vec<Value>>,
}

impl ApiPart {
    /// Ids for one external system as strings (e.g. `ids("LDraw")`),
    /// skipping any value that isn't a string or number.
    pub fn ids(&self, system: &str) -> Vec<String> {
        self.external_ids
            .get(system)
            .map(|vals| vals.iter().filter_map(value_to_id).collect())
            .unwrap_or_default()
    }

    /// Every external-id system as `system → string ids`, with each system's
    /// ids coerced and empty systems dropped. The caller pulls LDraw out
    /// separately (it's canonicalized into its own scalar), so it is *not*
    /// removed here — that's the caller's concern.
    pub fn all_ids(&self) -> BTreeMap<String, Vec<String>> {
        self.external_ids
            .iter()
            .filter_map(|(system, vals)| {
                let ids: Vec<String> = vals.iter().filter_map(value_to_id).collect();
                (!ids.is_empty()).then(|| (system.clone(), ids))
            })
            .collect()
    }
}

/// Coerce a Rebrickable external id (JSON string or integer) to a string.
/// Anything else (null, object) is dropped.
fn value_to_id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Fetch every part across all pages of the listing endpoint.
pub(crate) fn fetch_parts(api_key: &str) -> Result<Vec<ApiPart>> {
    let mut all: Vec<ApiPart> = Vec::new();
    let mut url = String::from(PARTS_LISTING_URL);
    let mut page = 1;
    loop {
        tracing::info!("GET parts page {page}");
        tracing::debug!("  url: {url}");
        let body = get_json(&url, api_key)?;
        let parts = parse_parts(&body).with_context(|| format!("parse parts from {url}"))?;
        all.extend(parts);
        let next = body.get("next").and_then(|v| v.as_str()).map(String::from);
        tracing::info!("  page {page}: total {} parts", all.len());
        match next {
            Some(n) => {
                url = n;
                page += 1;
                sleep(REQUEST_INTERVAL);
            }
            None => break,
        }
    }
    Ok(all)
}

pub(crate) fn parse_parts(body: &Value) -> Result<Vec<ApiPart>> {
    let results = body
        .get("results")
        .and_then(|v| v.as_array())
        .context("response missing `results` array")?;
    let mut out = Vec::with_capacity(results.len());
    for (i, raw) in results.iter().enumerate() {
        let part: ApiPart =
            serde_json::from_value(raw.clone()).with_context(|| format!("parse results[{i}]"))?;
        out.push(part);
    }
    Ok(out)
}

/// GET a URL as JSON, retrying transient `429` (rate limit) and `5xx`
/// responses with exponential backoff. A `Retry-After` header, when present,
/// takes precedence over the computed backoff. Other errors (4xx, transport,
/// JSON parse) surface immediately — retrying them won't help.
fn get_json(url: &str, api_key: &str) -> Result<Value> {
    let mut attempt = 0;
    loop {
        match ureq::get(url)
            .set("Authorization", &format!("key {api_key}"))
            .set("Accept", "application/json")
            .call()
        {
            Ok(resp) => {
                return resp
                    .into_json()
                    .with_context(|| format!("parse JSON from {url}"));
            }
            Err(ureq::Error::Status(code, resp))
                if (code == 429 || code >= 500) && attempt < MAX_RETRIES =>
            {
                attempt += 1;
                let backoff =
                    retry_after(&resp).unwrap_or(Duration::from_millis(500 * 2u64.pow(attempt)));
                tracing::warn!(
                    "{url}: status {code}; retry {attempt}/{MAX_RETRIES} after {:.1}s",
                    backoff.as_secs_f64(),
                );
                sleep(backoff);
            }
            Err(e) => return Err(e).with_context(|| format!("GET {url}")),
        }
    }
}

/// Parse a `Retry-After` header expressed in whole seconds (Rebrickable's
/// form). The HTTP-date form isn't emitted by this API, so it's ignored.
fn retry_after(resp: &ureq::Response) -> Option<Duration> {
    resp.header("Retry-After")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}
