// SPDX-License-Identifier: GPL-3.0-or-later
use super::context::{set_result, shell_policy};
use std::ffi::{CStr, c_char};

// ── Shio.run_shell(cmd) ──────────────────────────────────────────────────────

const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn run_shell_command(cmd: &str, timeout: std::time::Duration) -> String {
    let child = match spawn_shell(cmd) {
        Err(e) => return format!("Error running command: {e}"),
        Ok(c) => c,
    };

    let pid = child.id();

    // Run wait_with_output() in a background thread so stdout/stderr are
    // drained continuously; avoids deadlock when the child produces more
    // output than the OS pipe buffer.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let out = match rx.recv_timeout(timeout) {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return format!("Error reading command output: {e}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_shell_tree(pid);
            // Best-effort wait so the wait_with_output thread can observe the
            // SIGKILL and release the child's pipes. Bounded at 1s — if the
            // kernel hasn't reaped the group by then the thread (and its fds)
            // outlive this call, but the next gc will catch them.
            let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
            return format!("Error: command timed out after {}s", timeout.as_secs());
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return "Error: command thread panicked".to_string();
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(&stderr);
    }
    if !out.status.success() {
        result.push_str(&format!(
            "\n[exit code: {}]",
            out.status.code().unwrap_or(-1)
        ));
    }
    if result.is_empty() {
        "(no output)".to_string()
    } else {
        result
    }
}

fn spawn_shell(cmd: &str) -> std::io::Result<std::process::Child> {
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc_setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command.spawn()
}

#[cfg(unix)]
fn kill_shell_tree(pid: u32) {
    // kill(2) with a negative PID sends SIGKILL to the process group whose id
    // is `pid`. `spawn_shell` creates that group with setsid().
    libc_kill(-(pid as i32), 9);
}

#[cfg(not(unix))]
fn kill_shell_tree(_pid: u32) {}

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
fn libc_setsid() -> i32 {
    unsafe { setsid() }
}

#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) -> i32 {
    unsafe { kill(pid, sig) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_run_shell(
    cmd: *const c_char,
    error_out: *mut *const c_char,
) -> *const c_char {
    let _ = error_out; // errors returned as strings, not C errors
    let cmd = unsafe { CStr::from_ptr(cmd) }.to_string_lossy();

    let (allow, deny) = shell_policy();
    if let Err(msg) = crate::tools::check_shell_policy(&cmd, &allow, &deny) {
        return set_result(format!("Error: {msg}"));
    }

    set_result(run_shell_command(&cmd, SHELL_TIMEOUT))
}

#[cfg(test)]
mod tests {
    use super::run_shell_command;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn run_shell_timeout_kills_descendant_processes() {
        let path = std::env::temp_dir().join(format!(
            "shio_timeout_descendant_{}.txt",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let cmd = format!("sh -c 'sleep 2; touch {}' & wait", path.to_string_lossy());
        let out = run_shell_command(&cmd, std::time::Duration::from_millis(100));
        assert!(out.contains("timed out"), "{out}");
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(
            !path.exists(),
            "timeout should kill background descendants before they can touch the file"
        );
    }
}
