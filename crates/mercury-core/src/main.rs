use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use mercury_core::cli::{Admin, Cli, Command, ConfigCommand};
use mercury_core::config::Config;
use mercury_core::db::Db;
use mercury_core::internal::AllowList;
use mercury_core::orchestrator::Orchestrator;
use mercury_core::probe_judge::ProbeJudge;
use mercury_core::probes::{McpClient, ProbeConfig, Probes};
use mercury_core::questions::Questions;
use mercury_core::session::SessionConfig;
use mercury_judge::Judge;
use mercury_schema::config::ProjectsConfig;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // `--help` and `--version` print and exit 0; a wrong command line prints the usage and exits 2
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(2));
        }
    };
    let result = match cli.command {
        None => run().await,
        Some(Command::Healthcheck) => healthcheck().await,
        Some(Command::Admin(Admin::Config(command))) => config_check(command),
        Some(Command::Admin(Admin::Shell)) => shell().await,
        Some(Command::Admin(command)) => admin(command).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mercury-core: {err}");
            ExitCode::FAILURE
        }
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

async fn run() -> Result<(), String> {
    mercury_core::logging::init()?;
    let public = mercury_core::public_addr(env("MERCURY_PUBLIC_ADDR").as_deref())?;
    let internal = mercury_core::internal_addr(env("MERCURY_INTERNAL_ADDR").as_deref())?;
    let allow = AllowList::parse(env("MERCURY_INTERNAL_ALLOW_CIDRS").as_deref())?;
    let config_dir = mercury_core::config::dir(env("MERCURY_CONFIG_DIR").as_deref());
    // held for the life of the process; the probes and the orchestrator read it, the routes later
    let (config, notes) = mercury_core::config::load(&config_dir)?;
    let config = Arc::new(config);
    for note in &notes {
        tracing::warn!(file = note.file, key = %note.key, "{}", note.message);
    }
    let db_path = mercury_core::db::path(env("MERCURY_DB").as_deref());
    let db = Db::open(&db_path).await.map_err(|err| err.to_string())?;
    // nothing answers a question a previous run left running: it ends before anyone can follow it
    let orphans = mercury_core::questions::end_orphans(&db)
        .await
        .map_err(|err| format!("cannot end the questions a previous run left running: {err}"))?;
    if orphans > 0 {
        tracing::warn!(
            questions = orphans,
            "questions a previous run left running ended with an error"
        );
    }
    let probes = probes(db.clone(), &config.projects)?;
    // built at start, so a missing check catalogue or prompt stops the core here, not at its first question
    let (orchestrator, judge) = orchestrator(db.clone(), config.clone(), &config_dir, probes.clone())?;
    // the probes ask the same judge chain, behind the same circuit breaker
    let probe_judge = ProbeJudge::new(probes.clone(), config.clone(), judge);
    let public_listener = bind(public).await?;
    let internal_listener = bind(internal).await?;
    if allow.is_empty() {
        tracing::warn!("MERCURY_INTERNAL_ALLOW_CIDRS is empty: the internal listener refuses every source");
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %public,
        %internal,
        allowed_ranges = allow.len(),
        config = %config_dir.display(),
        db = %db_path.display(),
        "listening"
    );
    let sessions = SessionConfig::parse(
        env("MERCURY_PUBLIC_ORIGIN").as_deref(),
        env("MERCURY_SESSION_TTL").as_deref(),
        env("MERCURY_TRUSTED_PROXY_CIDRS").as_deref(),
    )?;
    if sessions.public_origin.is_none() {
        tracing::warn!("MERCURY_PUBLIC_ORIGIN is not set: the web client can't sign in");
    }
    let questions =
        Questions::new(db.clone(), config.clone(), Arc::new(orchestrator)).with_sessions(sessions);
    let hub = questions.hub.clone();
    // the sign-in limit counts by the peer's address, which only the connection knows
    let public_server = axum::serve(
        public_listener,
        mercury_core::app(questions).into_make_service_with_connect_info::<SocketAddr>(),
    );
    tokio::spawn(sweep_stale(probes.clone()));
    tokio::spawn(purge_sessions(db.clone()));
    tokio::spawn(expire_approvals(db.clone(), hub));
    tokio::spawn(retain_audit(
        db.clone(),
        config.policies.retention.audit_payload_days,
    ));
    tokio::spawn(mercury_core::alerts::watch(
        db.clone(),
        config.models.budgets.global_daily_usd.get(),
        config.projects.projects.keys().cloned().collect(),
    ));
    let internal_routes =
        mercury_core::probes::routes(probes).merge(mercury_core::probe_judge::routes(probe_judge));
    let internal_router = mercury_core::internal::router_with(allow, internal_routes);
    let internal_server = axum::serve(
        internal_listener,
        internal_router.into_make_service_with_connect_info::<SocketAddr>(),
    );
    tokio::select! {
        result = public_server => result.map_err(|err| format!("the public listener stopped: {err}")),
        result = internal_server => result.map_err(|err| format!("the internal listener stopped: {err}")),
    }
}

