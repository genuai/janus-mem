//! The public seam between the embedder and the `vectors.f32` payload.
//!
//! Unit tests cover each module's rules. This test uses only the library's
//! public API, so it also proves that P1b can resolve an embedder, embed a
//! batch, and hand the flat rows to `Vectors` without another adapter.

use std::path::{Path, PathBuf};

use janus_mem::config::Config;
use janus_mem::index::embed::{ModelDirs, Resolved, resolve};
use janus_mem::index::vectors::Vectors;

#[test]
fn hash_embedder_output_round_trips_through_the_payload_format() {
    let config = Config::from_toml(
        "[embedding]\nmodel = \"hash-8\"\n",
        Path::new("<vectors-test>"),
    )
    .expect("inline config parses");
    let dirs = ModelDirs {
        config: None,
        env: None,
        cache: PathBuf::from("/no/such/model/cache"),
    };
    let embedder = match resolve(&config, &dirs) {
        Resolved::Ready(embedder) => embedder,
        Resolved::Degraded(reason, detail) => panic!("expected Ready, got {reason}: {detail}"),
    };

    let texts = ["cache decision", "", "redis cache"];
    let rows = embedder.embed(&texts).expect("hashing cannot fail");
    let dim = embedder.id().dim;

    assert_eq!(rows.len(), texts.len());
    assert!(rows.iter().all(|row| row.len() == dim as usize));
    assert_eq!(rows[1], vec![0.0; dim as usize], "empty text is a zero row");

    let values: Vec<f32> = rows.into_iter().flatten().collect();
    let vectors = Vectors::new(dim, values).expect("the embedder rows are normalised");
    assert_eq!(vectors.len(), texts.len());
    assert_eq!(vectors.dim(), dim as usize);

    let decoded = Vectors::decode(&vectors.encode(), dim, texts.len())
        .expect("the bytes the writer produced are valid");
    assert_eq!(decoded, vectors);
    assert_eq!(decoded.row(1), Some(vec![0.0; dim as usize].as_slice()));
}
