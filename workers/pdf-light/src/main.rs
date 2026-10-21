//! `pdf-light` worker entry point: one JSON request on stdin, one JSON response on stdout.

use std::io::{Read, Write};
use std::process::ExitCode;

use pdf_light::{handle, ErrorCode, Request, Response, WorkerError};

fn main() -> ExitCode {
    let mut input = String::new();
    let response = match std::io::stdin().read_to_string(&mut input) {
        Err(e) => bad_request(format!("Could not read the request ({e}).")),
        Ok(_) => match serde_json::from_str::<Request>(&input) {
            Ok(request) => handle(request),
            Err(e) => bad_request(format!("Invalid request ({e}).")),
        },
    };

    let mut stdout = std::io::stdout().lock();
    let written = serde_json::to_writer(&mut stdout, &response)
        .map_err(std::io::Error::from)
        .and_then(|_| stdout.write_all(b"\n"))
        .and_then(|_| stdout.flush());
    if written.is_err() {
        // The core treats a missing response as a failure, so there is nothing else to do.
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn bad_request(message: String) -> Response {
    Response::Err {
        ok: false,
        error: WorkerError {
            code: ErrorCode::BadRequest,
            message,
        },
    }
}
