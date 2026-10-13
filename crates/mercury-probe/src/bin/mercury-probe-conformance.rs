use std::process::ExitCode;

use mercury_probe::conformance::{self, USAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match conformance::parse_args(
        std::env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .skip(1),
    ) {
        Ok(Some(config)) => config,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("mercury-probe-conformance: {err}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let report = conformance::run(&config).await;
    println!("{report}");
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
