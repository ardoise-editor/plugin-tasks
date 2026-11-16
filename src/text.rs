//! The texts of the plugin, in English and French.

pub fn t(french: bool, key: &str) -> &'static str {
    let (en, fr) = match key {
        "pick" => ("Pick a task", "Choisir une tâche"),
        "run" => ("Run", "Lancer"),
        "stop" => ("Stop", "Arrêter"),
        "rerun" => ("Run again", "Relancer"),
        "clear" => ("Clear", "Effacer"),
        "none" => (
            "No task in this project (Cargo, npm, Make, Go or .simpleedit/tasks.json)",
            "Aucune tâche dans ce projet (Cargo, npm, Make, Go ou .simpleedit/tasks.json)",
        ),
        "running" => ("Running…", "En cours…"),
        "ok" => ("Done", "Terminé"),
        "failed" => ("Failed with code", "Échec avec le code"),
        "ended" => ("Ended", "Terminé sans code"),
        "stopped" => ("Stopped", "Arrêté"),
        "trust" => (
            "This project has commands of its own. Run them? They are in",
            "Ce projet a ses propres commandes. Les lancer ? Elles sont dans",
        ),
        _ => ("", ""),
    };
    if french {
        fr
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_text_exists_in_both_languages() {
        for key in [
            "pick", "run", "stop", "rerun", "clear", "none", "running", "ok", "failed", "ended",
            "stopped", "trust",
        ] {
            assert!(
                !super::t(false, key).is_empty() && !super::t(true, key).is_empty(),
                "{key}"
            );
        }
    }
}
