use std::io::Write;
use std::path::PathBuf;

use ok_zim::write::{Compression, ZimBuilder};
use ok_zim::{Archive, DirentKind, Error, NS_CONTENT};

fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    f
}

fn builder(compression: Compression, extended: bool) -> ZimBuilder {
    ZimBuilder::new()
        .compression(compression)
        .extended_clusters(extended)
        .blobs_per_cluster(2)
        .article("Zebra", "Zebra", "<p>stripes</p>")
        .article("Apple", "Apple", "<p>fruit</p>")
        .article("Banana", "Banana", "<p>yellow</p>")
        .redirect("Apples", "Apples", "Apple")
        .redirect("Pomme", "Pomme", "Apples")
        .redirect("Style", "Style", "_res_/style.css")
        .resource("_res_/style.css", "text/css", b"p{}")
        .metadata("Title", "Test wiki")
        .main_page("Apple")
}

fn sample(compression: Compression, extended: bool) -> Vec<u8> {
    builder(compression, extended).build()
}

fn header_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[test]
fn reads_every_layout_variant() {
    let variants = [
        (Compression::None, false, false),
        (Compression::Zstd, false, false),
        (Compression::Zstd, true, false),
        (Compression::None, true, false),
        // The last cluster in the file is compressed and followed by dirents and pointer lists.
        (Compression::Zstd, false, true),
    ];
    for (compression, extended, listing_compressed) in variants {
        let file = write_temp(&builder(compression, extended).listing_compressed(listing_compressed).build());
        let zim = Archive::open(file.path()).unwrap();
        let label = format!("{compression:?} extended={extended} listing_compressed={listing_compressed}");
        let apple = zim.find_by_path(NS_CONTENT, b"Apple").unwrap().expect("Apple exists");
        assert_eq!(&*zim.content(apple).unwrap(), b"<p>fruit</p>", "{label}");
        let zebra = zim.find_by_path(NS_CONTENT, b"Zebra").unwrap().unwrap();
        assert_eq!(&*zim.content(zebra).unwrap(), b"<p>stripes</p>", "{label}");
        assert_eq!(zim.front_articles().unwrap().len(), 5, "{label}");
    }
}

#[test]
fn follows_redirect_chains() {
    let file = write_temp(&sample(Compression::Zstd, false));
    let zim = Archive::open(file.path()).unwrap();
    let pomme = zim.find_by_path(NS_CONTENT, b"Pomme").unwrap().unwrap();
    let apple = zim.find_by_path(NS_CONTENT, b"Apple").unwrap().unwrap();
    assert!(zim.dirent(pomme).unwrap().is_redirect());
    assert_eq!(zim.resolve(pomme).unwrap(), apple);
    assert_eq!(&*zim.content(pomme).unwrap(), b"<p>fruit</p>");
}

#[test]
fn missing_paths_and_ranges() {
    let file = write_temp(&sample(Compression::Zstd, false));
    let zim = Archive::open(file.path()).unwrap();
    assert_eq!(zim.find_by_path(NS_CONTENT, b"Cherry").unwrap(), None);
    assert_eq!(zim.find_by_path(NS_CONTENT, b"").unwrap(), None);
    assert_eq!(zim.find_by_path(b'Q', b"Apple").unwrap(), None);
    assert!(matches!(zim.dirent(zim.entry_count()), Err(Error::EntryOutOfRange { .. })));
    assert!(zim.blob(zim.header().cluster_count, 0).is_err());
    assert!(zim.blob(0, 99).is_err());
}

#[test]
fn front_articles_are_articles_and_their_redirects_by_title() {
    let file = write_temp(&sample(Compression::Zstd, false));
    let zim = Archive::open(file.path()).unwrap();
    let titles: Vec<String> =
        zim.front_articles().unwrap().into_iter().map(|i| zim.dirent(i).unwrap().title_str()).collect();
    // "Style" redirects to a stylesheet, so it is not a front article.
    assert_eq!(titles, ["Apple", "Apples", "Banana", "Pomme", "Zebra"]);
    let range = zim.namespace_range(NS_CONTENT).unwrap();
    assert_eq!(range.len(), 7);
}

#[test]
fn metadata_main_entry_mime_and_checksum() {
    let file = write_temp(&sample(Compression::Zstd, false));
    let zim = Archive::open(file.path()).unwrap();
    assert_eq!(zim.metadata("Title").unwrap().as_deref(), Some("Test wiki"));
    assert_eq!(zim.metadata("Nope").unwrap(), None);
    let main = zim.main_entry().unwrap().unwrap();
    assert_eq!(zim.dirent(main).unwrap().path, b"Apple");
    let css = zim.dirent(zim.find_by_path(NS_CONTENT, b"_res_/style.css").unwrap().unwrap()).unwrap();
    assert_eq!(zim.mime_type(&css), Some("text/css"));
    assert_eq!(zim.article_namespace(), NS_CONTENT);
    assert!(zim.verify_checksum().unwrap());
}

