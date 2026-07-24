//! `braze-docs` — RAG léxico offline sobre documentación, la base del
//! modo doc-QA (`docs/docs-qa-mode-design-2026-07-23.md`).
//!
//! Dos responsabilidades, ambas sin dependencias de ML y deterministas:
//!
//! 1. **Chunking** ([`chunk_wiki`] / [`chunk_markdown`]): parte una wiki
//!    markdown por headings ATX, cae a párrafos cuando una sección
//!    excede un tope de palabras, y ancla cada [`DocChunk`] a su
//!    archivo+sección (la procedencia habilita "cita la fuente").
//! 2. **Recuperación** ([`LexicalIndex`], detrás del trait
//!    [`Retriever`]): rankea chunks por solape de tokens contra una
//!    query. Es un **port** del scoring de `search_tools`
//!    (`braze_engine::tool_search::search_stubs`) — el mismo mecanismo
//!    de la fig3 del paper, reapuntado de un inventario de tools a un
//!    corpus de documentación. "Deliberadamente no-BM25": el consumidor
//!    es un modelo chico y valen más el determinismo y la
//!    explicabilidad que el ranking fino.
//!
//! El trait [`Retriever`] existe para que un backend de embeddings
//! pueda entrar más tarde sin tocar el chunker, el prompt ni el loop —
//! el MVP es léxico a propósito (correr en "un PC más o menos viejo",
//! ver el design doc § "Retrieval: por qué léxico y no embeddings").

mod chunk;
mod error;
mod retrieve;

pub use chunk::{DEFAULT_MAX_CHUNK_WORDS, DocChunk, chunk_markdown, chunk_wiki};
pub use error::DocsError;
pub use retrieve::{LexicalIndex, Retriever};
