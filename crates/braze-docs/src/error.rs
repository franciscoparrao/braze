use std::path::PathBuf;

/// Fallas de indexación de una wiki. El retrieval en sí no falla (una
/// query sin hits devuelve vacío, no error) — solo la lectura de disco.
#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("no se pudo listar el directorio {}: {source}", .path.display())]
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("no se pudo leer el archivo {}: {source}", .path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