#[test]
fn cache_respects_its_byte_budget() {
    let mut b = ZimBuilder::new().blobs_per_cluster(1);
    let big = "x".repeat(200_000);
    for i in 0..20 {
        b = b.article(&format!("A{i:02}"), "t", &big);
    }
    let file = write_temp(&b.build());
    let zim = Archive::open(file.path()).unwrap();
    zim.set_cache_budget(500_000);
    for i in 0..20 {
        let e = zim.find_by_path(NS_CONTENT, format!("A{i:02}").as_bytes()).unwrap().unwrap();
        assert_eq!(zim.content(e).unwrap().len(), 200_000);
    }
}

#[test]
fn rejects_corrupt_files_without_panicking() {
    assert!(matches!(Archive::open(write_temp(b"hello").path()), Err(Error::Corrupt(_))));
    let mut bytes = sample(Compression::Zstd, false);
    bytes[0] ^= 0xff;
    assert!(matches!(Archive::open(write_temp(&bytes).path()), Err(Error::BadMagic(_))));

    // Flip every byte of an uncompressed file with 64-bit offsets (so the flips
    // land in offset tables, not only in compressed data), and every third byte
    // of a zstd one. Opening and reading must return errors, never panic.
    for (good, step) in [(sample(Compression::None, true), 1), (sample(Compression::Zstd, false), 3)] {
        for pos in (0..good.len()).step_by(step) {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut bytes = good.clone();
                bytes[pos] ^= flip;
                let file = write_temp(&bytes);
                if let Ok(zim) = Archive::open(file.path()) {
                    for i in 0..zim.entry_count().min(16) {
                        let _ = zim.dirent(i);
                        let _ = zim.content(i);
                    }
                    let _ = zim.front_articles();
                    let _ = zim.main_entry();
                    let _ = zim.find_by_path(NS_CONTENT, b"Apple");
                }
            }
        }
    }
}

#[test]
fn huge_first_offset_in_an_uncompressed_cluster_is_an_error() {
    let mut bytes = sample(Compression::None, true);
    let cluster_ptr_pos = header_u64(&bytes, 48) as usize;
    let first_cluster = header_u64(&bytes, cluster_ptr_pos) as usize;
    bytes[first_cluster + 1..first_cluster + 9].copy_from_slice(&0x8000_0000_0000_0000u64.to_le_bytes());
    let file = write_temp(&bytes);
    let zim = Archive::open(file.path()).unwrap();
    assert!(matches!(zim.blob(0, 0), Err(Error::Corrupt(_))));
    let _ = zim.read_cluster(0).map(|r| r.blob(0).is_err());
}

#[test]
fn over_long_strings_are_rejected_not_scanned() {
    let long_path = "p".repeat(70_000);
    let file = write_temp(&ZimBuilder::new().article(&long_path, "t", "x").build());
    let zim = Archive::open(file.path()).unwrap();
    let lookup = zim.find_by_path(NS_CONTENT, b"Apple");
    assert!(matches!(lookup, Err(Error::Corrupt(_))), "{lookup:?}");
}

/// Real files from openZIM's zim-testing-suite. Fetch them with
/// `scripts/fetch-test-data.sh`; the test fails loudly when they are missing.
fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name);
    assert!(path.exists(), "missing {} — run scripts/fetch-test-data.sh", path.display());
    path
}

#[test]
fn reads_openzim_small_fixture() {
    let zim = Archive::open(fixture("nons_small.zim")).unwrap();
    assert!(zim.verify_checksum().unwrap());
    let main = zim.main_entry().unwrap().expect("small.zim has a main page");
    assert!(!zim.content(main).unwrap().is_empty());
    for i in 0..zim.entry_count() {
        if let DirentKind::Content { .. } = zim.dirent(i).unwrap().kind {
            zim.content(i).unwrap();
        }
    }
}

#[test]
fn reads_legacy_namespace_fixture() {
    let zim = Archive::open(fixture("withns_wikibooks_be_all_nopic_2017-02.zim")).unwrap();
    assert!(zim.verify_checksum().unwrap());
    assert_eq!(zim.article_namespace(), b'A');
    let front = zim.front_articles().unwrap();
    assert!(!front.is_empty());
    for i in front {
        assert_eq!(zim.dirent(i).unwrap().namespace, b'A');
        zim.content(i).unwrap();
    }
}

#[test]
fn reads_real_wikipedia_fixture() {
    let zim = Archive::open(fixture("nons_wikipedia_en_climate_change_mini_2024-06.zim")).unwrap();
    assert_eq!(zim.metadata("Language").unwrap().as_deref(), Some("eng"));
    let front = zim.front_articles().unwrap();
    assert!(front.len() > 100);
    let mut html_articles = 0;
    for &i in &front {
        let resolved = zim.resolve(i).unwrap();
        let d = zim.dirent(resolved).unwrap();
        if zim.mime_type(&d) == Some("text/html") {
            html_articles += 1;
            assert!(std::str::from_utf8(&zim.content(resolved).unwrap()).unwrap().contains("<html"));
        }
    }
    assert!(html_articles > 100);
    for i in 0..zim.header().cluster_count {
        let reader = zim.read_cluster(i).unwrap();
        reader.blob(0).unwrap();
    }
}
