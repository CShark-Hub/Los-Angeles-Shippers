use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use mercury_probe::access::{limit_name, mode_name};
use mercury_probe::index::Index;
use mercury_probe::judging::{JudgeClient, Report};
use mercury_probe::mcp::{self, ProbeToken};
use mercury_probe::net::egress::{self, Allowlist, EgressArgs, Policy};
use mercury_probe::net::{healthcheck, reach};
use mercury_probe::probe::{LiveIndex, Probe, REFRESH_EVERY, RULES_RECHECK, Source};
use mercury_probe::reader::client::ReaderClient;
use mercury_probe::reader::{self, Reader, ReaderConfig};
use mercury_probe::registration;
use mercury_probe::serve::{INDEX_FILE, META_DIR, REPO_DIR};
use mercury_probe::sync::github::{API_URL, GitHub, GitHubToken};
use mercury_probe::sync::{self, PROJECTS_FILE, SyncConfig, Syncer};
use mercury_probe::tools::Tools;
use mercury_probe::{Args, Command, DEFAULT_LISTEN, Mode, USAGE};
use mercury_schema::api::ProbeRegistration;
use mercury_schema::{AccessLimit, AccessMode, ProjectId};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// How often `serve` looks for the metadata until it has built its first index.
const WAIT_EVERY: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let command = match mercury_probe::parse_os_command(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("mercury-probe: {err}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // the health check and the network checks are one short exchange each: no runtime for them
    let ran = match command {
        Command::Healthcheck(port) => healthcheck::check(port),
        Command::Reach(args) => reach(&args),
        Command::Egress(args) => egress(args),
        Command::Run(args) => run(args),
    };
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mercury-probe: {err}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn run(args: Args) -> Result<(), String> {
    match args.mode {
        Mode::Sync => sync().await,
        Mode::Reader => reader(args).await,
        Mode::Dev | Mode::Serve => serve(args).await,
    }
}

/// `reach`: the status line, when there is one, on stdout; why the target is unreachable, on stderr.
fn reach(args: &reach::ReachArgs) -> Result<(), String> {
    let outcome = reach::run(args, |name| std::env::var(name).ok());
    if let Some(status) = &outcome.status {
        println!("{status}");
    }
    outcome.failure.map_or(Ok(()), Err)
}

/// `egress`: the CONNECT proxy, until the process is stopped. It writes no file.
#[tokio::main]
async fn egress(args: EgressArgs) -> Result<(), String> {
    let policy = Policy::new(Allowlist::read(&args.allow)?, &args.ports);
    let listener = TcpListener::bind(args.listen)
        .await
        .map_err(|err| format!("cannot listen on {}: {err}", args.listen))?;
    let addr = listener
        .local_addr()
        .map_err(|err| format!("cannot listen on {}: {err}", args.listen))?;
    eprintln!("{}", egress::start_line(addr, &policy, &args.allow));
    let log: egress::Log = Arc::new(|line| eprintln!("mercury-probe: {line}"));
    egress::serve(listener, Arc::new(policy), log).await
}

/// `sync`: the GitHub API into `/meta`, until the process is stopped.
async fn sync() -> Result<(), String> {
    let config = SyncConfig::from_env(|name| std::env::var(name).ok())?;
    // the snapshot lives in RAM only: no tmpfs at /repo, no sync
    sync::snapshot::check_tmpfs(REPO_DIR)?;
    let token = GitHubToken::from_file(&config.token_file)?;
    // the replies kept for their ETags outlive a restart; none of them names the code's files
    let source = GitHub::new(API_URL, &config.repo, &token)?
        .with_cache_file(&Path::new(META_DIR).join(sync::ETAGS_FILE));
    eprintln!(
        "mercury-probe {} (sync): project {}, the GitHub API through HTTPS_PROXY, the mode every {} s, the metadata every {} s, snapshots up to {} MB on the tmpfs {REPO_DIR}",
        env!("CARGO_PKG_VERSION"),
        config.project,
        config.code_interval.as_secs(),
        config.meta_interval.as_secs(),
        config.snapshot_max_mb,
    );
    if config.code_reader {
        eprintln!(
            "mercury-probe: a code reader is configured, so a project that asks for code_model gets it"
        );
    }
    let syncer = Syncer::new(
        source,
        config.project.clone(),
        Path::new(PROJECTS_FILE),
        Path::new(META_DIR),
    )
    .with_snapshots(Path::new(REPO_DIR), config.snapshot_max_mb)
    .with_code_reader(config.code_reader);
    sync::run(syncer, &config, |line| eprintln!("mercury-probe: {line}")).await;
    Ok(())
}

/// `reader`: the code reader. One request in, one call to its one upstream, the model's words out;
/// it reads no folder and writes no file.
async fn reader(args: Args) -> Result<(), String> {
    let config = ReaderConfig::from_env(|name| std::env::var(name).ok(), args.listen)?;
    let started = reader::start_line(&config);
    let listen = config.listen;
    let log: reader::Log = Arc::new(|line| eprintln!("mercury-probe: {line}"));
    let server = Reader::new(config, log)?;
    let listener = TcpListener::bind(listen)
        .await
        .map_err(|err| format!("cannot listen on {listen}: {err}"))?;
    eprintln!("{started}");
    axum::serve(listener, reader::app(Arc::new(server)))
        .await
        .map_err(|err| format!("the server stopped: {err}"))
}

/// `dev` and `serve`: the index, MCP at `/mcp` and the health check, and registration with the core
/// once there is an index.
async fn serve(args: Args) -> Result<(), String> {
    let source = match (args.mode, args.repo, args.meta) {
        (Mode::Dev, Some(repo), Some(meta)) => Source::Dev {
            repo,
            meta,
            token: args.access,
        },
        _ => Source::Serve {
            repo: PathBuf::from(REPO_DIR),
            meta: PathBuf::from(META_DIR),
            project: project_from_env()?,
        },
    };
    // no core, no serve: checked before anything is opened
    let core = source.core_address(|name| std::env::var(name).ok())?;
    // a code reader the probe can't call is no code reader: the start stops, naming the variable
    let reader = ReaderClient::from_env(|name| std::env::var(name).ok())?;
    let dev = matches!(source, Source::Dev { .. });
    let index = if dev {
        Index::in_memory().await
    } else {
        Index::open(Path::new(INDEX_FILE)).await
    }
    .map_err(|err| err.to_string())?;
    let token = probe_token()?;
    if token.is_none() {
        eprintln!("mercury-probe: no probe token (PROBE_TOKEN_FILE), so /mcp refuses every request");
    }
    let mut tools = Tools::waiting(index.clone());
    if let Some(client) = &reader {
        tools = tools.with_reader(client.clone());
    }
    let mut live = LiveIndex::new(index, source).with_code_reader(reader.is_some());
    // the core's judge decides what the rules leave open; without a core (dev only), the rules alone
    if let Some(core) = &core {
        live = live.with_judge(JudgeClient::new(core, token.clone())?);
    }
    let mut keeper = Keeper {
        dev,
        reader_host: reader.as_ref().map(|client| client.host().to_owned()),
        tools: tools.clone(),
        core,
        token: token.clone(),
        registration: None,
        last: None,
        judged: None,
    };
    if dev {
        // dev starts only on a folder it can index
        let probe = live.refresh().await?.ok_or("the index wasn't built")?;
        keeper.log_judged(live.judged());
        keeper.rebuilt(&probe, &live.index, true).await?;
    }
    tokio::spawn(async move {
        let mut last_error: Option<String> = None;
        // serve tries at once; dev has just built its index
        let mut wait = dev;
        loop {
            if wait {
                tokio::time::sleep(if keeper.last.is_some() {
                    REFRESH_EVERY
                } else {
                    WAIT_EVERY
                })
                .await;
            }
            wait = true;
            let rebuilt = match live.refresh().await {
                Ok(rebuilt) => rebuilt,
                // the last index stays; the error names no file of the folder or the snapshot, and
                // is logged once until it changes
                Err(err) => {
                    if last_error.as_deref() != Some(err.as_str()) {
                        eprintln!("mercury-probe: the index wasn't rebuilt: {err}");
                        last_error = Some(err);
                    }
                    None
                }
            };
            if let Some(probe) = rebuilt {
                last_error = None;
                keeper.log_judged(live.judged());
                let fresh = !live.kept();
                if !fresh {
                    eprintln!(
                        "mercury-probe: the snapshot isn't there yet: the last index answers until it is back"
                    );
                }
                if let Err(err) = keeper.rebuilt(&probe, &live.index, fresh).await {
                    eprintln!("mercury-probe: {err}");
                }
            }
        }
    });

    let app = mcp::app(tools, ProbeToken::new(token.as_deref()));
    let addr = match args.listen {
        Some(addr) => addr,
        None => mercury_probe::listen_addr(
            &std::env::var("PROBE_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_owned()),
        )?,
    };
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("cannot listen on {addr}: {err}"))?;
    eprintln!(
        "mercury-probe {} ({}): MCP at /mcp and the health check, on http://{addr}{}",
        env!("CARGO_PKG_VERSION"),
        args.mode.as_str(),
        if dev {
            ""
        } else {
            "; the tools answer once the first index is built"
        },
    );
    axum::serve(listener, app)
        .await
        .map_err(|err| format!("the server stopped: {err}"))
}

/// What follows a rebuild: the tools' description, and registration with the latest body.
struct Keeper {
    dev: bool,
    /// The host of the code reader, when one is configured.
    reader_host: Option<String>,
    tools: Tools,
    core: Option<String>,
    token: Option<String>,
    registration: Option<watch::Sender<ProbeRegistration>>,
    /// The mode and manifest last logged.
    last: Option<(AccessMode, Option<AccessLimit>, String)>,
    /// The judge's line last logged.
    judged: Option<String>,
}

impl Keeper {
    /// What the judge answered at a rebuild, counts only, logged once until it changes.
    fn log_judged(&mut self, report: Option<&Report>) {
        let Some(report) = report else {
            return;
        };
        let line = if report.failed() {
            format!(
                "the core's judge left {} questions unanswered ({}): their conflicts stay unclear, \
                 their kinds unknown and their titles withheld until the next tick asks again",
                report.unanswered,
                report
                    .error
                    .as_ref()
                    .map_or_else(|| "one couldn't be sent".to_owned(), ToString::to_string)
            )
        } else if report.by_rules > 0 {
            format!(
                "the core answered {} titles with its rules alone: they stay withheld until a judge \
                 answers them, asked again every {} minutes",
                report.by_rules,
                RULES_RECHECK.as_secs() / 60
            )
        } else {
            format!(
                "the core's judge answered {} questions in {} requests, {} more from the cache",
                report.answered, report.requests, report.cached
            )
        };
        if self.judged.as_deref() != Some(line.as_str()) {
            eprintln!("mercury-probe: {line}");
            self.judged = Some(line);
        }
    }
    /// `fresh`: the index was rebuilt for `probe`, and its counts are logged.
    async fn rebuilt(&mut self, probe: &Probe, index: &Index, fresh: bool) -> Result<(), String> {
        self.tools.set(probe)?;
        let now = (
            probe.access,
            probe.access_limited_by,
            probe.manifest_hash.as_str().to_owned(),
        );
        if self.last.as_ref() != Some(&now) {
            log_probe(probe, self.dev, self.reader_host.as_deref());
            self.last = Some(now);
        }
        if fresh {
            log_index(index).await;
        }
        let body = registration::body(probe);
        match (&self.registration, &self.core) {
            (Some(sender), _) => {
                sender.send_replace(body);
            }
            (None, Some(core)) => {
                let (sender, receiver) = watch::channel(body);
                tokio::spawn(registration::keep_registering(
                    core.clone(),
                    self.token.clone(),
                    receiver,
                    registration::REGISTER_EVERY,
                ));
                self.registration = Some(sender);
            }
            (None, None) => {}
        }
        Ok(())
    }
}

/// `PROBE_PROJECT`, the project `serve`'s manifest must name, when it is set.
fn project_from_env() -> Result<Option<ProjectId>, String> {
    match std::env::var("PROBE_PROJECT") {
        Ok(value) if !value.trim().is_empty() => ProjectId::parse(value.trim())
            .map(Some)
            .map_err(|_| "PROBE_PROJECT is not a project id".to_owned()),
        _ => Ok(None),
    }
}

/// The probe token from the file `PROBE_TOKEN_FILE` names; `None` when the variable is unset.
/// The error names the variable, never the file's contents.
fn probe_token() -> Result<Option<String>, String> {
    let Ok(path) = std::env::var("PROBE_TOKEN_FILE") else {
        return Ok(None);
    };
    let token =
        std::fs::read_to_string(&path).map_err(|err| format!("cannot read PROBE_TOKEN_FILE: {err}"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err("PROBE_TOKEN_FILE is empty".to_owned());
    }
    Ok(Some(token.to_owned()))
}

/// The project, the mode, the count of files and the code reader's host: never a file's name or
/// contents.
fn log_probe(probe: &Probe, dev: bool, reader_host: Option<&str>) {
    // in dev, --access stands in for the token
    let token = if dev { "the token (--access)" } else { "the token" };
    eprintln!(
        "mercury-probe: project {}, access {}{}, {}{}",
        probe.manifest.project,
        mode_name(probe.access),
        probe
            .access_limited_by
            .map(|limit| format!(" (limited by {})", limit_name(limit, token)))
            .unwrap_or_default(),
        probe
            .files
            .as_ref()
            .map_or("no files read".to_owned(), |files| format!(
                "{} files listed",
                files.len()
            )),
        reader_host.map_or_else(String::new, |host| format!(", code reader at {host}")),
    );
}

/// Counts only: never a title, a module's numbers or a file.
async fn log_index(index: &Index) {
    match (index.facts().await, index.module_stats().await) {
        (Ok(facts), Ok(modules)) => eprintln!(
            "mercury-probe: index built, {} facts in {} modules",
            facts.len(),
            modules.len()
        ),
        (Err(err), _) | (_, Err(err)) => eprintln!("mercury-probe: the index can't be read: {err}"),
    }
}