/// The probes of `projects.yaml`. A project whose probe token file can't be read can't
/// register, and the core runs without it.
fn probes(db: Db, projects: &ProjectsConfig) -> Result<Probes, String> {
    let (configs, skipped) = ProbeConfig::from_projects(projects);
    for (project, kind) in skipped {
        tracing::warn!(project, error = %kind, "the probe token file can't be read: the probe can't register");
    }
    let http = McpClient::http().map_err(|_| "the probes' HTTP client can't be built".to_owned())?;
    Ok(Probes::new(db, http, configs))
}

/// The orchestrator, with the judge chain, the planner and the composer of `models.yaml` and the
/// prompts in `<config>/prompts/`; and the judge chain, which the probes' requests share.
fn orchestrator(
    db: Db,
    config: Arc<Config>,
    dir: &Path,
    probes: Probes,
) -> Result<(Orchestrator, Arc<dyn Judge>), String> {
    let parts = mercury_core::orchestrator::build::parts(db, config, dir, Arc::new(probes))
        .map_err(|err| err.to_string())?;
    let judge = parts.judge.clone();
    let orchestrator = Orchestrator::new(parts).map_err(|err| err.to_string())?;
    Ok((orchestrator, judge))
}

/// Marks probes that stopped registering as stale, every 30 s.
async fn sweep_stale(probes: Probes) {
    let mut ticks = tokio::time::interval(Duration::from_secs(30));
    loop {
        ticks.tick().await;
        if let Err(err) = probes.sweep_stale(mercury_core::db::now()).await {
            tracing::warn!(error = %err, "cannot mark silent probes as stale");
        }
    }
}

/// Deletes the sessions that have expired, at start and then once a day.
async fn purge_sessions(db: Db) {
    let mut ticks = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
    loop {
        ticks.tick().await;
        match mercury_core::session::purge_expired(&db, mercury_core::db::now()).await {
            Ok(0) => {}
            Ok(purged) => tracing::info!(purged, "expired sessions deleted"),
            Err(err) => tracing::warn!(error = %err, "cannot delete the expired sessions"),
        }
    }
}

/// Refuses the held answers nobody decided on within a day, every minute; the streams that wait
/// on them end with the refusal.
async fn expire_approvals(db: Db, hub: mercury_core::questions::Hub) {
    let mut ticks = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticks.tick().await;
        match mercury_core::approvals::expire_due(&db, &hub, mercury_core::db::now()).await {
            Ok(0) => {}
            Ok(expired) => tracing::info!(expired, "held answers nobody decided on were refused"),
            Err(err) => tracing::warn!(error = %err, "cannot refuse the held answers that expired"),
        }
    }
}

