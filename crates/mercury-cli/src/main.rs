//! The `mercury` binary.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use mercury_cli::cli::Cli;
use mercury_cli::credentials::Keychain;
use mercury_cli::run::{ERROR, Io, run};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // `--help` and `--version` print and exit 0; a wrong command line is an error like any other
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(if err.exit_code() == 0 { 0 } else { ERROR });
        }
    };
    let stdin = std::io::stdin();
    let stdin_is_terminal = stdin.is_terminal();
    let code = run(
        cli.command,
        Io {
            var: &|name| std::env::var(name).ok(),
            store: &Keychain,
            stdin: &mut stdin.lock(),
            stdin_is_terminal,
            stdout: &mut std::io::stdout().lock(),
            stderr: &mut std::io::stderr().lock(),
        },
    );
    ExitCode::from(code)
}
