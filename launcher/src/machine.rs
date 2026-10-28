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
use nes_emulator::input::{Input, PadLayout};
use nes_emulator::joypad::JoypadButton;
use nes_emulator::nes::{FRAME_HEIGHT, FRAME_WIDTH, Nes, Options};
use nes_emulator::region::{Console, Region};
use sdl2::event::Event;
use sdl2::controller::{Axis, Button};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError,  RecvTimeoutError};
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
    Pause(bool),
    Speed(u32),
    SaveState { slot: usize, path: PathBuf },
    LoadState { slot: usize, path: PathBuf },
    Keyboard([JoypadButton; 2], bool),
    PadLayouts(Box<[PadLayout; 2]>),
    Volume(f32)
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
    },
    StateSaved {
        slot: usize,
        result: Result<(), String>
    },
    StateLoaded {
        slot: usize,
        result: Result<(), String>
    },
    Gamepad {
        button: Button,
        pressed: bool
    }
}

const IDLE_POLL: Duration = Duration::from_millis(10);

const STICK_PRESS: i16 = 20000;
const STICK_RELEASE: i16 = 12000;

pub struct Screen {
    pub picture: Vec<u8>,
    pub frame: u64
}

pub struct Machine {
    commands: Sender<Command>,
    reports: Receiver<Report>,
    pub screen: Arc<Mutex<Screen>>
}

