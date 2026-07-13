#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct TmuxExecutionDefaults {
    pub session: Option<String>,
    pub window: Option<String>,
    pub agent_command: Option<String>,
}

impl TmuxExecutionDefaults {
    pub fn from_environment() -> Self {
        Self {
            session: non_empty_env("HUMANIZE_TMUX_SESSION"),
            window: non_empty_env("HUMANIZE_TMUX_WINDOW"),
            agent_command: non_empty_env("HUMANIZE_AGENT_COMMAND"),
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn from_environment_reads_humanize_execution_defaults() {
        let _guard = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let names = [
            "HUMANIZE_TMUX_SESSION",
            "HUMANIZE_TMUX_WINDOW",
            "HUMANIZE_AGENT_COMMAND",
        ];
        let prior = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect::<Vec<_>>();

        unsafe {
            std::env::set_var("HUMANIZE_TMUX_SESSION", " host-a ");
            std::env::set_var("HUMANIZE_TMUX_WINDOW", " ");
            std::env::set_var("HUMANIZE_AGENT_COMMAND", " humanize-test-agent ");
        }

        let defaults = TmuxExecutionDefaults::from_environment();

        restore_env(prior);
        assert_eq!(
            defaults,
            TmuxExecutionDefaults {
                session: Some("host-a".into()),
                window: None,
                agent_command: Some("humanize-test-agent".into()),
            }
        );
    }

    fn restore_env(prior: Vec<(&'static str, Option<OsString>)>) {
        for (name, value) in prior {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
