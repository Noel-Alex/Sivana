// Sivana WASM engine loader (browser side).
//
// Wires an AudioWorklet PCM stream into the Rust fingerprinter compiled
// with wasm-bindgen (crate sivana-wasm). Emits SFP1 binary batches —
// fingerprints only, never audio — to a consumer callback.
//
//   import { SivanaEngine } from "./engine.js";
//   const engine = await SivanaEngine.create("./sivana_wasm_bg.wasm");
//   engine.onBatch = (bytes) => socket.send(bytes);
//
// The wasm module is expected to be built via wasm-bindgen; see
// crates/sivana-wasm.

const BATCH_MAGIC = [0x53, 0x46, 0x50, 0x31]; // "SFP1"

export class SivanaEngine {
  /**
   * @param {WebAssembly.Module} module compiled sivana-wasm module
   * @param {number} sampleRateHz mono PCM sample rate
   */
  constructor(module, sampleRateHz) {
    this.module = module;
    this.sampleRateHz = sampleRateHz;
    this.onBatch = null; // (Uint8Array) => void
    this.fingerprintCount = 0;
    this._ready = null;
  }

  /** Instantiate from a URL served next to the site. */
  static async create(wasmUrl, sampleRateHz) {
    const instance = await WebAssembly.instantiateStreaming(fetch(wasmUrl), {
      // wasm-bindgen default imports live on __wbg_* keys; none are needed
      // by the fingerprinter itself, so an empty stub env suffices until a
      // bindgen-generated glue file is wired in the website build.
      wbg: {},
    });
    const engine = new SivanaEngine(instance.instance, sampleRateHz);
    return engine;
  }

  /** Feed one mono PCM chunk (Float32Array). */
  process(pcm) {
    // The bindgen wrapper exposes process(pcm_ptr, len) semantics through
    // its exported memory API; the website build wires this call via the
    // generated glue. Kept as the single integration point.
    if (!this.exports) {
      this.exports = this.module.exports;
    }
    const bytes = this.exports.process(pcm);
    if (bytes && bytes.length > 16 && bytes[0] === BATCH_MAGIC[0]) {
      this.fingerprintCount += new DataView(
        bytes.buffer,
        bytes.byteOffset + 12,
        4
      ).getUint32(0, true);
      if (this.onBatch) this.onBatch(bytes);
    }
  }

  /** End of capture; flush trailing fingerprints. */
  finish() {
    if (this.exports) {
      const bytes = this.exports.finish();
      if (bytes && bytes.length > 16 && this.onBatch) this.onBatch(bytes);
    }
  }
}