impl Machine {
    pub fn start(ctx: egui::Context) -> Machine {
        let (commands, command_receiver) = mpsc::channel();
        let (report_sender, reports) = mpsc::channel();
        let screen = Arc::new(Mutex::new(Screen {
            picture: vec![0; FRAME_HEIGHT * FRAME_WIDTH * 3],
            frame: 0
        }));
        let thread_screen = screen.clone();
        std::thread::Builder::new()
            .name("machine".into())
            .spawn(move || {
                let mut thread = MachineThread::new(ctx, thread_screen, report_sender);
                loop {
                    match command_receiver.recv_timeout(IDLE_POLL) {
                        Ok(Command::Start(game)) => thread.run(*game, &command_receiver),
                        Ok(command) => thread.settings_command(command),
                        Err(RecvTimeoutError::Timeout) => thread.poll_events(),
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("couldn't start the emulator thread");
        Machine {
            commands,
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

    pub fn set_speed(&self, frames: u32) {
        let _ = self.commands.send(Command::Speed(frames));
    }

    pub fn save_state(&self, slot: usize, path: PathBuf) {
        let _ = self.commands.send(Command::SaveState { slot, path });
    }

    pub fn load_state(&self, slot: usize, path: PathBuf) {
        let _ = self.commands.send(Command::LoadState { slot, path });
    }

    pub fn set_keyboard(&self, buttons: [JoypadButton; 2], microphone: bool) {
        let _ = self.commands.send(Command::Keyboard(buttons, microphone));
    }

    pub fn set_pad_layouts(&self, layouts: [PadLayout; 2]) {
        let _ = self.commands.send(Command::PadLayouts(Box::new(layouts)));
    }

    pub fn set_volume(&self, gain: f32) {
        let _ = self.commands.send(Command::Volume(gain));
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
    sdl_error: Option<String>,
    pad_reporter: Rc<PadReporter>,
    volume: Rc<Cell<f32>>
}

impl MachineThread {
    fn new(
        ctx: egui::Context,
        screen: Arc<Mutex<Screen>>,
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

        let pad_reporter = Rc::new(PadReporter {
            reports: reports.clone(),
            ctx: ctx.clone(),
            stick: Cell::new([false; 4])
        });

        MachineThread {
            ctx,
            screen,
            reports,
            audio,
            input: Rc::new(RefCell::new(Input::new(controllers))),
            events: Rc::new(RefCell::new(events)),
            sdl,
            sdl_error,
            pad_reporter,
            volume: Rc::new(Cell::new(1.0))
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
        let muted = Rc::new(Cell::new(false));
        match self.open_audio() {
            Ok(mut output) => {
                let muted = muted.clone();
                let volume = self.volume.clone();
                let mut scaled = Vec::new();
                nes.set_audio_output(output.sample_rate(), move |samples| {
                    if muted.get() {
                        return 1.0;
                    }
                    let volume = volume.get();
                    if volume == 1.0 {
                        return output.push(samples);
                    }
                    scaled.clear();
                    scaled.extend(samples.iter().map(|sample| sample * volume));
                    output.push(&scaled)
                });
            }
            Err(e) => warning = warning.or(Some(format!("No Sound: {e}")))
        }
        nes.set_input(self.input_hook());

        let save_path = game.path.with_extension("sav");
        let mut saved = nes.battery_ram();
        let mut frames_since_save = 0;
        let mut limiter = FrameLimiter::new(nes.frames_per_second());
        let frame_time = Duration::from_secs_f64(1.0 / nes.frames_per_second());
        let mut speed = 1;
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
                    match commands.recv_timeout(IDLE_POLL) {
                        Err(RecvTimeoutError::Timeout) => {
                            self.poll_events();
                            continue;
                        }
                        command => command.map_err(|_| TryRecvError::Disconnected),
                    }
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
                    Ok(Command::Speed(frames)) => {
                        speed = frames;
                        muted.set(frames != 1);
                    }
                    Ok(Command::SaveState { slot, path }) => {
                        let result = write_state(&path, &nes.save_state());
                        let _ = self.reports.send(Report::StateSaved { slot, result });
                    }
                    Ok(Command::LoadState { slot, path }) => {
                        let result = std::fs::read(&path)
                            .map_err(|e| format!("can't read the save state: {e}"))
                            .and_then(|data| nes.load_state(&data));
                        if result.is_ok() {
                            self.show(nes.picture());
                        }
                        let _ = self.reports.send(Report::StateLoaded { slot, result });
                    }
                    Ok(command @ (Command::Keyboard(..) | Command::PadLayouts(_) | Command::Volume(_))) => {
                        self.settings_command(command)
                    }
                    Ok(Command::Start(_)) => {},
                    Err(TryRecvError::Empty) => break,
                }
            }
           
            frames_since_save += run_frames(&mut nes, speed, frame_time);
            self.show(nes.picture());

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

    fn settings_command(&self, command: Command) {
        let mut input = self.input.borrow_mut();
        match command {
            Command::Keyboard(buttons, microphone) => input.set_keyboard(buttons, microphone),
            Command::PadLayouts(layouts) => input.set_pad_layouts(*layouts),
            Command::Volume(gain) => self.volume.set(gain),
            _ => {}
        }
    }

    fn poll_events(&self) {
        if let Some(events) = self.events.borrow_mut().as_mut() {
            let mut input = self.input.borrow_mut();
            for event in events.poll_iter() {
                input.handle_event(&event);
                self.pad_reporter.event(&event);
            }
        }
    }

    fn show(&self, picture: &[u8]) {
        {
            let mut screen = self.screen.lock().unwrap();
            screen.picture.copy_from_slice(picture);
            screen.frame += 1;
        }
        self.ctx.request_repaint();
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
        let pad_reporter = self.pad_reporter.clone();
        move |joypads| {
            let mut input = input.borrow_mut();
            if let Some(events) = events.borrow_mut().as_mut() {
                for event in events.poll_iter() {
                    input.handle_event(&event);
                    pad_reporter.event(&event);
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

fn run_frames(nes: &mut Nes, speed: u32, frame_time: Duration) -> u32 {
    if speed > 0 {
        for _ in 0..speed {
            nes.run_frame();
        }
        return  speed;
    }
    let until = Instant::now() + frame_time;
    let mut frames = 0;
    while frames == 0 || Instant::now() < until {
        nes.run_frame();
        frames += 1;
    }
    frames
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

fn write_state(path: &Path, state: &[u8]) -> Result<(), String> {
    let error = |e: std::io::Error| format!("can't write the save state: {e}");
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(error)?;
    }
    let new = path.with_extension("new");
    std::fs::write(&new, state).map_err(error)?;
    std::fs::rename(&new, path).map_err(error)
}

struct PadReporter {
    reports: Sender<Report>,
    ctx: egui::Context,
    stick: Cell<[bool; 4]>
}

impl PadReporter {
    fn event(&self, event: &Event) {
        match *event {
            Event::ControllerButtonDown { button, .. } => self.send(button, true),
            Event::ControllerButtonUp { button, .. } => self.send(button, false),
            Event::ControllerAxisMotion { axis, value, .. } => {
                let (first, negative, positive) = match axis {
                    Axis::LeftY => (0, Button::DPadUp, Button::DPadDown),
                    Axis::LeftX => (2, Button::DPadLeft, Button::DPadRight),
                    _ => return,
                };
                let mut stick = self.stick.get();
                for (index, button, push) in [(first, negative, -value), (first + 1, positive, value)] {
                    let held = if stick[index] {
                        push > STICK_RELEASE
                    } else {
                        push >= STICK_PRESS
                    };
                    if held != stick[index] {
                        stick[index] = held;
                        self.send(button, held);
                    }
                }
                self.stick.set(stick);
            }
            _ => {}
        }
    }

    fn send(&self, button: Button, pressed: bool) {
        let _ = self.reports.send(Report::Gamepad { button, pressed });
        self.ctx.request_repaint();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn stick(reporter: &PadReporter, axis: Axis, value: i16) {
        reporter.event(&Event::ControllerAxisMotion { timestamp: 0, which: 0, axis, value });
    }

    #[test]
    fn test_left_stick_reports_as_the_d_pad() {
        let (reports, received) = mpsc::channel();
        let reporter = PadReporter { reports, ctx: egui::Context::default(), stick: Cell::new([false; 4]) };
        stick(&reporter, Axis::LeftX, 25000);
        stick(&reporter, Axis::LeftX, 15000); // not back far enough to let go
        stick(&reporter, Axis::LeftX, 0);
        stick(&reporter, Axis::LeftY, -30000);
        stick(&reporter, Axis::RightX, 30000);
        let got: Vec<(Button, bool)> = received
            .try_iter()
            .map(|r| match r {
                Report::Gamepad { button, pressed } => (button, pressed),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            got,
            [(Button::DPadRight, true), (Button::DPadRight, false), (Button::DPadUp, true)]
        );
    }
} 