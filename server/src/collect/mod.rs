pub mod indeed;

/// Erreur d'analyse d'une réponse de source de données.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// Contenu qui n'a pas la forme attendue (page d'erreur, colonnes absentes, nombre invalide…).
    #[error("format inattendu : {0}")]
    Format(String),
    /// Données lisibles mais incohérentes entre elles (dates différentes selon les catégories).
    #[error("données incohérentes : {0}")]
    Mismatch(String),
}
