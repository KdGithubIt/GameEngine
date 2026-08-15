use std::process::ExitCode;

fn main() -> ExitCode {
    match engine_cli::run_cli_with_status(std::env::args().skip(1)) {
        Ok(result) => {
            println!("{}", result.output);
            ExitCode::from(result.exit_code as u8)
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code() as u8)
        }
    }
}
