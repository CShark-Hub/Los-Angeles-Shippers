//! `just judge-eval`: the labelled sets through each backend → `tests/golden/reports/<date>-judge.md`.
//!
//! The rules always run. Jev and the chat fallback run live when the OpenRouter key file holds a
//! key, and are reported as skipped otherwise. `--stand-in` runs them against local stand-ins
//! instead, which answer every check with the same middling probability: that tests the harness,
//! the parse and the timing, not the checks.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use axum::Json;
use axum::routing::post;
use mercury_judge::eval::{self, BackendReport, EvalContext, Sets};
use mercury_judge::{
    ApiKey, Catalogue, ChatClient, ChatConfig, ChatJudge, JevConfig, JevJudge, Judge, Prompt,
};
use regex::Regex;
use serde_json::{Value, json};

const USAGE: &str = "usage: judge-eval [--sets DIR] [--catalogue FILE] [--out DIR] [--check ID] [--stand-in]
                  [--key-file FILE] [--date YYYY-MM-DD] [--fallback-prompt FILE] [--no-modules] [--name SUFFIX]
                  [--dump DIR]";

struct Args {
    sets: PathBuf,
    catalogue: PathBuf,
    out: PathBuf,
    check: Option<String>,
    stand_in: bool,
    key_file: PathBuf,
    date: Option<String>,
    /// The chat fallback's prompt: the newest `judge_fallback.v<N>.md` of the examples by default.
    fallback_prompt: Option<PathBuf>,
    /// The tool stage without the project's modules in its state, to measure what they add.
    no_modules: bool,
    /// A suffix for the report's file name, such as `before-threshold-change`.
    name: Option<String>,
    /// A folder to write what each backend answered to every item into, one `<backend>.tsv` each.
    dump: Option<PathBuf>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        sets: "tests/golden".into(),
        catalogue: "config-examples/checks.yaml".into(),
        out: "tests/golden/reports".into(),
        check: None,
        stand_in: false,
        key_file: "deploy/compose/secrets/openrouter_api_key".into(),
        date: None,
        fallback_prompt: None,
        no_modules: false,
        name: None,
        dump: None,
    };
    while let Some(flag) = args.next() {
        if flag == "--stand-in" {
            parsed.stand_in = true;
            continue;
        }
        if flag == "--no-modules" {
            parsed.no_modules = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--sets" => parsed.sets = value.into(),
            "--catalogue" => parsed.catalogue = value.into(),
            "--out" => parsed.out = value.into(),
            "--check" => parsed.check = Some(value),
            "--key-file" => parsed.key_file = value.into(),
            "--date" => parsed.date = Some(value),
            "--fallback-prompt" => parsed.fallback_prompt = Some(value.into()),
            "--name" => parsed.name = Some(value),
            "--dump" => parsed.dump = Some(value.into()),
            _ => return Err(format!("unknown option {flag:?}")),
        }
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("judge-eval: {err}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(path) => {
            println!("judge-eval: report written to {path}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("judge-eval: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<String, String> {
    let catalogue = Catalogue::load(&args.catalogue).map_err(|err| err.to_string())?;
    if let Some(check) = &args.check
        && catalogue.get(check).is_none()
        && check != "title_unsafe"
    {
        return Err(format!("unknown check {check:?}"));
    }
    let sets = Sets::load(&args.sets)?;
    let ctx = EvalContext {
        catalogue: &catalogue,
        clients: clients(),
        only: args.check.as_deref(),
        modules: !args.no_modules,
    };
    let date = args.date.unwrap_or_else(today);
    let mut notes = Vec::new();
    let mut reports = both("rules", None, &sets, &ctx).await;

    let prompt_file = match &args.fallback_prompt {
        Some(file) => file.clone(),
        None => newest_prompt(&PathBuf::from("config-examples/prompts"), "judge_fallback")?,
    };
    let prompt = Prompt::read(&prompt_file).map_err(|err| {
        format!(
            "cannot read the fallback judge's prompt {}: {err}",
            prompt_file.display()
        )
    })?;
    notes.push(format!("The chat fallback answers from `{}`.", prompt.version));
    if args.no_modules {
        notes.push(
            "The tool stage's state carries no modules (`--no-modules`): the project's modules and their \
             labels are left out, to measure what they add."
                .to_owned(),
        );
    }
    if args.stand_in {
        let addr = stand_in().await?;
        notes.push(
            "`jev (stand-in)` and `chat (stand-in)` ran against local stand-ins that answer every check with the \
             same probability, 0.5: they test the harness, the parse and the timing; their accuracy means nothing."
                .to_owned(),
        );
        let key = ApiKey::new("stand-in");
        let mut jev = JevConfig::new(format!("http://{addr}/decisions"), "stand-in/jev", key.clone());
        jev.https_only = false;
        let jev = JevJudge::new(jev).map_err(|err| err.to_string())?;
        reports.extend(both("jev (stand-in)", Some(&jev as &dyn Judge), &sets, &ctx).await);
        let mut chat = ChatConfig::openrouter(format!("http://{addr}/chat"), "stand-in/chat", key);
        chat.https_only = false;
        let chat = ChatJudge::new(ChatClient::new(chat).map_err(|err| err.to_string())?, prompt);
        reports.extend(both("chat (stand-in)", Some(&chat as &dyn Judge), &sets, &ctx).await);
    } else {
        let key = ApiKey::read(&args.key_file).ok().flatten();
        match key {
            None => {
                for name in ["jev", "chat"] {
                    reports.push(BackendReport {
                        name: name.to_owned(),
                        skipped: Some("no OpenRouter key in the key file".to_owned()),
                        ..BackendReport::default()
                    });
                }
            }
            Some(key) => {
                let jev = JevJudge::new(JevConfig::new(
                    "https://openrouter.ai/api/alpha/decisions",
                    "typesafe/jev-1.13",
                    Some(key.clone()),
                ))
                .map_err(|err| err.to_string())?;
                reports.extend(both("jev", Some(&jev as &dyn Judge), &sets, &ctx).await);
                let model = std::env::var("JUDGE_EVAL_CHAT_MODEL").unwrap_or_default();
                if model.is_empty() {
                    reports.push(BackendReport {
                        name: "chat".to_owned(),
                        skipped: Some("no fallback model chosen (JUDGE_EVAL_CHAT_MODEL)".to_owned()),
                        ..BackendReport::default()
                    });
                } else {
                    let chat = ChatConfig::openrouter(
                        "https://openrouter.ai/api/v1/chat/completions",
                        model,
                        Some(key),
                    );
                    let chat = ChatJudge::new(ChatClient::new(chat).map_err(|err| err.to_string())?, prompt);
                    reports.extend(both("chat", Some(&chat as &dyn Judge), &sets, &ctx).await);
                }
            }
        }
    }
    if let Some(check) = &args.check {
        notes.push(format!("Only `{check}`."));
    }
    if let Some(dir) = &args.dump {
        std::fs::create_dir_all(dir).map_err(|err| format!("cannot create the dump folder: {err}"))?;
        for report in reports.iter().filter(|r| r.skipped.is_none()) {
            let tsv: String = report
                .readings
                .iter()
                .map(|(id, key, answer)| format!("{id}\t{key}\t{answer}\n"))
                .collect();
            let name = report.name.replace(' ', "-").replace(',', "");
            std::fs::write(dir.join(format!("{name}.tsv")), tsv)
                .map_err(|err| format!("cannot write the dump: {err}"))?;
        }
    }
    let report = eval::render(&date, &sets, &reports, &notes);
    std::fs::create_dir_all(&args.out).map_err(|err| format!("cannot create the report folder: {err}"))?;
    let suffix = args
        .name
        .as_deref()
        .map_or(String::new(), |name| format!("-{name}"));
    let path = args.out.join(format!("{date}-judge{suffix}.md"));
    std::fs::write(&path, report).map_err(|err| format!("cannot write the report: {err}"))?;
    Ok(path.display().to_string())
}

/// A backend on the sets in two readings: the part the thresholds were tuned on, and the held-out part
/// they never saw, each with its own counts.
async fn both(
    name: &str,
    judge: Option<&dyn Judge>,
    sets: &Sets,
    ctx: &EvalContext<'_>,
) -> Vec<BackendReport> {
    vec![
        eval::run(name, judge, &sets.part(false), ctx).await,
        eval::run(&format!("{name}, held out"), judge, &sets.part(true), ctx).await,
    ]
}

/// The newest `<role>.v<N>.md` of a prompts folder, as the core reads them.
fn newest_prompt(dir: &std::path::Path, role: &str) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let number = name.strip_prefix(role)?.strip_prefix(".v")?.strip_suffix(".md")?;
            Some((number.parse::<u32>().ok()?, entry.path()))
        })
        .max_by_key(|(number, _)| *number)
        .map(|(_, path)| path)
        .ok_or_else(|| format!("no {role}.v<N>.md in {}", dir.display()))
}

