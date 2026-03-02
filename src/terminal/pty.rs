use std::env;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("open pty failed: {0}")]
    Open(String),
    #[error("spawn command failed: {0}")]
    Spawn(String),
    #[error("read output failed: {0}")]
    Read(String),
    #[error("write input failed: {0}")]
    Write(String),
    #[error("resize failed: {0}")]
    Resize(String),
    #[error("wait command failed: {0}")]
    Wait(String),
    #[error("pty reader already taken")]
    ReaderTaken,
}

pub struct LivePty {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    reader: Option<Box<dyn Read + Send>>,
    child: Box<dyn Child + Send + Sync>,
}

impl LivePty {
    pub fn spawn(
        rows: u16,
        cols: u16,
        shell: &str,
        working_directory: Option<&str>,
        startup_command: Option<&str>,
    ) -> Result<Self, PtyError> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let mut cmd = CommandBuilder::new(shell);
        if let Some(dir) = working_directory {
            cmd.cwd(dir);
        }
        if let Some(command) = startup_command {
            cmd.arg("-lc");
            // Run requested command first, then continue in an interactive shell.
            cmd.arg(format!("{command}; exec {} -li", shell_single_quote(shell)));
        } else {
            cmd.arg("-li");
        }
        // neoshd is started via non-interactive bootstrap, so TERM may be missing.
        // Set a sane default to keep readline/backspace and termcap behavior stable.
        let term = env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
        cmd.env("TERM", term);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Write(e.to_string()))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Read(e.to_string()))?;

        Ok(Self {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            reader: Some(reader),
            child,
        })
    }

    pub fn take_reader(&mut self) -> Result<Box<dyn Read + Send>, PtyError> {
        self.reader.take().ok_or(PtyError::ReaderTaken)
    }

    pub fn writer(&self) -> Arc<Mutex<Box<dyn Write + Send>>> {
        Arc::clone(&self.writer)
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Resize(e.to_string()))
    }

    pub fn wait(&mut self) -> Result<(), PtyError> {
        self.child
            .wait()
            .map(|_| ())
            .map_err(|e| PtyError::Wait(e.to_string()))
    }
}

fn shell_single_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", input.replace('\'', r"'\''"))
}

pub struct PtyRuntime {
    rows: u16,
    cols: u16,
}

impl PtyRuntime {
    pub fn new(rows: u16, cols: u16) -> Result<Self, PtyError> {
        Ok(Self { rows, cols })
    }

    pub fn run_shell_capture(&mut self, script: &str) -> Result<String, PtyError> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: self.rows,
                cols: self.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let mut cmd = CommandBuilder::new("sh");
        cmd.arg("-lc");
        cmd.arg(script);

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Read(e.to_string()))?;

        let mut output = String::new();
        reader
            .read_to_string(&mut output)
            .map_err(|e| PtyError::Read(e.to_string()))?;

        let _status = child.wait().map_err(|e| PtyError::Wait(e.to_string()))?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_runtime_captures_output() {
        let mut rt = PtyRuntime::new(24, 80).unwrap();
        let out = rt.run_shell_capture("printf pty-ok").unwrap();
        assert!(out.contains("pty-ok"));
    }

    #[test]
    fn live_pty_supports_resize() {
        let pty = LivePty::spawn(24, 80, "sh", None, None).unwrap();
        assert!(pty.resize(40, 120).is_ok());
    }
}
