use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::process::{Command, CommandEnvs};

use tempfile::NamedTempFile;

/// A wrapper around [`Command`] that adds support for arg files.
/// This is useful as we have some commands that can get very long and at times
/// hit the OS limit (usually Windows)
///
/// This implementation is based off the `ProcessBuilder` implementation in Cargo
/// but simplified.
///
/// NOTE: In most scenarios we want to avoid arg files as it makes debugging more complicated
///       so we try to avoid it if the command is not close to the OS limit.
#[derive(Debug)]
pub struct ArgFileCommand {
    command: Command,
    args: Vec<OsString>,
}

impl ArgFileCommand {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        let command = Command::new(program);
        Self { command, args: Vec::new() }
    }
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|s| s.as_ref().to_os_string()));
        self
    }

    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.command.env(key, val);
        self
    }

    pub fn get_envs(&self) -> CommandEnvs<'_> {
        self.command.get_envs()
    }

    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.command.env_remove(key);
        self
    }

    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.command.current_dir(dir);
        self
    }

    pub fn stdin(&mut self, stdin: std::process::Stdio) -> &mut Self {
        self.command.stdin(stdin);
        self
    }

    pub fn build(mut self) -> std::io::Result<(Command, Option<NamedTempFile>)> {
        // On Windows there is a hard limit of ~32KB, so we cut off at 30KB to
        // give some buffer just incase.

        let threshold: usize;
        let total_arg_len: usize;
        #[cfg(windows)]
        {
            threshold = (30 * 1024);
            total_arg_len = self.args.iter().map(|a| a.len() + 1).sum();
        }
        // On unix the limit is defined by ARG_MAX. If its not explicitly set we set it to 1MB
        // which is fairly large but lower than the ~2MB that it defaults to on most systems.
        #[cfg(unix)]
        {
            let ptr_size = size_of::<usize>();

            let command_size = self.command.get_program()
                .as_encoded_bytes()
                .len()
                .saturating_add(1) // nul
                .saturating_add(ptr_size);

            let arg_size = self.args.iter().fold(0usize, |acc, a| {
                let arg = a.as_encoded_bytes().len();
                let nul = 1;
                acc.saturating_add(arg).saturating_add(nul).saturating_add(ptr_size)
            });

            let envs_size = std::env::vars_os().fold(0usize, |acc, (k, v)| {
                let k = k.as_encoded_bytes().len();
                let eq = 1;
                let v = v.as_encoded_bytes().len();
                let nul = 1;
                acc.saturating_add(k)
                    .saturating_add(eq)
                    .saturating_add(v)
                    .saturating_add(nul)
                    .saturating_add(ptr_size)
            });

            total_arg_len = arg_size + envs_size + command_size;

            threshold = match unsafe { libc::sysconf(libc::_SC_ARG_MAX) } {
                -1 => 1024 * 1024, // 1MB
                max => max as usize,
            };

        }

        if total_arg_len <= threshold {
            self.command.args(self.args);
            return Ok((self.command, None));
        }

        let mut tmp = tempfile::Builder::new().prefix("bootstrap-argfile.").tempfile()?;

        let mut arg = OsString::from("@");
        arg.push(tmp.path());
        self.command.arg(arg);

        let mut buf = Vec::with_capacity(total_arg_len);
        for arg in &self.args {
            let arg = arg.to_str().ok_or_else(|| {
                std::io::Error::other(format!(
                    "argument for argfile contains invalid UTF-8 characters: `{}`",
                    arg.to_string_lossy()
                ))
            })?;
            if arg.contains('\n') {
                return Err(std::io::Error::other(format!(
                    "argument for argfile contains newlines: `{arg}`"
                )));
            }
            writeln!(buf, "{arg}")?;
        }
        tmp.write_all(&buf)?;
        tmp.flush()?;

        Ok((self.command, Some(tmp)))
    }
}
