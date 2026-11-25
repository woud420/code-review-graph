fn main() {
    std::process::exit(code_review_graph_rs::daemon::run(
        std::env::args_os().skip(1),
    ));
}
