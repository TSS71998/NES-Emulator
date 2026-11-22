class NesAudio extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.target = options.processorOptions.targetSamples;
    this.queue = new Float32Array(Math.ceil(sampleRate));
    this.start = 0;
    this.length = 0;
    this.port.onmessage = (event) => this.push(event.data);
  }

  push(samples) {
    if (samples === "clear") {
      this.length = 0;
      return;
    }
    if (this.length === 0) {
      this.write(new Float32Array(Math.max(0, this.target - samples.length)));
    } else if (this.length + samples.length > this.target * 3) {
      const drop = this.length + samples.length - this.target;
      this.start = (this.start + Math.min(drop, this.length)) % this.queue.length;
      this.length -= Math.min(drop, this.length);
    }
    this.write(samples);
    this.port.postMessage(this.length);
  }

  write(samples) {
    const size = this.queue.length;
    for (let i = 0; i < samples.length && this.length < size; i++) {
      this.queue[(this.start + this.length) % size] = samples[i];
      this.length++;
    }
  }

  process(_inputs, outputs) {
    const out = outputs[0][0];
    const size = this.queue.length;
    for (let i = 0; i < out.length; i++) {
      if (this.length > 0) {
        out[i] = this.queue[this.start];
        this.start = (this.start + 1) % size;
        this.length--;
      } else {
        out[i] = 0;
      }
    }
    return true;
  }
}

registerProcessor("nes-audio", NesAudio);