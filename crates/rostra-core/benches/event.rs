use divan::Bencher;
use rostra_core::Event;
use rostra_core::event::{EventContentRaw, EventKind};
use rostra_core::id::RostraIdSecretKey;

fn main() {
    // Run registered benchmarks.
    divan::main();
}

#[divan::bench]
fn sign_event(bencher: Bencher) {
    let id_secret = RostraIdSecretKey::generate();

    bencher.bench_local(move || {
        let event = Event::builder_raw_content()
            .author(id_secret.id())
            .kind(EventKind::RAW)
            .content(&EventContentRaw::new(b"test".to_vec()))
            .build();

        event.signed_by(id_secret)
    });
}

#[divan::bench]
fn verify_event(bencher: Bencher) {
    let id_secret = RostraIdSecretKey::generate();
    let event = Event::builder_raw_content()
        .author(id_secret.id())
        .kind(EventKind::RAW)
        .content(&EventContentRaw::new(b"test".to_vec()))
        .build();

    let event_signed = event.signed_by(id_secret);

    bencher.bench_local(move || {
        let event_signed = divan::black_box(event_signed);
        event_signed
            .event
            .verify_signature(event_signed.sig)
            .expect("Can't fail")
    });
}
