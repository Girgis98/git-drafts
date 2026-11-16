fn main() {
    if let Err(e) = git_drafts::run() {
        eprintln!("gd: error: {e}");
        std::process::exit(1);
    }
}
