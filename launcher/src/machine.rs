//! Running games inside the launcher. The emulator runs on its own thread
//! (the "machine" thread), paced to the console's frame rate, and hands
//! each finished picture to the window. That thread also owns SDL, which
//! plays the sound and reads gamepads with the emulator's own `AudioOutput`
//! and `Input`; the window passes its key presses on as SDL key events, so
//! the keys are the same as in the standalone emulator.
//!
//! One thread for the whole run of the launcher: SDL may only be started
//! from one thread.

use eframe::egui;
use nes_emulator::audio::AudioOutput;
use nes_emulator::frame_limiter::FrameLimiter;
use nes_emulator::input::Input;
use nes_emulator::nes::{FRAME_HEIGHT, FRAME_WIDTH, Nes, Options};
use nes_emulator::region::{Console, Region};
use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Mod};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SAVE_INTERVAL_FRAMES: u32 = 300;

pub struct StartGame {
    pub key: String,
    pub path: PathBuf,
    pub region: Option<Region>,
    pub console: Option<Console>,
    pub oam_row_corruption: bool
}

enum Command {
    Start(Box<StartGame>),
    Stop,
    Reset,
    Pause(bool)
}

pub enum Report {
    Started {
        key: String,
        warning: Option<String>
    },
    Stopped {
        key: String,
        played: Duration
    },
    Failed {
        key: String,
        error: String
    }
}

pub struct Screen {
    pub picture: Vec<u8>,
    pub frame: u64
}

pub struct Machine {
    commands: Sender<Command>,
    keys: Sender<(Keycode, bool)>,
    reports: Receiver<Report>,
    pub screen: Arc<Mutex<Screen>>
}

impl Machine {
    pub fn start(ctx: egui::Context) -> Machine {
        let (commands, command_receiver) = mpsc::channel();
        let (keys, key_receiver) = mpsc::channel();
        let (report_sender, reports) = mpsc::channel();
        let screen = Arc::new(Mutex::new(Screen {
            picture: vec![0; FRAME_HEIGHT * FRAME_WIDTH * 3],
            frame: 0
        }));
        let thread_screen = screen.clone();
        std::thread::Builder::new()
            .name("machine".into())
            .spawn(move || {
                let mut thread = MachineThread::new(ctx, thread_screen, key_receiver, report_sender);
                while let Ok(command) = command_receiver.recv() {
                    if let Command::Start(game) = command {
                        thread.run(*game, &command_receiver);
                    }
                }
            })
            .expect("couldn't start the emulator thread");
        Machine {
            commands,
            keys,
            reports,
            screen
        }
    }

    pub fn play(&self, game: StartGame) {
        let _ = self.commands.send(Command::Start(Box::new(game)));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    pub fn reset(&self) {
        let _ = self.commands.send(Command::Reset);
    }

    pub fn pause(&self, paused: bool) {
        let _ = self.commands.send(Command::Pause(paused));
    }

    pub fn key(&self, key: Keycode, down: bool) {
        let _ = self.keys.send((key, down));
    }

    pub fn poll(&self) -> Option<Report> {
        self.reports.try_recv().ok()
    }

    pub fn wait(&self, timeout: Duration) -> Option<Report> {
        self.reports.recv_timeout(timeout).ok()
    }
}

struct MachineThread {
    ctx: egui::Context,
    screen: Arc<Mutex<Screen>>,
    reports: Sender<Report>,
    sdl: Option<sdl2::Sdl>,
    audio: Option<sdl2::AudioSubsystem>,
    input: Rc<RefCell<Input>>,
    events: Rc<RefCell<Option<sdl2::EventPump>>>,
    keys: Rc<Receiver<(Keycode, bool)>>,
    sdl_error: Option<String>
}

impl MachineThread {
    fn new(
        ctx: egui::Context,
        screen: Arc<Mutex<Screen>>,
        keys: Receiver<(Keycode, bool)>,
        reports: Sender<Report>,
    ) -> MachineThread {
        sdl2::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        let (sdl, sdl_error) = match sdl2::init() {
            Ok(sdl) => (Some(sdl), None),
            Err(e) => (None, Some(e))
        };
        let audio = sdl.as_ref().and_then(|s| s.audio().ok());
        let controllers = sdl.as_ref().and_then(|s| s.game_controller().ok());
        let events = sdl.as_ref().and_then(|s| s.event_pump().ok());
        MachineThread {
            ctx,
            screen,
            reports,
            audio,
            input: Rc::new(RefCell::new(Input::new(controllers))),
            events: Rc::new(RefCell::new(events)),
            keys: Rc::new(keys),
            sdl,
            sdl_error
        }
    }

