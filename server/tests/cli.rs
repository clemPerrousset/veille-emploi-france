//! Tests de la ligne de commande sur le binaire réel (sans réseau, sans identifiants).

use std::process::Command;

fn bin(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_veille_emploi"));
    // Dossier courant vide : aucun `.env` n'est lu.
    c.current_dir(dir)
        .env_remove("FT_CLIENT_ID")
        .env_remove("FT_CLIENT_SECRET")
        .env("VE_DATA_DIR", dir.join("data"));
    c
}

#[test]
fn once_prints_exactly_one_line_on_stdout_and_logs_go_to_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin(dir.path()).args(["once", "rome"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "stdout = {stdout:?}");
    assert!(
        stdout.contains("rome") && stdout.contains("échec"),
        "{stdout}"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("collecte"),
        "journaux attendus sur stderr : {stderr:?}"
    );
}

#[test]
fn unknown_job_exits_with_code_2() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin(dir.path()).args(["once", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
