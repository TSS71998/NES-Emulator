const GAME = "moonlight.nes";
const TARGET_LATENCY = 0.05;

const A = 1, B = 2, SELECT = 4, START = 8, UP = 16, DOWN = 32, LEFT = 64, RIGHT = 128;

const KEYS = {
  ArrowUp: [0, UP], ArrowDown: [0, DOWN], ArrowLeft: [0, LEFT], ArrowRight: [0, RIGHT],
  KeyA: [0, A], KeyS: [0, B], Enter: [0, START], Space: [0, SELECT],
  KeyI: [1, UP], KeyK: [1, DOWN], KeyJ: [1, LEFT], KeyL: [1, RIGHT],
  KeyG: [1, A], KeyF: [1, B], ShiftRight: [1, START], ControlRight: [1, SELECT],
};

const $ = (id) => document.getElementById(id);
const canvas = $("canvas");
const ctx = canvas.getContext("2d");
const playButton = $("play");

let wasm = null;
let running = false;
let audio = null;
let audioFill = 0;
const keyboard = [0, 0];

async function loadWasm() {
  const response = await fetch("emulator.wasm");
  if (!response.ok) throw new Error(`couldn't download emulator.wasm (${response.status})`);
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
  return instance.exports;
}

function memory() {
  return wasm.memory.buffer;
}

function startRom(bytes) {
  const ptr = wasm.rom_buffer(bytes.length);
  new Uint8Array(memory(), ptr, bytes.length).set(bytes);
  const sampleRate = audio ? audio.context.sampleRate : 48000;
  if (wasm.load(sampleRate) !== 0) {
    const text = new TextDecoder().decode(new Uint8Array(memory(), wasm.error_ptr(), wasm.error_len()));
    throw new Error(text);
  }
  audio?.node.port.postMessage("clear");
  frameTime = 1000 / wasm.frames_per_second();
  lag = 0;
}

async function startAudio() {
  const AudioContext = window.AudioContext || window.webkitAudioContext;
  if (!AudioContext) return null;
  const context = new AudioContext({ latencyHint: "interactive" });
  if (!context.audioWorklet) {
    status("This browser can't play the emulator's sound here (it needs an https page).");
    return null;
  }
  await context.audioWorklet.addModule("audio-worklet.js");
  const node = new AudioWorkletNode(context, "nes-audio", {
    numberOfInputs: 0,
    outputChannelCount: [1],
    processorOptions: { targetSamples: Math.round(TARGET_LATENCY * context.sampleRate) },
  });
  node.port.onmessage = (event) => { audioFill = event.data; };
  const gain = context.createGain();
  node.connect(gain).connect(context.destination);
  await context.resume();
  return { context, node, gain };
}

const volume = $("volume");
const muteButton = $("mute");
let muted = false;
try {
  volume.value = localStorage.getItem("volume") ?? volume.value;
  muted = localStorage.getItem("muted") === "1";
} catch {}

function applyVolume() {
  muteButton.textContent = muted ? "Unmute" : "Mute";
  if (audio) {
    audio.gain.gain.value = muted ? 0 : (volume.value / 100) ** 2;
  }
  try {
    localStorage.setItem("volume", volume.value);
    localStorage.setItem("muted", muted ? "1" : "0");
  } catch {}
}
volume.addEventListener("input", () => { muted = false; applyVolume(); });
muteButton.addEventListener("click", () => { muted = !muted; applyVolume(); });
applyVolume();

function typing(event) {
  return event.target instanceof HTMLInputElement && event.target.type !== "range";
}

window.addEventListener("keydown", (event) => {
  if (!running || typing(event)) return;
  if (event.ctrlKey && event.code === "KeyR") {
    event.preventDefault();
    wasm.reset();
    return;
  }
  const key = KEYS[event.code];
  if (key) {
    event.preventDefault();
    keyboard[key[0]] |= key[1];
  }
});

window.addEventListener("keyup", (event) => {
  const key = KEYS[event.code];
  if (key) keyboard[key[0]] &= ~key[1];
});

window.addEventListener("blur", () => { keyboard[0] = keyboard[1] = 0; });