    fn run(&mut self, game: StartGame, commands: &Receiver<Command>) {
        let key = game.key.clone();
        let mut nes = match self.load(&game) {
            Ok(nes) => nes,
            Err(error) => {
                let _ = self.reports.send(Report::Failed { key, error });
                return;
            }
        };

        let mut warning = self.sdl_error.as_ref().map(|e| format!("No sound or gamepad: {e}"));
        match self.open_audio() {
            Ok(mut output) => {
                nes.set_audio_output(output.sample_rate(), move |samples| output.push(samples));
            }
            Err(e) => warning = warning.or(Some(format!("No Sound: {e}")))
        }
        nes.set_input(self.input_hook());

        let save_path = game.path.with_extension("sav");
        let mut saved = nes.battery_ram();
        let mut frames_since_save = 0;
        let mut limiter = FrameLimiter::new(nes.frames_per_second());
        let started = Instant::now();
        let _ = self.reports.send(Report::Started {
            key: key.clone(),
            warning
        });

        let mut paused_since: Option<Instant> = None;
        let mut paused_for = Duration::ZERO;
        'game: loop {
            loop {
                let command = if paused_since.is_some() {
                    commands.recv().map_err(|_| TryRecvError::Disconnected)
                } else {
                    commands.try_recv()
                };
                match command {
                    Ok(Command::Stop) | Err(TryRecvError::Disconnected) => break 'game,
                    Ok(Command::Reset) => nes.reset(),
                    Ok(Command::Pause(true)) => {
                        if paused_since.is_none() {
                            paused_since = Some(Instant::now());
                            save_battery(&nes, &save_path, &mut saved);
                        }
                    }
                    Ok(Command::Pause(false)) => {
                        if let Some(since) = paused_since.take() {
                            paused_for += since.elapsed();
                        }
                    }
                    Ok(Command::Start(_)) => {},
                    Err(TryRecvError::Empty) => break,
                }
            }
            let picture = nes.run_frame();
            {
                let mut screen = self.screen.lock().unwrap();
                screen.picture.copy_from_slice(picture);
                screen.frame += 1;
            }
            self.ctx.request_repaint();

            frames_since_save += 1;
            if frames_since_save >= SAVE_INTERVAL_FRAMES {
                frames_since_save = 0;
                save_battery(&nes, &save_path, &mut saved);
            }
            limiter.wait();
        }
        if let Some(since) = paused_since {
            paused_for += since.elapsed();
        }
        save_battery(&nes, &save_path, &mut saved);
        let _ = self.reports.send(Report::Stopped {
            key,
            played: started.elapsed().saturating_sub(paused_for)
        });
    }

    fn load(&self, game: &StartGame) -> Result<Nes, String> {
        let file = std::fs::read(&game.path)
            .map_err(|e| format!("can't read {}: {e}", game.path.display()))?;
        let file_name = game
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let options = Options {
            region: game.region,
            console: game.console,
            oam_row_corruption: game.oam_row_corruption,
            battery_ram: std::fs::read(game.path.with_extension("sav")).ok()
        };
        Nes::new(&file, &file_name, options)
    }

    fn open_audio(&self) -> Result<AudioOutput, String> {
        let audio = self.audio.as_ref().ok_or("SDL audio isn't available")?;
        let desired = sdl2::audio::AudioSpecDesired {
            freq: Some(44100),
            channels: Some(1),
            samples: None
        };
        audio.open_playback(None, &desired, |_| Silence)?;
        Ok(AudioOutput::new(audio))
    }

    fn input_hook(&self) -> impl FnMut(&mut [nes_emulator::joypad::Joypad; 2]) + 'static {
        let input = self.input.clone();
        let events = self.events.clone();
        let keys = self.keys.clone();
        move |joypads| {
            let mut input = input.borrow_mut();
            for (key, down) in keys.try_iter() {
                input.handle_event(&key_event(key, down));
            }
            if let Some(events) = events.borrow_mut().as_mut() {
                for event in events.poll_iter() {
                    input.handle_event(&event);
                }
            }
            input.apply(joypads);
        }
    }
}

impl Drop for MachineThread {
    fn drop(&mut self) {
        self.events.borrow_mut().take();
        self.audio.take();
        self.sdl.take();
    }
}

struct Silence;

impl sdl2::audio::AudioCallback for Silence {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        out.fill(0.0);
    }
}

fn key_event(key: Keycode, down: bool) -> Event {
    if down {
        Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(key),
            scancode: None,
            keymod: Mod::NOMOD,
            repeat: false,
        }
    } else {
        Event::KeyUp {
            timestamp: 0,
            window_id: 0,
            keycode: Some(key),
            scancode: None,
            keymod: Mod::NOMOD,
            repeat: false,
        }
    }
}

fn save_battery(nes: &Nes, path: &std::path::Path, saved: &mut Option<Vec<u8>>) {
    let Some(ram) = nes.battery_ram() else {
        return;
    };
    if saved.as_ref() == Some(&ram) {
        return;
    }
    match std::fs::write(path, &ram) {
        Ok(()) => *saved = Some(ram),
        Err(e) => eprintln!("warning: couldn't write {}: {e}", path.display())
    }
}

pub fn emulator_key(key: egui::Key) -> Option<Keycode> {
    use egui::Key as K;
    Some(match key {
        K::ArrowUp => Keycode::Up,
        K::ArrowDown => Keycode::Down,
        K::ArrowLeft => Keycode::Left,
        K::ArrowRight => Keycode::Right,
        K::A => Keycode::A,
        K::S => Keycode::S,
        K::Enter => Keycode::Return,
        K::Space => Keycode::Space,
        K::I => Keycode::I,
        K::K => Keycode::K,
        K::J => Keycode::J,
        K::L => Keycode::L,
        K::G => Keycode::G,
        K::F => Keycode::F,
        K::M => Keycode::M,
        _ => return None,
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_key_events_carry_the_key() {
        assert!(matches!(
            key_event(Keycode::A, true),
            Event::KeyDown {
                keycode: Some(Keycode::A),
                ..
            }
        ));
        assert!(matches!(
            key_event(Keycode::Return, false),
            Event::KeyUp {
                keycode: Some(Keycode::Return),
                ..
            }
        ));
    }

    #[test]
    fn test_player_1_keys() {
        assert_eq!(emulator_key(egui::Key::Enter), Some(Keycode::Return));
        assert_eq!(emulator_key(egui::Key::ArrowLeft), Some(Keycode::Left));
        assert_eq!(emulator_key(egui::Key::Escape), None);
    }
}