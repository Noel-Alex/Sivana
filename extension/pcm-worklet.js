// Sivana PCM capture AudioWorklet processor.
//
// Collects mono render-quantum samples and hands them to the main thread
// in larger chunks (~1024 samples) to keep postMessage overhead low.
// The main thread feeds the chunks into the sivana-wasm fingerprinter;
// raw audio never leaves the page.
//
// Usage (main thread):
//   await audioContext.audioWorklet.addModule("pcm-worklet.js");
//   const node = new AudioWorkletNode(audioContext, "sivana-pcm-capture");
//   node.port.onmessage = (e) => engine.process(e.data);
//   source.connect(node);

class SivanaPcmCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this._buf = new Float32Array(1024);
    this._fill = 0;
  }

  process(inputs) {
    const input = inputs[0];
    if (input && input.length > 0) {
      // Average downmix to mono.
      const ch = input[0];
      for (let i = 0; i < ch.length; i++) {
        let s = ch[i];
        for (let c = 1; c < input.length; c++) s += input[c][i];
        this._buf[this._fill++] = s / input.length;
        if (this._fill === this._buf.length) {
          this.port.postMessage(this._buf.slice(0));
          this._fill = 0;
        }
      }
    }
    return true;
  }
}

registerProcessor("sivana-pcm-capture", SivanaPcmCapture);