function gamepadButtons(pad) {
  const held = (i) => pad.buttons[i]?.pressed;
  let bits = 0;
  if (held(1) || held(3)) bits |= A;
  if (held(0) || held(2)) bits |= B;
  if (held(8)) bits |= SELECT;
  if (held(9)) bits |= START;
  if (held(12) || pad.axes[1] < -0.5) bits |= UP;
  if (held(13) || pad.axes[1] > 0.5) bits |= DOWN;
  if (held(14) || pad.axes[0] < -0.5) bits |= LEFT;
  if (held(15) || pad.axes[0] > 0.5) bits |= RIGHT;
  return bits;
}

function readControllers() {
  const players = [keyboard[0], keyboard[1]];
  const pads = navigator.getGamepads ? [...navigator.getGamepads()].filter(Boolean) : [];
  pads.slice(0, 2).forEach((pad, player) => { players[player] |= gamepadButtons(pad); });
  players.forEach((bits, player) => {
    if ((bits & (LEFT | RIGHT)) === (LEFT | RIGHT)) bits &= ~(LEFT | RIGHT);
    if ((bits & (UP | DOWN)) === (UP | DOWN)) bits &= ~(UP | DOWN);
    wasm.set_buttons(player, bits);
  });
}

let frameTime = 1000 / 60.0988;
let lag = 0;
let last = 0;

function runFrame() {
  readControllers();
  wasm.set_audio_fill(audioFill);
  wasm.run_frame();
  const length = wasm.audio_len();
  if (audio && length > 0) {
    audio.node.port.postMessage(new Float32Array(memory(), wasm.audio_ptr(), length).slice());
  }
}

function draw() {
  const picture = new ImageData(new Uint8ClampedArray(memory(), wasm.frame_ptr(), 256 * 240 * 4), 256, 240);
  ctx.putImageData(picture, -8, -8, 8, 8, 240, 224);
}

function tick(now) {
  if (!running) return;
  lag += Math.min(now - last, 250);
  last = now;
  let frames = 0;
  while (lag >= frameTime && frames < 4) {
    runFrame();
    lag -= frameTime;
    frames++;
  }
  if (frames === 4) lag = 0;
  if (frames > 0) draw();
  requestAnimationFrame(tick);
}

function begin() {
  running = true;
  last = performance.now();
  lag = frameTime;
  requestAnimationFrame(tick);
}

document.addEventListener("visibilitychange", () => {
  if (!audio) return;
  if (document.hidden) audio.context.suspend();
  else audio.context.resume();
});

function status(text) {
  $("status").textContent = text;
}

function showError(text) {
  const error = $("error");
  error.textContent = text;
  error.hidden = false;
}

async function play() {
  playButton.disabled = true;
  try {
    try {
      audio = await startAudio();
    } catch (error) {
      console.warn("no sound:", error);
      status("Sound couldn't start in this browser; the game runs without it.");
    }
    applyVolume();
    const response = await fetch(GAME);
    if (!response.ok) throw new Error(`couldn't download ${GAME} (${response.status})`);
    startRom(new Uint8Array(await response.arrayBuffer()));
    $("overlay").hidden = true;
    begin();
  } catch (error) {
    showError(`Couldn't start the game: ${error.message}`);
    playButton.disabled = false;
  }
}

async function loadFile(file) {
  if (!file) return;
  if (!audio) {
    try { audio = await startAudio(); } catch {}
    applyVolume();
  }
  try {
    startRom(new Uint8Array(await file.arrayBuffer()));
    $("overlay").hidden = true;
    status(`Playing ${file.name}`);
    if (!running) begin();
  } catch (error) {
    status(`Can't play ${file.name}: ${error.message}`);
  }
}

playButton.addEventListener("click", play);
$("reset").addEventListener("click", () => running && wasm.reset());
$("fullscreen").addEventListener("click", () => $("screen").requestFullscreen?.());
$("rom-file").addEventListener("change", (event) => loadFile(event.target.files[0]));

const screen = $("screen");
screen.addEventListener("dragover", (event) => { event.preventDefault(); screen.classList.add("dragging"); });
screen.addEventListener("dragleave", () => screen.classList.remove("dragging"));
screen.addEventListener("drop", (event) => {
  event.preventDefault();
  screen.classList.remove("dragging");
  loadFile(event.dataTransfer.files[0]);
});

try {
  wasm = await loadWasm();
  playButton.textContent = "▶ Play";
  playButton.disabled = false;
} catch (error) {
  playButton.textContent = "Unavailable";
  showError(`The emulator couldn't load: ${error.message}`);
}