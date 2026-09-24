fn main() {
    if std::env::args().any(|argument| argument == "--version") {
        println!("nezha-agent-rust 0.1.1");
    }
}
