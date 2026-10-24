//! Finding the emulator and running a game in it, as a separate process.

use crate::config::GameRecord;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 16 * 1024;

pub fn find_emulator(configured: Option<&Path>) -> Option<PathBuf> {
    match configured {
        Some(path) => path.is_file().then(|| path.to_path_buf()),
        None => candidates().into_iter().find(|p| p.is_file())
    }
}

fn candidates() -> Vec<PathBuf> {
    let exe = format!("nes_emulator{}", std::env::consts::EXE_SUFFIX);
    let mut roots = Vec::new();
    let mut places = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        places.push(dir.join(&exe));
        roots.extend(dir.ancestors().skip(1).take(4).map(Path::to_path_buf));
    }

    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."));
    for profile in ["release", "debug"] {
        places.extend(
            roots
                .iter()
                .map(|r| r.join("target").join(profile).join(&exe))
        );
    }
    places
}

pub fn emulator_args(rom: &Path, record: &GameRecord) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(region) = record.region {
        args.extend(["--region".into(), region.arg().into()]);
    }
    if let Some(console) = record.console {
        args.extend(["--console".into(), console.arg().into()]);
    }
    if record.no_oam_corruption {
        args.push("--no-oam-corruption".into());
    }
    args.push(
        std::path::absolute(rom)
        .unwrap_or_else(|_| rom.to_path_buf())
        .into()
    );
    args
}

pub struct Running {
    pub key: String,
    pub title: String,
    child: Child,
    started: Instant,
    output: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>
}

pub struct Finished {
    pub key: String,
    pub title: String,
    pub played: Duration,
    pub status: ExitStatus,
    pub output: String
}

impl Running {
    pub fn start(
        emulator: &Path,
        args: Vec<OsString>,
        key: String,
        title: String
    ) -> std::io::Result<Running> {
        let mut command = Command::new(emulator);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let output = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(collect(stdout, output.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(collect(stderr, output.clone()));
        }
        Ok(Running {
            key,
            title,
            child,
            started: Instant::now(),
            output,
            readers
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn poll(&mut self) -> Option<Finished> {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return None,
            Err(_) => self.child.wait().ok()?
        };
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        let output = std::mem::take(&mut *self.output.lock().unwrap());
        Some(Finished {
            key: self.key.clone(),
            title: self.title.clone(),
            played: self.started.elapsed(),
            status,
            output
        })
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
    }
}

fn collect(pipe: impl Read + Send + 'static, output: Arc<Mutex<String>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            let mut output = output.lock().unwrap();
            output.push_str(&line);
            output.push('\n');
            if output.len() > MAX_OUTPUT {
                let mut cut = output.len() - MAX_OUTPUT;
                while !output.is_char_boundary(cut) {
                    cut += 1;
                }
                output.drain(..cut);
            }
        }
    })
}


#[cfg(test)]
mod test {
    use super::*;
    use crate::library::{Console, Region};

    #[test]
    fn test_args() {
        let rom = std::env::temp_dir().join("Game (Europe).nes");
        assert_eq!(
            emulator_args(&rom, &GameRecord::default()),
            [rom.as_os_str()]
        );
        let record = GameRecord {
            region: Some(Region::Dendy),
            console: Some(Console::Famicom),
            no_oam_corruption: true,
            ..Default::default()
        };
        let args: Vec<_> = emulator_args(&rom, &record)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--region",
                "dendy",
                "--console",
                "famicom",
                "--no-oam-corruption",
                &rom.to_string_lossy()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_session_reports_output_and_status() {
        let script = std::env::temp_dir().join(format!("fake_emulator_{}.sh", std::process::id()));
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"started $1\"\necho 'error: mapper 5' >&2\nexit 1\n",
        )
        .unwrap();
        let mut running = Running::start(
            Path::new("/bin/sh"),
            vec![script.clone().into(), "game.nes".into()],
            "k".into(),
            "Game".into(),
        )
        .unwrap();
        let finished = loop {
            if let Some(f) = running.poll() {
                break f;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!finished.status.success());
        assert!(finished.output.contains("started game.nes"));
        assert!(finished.output.contains("error: mapper 5"));
        std::fs::remove_file(script).unwrap();
    }
}