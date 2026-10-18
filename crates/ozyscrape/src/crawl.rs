use crate::{Document, Rule};
use futures_util::{StreamExt, stream};
use ozy_core::{Error, net::SafeHttp};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Checkpoint {
    pub pending: VecDeque<(String, usize)>,
    pub visited: BTreeSet<String>,
    pub pages: Vec<serde_json::Value>,
}
/// Each batch yields a serializable checkpoint for durable callers.
pub async fn batch(
    http: &SafeHttp,
    mut checkpoint: Checkpoint,
    rules: &[Rule],
    depth: usize,
    max_pages: usize,
    concurrency: usize,
) -> Result<Checkpoint, Error> {
    if depth > 3 || max_pages > 20 || !(1..=4).contains(&concurrency) {
        return Err(Error::Invalid("Crawl limits: depth 3, pages 20, concurrency 4".into()));
    }
    let mut tasks = Vec::new();
    while tasks.len() < concurrency && checkpoint.visited.len() < max_pages {
        let Some((url, level)) = checkpoint.pending.pop_front() else {
            break;
        };
        if !checkpoint.visited.insert(url.clone()) {
            continue;
        }
        tasks.push((url, level));
    }
    let results = stream::iter(tasks.into_iter().map(|(url, level)| async move {
        let response = http.fetch(&url, true).await;
        (url, level, response)
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;
    for (url, level, response) in results {
        match response {
            Ok(response) => {
                let source = String::from_utf8_lossy(&response.body);
                let doc = Document::parse(&source)?;
                let fields: BTreeMap<_, _> = rules
                    .iter()
                    .map(|rule| doc.extract(rule, None).map(|v| (rule.name.clone(), v)))
                    .collect::<Result<_, _>>()?;
                if level < depth {
                    for link in doc.links(&response.url) {
                        if !checkpoint.visited.contains(&link) && checkpoint.pending.len() < 100 {
                            checkpoint.pending.push_back((link, level + 1));
                        }
                    }
                }
                checkpoint
                    .pages
                    .push(serde_json::json!({"url":url,"status":response.status,"fields":fields}));
            }
            Err(error) => checkpoint.pages.push(serde_json::json!({"url":url,"error":error.to_string()})),
        }
    }
    Ok(checkpoint)
}
