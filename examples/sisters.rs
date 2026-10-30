//! Lists the sisters and where each is installed, without opening any.
//! `cargo run --example sisters [file...]` also says where each file goes.
fn main() {
    for s in &core9000::sisters::ALL {
        println!("{:<11} {:<18} {}", s.name, s.kind, s.find(None).map_or("not installed".into(), |p| p.display().to_string()));
    }
    for f in std::env::args().skip(1) {
        println!("{f} -> {}", core9000::sisters::for_file(std::path::Path::new(&f)).map_or("(none)", |s| s.name));
    }
}
