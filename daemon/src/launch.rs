//! Starting other programs.
//!
//! Everything the launcher opens goes through here so it lands in its own
//! systemd scope: the child gets an independent cgroup and survives the daemon,
//! which a plain fork does not guarantee.

use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::process::{Command, Stdio};

pub fn detached<S: AsRef<OsStr>>(program: &str, args: &[S]) -> Result<()> {
    let mut attempts: Vec<Command> = Vec::new();

    let mut scoped = Command::new("systemd-run");
    scoped.args(["--user", "--collect", "--scope", "--quiet", "--", program]);
    scoped.args(args);
    attempts.push(scoped);

    // Without systemd, a detached spawn is the best available.
    let mut direct = Command::new(program);
    direct.args(args);
    attempts.push(direct);

    let mut last_err = None;
    for mut command in attempts {
        match command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // systemd-run returns as soon as the scope is up. Reap it off-thread
                // so we neither block the caller nor leave a zombie behind.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!("could not run {program}: {last_err:?}"))
}
