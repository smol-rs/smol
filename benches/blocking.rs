use criterion::*;
use futures::StreamExt;
use rand::prelude::*;
use ring::{
    digest,
    signature::{self, KeyPair},
};

const NUM_BATCHES: usize = 100;
const BATCH_SIZE: usize = 10_000; // 10kb

pub fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto work");
    group.sample_size(10);
    group.bench_function("sync", |b| {
        b.iter(|| {
            (0..NUM_BATCHES).for_each(|_| {
                do_work();
            });
        })
    });
    group.bench_function("smol_blocking_dispatch", |b| {
        b.iter(|| {
            smol::run(
                futures::stream::iter(0..NUM_BATCHES).for_each_concurrent(None, |_| {
                    smol::Task::blocking(async {
                        do_work();
                    })
                }),
            );
        })
    });
    group.bench_function("async_std_blocking_dispatch", |b| {
        b.iter(|| {
            async_std::task::block_on(
                futures::stream::iter(0..NUM_BATCHES).for_each_concurrent(None, |_| {
                    async_std::task::spawn_blocking(do_work)
                }),
            );
        })
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

fn do_work() {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8_bytes = signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key_pair = signature::Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref()).unwrap();
    let peer_public_key_bytes = key_pair.public_key().as_ref();
    let peer_public_key =
        signature::UnparsedPublicKey::new(&signature::ED25519, peer_public_key_bytes);
    let mut data = vec![0; BATCH_SIZE];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut data);
    for _ in 0..100 {
        let digest = digest::digest(&digest::SHA256, &data);
        let sig = key_pair.sign(digest.as_ref());
        let result = peer_public_key.verify(digest.as_ref(), sig.as_ref());
        black_box(result).unwrap();
    }
}
