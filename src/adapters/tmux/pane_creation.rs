use super::*;

impl<R: CommandRunner> TmuxAdapter<R> {
    pub fn create_session_with_window_pane(
        &self,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Result<(TmuxSession, TmuxWindow, TmuxPane), TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );

        Ok((session, window, pane))
    }

    pub(crate) fn create_session_with_window_pane_identified(
        &self,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
        operation_id: &str,
    ) -> Result<(TmuxSession, TmuxWindow, TmuxPane), TmuxError> {
        let session = TmuxSession::new(session_id);
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let shell_command = identified_pane_shell_command(operation_id)?;
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-s",
                session.id(),
                "-n",
                window_name.as_str(),
            ],
            [shell_command.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );
        Ok((session, window, pane))
    }

    pub fn create_window_named_with_pane(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
    ) -> Result<(TmuxWindow, TmuxPane), TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-t",
                session.id(),
                "-n",
            ],
            [window_name.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );

        Ok((window, pane))
    }

    pub(crate) fn create_window_named_with_pane_identified(
        &self,
        session: &TmuxSession,
        run_id: impl Into<String>,
        window_name: impl Into<String>,
        activation_id: impl Into<String>,
        operation_id: &str,
    ) -> Result<(TmuxWindow, TmuxPane), TmuxError> {
        validate_session_id(session.id())?;
        let run_id = run_id.into();
        let window_name = window_name.into();
        let activation_id = activation_id.into();
        let shell_command = identified_pane_shell_command(operation_id)?;
        let output = self.run_checked(argv(
            [
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}|#{pane_id}",
                "-t",
                session.id(),
                "-n",
                window_name.as_str(),
            ],
            [shell_command.as_str()],
        ))?;
        let (window_id, pane_id) = window_pane_stdout(&output)?;
        let window = TmuxWindow::new_named(session.id(), run_id, window_name, window_id);
        let pane = TmuxPane::new_in_session(
            session.id(),
            window.id(),
            activation_id.as_str(),
            pane_id.as_str(),
        );
        Ok((window, pane))
    }

    pub fn split_pane_for_activation(
        &self,
        window: &TmuxWindow,
        activation_id: impl Into<String>,
    ) -> Result<TmuxPane, TmuxError> {
        validate_owned_session_id("window", window.session_id())?;
        let activation_id = activation_id.into();
        let target = window_target(window);
        let output = self.run_checked(argv(
            [
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                target.as_str(),
            ],
            ["-v"],
        ))?;
        let pane_id = trimmed_stdout(&output, "pane id")?;

        Ok(TmuxPane::new_in_session(
            window.session_id(),
            window.id(),
            activation_id,
            pane_id,
        ))
    }

    pub(crate) fn split_pane_for_activation_identified(
        &self,
        window: &TmuxWindow,
        activation_id: impl Into<String>,
        operation_id: &str,
    ) -> Result<TmuxPane, TmuxError> {
        validate_owned_session_id("window", window.session_id())?;
        let activation_id = activation_id.into();
        let target = window_target(window);
        let shell_command = identified_pane_shell_command(operation_id)?;
        let output = self.run_checked(argv(
            [
                "tmux",
                "split-window",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                target.as_str(),
                "-v",
            ],
            [shell_command.as_str()],
        ))?;
        let pane_id = trimmed_stdout(&output, "pane id")?;
        Ok(TmuxPane::new_in_session(
            window.session_id(),
            window.id(),
            activation_id,
            pane_id,
        ))
    }

    pub(crate) fn find_identified_pane(
        &self,
        session_id: &str,
        run_id: &str,
        window_name: &str,
        activation_id: &str,
        operation_id: &str,
    ) -> Result<Option<(TmuxWindow, TmuxPane)>, TmuxError> {
        validate_session_id(session_id)?;
        let marker = identified_pane_marker(operation_id)?;
        let output = self.runner.run(argv(
            ["tmux", "list-panes", "-a", "-F"],
            ["#{session_name}|#{window_id}|#{window_name}|#{pane_id}|#{pane_start_command}"],
        ))?;
        if !output.is_success() {
            return Ok(None);
        }
        for line in output.stdout.lines() {
            let fields = if line.contains('|') {
                line.splitn(5, '|').collect::<Vec<_>>()
            } else {
                line.splitn(5, '\t').collect::<Vec<_>>()
            };
            if fields.len() != 5
                || fields[0] != session_id
                || fields[2] != window_name
                || !fields[4].contains(&marker)
            {
                continue;
            }
            let window = TmuxWindow::new_named(session_id, run_id, window_name, fields[1]);
            let pane = TmuxPane::new_in_session(session_id, fields[1], activation_id, fields[3]);
            return Ok(Some((window, pane)));
        }
        Ok(None)
    }
}

fn identified_pane_shell_command(operation_id: &str) -> Result<String, TmuxError> {
    let marker = identified_pane_marker(operation_id)?;
    Ok(format!("exec env {marker} \"${{SHELL:-/bin/sh}}\""))
}

fn identified_pane_marker(operation_id: &str) -> Result<String, TmuxError> {
    if operation_id.is_empty()
        || !operation_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(TmuxError::InvalidOperationId);
    }
    Ok(format!("HUMANIZE_TMUX_OPERATION_ID={operation_id}"))
}
