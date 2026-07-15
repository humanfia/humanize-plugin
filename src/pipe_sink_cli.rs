use std::error::Error;
use std::io;
use std::path::Path;

use crate::pipe_sink::{
    PipeSinkAckRequest, PipeSinkIdentity, append_reader_to_pipe_log_under_root_with_completion,
};

pub fn run_pipe_sink(args: &[String]) -> Result<(), Box<dyn Error>> {
    let value = |flag: &str| -> Result<&str, Box<dyn Error>> {
        args.windows(2)
            .find_map(|window| (window[0] == flag).then_some(window[1].as_str()))
            .ok_or_else(|| format!("missing {flag}").into())
    };
    let root = Path::new(value("--root")?);
    let relative = Path::new(value("--relative")?);
    let ack_relative = Path::new(value("--ack-relative")?);
    let completion_relative = Path::new(value("--completion-relative")?);
    let ack_nonce = value("--ack-nonce")?;
    let identity = PipeSinkIdentity {
        dev: value("--dev")?.parse()?,
        ino: value("--ino")?.parse()?,
        uid: value("--uid")?.parse()?,
        mode: value("--mode")?.parse()?,
        nlink: value("--nlink")?.parse()?,
    };
    let ack = PipeSinkAckRequest::new(ack_relative, ack_nonce);
    let completion = PipeSinkAckRequest::new(completion_relative, ack_nonce);
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    append_reader_to_pipe_log_under_root_with_completion(
        root,
        relative,
        &identity,
        Some(&ack),
        Some(&completion),
        &mut reader,
    )?;
    Ok(())
}