/// The client names the redaction list stands for, as `config-examples/policies.yaml` and the
/// fixture's manifest list them.
fn clients() -> Vec<Regex> {
    [r"(?i)\bacme(\s+corp)?\b", r"(?i)\bmegabank\b"]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
}

fn today() -> String {
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    now.format("%Y-%m-%d").to_string()
}

/// Answers every question of a batch with the middle: 0.5 for a noul, every option alike for a
/// choice, the middle level for a score.
fn middle(questions: &Value) -> Value {
    let mut answers = serde_json::Map::new();
    for (key, question) in questions.as_object().into_iter().flatten() {
        let answer = match question["type"].as_str() {
            Some("choice") => {
                let options: Vec<&String> = question["criteria"]
                    .as_object()
                    .map(|o| o.keys().collect())
                    .unwrap_or_default();
                #[allow(clippy::cast_precision_loss, reason = "a handful of options")]
                let p = 1.0 / options.len().max(1) as f64;
                json!({
                    "type": "choice",
                    "choice": options.first().map(|o| o.as_str()).unwrap_or_default(),
                    "probabilities": options.iter().map(|o| ((*o).clone(), json!(p))).collect::<serde_json::Map<_, _>>(),
                    "confidence": 0.5,
                })
            }
            _ => json!({ "type": "noul", "noul": 0.5 }),
        };
        answers.insert(key.clone(), answer);
    }
    Value::Object(answers)
}

/// The stand-ins on a free local port: `/decisions` as Jev answers, `/chat` as a chat model.
async fn stand_in() -> Result<SocketAddr, String> {
    let app = axum::Router::new()
        .route(
            "/decisions",
            post(|Json(body): Json<Value>| async move {
                Json(json!({
                    "id": "stand-in",
                    "model": "stand-in/jev",
                    "answers": middle(&body["questions"]),
                    "usage": { "input_tokens": 0, "output_tokens": 0, "cost": 0.0 },
                }))
            }),
        )
        .route(
            "/chat",
            post(|Json(body): Json<Value>| async move {
                let asked: Value = body["messages"][1]["content"]
                    .as_str()
                    .and_then(|c| serde_json::from_str(c).ok())
                    .unwrap_or(Value::Null);
                let content = json!({ "answers": middle(&asked["questions"]) }).to_string();
                Json(json!({
                    "id": "stand-in",
                    "model": "stand-in/chat",
                    "choices": [{ "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
                    "usage": { "cost": 0.0 },
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| format!("the stand-ins can't listen: {err}"))?;
    let addr = listener.local_addr().map_err(|err| err.to_string())?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(addr)
}
