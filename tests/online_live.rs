//! Hits the real iTunes / MusicBrainz / Cover Art Archive services. Run on demand:
//! `cargo test --test online_live -- --ignored --nocapture`
use skratch::online;

#[test]
#[ignore = "needs network"]
fn live_search_finds_and_downloads_a_cover() {
    let found = online::search("Daft Punk", "Discovery").unwrap();
    for c in found.iter().take(4) {
        println!("{:12} {}", c.source, c.label);
    }
    assert!(found.iter().any(|c| c.source == "iTunes"));
    assert!(found.iter().any(|c| c.source == "MusicBrainz"));
    let apple = found.iter().find(|c| c.source == "iTunes").unwrap();
    let bytes = online::fetch_image(&apple.full).unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    println!("iTunes full cover: {}x{}", image.width(), image.height());
    assert!(image.width() >= 500);
    // not every MusicBrainz group has art, but at least one of the first few should
    let working = found
        .iter()
        .filter(|c| c.source == "MusicBrainz")
        .filter(|c| online::fetch_image(&c.thumb).is_ok())
        .count();
    println!("MusicBrainz thumbnails that downloaded: {working}");
    assert!(working > 0);
}
