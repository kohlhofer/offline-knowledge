//! Prints a line per sampled entry for cross-checking against libzim:
//! `cargo run --release -p ok-zim --example dump -- <file.zim> <step>`
//! `cargo run --release -p ok-zim --example dump -- --write-sample <out.zim>`

use md5::{Digest, Md5};
use ok_zim::write::ZimBuilder;
use ok_zim::{Archive, DirentKind};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args[0] == "--write-sample" {
        let bytes = ZimBuilder::new()
            .blobs_per_cluster(2)
            .article("Apple", "Apple", "<p>fruit</p>")
            .article("Banana", "Banana", "<p>yellow</p>")
            .redirect("Apples", "Apples", "Apple")
            .metadata("Title", "Sample")
            .main_page("Apple")
            .build();
        std::fs::write(&args[1], bytes).unwrap();
        return;
    }
    let zim = Archive::open(&args[0]).unwrap();
    let step: u32 = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(1);
    for i in (0..zim.entry_count()).step_by(step as usize) {
        let d = zim.dirent(i).unwrap();
        let path = format!("{}/{}", d.namespace as char, d.path_str());
        let detail = match d.kind {
            DirentKind::Redirect { .. } => format!("redirect {}", zim.resolve(i).unwrap()),
            DirentKind::Content { .. } => {
                let content = zim.content(i).unwrap();
                let hex: String = Md5::digest(&*content).iter().map(|b| format!("{b:02x}")).collect();
                format!("{} {hex}", zim.mime_type(&d).unwrap_or("?"))
            }
            DirentKind::Other => "other".into(),
        };
        println!("{i}\t{path}\t{}\t{detail}", d.title_str());
    }
}
