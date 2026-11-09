pub mod core;
pub mod dares;
pub mod eurostat;
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

/// Échec d'une collecte. Les messages n'embarquent jamais d'URL avec paramètres ni de secret.
#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    /// Source injoignable, statut HTTP non 2xx ou réponse vide.
    #[error("récupération impossible : {0}")]
    Fetch(String),
    /// Réponse reçue mais inexploitable.
    #[error("analyse impossible : {0}")]
    Parse(#[from] ParseError),
    /// Le fichier de données n'a pas pu être écrit (l'ancien reste en place).
    #[error("écriture impossible : {0}")]
    Write(String),
}
