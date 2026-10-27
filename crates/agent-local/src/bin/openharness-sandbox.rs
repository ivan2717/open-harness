//! Trusted Windows launcher. A launch failure never executes the command outside
//! AppContainer. The caller must supervise this process with a kill-on-close Job.
#[cfg(windows)]
#[path = "../windows_sandbox/mod.rs"]
mod windows_sandbox;

fn main() {
    #[cfg(windows)]
    let code: u32 = match windows_sandbox::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "Windows command sandbox failed: {error}. The command was not retried without isolation."
            );
            125
        }
    };
    #[cfg(not(windows))]
    let code: u32 = {
        eprintln!("The Windows command sandbox is only available on Windows.");
        125
    };
    // run() has already dropped native handles and completed/deferred cleanup.
    // Keep the full Windows exit status instead of narrowing it to eight bits.
    std::process::exit(code as i32);
}
