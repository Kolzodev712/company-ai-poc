fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(company_ai::main_from(std::env::args_os()) as u8)
}
