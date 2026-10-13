//! Writes the OpenAPI document to the path given, `openapi.json` by default (`just openapi`).

use std::process::ExitCode;

fn main() -> ExitCode {
    let path = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "openapi.json".into());
    match std::fs::write(&path, mercury_schema::openapi::document_json()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("openapi: can't write {}: {err}", path.to_string_lossy());
            ExitCode::FAILURE
        }
    }
}
