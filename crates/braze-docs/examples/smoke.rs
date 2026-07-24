//! Smoke manual: indexa una wiki markdown y corre una query léxica.
//!
//! `cargo run -p braze-docs --example smoke -- <dir> <query...>`
//!
//! Sirve de ancla "en vivo" mientras no exista el modo `braze docs`:
//! prueba el chunker + el retriever contra markdown real, no fixtures.

use braze_docs::{DEFAULT_MAX_CHUNK_WORDS, LexicalIndex, Retriever, chunk_wiki};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| {
        eprintln!("uso: smoke <dir> <query...>");
        std::process::exit(2);
    });
    let query: String = args.collect::<Vec<_>>().join(" ");

    let chunks = match chunk_wiki(std::path::Path::new(&dir)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error indexando {dir}: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "indexados {} chunks (tope {} palabras) de {dir}",
        chunks.len(),
        DEFAULT_MAX_CHUNK_WORDS,
    );

    if query.trim().is_empty() {
        return;
    }

    let index = LexicalIndex::new(chunks);
    let hits = index.top_k(&query, 5);
    println!("\ntop {} para «{query}»:\n", hits.len());
    for (rank, chunk) in hits.iter().enumerate() {
        let preview: String = chunk.text.chars().take(90).collect();
        println!(
            "{}. [{}] {}\n   {}…\n",
            rank + 1,
            chunk.source,
            if chunk.heading.is_empty() {
                "(sin heading)"
            } else {
                &chunk.heading
            },
            preview.replace('\n', " "),
        );
    }
}
