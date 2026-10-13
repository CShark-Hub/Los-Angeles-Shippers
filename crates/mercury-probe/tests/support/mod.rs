//! A stand-in for a code reader that has been talked into leaking. A reader can't return what it
//! never saw, so it answers only with what the request carried: every path, file name, long word
//! and line of the files, each bare and inside a sentence, beside three plain findings that must
//! pass (so a run also shows that the filter doesn't drop everything).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "each test binary uses some of it; the fixtures are literals and a panic is the test failing"
)]

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use mercury_probe::reader::wire::{ReadReply, ReadRequest};
use serde_json::{Value, json};
use std::sync::Arc;

/// Findings no corpus string is in.
pub const PLAIN: [&str; 3] = [
    "The export writes one file for every batch of invoices.",
    "Nothing in the invoice module handles a second currency.",
    "A rough estimate is two weeks of work.",
];

/// How many leaking candidates each reply carries beside the three plain findings: twelve in all.
const WINDOW: usize = 9;

/// What a request's files hold.
#[derive(Debug, Default)]
pub struct Carried {
    pub paths: Vec<String>,
    pub names: BTreeSet<String>,
    pub words: BTreeSet<String>,
    pub lines: BTreeSet<String>,
}

/// The files of a user message, between the lines that frame them.
pub fn carried(user: &str) -> Carried {
    let mut carried = Carried::default();
    let mut in_file = false;
    for line in user.split("FILES\n").nth(1).unwrap_or_default().lines() {
        if let Some(rest) = line.strip_prefix("<<<FILE ") {
            let path = rest.split_once(">>> ").map_or("", |(_, path)| path);
            carried.paths.push(path.to_owned());
            if let Some((_, name)) = path.rsplit_once('/') {
                carried.names.insert(name.to_owned());
            }
            in_file = true;
        } else if line.starts_with("<<<END ") {
            in_file = false;
        } else if in_file {
            if line.trim().chars().count() >= 12 {
                carried.lines.insert(line.trim().to_owned());
            }
            for word in line.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                if word.len() >= 8 {
                    carried.words.insert(word.to_owned());
                }
            }
        }
    }
    carried
}

/// Every candidate leak: each of `carried`, bare and inside a sentence.
pub fn candidates(carried: &Carried) -> Vec<String> {
    let everything = carried
        .paths
        .iter()
        .chain(&carried.names)
        .chain(&carried.words)
        .chain(&carried.lines);
    everything
        .flat_map(|found| [found.clone(), format!("The reader found {found} in the code.")])
        .collect()
}

/// The state of a stub reader: what it was sent, and how far through the candidates it has got.
#[derive(Default)]
pub struct Stub {
    pub requests: Mutex<Vec<String>>,
    cursor: AtomicUsize,
    total: AtomicUsize,
}

impl Stub {
    /// The reply's findings for a request's user message: the three plain ones and the next window
    /// of candidates, going round.
    pub fn findings(&self, user: &str) -> String {
        self.requests.lock().unwrap().push(user.to_owned());
        let candidates = candidates(&carried(user));
        self.total.store(candidates.len(), Ordering::SeqCst);
        let start = self.cursor.fetch_add(WINDOW, Ordering::SeqCst);
        let mut findings: Vec<Value> = PLAIN.iter().map(|text| finding(text)).collect();
        for offset in 0..WINDOW.min(candidates.len()) {
            findings.push(finding(&candidates[(start + offset) % candidates.len()]));
        }
        json!({ "findings": findings }).to_string()
    }

    /// How many candidates the last request carried.
    pub fn total(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }

    /// The calls it takes to have gone through every candidate once.
    pub fn calls_to_cover(&self) -> usize {
        self.total().div_ceil(WINDOW).max(1)
    }

    pub fn sent(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn finding(text: &str) -> Value {
    json!({ "text": text, "module": null, "confidence": 50 })
}

/// A stub reader on `POST /read`, as `serve` calls it: the base URL, and the stub's state.
pub async fn reader() -> (String, Arc<Stub>) {
    async fn read(State(stub): State<Arc<Stub>>, Json(request): Json<ReadRequest>) -> Json<ReadReply> {
        let user = request
            .messages
            .last()
            .map_or("", |message| message.content.as_str());
        Json(ReadReply {
            content: stub.findings(user),
            cost_usd: 0.01,
        })
    }
    let stub = Arc::new(Stub::default());
    let app = Router::new().route("/read", post(read)).with_state(stub.clone());
    (serve(app).await, stub)
}

/// A stub upstream of the real reader, an OpenAI-style chat endpoint at `/v1/chat/completions`.
pub async fn upstream() -> (String, Arc<Stub>) {
    async fn chat(State(stub): State<Arc<Stub>>, Json(request): Json<Value>) -> Json<Value> {
        let user = request["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .and_then(|message| message["content"].as_str())
            .unwrap_or_default();
        Json(json!({
            "id": "gen-stub",
            "model": request["model"],
            "choices": [{
                "message": { "role": "assistant", "content": stub.findings(user) },
                "finish_reason": "stop",
            }],
            "usage": { "cost": 0.01 },
        }))
    }
    let stub = Arc::new(Stub::default());
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(stub.clone());
    (serve(app).await, stub)
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}
