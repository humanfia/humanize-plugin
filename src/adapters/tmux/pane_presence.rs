use super::{
    CommandRunner, TmuxActivationMetadata, TmuxAdapter, TmuxError, TmuxPane, TmuxPaneIdentity,
    argv, metadata_pane_target, pane_identity_stdout, pane_identity_text, pane_target,
    validate_owned_session_id,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum TmuxPanePresence {
    Present,
    Absent,
}

impl<R: CommandRunner> TmuxAdapter<R> {
    pub(crate) fn probe_exact_pane_presence(
        &self,
        metadata: &TmuxActivationMetadata,
    ) -> Result<TmuxPanePresence, TmuxError> {
        validate_owned_session_id("pane", metadata.session_id())?;
        self.probe_pane_identity(
            &metadata_pane_target(metadata),
            &TmuxPaneIdentity::new(
                metadata.session_id(),
                metadata.window_id(),
                metadata.window_name(),
                metadata.pane_id(),
            ),
            true,
        )
    }

    pub(crate) fn probe_pane_presence(
        &self,
        pane: &TmuxPane,
    ) -> Result<TmuxPanePresence, TmuxError> {
        validate_owned_session_id("pane", pane.session_id())?;
        self.probe_pane_identity(
            &pane_target(pane),
            &TmuxPaneIdentity::new(pane.session_id(), pane.window_id(), "", pane.id()),
            false,
        )
    }

    fn probe_pane_identity(
        &self,
        target: &str,
        expected: &TmuxPaneIdentity,
        match_window_name: bool,
    ) -> Result<TmuxPanePresence, TmuxError> {
        let display_argv = argv(
            ["tmux", "display-message", "-p", "-t", target],
            ["#{session_name}|#{window_id}|#{window_name}|#{pane_id}"],
        );
        let display = self.runner.run(display_argv.clone())?;
        if display.is_success() {
            let actual = pane_identity_stdout(&display)?;
            return Ok(
                if pane_identity_matches(expected, &actual, match_window_name) {
                    TmuxPanePresence::Present
                } else {
                    TmuxPanePresence::Absent
                },
            );
        }

        let display_error =
            TmuxError::command_failed(&display_argv, display.status, &display.stderr);
        let inventory = self.run_checked(argv(
            ["tmux", "list-panes", "-a", "-F"],
            ["#{session_name}|#{window_id}|#{window_name}|#{pane_id}"],
        ))?;
        for line in inventory
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let actual = pane_identity_text(line)?;
            if pane_identity_matches(expected, &actual, match_window_name) {
                return Err(display_error);
            }
        }
        Ok(TmuxPanePresence::Absent)
    }
}

fn pane_identity_matches(
    expected: &TmuxPaneIdentity,
    actual: &(String, String, String, String),
    match_window_name: bool,
) -> bool {
    expected.session_id == actual.0
        && expected.window_id == actual.1
        && (!match_window_name || expected.window_name == actual.2)
        && expected.pane_id == actual.3
}
