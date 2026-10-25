//! The `mejl` executable. All logic lives in the library; see `cli`.

fn main() -> std::process::ExitCode {
    mejl::cli::main()
}
