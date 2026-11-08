// Plays downloaded audio bytes through mpv. One player at a time: starting a new
// sound kills the previous process.

use std::io::Write;
use std::process::{Child, Command, Stdio};

pub struct Player {
    current: Option<Child>,
}

impl Player {
    pub fn new() -> Self {
        Player { current: None }
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.current.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Feed raw audio bytes to mpv over stdin.
    pub fn play(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stop();
        let mut child = Command::new("mpv")
            .args(["--no-video", "--really-quiet", "--no-terminal", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("mpv failed to start: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("no mpv stdin")?
            .write_all(bytes)
            .map_err(|e| e.to_string())?;
        self.current = Some(child);
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}