/// Retention, at start and then once a day: payloads, question texts and answers older than
/// `days` are deleted; the rows and their hashes stay. Counts only in the log.
async fn retain_audit(db: Db, days: u32) {
    let mut ticks = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
    loop {
        ticks.tick().await;
        match mercury_core::audit::retain(&db, days, mercury_core::db::now()).await {
            Ok(retained) if retained.payloads == 0 && retained.questions == 0 && retained.approvals == 0 => {}
            Ok(retained) => tracing::info!(
                payloads = retained.payloads,
                questions = retained.questions,
                approvals = retained.approvals,
                days,
                "audit retention cleared what is older than its window"
            ),
            Err(err) => {
                tracing::warn!(error = %err, "the audit retention didn't run; the next day tries again")
            }
        }
    }
}

async fn bind(addr: SocketAddr) -> Result<TcpListener, String> {
    TcpListener::bind(addr)
        .await
        .map_err(|err| format!("cannot listen on {addr}: {err}"))
}

async fn healthcheck() -> Result<(), String> {
    let public = mercury_core::public_addr(env("MERCURY_PUBLIC_ADDR").as_deref())?;
    tokio::task::spawn_blocking(move || mercury_core::healthcheck::check(public))
        .await
        .map_err(|err| format!("the health check didn't finish: {err}"))?
}

/// An admin command on the database at `MERCURY_DB`, opened (and brought up to date) for it. The
/// commands that name a project read the configuration in `MERCURY_CONFIG_DIR`.
async fn admin(command: Admin) -> Result<(), String> {
    let path = mercury_core::db::path(env("MERCURY_DB").as_deref());
    let config_dir = mercury_core::config::dir(env("MERCURY_CONFIG_DIR").as_deref());
    let db = Db::open(&path).await.map_err(|err| err.to_string())?;
    let result = mercury_core::admin::run(
        command,
        &db,
        &config_dir,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await;
    db.pool().close().await;
    result.map_err(|err| err.to_string())
}

/// `admin shell`: a prompt of its own for the admin commands, on one open database. A terminal on
/// the input gets the banner and the prompt; lines piped in run as a script and print the commands'
/// output alone, and a script that stops at a failing line exits 1.
async fn shell() -> Result<(), String> {
    use std::io::IsTerminal;

    let path = mercury_core::db::path(env("MERCURY_DB").as_deref());
    let config_dir = mercury_core::config::dir(env("MERCURY_CONFIG_DIR").as_deref());
    let db = Db::open(&path).await.map_err(|err| err.to_string())?;
    // colours and glyphs follow the terminal: none when the output isn't one, when NO_COLOR is set
    // or the terminal says it is dumb; plain ASCII when MERCURY_ASCII is set or on the linux console
    let term = env("TERM");
    let style = mercury_core::shell::Style {
        interactive: std::io::stdin().is_terminal(),
        colour: std::io::stdout().is_terminal()
            && env("NO_COLOR").is_none_or(|value| value.is_empty())
            && term.as_deref() != Some("dumb"),
        unicode: env("MERCURY_ASCII").is_none() && term.as_deref() != Some("linux"),
    };
    let mut lines = mercury_core::terminal::lines(&style);
    let result = mercury_core::shell::run(
        &db,
        &config_dir,
        &path.display().to_string(),
        lines.as_mut(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        &style,
    )
    .await;
    drop(lines);
    db.pool().close().await;
    match result {
        Ok(mercury_core::shell::Ended::ScriptFailed(line)) => Err(format!(
            "the script stopped at line {line}, which failed; nothing after it ran"
        )),
        Ok(_) => Ok(()),
        Err(err) => Err(format!("the shell can't read or write: {err}")),
    }
}

/// `admin config check`: the files only, so it needs no database.
fn config_check(command: ConfigCommand) -> Result<(), String> {
    let ConfigCommand::Check { dir } = command;
    let dir = dir.unwrap_or_else(|| mercury_core::config::dir(env("MERCURY_CONFIG_DIR").as_deref()));
    let report = mercury_core::config::read(&dir);
    let mut out = std::io::stdout().lock();
    mercury_core::config::render(&report, &dir, &mut out)
        .map_err(|err| format!("cannot write the output: {err}"))?;
    match report.problems.len() {
        0 => Ok(()),
        n => Err(format!("config check: {n} problem(s)")),
    }
}
