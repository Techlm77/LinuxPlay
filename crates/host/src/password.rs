//! The optional permanent pairing password, for unattended hosts where nobody
//! is there to read a PIN. It never travels in process arguments or the
//! environment: it lives in a `0600` file, set with `linuxplay-host
//! set-password`, which reads it from the terminal without echo.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use linuxplay_net::{fsutil, tls};

/// Shorter passwords are refused; the pairing exchange stops offline
/// guessing, and the rate limits make online guessing slow, but a pairing
/// password is long-lived.
pub const MIN_LEN: usize = 12;

pub fn default_path() -> PathBuf {
    tls::config_dir("host").join("password")
}

fn check(pw: &str) -> Result<()> {
    if pw.chars().count() < MIN_LEN {
        bail!("the pairing password must be at least {MIN_LEN} characters");
    }
    Ok(())
}

/// Read the password file, if there is one.
pub fn load(path: &Path) -> Result<Option<String>> {
    let Some(text) = fsutil::read_private_text(path)? else { return Ok(None) };
    let pw = text.lines().next().unwrap_or("").to_string();
    check(&pw)?;
    Ok(Some(pw))
}

pub fn save(path: &Path, pw: &str) -> Result<()> {
    check(pw)?;
    fsutil::write_private(path, format!("{pw}\n").as_bytes())
}

/// Read a line from the terminal with echo turned off.
fn read_hidden(prompt: &str) -> Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("set-password needs a terminal to read the password from");
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let fd = libc::STDIN_FILENO;
    // SAFETY: plain termios calls on stdin with a zeroed struct to fill.
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut term) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let saved = term;
    term.c_lflag &= !libc::ECHO;
    term.c_lflag |= libc::ECHONL;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    let mut line = String::new();
    let r = stdin.lock().read_line(&mut line);
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    r?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Ask for a new password twice.
pub fn prompt_new() -> Result<String> {
    let a = read_hidden(&format!("New pairing password (at least {MIN_LEN} characters): "))?;
    check(&a)?;
    let b = read_hidden("Again: ")?;
    if a != b {
        bail!("the passwords don't match");
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_passwords_are_refused_and_strong_ones_round_trip() {
        let dir = std::env::temp_dir().join(format!("linuxplay-pw-{}", std::process::id()));
        let path = dir.join("password");
        assert!(save(&path, "short").is_err());
        assert!(load(&path).unwrap().is_none());
        save(&path, "correct horse battery").unwrap();
        assert_eq!(load(&path).unwrap().as_deref(), Some("correct horse battery"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
