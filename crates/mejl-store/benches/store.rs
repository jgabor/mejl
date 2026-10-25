//! Store benchmarks on a 100k-message mailbox.
//!
//! Run with `cargo bench -p mejl-store`. The store is built once per run
//! (it takes several seconds) in a temp dir.

#[path = "support/synth.rs"]
mod synth;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use mejl_jmap::method::ChangesResponse;
use mejl_jmap::types::Email;
use mejl_store::{Reader, ReaderFactory, SearchOptions, Store, StoreOptions};

const N: usize = 100_000;
const PAGE: usize = 60;

struct Shared {
    _dir: tempfile::TempDir,
    readers: ReaderFactory,
    writer_dir: PathBuf,
    thread: String,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let dir = tempfile::tempdir().expect("temp dir");
        let read_dir = dir.path().join("read");
        let started = std::time::Instant::now();
        let store = synth::build(&read_dir, N, synth::now());
        eprintln!("built {N} messages in {:?}", started.elapsed());
        let readers = store.reader_factory();
        drop(store);
        // A second copy for the writer bench, so it cannot disturb reads.
        let writer_dir = dir.path().join("write");
        std::fs::create_dir_all(&writer_dir).expect("writer dir");
        for name in ["cache.db", "state.db"] {
            std::fs::copy(read_dir.join(name), writer_dir.join(name)).expect("copy");
        }
        Shared {
            readers,
            writer_dir,
            thread: synth::long_thread(N, synth::now()),
            _dir: dir,
        }
    })
}

thread_local! {
    static READER: Reader = shared().readers.open().expect("reader");
}

fn writer() -> &'static Mutex<Store> {
    static WRITER: OnceLock<Mutex<Store>> = OnceLock::new();
    WRITER.get_or_init(|| {
        Mutex::new(Store::open(&shared().writer_dir, StoreOptions::default()).expect("writer"))
    })
}

fn main() {
    shared();
    divan::main();
}

#[divan::bench(args = [0, 1_000, 30_000])]
fn conversation_page(bencher: divan::Bencher, offset: usize) {
    READER.with(|r| {
        bencher.bench_local(|| r.conversations(synth::INBOX, offset, PAGE).expect("page"));
    });
}

#[divan::bench(args = [0, 30_000])]
fn message_page(bencher: divan::Bencher, offset: usize) {
    READER.with(|r| {
        bencher.bench_local(|| r.messages(synth::INBOX, offset, PAGE).expect("page"));
    });
}

#[divan::bench]
fn conversation_detail(bencher: divan::Bencher) {
    let thread = shared().thread.clone();
    READER.with(|r| {
        bencher.bench_local(|| r.conversation(&thread).expect("thread"));
    });
}

#[divan::bench]
fn mailbox_list(bencher: divan::Bencher) {
    READER.with(|r| {
        bencher.bench_local(|| r.mailboxes().expect("mailboxes"));
    });
}

#[divan::bench(args = ["g", "ga", "gar", "gard", "garde", "garden", "garden ro", "from:juniper is:unread", "\"compost kettle\""])]
fn search_keystroke(bencher: divan::Bencher, query: &str) {
    let options = SearchOptions::default();
    READER.with(|r| {
        bencher.bench_local(|| r.search(query, &options).expect("search"));
    });
}

#[divan::bench(sample_count = 30, sample_size = 1)]
fn apply_500_email_changes(bencher: divan::Bencher) {
    let mut round = 0u64;
    bencher
        .with_inputs(|| {
            round += 1;
            let fetched = synth::flag_batch(N, 500, round);
            let mut changes = ChangesResponse::<Email>::new("A1", "s", "s");
            changes.updated = fetched.iter().map(|e| e.id.clone()).collect();
            (changes, fetched)
        })
        .bench_local_values(|(changes, fetched)| {
            let mut store = writer().lock().unwrap_or_else(|e| e.into_inner());
            let mut batch = store.batch().expect("batch");
            batch
                .apply_email_changes(&changes, &fetched)
                .expect("apply");
            batch.commit().expect("commit");
        });
}
