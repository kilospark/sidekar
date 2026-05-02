//! `! cmd` shell escape with spinner + elapsed timing.
//!
//! Pipes the subprocess's stdout and stderr (stdin stays inherited) so a
//! background thread can show a spinner until the subprocess produces its
//! first byte — useful for silent slow commands like `uv run`. Once output
//! starts the spinner row is cleared and bytes pass through verbatim.
//!
//! Trade-off: piping stdout means the subprocess thinks it's not on a TTY,
//! so colored output / progress bars from the subprocess are typically
//! suppressed. Interactive shells (`vim`, `python` REPL, `top`) won't
//! render correctly under `!` — use a separate terminal for those.
//!
//! **Ctrl+C (SIGINT)**: With ISIG on in cooked mode, the tty driver sends
//! SIGINT to every process in the **foreground process group**. The child
//! `sh` shares that group with Sidecar, so a naive `wait()` would also hit
//! the parent — interrupting reads, leaving the terminal/line discipline in
//! a bad state, and making the REPL feel sluggish after the subprocess exits.
//! On Unix we ignore SIGINT in the parent for the duration of the subshell
//! and reset SIGINT to default in the child before `exec`, so only the shell
//! receives the interactive interrupt.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::editor::RawModeGuard;
use super::spinner;
use crate::tunnel::tunnel_println;

#[cfg(unix)]
struct BlockParentSigint {
    old: libc::sigaction,
}

#[cfg(unix)]
impl BlockParentSigint {
    fn install() -> Option<Self> {
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut new: libc::sigaction = unsafe { std::mem::zeroed() };
        new.sa_sigaction = libc::SIG_IGN;
        unsafe {
            if libc::sigemptyset(&mut new.sa_mask) != 0 {
                return None;
            }
            if libc::sigaction(libc::SIGINT, &new, &mut old) != 0 {
                return None;
            }
        }
        Some(Self { old })
    }
}

#[cfg(unix)]
impl Drop for BlockParentSigint {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::sigaction(libc::SIGINT, &self.old, std::ptr::null_mut());
        }
    }
}

pub(super) fn run(cmd: &str) {
    let _guard = RawModeGuard::enter_cooked();
    #[cfg(unix)]
    let _sigint_parent = BlockParentSigint::install();
    let started = Instant::now();

    let mut cmd_builder = std::process::Command::new("sh");
    cmd_builder
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd_builder.pre_exec(|| {
                // Fork copied parent's SIG_IGN; restore default so ^C from the
                // tty reaches `sh` / the command, not the Rust runtime.
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                Ok(())
            });
        }
    }

    let mut child = match cmd_builder.spawn() {
        Ok(c) => c,
        Err(e) => {
            tunnel_println(&format!("\x1b[31mFailed to run command: {e}\x1b[0m"));
            return;
        }
    };

    // `output_started` flips to true on the subprocess's first byte. The
    // spinner uses it to decide whether to keep emitting frames; the reader
    // uses it to know it must clear the spinner row before its first write.
    // `term_lock` serializes terminal writes so spinner ticks don't
    // interleave with subprocess output.
    let output_started = Arc::new(AtomicBool::new(false));
    let term_lock = Arc::new(Mutex::new(()));
    let stop_spinner = Arc::new(AtomicBool::new(false));

    let spinner_handle = spawn_spinner(
        output_started.clone(),
        term_lock.clone(),
        stop_spinner.clone(),
    );

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let h_out = spawn_reader(stdout, output_started.clone(), term_lock.clone());
    let h_err = spawn_reader(stderr, output_started.clone(), term_lock.clone());

    let status = child.wait();
    let _ = h_out.join();
    let _ = h_err.join();
    stop_spinner.store(true, Ordering::Relaxed);
    let _ = spinner_handle.join();

    // Final cleanup: if the spinner showed but the subprocess never produced
    // output, the spinner row is still there. Erase it so the completion
    // line doesn't sit beside a stale frame.
    if !output_started.load(Ordering::Relaxed) {
        let _g = term_lock.lock();
        let _ = std::io::stdout().write_all(b"\r\x1b[2K");
        let _ = std::io::stdout().flush();
    }

    let elapsed = format_elapsed(started.elapsed());
    match status {
        Ok(s) if !s.success() => {
            tunnel_println(&format!(
                "\x1b[2m[exit {}, {elapsed}]\x1b[0m",
                s.code().unwrap_or(-1)
            ));
        }
        Err(e) => {
            tunnel_println(&format!("\x1b[31mFailed to wait: {e}\x1b[0m"));
        }
        _ => {}
    }
}

fn spawn_spinner(
    output_started: Arc<AtomicBool>,
    term_lock: Arc<Mutex<()>>,
    stop_spinner: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut i = 0usize;
        while !stop_spinner.load(Ordering::Relaxed) {
            if !output_started.load(Ordering::Relaxed)
                && let Ok(_g) = term_lock.lock()
            {
                // Re-check inside the lock so we don't race the reader.
                if !output_started.load(Ordering::Relaxed) {
                    // \r\x1b[2K returns to col 0 and clears the line so
                    // each tick replaces the previous frame in place.
                    let line = format!(
                        "\r\x1b[2K{}",
                        spinner::frame(i, started.elapsed(), "running…")
                    );
                    let mut out = std::io::stdout();
                    let _ = out.write_all(line.as_bytes());
                    let _ = out.flush();
                }
            }
            i = i.wrapping_add(1);
            std::thread::sleep(spinner::TICK);
        }
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut pipe: R,
    output_started: Arc<AtomicBool>,
    term_lock: Arc<Mutex<()>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let Ok(_g) = term_lock.lock() else { break };
                    if !output_started.swap(true, Ordering::Relaxed) {
                        // First byte from the subprocess: erase the spinner
                        // frame the user is currently looking at before the
                        // subprocess output lands on the same row.
                        let _ = std::io::stdout().write_all(b"\r\x1b[2K");
                    }
                    let _ = std::io::stdout().write_all(&buf[..n]);
                    let _ = std::io::stdout().flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn format_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f32())
    }
}
