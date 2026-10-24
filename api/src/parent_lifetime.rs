use std::io::{self, Read};
use tokio::sync::oneshot;

/// The desktop owns stdin's write end. EOF requests graceful shutdown on every
/// platform, including Windows where a process "SIGTERM" is a hard termination.
/// CLI/server launches retain their normal signal lifecycle unless opted in.
pub fn watch() -> io::Result<Option<oneshot::Receiver<()>>> {
    if std::env::var("OPENHARNESS_EXIT_ON_STDIN_EOF").as_deref() != Ok("1") {
        return Ok(None);
    }
    let (sender, receiver) = oneshot::channel();
    // Do not use tokio's blocking stdin task: an open terminal can otherwise
    // hold runtime shutdown open after SIGINT/SIGTERM. This thread is detached.
    std::thread::Builder::new()
        .name("desktop-lifetime".into())
        .spawn(move || {
            wait_for_close(&mut io::stdin().lock());
            let _ = sender.send(());
        })?;
    Ok(Some(receiver))
}

fn wait_for_close(reader: &mut impl Read) {
    let mut buffer = [0; 256];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => (), // Data is not a command channel; only closure matters.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
            Err(_) => return,
        }
    }
}
