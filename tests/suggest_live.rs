//! Needs the network: `cargo test --test suggest_live -- --ignored --nocapture`.
use skratch::{model::Track, suggest};

#[test]
#[ignore = "needs the network"]
fn a_known_song_gets_suggestions() {
    let track = Track {
        id: 1,
        title: "Bohemian Rhapsody".into(),
        artist: "Queen".into(),
        duration: 355.0,
        ..Track::default()
    };
    let started = std::time::Instant::now();
    let choices = suggest::find(&track).unwrap();
    for c in &choices {
        println!(
            "{:?} cover={:?} thumb={}",
            c.changes,
            c.cover.as_ref().map(|c| &c.label),
            c.thumb.is_some()
        );
    }
    println!("took {:?}", started.elapsed());
    assert!(!choices.is_empty());
}
